use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::io::read_optional_config;

pub(super) const MAX_CONFIG_EXTENDS_DEPTH: usize = 16;

pub(super) type ConfigOrigins = BTreeMap<Vec<ConfigKeyPathSegment>, PathBuf>;

pub(super) struct ConfigTree {
    pub(super) value: toml::Value,
    pub(super) origins: ConfigOrigins,
    pub(super) sources: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum ConfigKeyPathSegment {
    Key(String),
    Index(usize),
}

pub(crate) fn effective_config_contains_section(
    path: &Path,
    section: &str,
) -> Result<bool, String> {
    load_config_tree(path).map(|tree| {
        tree.is_some_and(|tree| {
            tree.value
                .as_table()
                .is_some_and(|table| table.contains_key(section))
        })
    })
}

pub(super) fn load_config_tree(path: &Path) -> Result<Option<ConfigTree>, String> {
    match read_optional_config(path) {
        Ok(None) => Ok(None),
        Ok(Some(content)) => {
            let mut chain = Vec::new();
            load_config_file(path, content, None, &mut chain).map(Some)
        }
        Err(err) => Err(format!("config read error: {}: {err}", path.display())),
    }
}

fn load_config_file(
    path: &Path,
    content: String,
    extended_from: Option<&Path>,
    chain: &mut Vec<(PathBuf, PathBuf)>,
) -> Result<ConfigTree, String> {
    if chain.len() >= MAX_CONFIG_EXTENDS_DEPTH {
        return Err(format!(
            "config inheritance error: {} exceeds the maximum chain depth of {MAX_CONFIG_EXTENDS_DEPTH}{}",
            path.display(),
            inheritance_edge(extended_from)
        ));
    }
    let canonical = std::fs::canonicalize(path).map_err(|err| {
        format!(
            "config read error: {}: {err}{}",
            path.display(),
            inheritance_edge(extended_from)
        )
    })?;
    if let Some(index) = chain
        .iter()
        .position(|(candidate, _)| candidate == &canonical)
    {
        let cycle = chain[index..]
            .iter()
            .map(|(_, source)| source.display().to_string())
            .chain(std::iter::once(path.display().to_string()))
            .collect::<Vec<_>>()
            .join(" -> ");
        return Err(format!("config inheritance error: cycle detected: {cycle}"));
    }
    chain.push((canonical, path.to_path_buf()));

    let mut value = content.parse::<toml::Value>().map_err(|err| {
        format!(
            "config parse error: {}{}: {err}",
            path.display(),
            inheritance_edge(extended_from)
        )
    })?;
    let table = value.as_table_mut().ok_or_else(|| {
        format!(
            "config parse error: {}{}: top-level config must be a table",
            path.display(),
            inheritance_edge(extended_from)
        )
    })?;
    let extends = match table.remove("extends") {
        Some(toml::Value::String(value)) if !value.trim().is_empty() => Some(value),
        Some(_) => {
            return Err(format!(
                "config inheritance error: {}: extends must be one non-empty path string",
                path.display()
            ));
        }
        None => None,
    };

    let mut origins = BTreeMap::new();
    collect_config_origins(&value, &mut Vec::new(), path, &mut origins);
    let mut tree = ConfigTree {
        value,
        origins,
        sources: vec![path.to_path_buf()],
    };
    if let Some(extends) = extends {
        let parent_path = resolve_extended_config_path(path, &extends)?;
        let parent_content = read_optional_config(&parent_path)
            .map_err(|err| {
                format!(
                    "config read error: {}: {err}{}",
                    parent_path.display(),
                    inheritance_edge(Some(path))
                )
            })?
            .ok_or_else(|| {
                format!(
                    "config read error: {}: file not found{}",
                    parent_path.display(),
                    inheritance_edge(Some(path))
                )
            })?;
        let mut parent = load_config_file(&parent_path, parent_content, Some(path), chain)?;
        merge_config_values(&mut parent.value, tree.value);
        parent.origins.append(&mut tree.origins);
        parent.sources.push(path.to_path_buf());
        tree = parent;
    }
    chain.pop();
    Ok(tree)
}

fn inheritance_edge(extended_from: Option<&Path>) -> String {
    extended_from.map_or_else(String::new, |path| {
        format!(" (extended from {})", path.display())
    })
}

fn resolve_extended_config_path(source: &Path, value: &str) -> Result<PathBuf, String> {
    let expanded = if value == "~" {
        config_home_dir().ok_or_else(|| {
            format!(
                "config inheritance error: {}: cannot resolve extends path {value:?} without a home directory",
                source.display()
            )
        })?
    } else if let Some(rest) = value.strip_prefix("~/") {
        config_home_dir()
            .ok_or_else(|| {
                format!(
                    "config inheritance error: {}: cannot resolve extends path {value:?} without a home directory",
                    source.display()
                )
            })?
            .join(rest)
    } else if value.starts_with('~') {
        return Err(format!(
            "config inheritance error: {}: unsupported extends path {value:?}; use ~ or ~/path",
            source.display()
        ));
    } else {
        PathBuf::from(value)
    };
    if expanded.is_absolute() || value.starts_with('~') {
        Ok(expanded)
    } else {
        Ok(source
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(expanded))
    }
}

fn config_home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

pub(super) fn merge_config_values(base: &mut toml::Value, child: toml::Value) {
    match (base, child) {
        (toml::Value::Table(base), toml::Value::Table(child)) => {
            for (key, value) in child {
                if let Some(existing) = base.get_mut(&key) {
                    merge_config_values(existing, value);
                } else {
                    base.insert(key, value);
                }
            }
        }
        (base, child) => *base = child,
    }
}

fn collect_config_origins(
    value: &toml::Value,
    path: &mut Vec<ConfigKeyPathSegment>,
    source: &Path,
    origins: &mut ConfigOrigins,
) {
    if !path.is_empty() {
        origins.insert(path.clone(), source.to_path_buf());
    }
    match value {
        toml::Value::Table(table) => {
            for (key, value) in table {
                path.push(ConfigKeyPathSegment::Key(key.clone()));
                collect_config_origins(value, path, source, origins);
                path.pop();
            }
        }
        toml::Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                path.push(ConfigKeyPathSegment::Index(index));
                collect_config_origins(value, path, source, origins);
                path.pop();
            }
        }
        _ => {}
    }
}
