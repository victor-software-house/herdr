use std::path::{Path, PathBuf};

use tracing::warn;

use super::inheritance::{load_config_tree, ConfigKeyPathSegment, ConfigOrigins, ConfigTree};
#[cfg(test)]
use super::inheritance::{merge_config_values, MAX_CONFIG_EXTENDS_DEPTH};
use super::{model::LoadedConfig, Config, CONFIG_PATH_ENV_VAR};

const KNOWN_TOP_LEVEL_CONFIG_KEYS: &[&str] = &[
    "advanced",
    "experimental",
    "keys",
    "onboarding",
    "remote",
    "server",
    "session",
    "terminal",
    "theme",
    "ui",
    "update",
    "worktrees",
];

pub fn app_dir_name() -> &'static str {
    crate::product::app_dir_name()
}

pub fn config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(dir).join(app_dir_name());
    }
    platform_config_dir()
}

pub fn state_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_STATE_HOME") {
        return PathBuf::from(dir).join(app_dir_name());
    }
    platform_state_dir()
}

#[cfg(windows)]
fn platform_config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("APPDATA") {
        return PathBuf::from(dir).join(app_dir_name());
    }
    if let Ok(profile) = std::env::var("USERPROFILE") {
        return PathBuf::from(profile)
            .join("AppData")
            .join("Roaming")
            .join(app_dir_name());
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(format!(".config/{}", app_dir_name()));
    }
    std::env::temp_dir().join(app_dir_name())
}

#[cfg(not(windows))]
fn platform_config_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(format!(".config/{}", app_dir_name()))
    } else {
        std::env::temp_dir().join(app_dir_name())
    }
}

#[cfg(windows)]
fn platform_state_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("LOCALAPPDATA") {
        return PathBuf::from(dir).join(app_dir_name());
    }
    if let Ok(profile) = std::env::var("USERPROFILE") {
        return PathBuf::from(profile)
            .join("AppData")
            .join("Local")
            .join(app_dir_name());
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(format!(".local/state/{}", app_dir_name()));
    }
    std::env::temp_dir().join(format!("{}-state", app_dir_name()))
}

#[cfg(not(windows))]
fn platform_state_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(format!(".local/state/{}", app_dir_name()))
    } else {
        std::env::temp_dir().join(format!("{}-state", app_dir_name()))
    }
}

/// Normalize UTF-8 byte-order marks in config text.
///
/// TOML tolerates a single BOM at the very start of the document, but a BOM at
/// the start of a later line makes the parser reject the whole file. A
/// line-oriented edit can displace a leading BOM into the middle of the file,
/// so drop line-start BOMs that the TOML parser actually rejects. A U+FEFF that
/// is valid string data is kept, because its parse error would not point at it.
fn normalize_utf8_bom(content: &str) -> String {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    if !content.contains('\u{feff}') {
        return content.to_owned();
    }

    let mut normalized = content.to_owned();
    while let Err(error) = normalized.parse::<toml::Value>() {
        let Some(span) = error.span() else {
            break;
        };
        if normalized.get(span.clone()) != Some("\u{feff}") {
            break;
        }
        normalized.replace_range(span, "");
    }
    normalized
}

pub(super) fn read_optional_config(path: &Path) -> std::io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(Some(normalize_utf8_bom(&content))),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

impl Config {
    pub fn load() -> LoadedConfig {
        let path = config_path();
        let tree = match load_config_tree(&path) {
            Ok(Some(tree)) => tree,
            Ok(None) => {
                return LoadedConfig {
                    config: Self::default(),
                    diagnostics: Vec::new(),
                    invalid_sections: Vec::new(),
                };
            }
            Err(err) => {
                warn!(err = %err, "config load error, using defaults");
                return LoadedConfig {
                    config: Self::default(),
                    diagnostics: vec![format!("{err}; using defaults")],
                    invalid_sections: Vec::new(),
                };
            }
        };

        match deserialize_with_ignored::<Config, _>(tree.value.clone()) {
            Ok((config, ignored_keys)) => {
                let table = tree
                    .value
                    .as_table()
                    .expect("config tree is always a table");
                let origins = (tree.sources.len() > 1).then_some(&tree.origins);
                let (unknown_sections, mut diagnostics) =
                    unknown_top_level_sections(table, origins);
                diagnostics.extend(unknown_config_key_diagnostics(
                    ignored_keys
                        .into_iter()
                        .filter(|path| {
                            !matches!(path.as_slice(), [ConfigKeyPathSegment::Key(key)] if unknown_sections.contains(key))
                        })
                        .collect(),
                    None,
                    origins,
                ));
                diagnostics.extend(config.collect_diagnostics());
                LoadedConfig {
                    config,
                    diagnostics,
                    invalid_sections: Vec::new(),
                }
            }
            Err(err) => {
                let diagnostic = if tree.sources.len() > 1 {
                    let sources = tree
                        .sources
                        .iter()
                        .map(|source| source.display().to_string())
                        .collect::<Vec<_>>()
                        .join(" -> ");
                    format!(
                        "config parse error: effective config from {sources}: {err}; using defaults"
                    )
                } else {
                    format!("config parse error: {err}; using defaults")
                };
                warn!(err = %err, "config parse error, using defaults");
                LoadedConfig {
                    config: Self::default(),
                    diagnostics: vec![diagnostic],
                    invalid_sections: Vec::new(),
                }
            }
        }
    }
}

pub(super) fn resolve_config_relative_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }

    config_path()
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(path)
}

pub fn config_path() -> PathBuf {
    if let Ok(path) = std::env::var(CONFIG_PATH_ENV_VAR) {
        return PathBuf::from(path);
    }
    config_dir().join("config.toml")
}

pub fn config_diagnostic_summary(diagnostics: &[String]) -> Option<String> {
    if diagnostics.is_empty() {
        return None;
    }

    let target = config_path()
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config.toml")
        .to_string();
    let read_error = diagnostics
        .iter()
        .any(|diagnostic| diagnostic.starts_with("config read error:"));
    let impact = if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.contains("using defaults"))
    {
        if read_error {
            " unreadable; using defaults"
        } else {
            " invalid; using defaults"
        }
    } else if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.contains("keeping current config"))
    {
        if read_error {
            " unreadable; keeping current config"
        } else {
            " invalid; keeping current config"
        }
    } else if diagnostics
        .iter()
        .all(|diagnostic| diagnostic.starts_with("unknown config key "))
    {
        " has unknown keys"
    } else {
        ""
    };

    Some(format!(
        "{target}{impact}; {} config check",
        crate::product::BINARY_NAME
    ))
}

pub fn load_live_config() -> Result<LoadedConfig, Vec<String>> {
    let path = config_path();
    match load_config_tree(&path) {
        Ok(Some(tree)) => load_live_config_from_tree(tree),
        Ok(None) => Ok(LoadedConfig {
            config: Config::default(),
            diagnostics: Vec::new(),
            invalid_sections: Vec::new(),
        }),
        Err(err) => Err(vec![format!("{err}; keeping current config")]),
    }
}

#[cfg(test)]
fn load_live_config_from_str(content: &str) -> Result<LoadedConfig, Vec<String>> {
    let value = content
        .parse::<toml::Value>()
        .map_err(|err| vec![format!("config parse error: {err}; keeping current config")])?;
    load_live_config_from_tree(ConfigTree {
        value,
        origins: ConfigOrigins::new(),
        sources: Vec::new(),
    })
}

fn load_live_config_from_tree(tree: ConfigTree) -> Result<LoadedConfig, Vec<String>> {
    let table = tree.value.as_table().ok_or_else(|| {
        vec![
            "config parse error: top-level config must be a table; keeping current config"
                .to_string(),
        ]
    })?;
    let origins = (tree.sources.len() > 1).then_some(&tree.origins);

    let mut config = Config::default();
    let mut diagnostics = unknown_top_level_section_diagnostics(table, origins);
    diagnostics.extend(unknown_top_level_config_key_diagnostics(table, origins));
    let mut invalid_sections = Vec::new();

    if let Some(value) = table.get("onboarding") {
        match value.clone().try_into::<Option<bool>>() {
            Ok(onboarding) => config.onboarding = onboarding,
            Err(err) => {
                let path = vec![ConfigKeyPathSegment::Key("onboarding".to_string())];
                diagnostics.push(format!(
                    "invalid onboarding setting{}: {err}; keeping current onboarding state",
                    config_source_suffix(config_source(origins, &path))
                ));
            }
        }
    }

    load_live_section(
        table,
        "theme",
        "theme config",
        &mut diagnostics,
        &mut invalid_sections,
        origins,
        |section| config.theme = section,
    );
    load_live_section(
        table,
        "keys",
        "keybinding config",
        &mut diagnostics,
        &mut invalid_sections,
        origins,
        |section| config.keys = section,
    );
    load_live_section(
        table,
        "terminal",
        "terminal config",
        &mut diagnostics,
        &mut invalid_sections,
        origins,
        |section| config.terminal = section,
    );
    load_live_section(
        table,
        "session",
        "session config",
        &mut diagnostics,
        &mut invalid_sections,
        origins,
        |section| config.session = section,
    );
    load_live_section(
        table,
        "server",
        "server config",
        &mut diagnostics,
        &mut invalid_sections,
        origins,
        |section| config.server = section,
    );
    load_live_section(
        table,
        "update",
        "update config",
        &mut diagnostics,
        &mut invalid_sections,
        origins,
        |section| config.update = section,
    );
    load_live_section(
        table,
        "ui",
        "ui config",
        &mut diagnostics,
        &mut invalid_sections,
        origins,
        |section| config.ui = section,
    );
    load_live_section(
        table,
        "advanced",
        "advanced config",
        &mut diagnostics,
        &mut invalid_sections,
        origins,
        |section| config.advanced = section,
    );
    load_live_section(
        table,
        "worktrees",
        "worktree config",
        &mut diagnostics,
        &mut invalid_sections,
        origins,
        |section| config.worktrees = section,
    );
    load_live_section(
        table,
        "experimental",
        "experimental config",
        &mut diagnostics,
        &mut invalid_sections,
        origins,
        |section| config.experimental = section,
    );
    load_live_section(
        table,
        "remote",
        "remote config",
        &mut diagnostics,
        &mut invalid_sections,
        origins,
        |section| config.remote = section,
    );

    diagnostics.extend(config.theme.diagnostics());

    Ok(LoadedConfig {
        config,
        diagnostics,
        invalid_sections,
    })
}

fn unknown_top_level_sections(
    table: &toml::map::Map<String, toml::Value>,
    origins: Option<&ConfigOrigins>,
) -> (Vec<String>, Vec<String>) {
    let mut keys = Vec::new();
    let mut diagnostics = Vec::new();
    for (key, value) in table {
        let path = vec![ConfigKeyPathSegment::Key(key.clone())];
        if let Some(diagnostic) =
            unknown_top_level_section_diagnostic(key, value, config_source(origins, &path))
        {
            keys.push(key.clone());
            diagnostics.push(diagnostic);
        }
    }
    (keys, diagnostics)
}

fn unknown_top_level_section_diagnostics(
    table: &toml::map::Map<String, toml::Value>,
    origins: Option<&ConfigOrigins>,
) -> Vec<String> {
    table
        .iter()
        .filter_map(|(key, value)| {
            let path = vec![ConfigKeyPathSegment::Key(key.clone())];
            unknown_top_level_section_diagnostic(key, value, config_source(origins, &path))
        })
        .collect()
}

fn unknown_top_level_section_diagnostic(
    key: &str,
    value: &toml::Value,
    source: Option<&Path>,
) -> Option<String> {
    if KNOWN_TOP_LEVEL_CONFIG_KEYS.contains(&key) {
        return None;
    }

    let header = if value.is_table() {
        format!("[{key}]")
    } else if value
        .as_array()
        .is_some_and(|items| !items.is_empty() && items.iter().all(toml::Value::is_table))
    {
        format!("[[{key}]]")
    } else {
        return None;
    };

    if key == "toast" {
        Some(format!(
            "unknown config section {header}{}; did you mean [ui.toast]? ignoring section",
            config_source_suffix(source)
        ))
    } else {
        Some(format!(
            "unknown config section {header}{}; ignoring section",
            config_source_suffix(source)
        ))
    }
}

fn unknown_top_level_config_key_diagnostics(
    table: &toml::map::Map<String, toml::Value>,
    origins: Option<&ConfigOrigins>,
) -> Vec<String> {
    let paths = table
        .iter()
        .filter(|(key, value)| {
            !KNOWN_TOP_LEVEL_CONFIG_KEYS.contains(&key.as_str())
                && unknown_top_level_section_diagnostic(
                    key,
                    value,
                    config_source(origins, &[ConfigKeyPathSegment::Key(key.to_string())]),
                )
                .is_none()
        })
        .map(|(key, _)| vec![ConfigKeyPathSegment::Key(key.clone())])
        .collect();
    unknown_config_key_diagnostics(paths, None, origins)
}

fn config_key_path(path: &serde_ignored::Path<'_>) -> Vec<ConfigKeyPathSegment> {
    fn visit(path: &serde_ignored::Path<'_>, segments: &mut Vec<ConfigKeyPathSegment>) {
        match path {
            serde_ignored::Path::Root => {}
            serde_ignored::Path::Seq { parent, index } => {
                visit(parent, segments);
                segments.push(ConfigKeyPathSegment::Index(*index));
            }
            serde_ignored::Path::Map { parent, key } => {
                visit(parent, segments);
                segments.push(ConfigKeyPathSegment::Key(key.clone()));
            }
            serde_ignored::Path::Some { parent }
            | serde_ignored::Path::NewtypeStruct { parent }
            | serde_ignored::Path::NewtypeVariant { parent } => visit(parent, segments),
        }
    }

    let mut segments = Vec::new();
    visit(path, &mut segments);
    segments
}

fn format_config_key_path(path: &[ConfigKeyPathSegment]) -> String {
    path.iter()
        .map(|segment| match segment {
            ConfigKeyPathSegment::Key(key)
                if !key.is_empty()
                    && key.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
                    }) =>
            {
                key.clone()
            }
            ConfigKeyPathSegment::Key(key) => toml::Value::String(key.clone()).to_string(),
            ConfigKeyPathSegment::Index(index) => index.to_string(),
        })
        .collect::<Vec<_>>()
        .join(".")
}

fn unknown_config_key_diagnostics(
    paths: Vec<Vec<ConfigKeyPathSegment>>,
    section: Option<&str>,
    origins: Option<&ConfigOrigins>,
) -> Vec<String> {
    let mut paths: Vec<Vec<ConfigKeyPathSegment>> = paths
        .into_iter()
        .map(|mut path| {
            if let Some(section) = section {
                path.insert(0, ConfigKeyPathSegment::Key(section.to_string()));
            }
            path
        })
        .collect();
    paths.sort();
    paths.dedup();
    paths
        .into_iter()
        .map(|path| {
            format!(
                "unknown config key {}{}; ignoring key",
                format_config_key_path(&path),
                config_source_suffix(config_source(origins, &path))
            )
        })
        .collect()
}

fn config_source<'a>(
    origins: Option<&'a ConfigOrigins>,
    path: &[ConfigKeyPathSegment],
) -> Option<&'a Path> {
    let origins = origins?;
    (1..=path.len())
        .rev()
        .find_map(|length| origins.get(&path[..length]).map(PathBuf::as_path))
}

fn config_source_suffix(source: Option<&Path>) -> String {
    source.map_or_else(String::new, |path| format!(" in {}", path.display()))
}

fn deserialize_with_ignored<'de, T, D>(
    deserializer: D,
) -> Result<(T, Vec<Vec<ConfigKeyPathSegment>>), D::Error>
where
    T: serde::Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    let mut ignored = Vec::new();
    let value = serde_ignored::deserialize(deserializer, |path| {
        ignored.push(config_key_path(&path));
    })?;
    Ok((value, ignored))
}

fn load_live_section<T>(
    table: &toml::map::Map<String, toml::Value>,
    section: &'static str,
    label: &str,
    diagnostics: &mut Vec<String>,
    invalid_sections: &mut Vec<String>,
    origins: Option<&ConfigOrigins>,
    apply: impl FnOnce(T),
) where
    T: serde::de::DeserializeOwned,
{
    let Some(value) = table.get(section) else {
        return;
    };

    match deserialize_with_ignored(value.clone()) {
        Ok((section_config, ignored_keys)) => {
            diagnostics.extend(unknown_config_key_diagnostics(
                ignored_keys,
                Some(section),
                origins,
            ));
            apply(section_config);
        }
        Err(err) => {
            let source = vec![ConfigKeyPathSegment::Key(section.to_string())];
            diagnostics.push(format!(
                "invalid {label}{}: {err}; keeping current {section} settings",
                config_source_suffix(config_source(origins, &source))
            ));
            invalid_sections.push(section.to_string());
        }
    }
}

pub(crate) fn upsert_top_level_bool(content: &str, key: &str, value: bool) -> String {
    let replacement = format!("{key} = {value}");
    let mut lines: Vec<String> = content.lines().map(|line| line.to_string()).collect();
    let mut in_section = false;

    for line in &mut lines {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_section = true;
            continue;
        }
        if in_section {
            continue;
        }
        if trimmed.starts_with(&format!("{key} ")) || trimmed.starts_with(&format!("{key}=")) {
            *line = replacement.clone();
            return lines.join("\n") + "\n";
        }
    }

    if lines.is_empty() {
        format!("{replacement}\n")
    } else {
        format!("{replacement}\n{}\n", lines.join("\n").trim_end())
    }
}

/// Write a key = value pair in a TOML section (creates section if missing).
pub fn upsert_section_value(content: &str, section: &str, key: &str, value: &str) -> String {
    upsert_section_raw(content, section, key, value)
}

pub fn upsert_section_bool(content: &str, section: &str, key: &str, value: bool) -> String {
    upsert_section_raw(content, section, key, &value.to_string())
}

pub fn remove_section_key(content: &str, section: &str, key: &str) -> String {
    let header = format!("[{section}]");
    let lines: Vec<&str> = content.lines().collect();
    let mut result = Vec::new();
    let mut i = 0;
    let mut in_section = false;

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_section = trimmed == header;
            result.push(line.to_string());
            i += 1;
            continue;
        }

        if in_section
            && (trimmed.starts_with(&format!("{key} ")) || trimmed.starts_with(&format!("{key}=")))
        {
            i += 1;
            continue;
        }

        result.push(line.to_string());
        i += 1;
    }

    result.join("\n") + "\n"
}

pub fn remove_keybinding_config_sections(content: &str) -> (String, bool) {
    let mut result = Vec::new();
    let mut removed = false;
    let mut skipping_key_section = false;
    let mut in_table = false;

    for line in content.lines() {
        let trimmed = line.trim();

        if let Some(table_name) = toml_table_header_name(trimmed) {
            in_table = true;
            skipping_key_section = is_keys_table_name(table_name);
            if skipping_key_section {
                removed = true;
                continue;
            }
        } else if skipping_key_section || (!in_table && is_top_level_keys_assignment(trimmed)) {
            removed = true;
            continue;
        }

        result.push(line.to_string());
    }

    let mut updated = result.join("\n");
    if content.ends_with('\n') || !updated.is_empty() {
        updated.push('\n');
    }
    (updated, removed)
}

fn toml_table_header_name(trimmed: &str) -> Option<&str> {
    if let Some(name) = trimmed
        .strip_prefix("[[")
        .and_then(|value| value.strip_suffix("]]"))
    {
        return Some(name.trim());
    }
    trimmed
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .map(str::trim)
}

fn is_keys_table_name(name: &str) -> bool {
    name == "keys" || name.starts_with("keys.")
}

fn is_top_level_keys_assignment(trimmed: &str) -> bool {
    trimmed.starts_with("keys ") || trimmed.starts_with("keys=") || trimmed.starts_with("keys.")
}

fn upsert_section_raw(content: &str, section: &str, key: &str, value: &str) -> String {
    let header = format!("[{section}]");
    let assignment = format!("{key} = {value}");
    let lines: Vec<&str> = content.lines().collect();
    let mut result = Vec::new();
    let mut i = 0;
    let mut found_section = false;
    let mut inserted = false;

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        if trimmed == header {
            found_section = true;
            result.push(line.to_string());
            i += 1;

            while i < lines.len() {
                let current = lines[i];
                let current_trimmed = current.trim();
                if current_trimmed.starts_with('[') && current_trimmed.ends_with(']') {
                    if !inserted {
                        result.push(assignment.clone());
                        inserted = true;
                    }
                    break;
                }

                if current_trimmed.starts_with(&format!("{key} "))
                    || current_trimmed.starts_with(&format!("{key}="))
                {
                    result.push(assignment.clone());
                    inserted = true;
                } else {
                    result.push(current.to_string());
                }
                i += 1;
            }

            continue;
        }

        result.push(line.to_string());
        i += 1;
    }

    if !found_section {
        if !result.is_empty() && !result.last().is_some_and(|line| line.trim().is_empty()) {
            result.push(String::new());
        }
        result.push(header);
        result.push(assignment);
    } else if !inserted {
        result.push(assignment);
    }

    result.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inheritance_test_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "herdr-config-inheritance-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn config_inheritance_merges_tables_and_replaces_scalars_and_arrays() {
        let dir = inheritance_test_dir("merge");
        let base = dir.join("base.toml");
        let child = dir.join("config.toml");
        std::fs::write(
            &base,
            "[ui]\nmouse_capture = true\n[ui.toast]\ndelivery = \"system\"\n[custom]\nitems = [1, 2]\n",
        )
        .unwrap();
        std::fs::write(
            &child,
            "extends = \"base.toml\"\n[ui]\nmouse_capture = false\n[custom]\nitems = [3]\n",
        )
        .unwrap();

        let tree = load_config_tree(&child).unwrap().unwrap();
        assert_eq!(tree.value["ui"]["mouse_capture"].as_bool(), Some(false));
        assert_eq!(
            tree.value["ui"]["toast"]["delivery"].as_str(),
            Some("system")
        );
        assert_eq!(
            tree.value["custom"]["items"].as_array().unwrap(),
            &[toml::Value::Integer(3)]
        );
        assert!(tree.value.get("extends").is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn inherited_sections_remain_visible_to_child_only_config_commands() {
        let dir = inheritance_test_dir("effective-section");
        let child = dir.join("config.toml");
        std::fs::write(dir.join("base.toml"), "[keys]\nprefix = \"ctrl+a\"\n").unwrap();
        std::fs::write(&child, "extends = \"base.toml\"\n").unwrap();

        assert!(
            super::super::inheritance::effective_config_contains_section(&child, "keys").unwrap()
        );
        assert!(
            !super::super::inheritance::effective_config_contains_section(&child, "theme").unwrap()
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn config_inheritance_resolves_each_relative_edge_from_its_source() {
        let dir = inheritance_test_dir("relative-chain");
        let middle_dir = dir.join("nested");
        let leaf_dir = middle_dir.join("leaf");
        std::fs::create_dir_all(&leaf_dir).unwrap();
        std::fs::write(dir.join("base.toml"), "[ui]\nmouse_capture = false\n").unwrap();
        std::fs::write(
            middle_dir.join("middle.toml"),
            "extends = \"../base.toml\"\n[ui.toast]\ndelivery = \"system\"\n",
        )
        .unwrap();
        let child = leaf_dir.join("config.toml");
        std::fs::write(
            &child,
            "extends = \"../middle.toml\"\n[ui]\nredraw_on_focus_gained = false\n",
        )
        .unwrap();

        let tree = load_config_tree(&child).unwrap().unwrap();
        assert_eq!(tree.value["ui"]["mouse_capture"].as_bool(), Some(false));
        assert_eq!(
            tree.value["ui"]["toast"]["delivery"].as_str(),
            Some("system")
        );
        assert_eq!(
            tree.value["ui"]["redraw_on_focus_gained"].as_bool(),
            Some(false)
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn config_inheritance_expands_home_and_reports_unknown_key_sources() {
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        let dir = inheritance_test_dir("home-source");
        let home = dir.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let base = home.join("shared.toml");
        let child = dir.join("config.toml");
        std::fs::write(&base, "[ui]\nmouse_captur = false\n").unwrap();
        std::fs::write(&child, "extends = \"~/shared.toml\"\n").unwrap();
        let previous_home = std::env::var_os("HOME");
        std::env::set_var("HOME", &home);

        let tree = load_config_tree(&child).unwrap().unwrap();
        let loaded = load_live_config_from_tree(tree).unwrap();
        assert_eq!(loaded.diagnostics.len(), 1);
        assert!(loaded.diagnostics[0].contains("ui.mouse_captur"));
        assert!(loaded.diagnostics[0].contains(&base.display().to_string()));

        if let Some(previous_home) = previous_home {
            std::env::set_var("HOME", previous_home);
        } else {
            std::env::remove_var("HOME");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn startup_and_live_reload_share_the_effective_inherited_config() {
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        let dir = inheritance_test_dir("loader-parity");
        let child = dir.join("config.toml");
        std::fs::write(
            dir.join("base.toml"),
            "[ui]\nmouse_capture = false\n[ui.toast]\ndelivery = \"system\"\n",
        )
        .unwrap();
        std::fs::write(
            &child,
            "extends = \"base.toml\"\n[ui.toast]\ndelivery = \"herdr\"\n",
        )
        .unwrap();
        std::env::set_var(CONFIG_PATH_ENV_VAR, &child);

        let startup = Config::load();
        let live = load_live_config().unwrap();
        assert!(!startup.config.ui.mouse_capture);
        assert!(!live.config.ui.mouse_capture);
        assert_eq!(
            startup.config.ui.toast.delivery,
            super::super::ToastDelivery::Herdr
        );
        assert_eq!(
            live.config.ui.toast.delivery,
            super::super::ToastDelivery::Herdr
        );
        assert!(startup.diagnostics.is_empty());
        assert!(live.diagnostics.is_empty());

        std::env::remove_var(CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn inherited_value_errors_name_the_contributing_parent() {
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        let dir = inheritance_test_dir("invalid-inherited-value");
        let parent = dir.join("base.toml");
        let child = dir.join("config.toml");
        std::fs::write(&parent, "[server]\nheadless_cols = \"wide\"\n").unwrap();
        std::fs::write(&child, "extends = \"base.toml\"\n").unwrap();
        std::env::set_var(CONFIG_PATH_ENV_VAR, &child);

        let startup = Config::load();
        assert!(startup.diagnostics.iter().any(|diagnostic| {
            diagnostic.contains(&parent.display().to_string())
                && diagnostic.contains(&child.display().to_string())
                && diagnostic.contains("using defaults")
        }));
        let live = load_live_config().unwrap();
        assert_eq!(live.invalid_sections, vec!["server"]);
        assert!(live.diagnostics.iter().any(|diagnostic| {
            diagnostic.contains("invalid server config")
                && diagnostic.contains(&parent.display().to_string())
        }));

        std::env::remove_var(CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn config_inheritance_rejects_invalid_directives_and_missing_parents() {
        let dir = inheritance_test_dir("invalid-directive");
        let child = dir.join("config.toml");
        std::fs::write(&child, "extends = [\"base.toml\"]\n").unwrap();
        let error = load_config_tree(&child).err().unwrap();
        assert!(error.contains("extends must be one non-empty path string"));
        assert!(error.contains(&child.display().to_string()));

        std::fs::write(&child, "extends = \"missing.toml\"\n").unwrap();
        let error = load_config_tree(&child).err().unwrap();
        assert!(error.contains("file not found"));
        assert!(error.contains("missing.toml"));
        assert!(error.contains("extended from"));

        std::fs::write(&child, "extends = \"~someone/config.toml\"\n").unwrap();
        let error = load_config_tree(&child).err().unwrap();
        assert!(error.contains("unsupported extends path"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn config_inheritance_allows_child_values_to_change_type() {
        let mut base: toml::Value = "value = 1\n".parse().unwrap();
        let child: toml::Value = "value = { nested = true }\n".parse().unwrap();
        merge_config_values(&mut base, child);
        assert_eq!(base["value"]["nested"].as_bool(), Some(true));
    }

    #[cfg(unix)]
    #[test]
    fn config_inheritance_detects_cycles_through_symlink_aliases() {
        let dir = inheritance_test_dir("symlink-cycle");
        let child = dir.join("config.toml");
        let alias = dir.join("alias.toml");
        std::fs::write(&child, "extends = \"alias.toml\"\n").unwrap();
        std::os::unix::fs::symlink(&child, &alias).unwrap();

        let error = load_config_tree(&child).err().unwrap();
        assert!(error.contains("cycle detected"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn config_inheritance_rejects_cycles_with_the_source_chain() {
        let dir = inheritance_test_dir("cycle");
        let first = dir.join("first.toml");
        let second = dir.join("second.toml");
        std::fs::write(&first, "extends = \"second.toml\"\n").unwrap();
        std::fs::write(&second, "extends = \"first.toml\"\n").unwrap();

        let error = load_config_tree(&first).err().unwrap();
        assert!(error.contains("cycle detected"));
        assert!(error.contains("first.toml"));
        assert!(error.contains("second.toml"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn config_inheritance_rejects_chains_beyond_the_limit() {
        let dir = inheritance_test_dir("depth");
        for index in 0..=MAX_CONFIG_EXTENDS_DEPTH {
            let content = if index == MAX_CONFIG_EXTENDS_DEPTH {
                "[ui]\nmouse_capture = false\n".to_string()
            } else {
                format!("extends = \"{}.toml\"\n", index + 1)
            };
            std::fs::write(dir.join(format!("{index}.toml")), content).unwrap();
        }

        let error = load_config_tree(&dir.join("0.toml")).err().unwrap();
        assert!(error.contains("maximum chain depth of 16"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn invalid_inherited_config_names_the_parent_and_preserves_reload_state() {
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        let dir = inheritance_test_dir("invalid-parent");
        let parent = dir.join("base.toml");
        let child = dir.join("config.toml");
        std::fs::write(&parent, "[ui\n").unwrap();
        std::fs::write(&child, "extends = \"base.toml\"\n").unwrap();
        std::env::set_var(CONFIG_PATH_ENV_VAR, &child);

        let startup = Config::load();
        assert!(startup.diagnostics.iter().any(|diagnostic| diagnostic
            .contains(&parent.display().to_string())
            && diagnostic.contains("using defaults")));
        let live = load_live_config().unwrap_err();
        assert!(live.iter().any(|diagnostic| {
            diagnostic.contains(&parent.display().to_string())
                && diagnostic.contains("keeping current config")
        }));

        std::env::remove_var(CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn upsert_top_level_bool_replaces_existing_value() {
        let content = "onboarding = true\n[keys]\nprefix = \"ctrl+b\"\n";
        let updated = upsert_top_level_bool(content, "onboarding", false);
        assert!(updated.contains("onboarding = false"));
        assert!(!updated.contains("onboarding = true"));
    }

    #[test]
    fn upsert_section_bool_adds_missing_section() {
        let updated = upsert_section_bool("", "ui.toast", "enabled", true);
        assert!(updated.contains("[ui.toast]"));
        assert!(updated.contains("enabled = true"));
    }

    #[test]
    fn remove_section_key_removes_matching_key_from_section() {
        let content =
            "[ui.toast]\nenabled = true\ndelivery = \"herdr\"\n[ui.sound]\nenabled = true\n";
        let updated = remove_section_key(content, "ui.toast", "enabled");
        assert!(!updated.contains("[ui.toast]\nenabled = true"));
        assert!(updated.contains("delivery = \"herdr\""));
        assert!(updated.contains("[ui.sound]\nenabled = true"));
    }

    #[test]
    fn config_diagnostic_summary_uses_compact_actionable_banner() {
        let diagnostics = vec![
            "one".to_string(),
            "two".to_string(),
            "three".to_string(),
            "four".to_string(),
            "five".to_string(),
        ];

        assert_eq!(
            config_diagnostic_summary(&diagnostics).as_deref(),
            Some("config.toml; herdl config check")
        );
    }

    #[test]
    fn config_diagnostic_summary_reports_unknown_keys_compactly() {
        let diagnostics = vec![
            "unknown config key ui.mouse_captur; ignoring key".to_string(),
            "unknown config key keys.new_tabb; ignoring key".to_string(),
        ];

        assert_eq!(
            config_diagnostic_summary(&diagnostics).as_deref(),
            Some("config.toml has unknown keys; herdl config check")
        );
    }

    #[test]
    fn config_diagnostic_summary_keeps_mixed_diagnostics_generic() {
        let diagnostics = vec![
            "invalid ui config: invalid type: string; keeping current ui settings".to_string(),
            "unknown config key keys.new_tabb; ignoring key".to_string(),
        ];

        assert_eq!(
            config_diagnostic_summary(&diagnostics).as_deref(),
            Some("config.toml; herdl config check")
        );
    }

    #[test]
    fn config_diagnostic_summary_reports_default_fallback() {
        let diagnostics = vec![
            "config parse error: TOML parse error at line 33, column 8\n   |\n33 | type = \"popup\"\n   |        ^^^^^^^\nunknown variant `popup`; using defaults"
                .to_string(),
        ];

        assert_eq!(
            config_diagnostic_summary(&diagnostics).as_deref(),
            Some("config.toml invalid; using defaults; herdl config check")
        );
    }

    #[test]
    fn config_diagnostic_summary_reports_unreadable_config_impact() {
        let startup = vec!["config read error: permission denied; using defaults".to_string()];
        assert_eq!(
            config_diagnostic_summary(&startup).as_deref(),
            Some("config.toml unreadable; using defaults; herdl config check")
        );

        let reload =
            vec!["config read error: permission denied; keeping current config".to_string()];
        assert_eq!(
            config_diagnostic_summary(&reload).as_deref(),
            Some("config.toml unreadable; keeping current config; herdl config check")
        );
    }

    #[test]
    fn config_diagnostic_summary_reports_retained_live_config() {
        let diagnostics = vec![
            "config parse error: TOML parse error at line 7, column 4; keeping current config"
                .to_string(),
        ];

        assert_eq!(
            config_diagnostic_summary(&diagnostics).as_deref(),
            Some("config.toml invalid; keeping current config; herdl config check")
        );
    }

    #[test]
    fn config_loaders_report_unreadable_path() {
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        let path =
            std::env::temp_dir().join(format!("herdr-config-unreadable-{}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        std::env::set_var(CONFIG_PATH_ENV_VAR, &path);

        let startup = Config::load();
        assert!(startup
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("config read error")
                && diagnostic.contains("using defaults")));

        let reload = load_live_config().unwrap_err();
        assert!(reload.iter().any(|diagnostic| {
            diagnostic.contains("config read error")
                && diagnostic.contains("keeping current config")
        }));

        std::env::remove_var(CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn load_live_config_parses_session_section() {
        let loaded = load_live_config_from_str(
            r#"
[session]
resume_agents_on_restore = true
"#,
        )
        .unwrap();

        assert!(loaded.config.session.resume_agents_on_restore);
        assert!(loaded.diagnostics.is_empty());
        assert!(loaded.invalid_sections.is_empty());
    }

    #[test]
    fn load_live_config_warns_about_unknown_theme_names() {
        let loaded = load_live_config_from_str(
            r#"
[theme]
name = "catppucin"
"#,
        )
        .unwrap();

        assert_eq!(loaded.diagnostics.len(), 1);
        assert!(loaded.diagnostics[0].contains("theme.name = \"catppucin\""));
    }

    #[test]
    fn load_live_config_warns_about_unknown_top_level_sections() {
        let loaded = load_live_config_from_str(
            r#"
[toast]
delivery = "system"

[ui.toast]
delivery = "herdr"
"#,
        )
        .unwrap();

        assert_eq!(
            loaded.diagnostics,
            vec!["unknown config section [toast]; did you mean [ui.toast]? ignoring section"]
        );
        assert!(loaded.invalid_sections.is_empty());
        assert_eq!(
            loaded.config.ui.toast.delivery,
            super::super::ToastDelivery::Herdr
        );
    }

    #[test]
    fn load_live_config_warns_about_unknown_keys_and_applies_known_siblings() {
        let loaded = load_live_config_from_str(
            r##"
plugin = []

[theme.custom]
accentt = "#ffffff"

[advanced]
scrollback_lines = 42

[keys]
fullscreen = "prefix+z"
new_tabb = "prefix+t"

[[keys.command]]
key = "prefix+g"
command = "git status"
descrption = "status"

[ui]
mouse_capture = false
mouse_captur = true
"foo.bar" = true
"foo.?.bar" = false

[ui.toast]
enabled = true
delivry = "system"

[ui.sidebar.agents.rows_by_agent]
claude = [["terminal_title"]]
"##,
        )
        .unwrap();

        assert_eq!(
            loaded.diagnostics,
            vec![
                "unknown config key plugin; ignoring key",
                "unknown config key theme.custom.accentt; ignoring key",
                "unknown config key keys.command.0.descrption; ignoring key",
                "unknown config key keys.new_tabb; ignoring key",
                "unknown config key ui.\"foo.?.bar\"; ignoring key",
                "unknown config key ui.\"foo.bar\"; ignoring key",
                "unknown config key ui.mouse_captur; ignoring key",
                "unknown config key ui.toast.delivry; ignoring key",
            ]
        );
        assert!(loaded.invalid_sections.is_empty());
        assert_eq!(loaded.config.advanced.scrollback_limit_bytes, 42);
        assert!(!loaded.config.ui.mouse_capture);
        assert_eq!(
            loaded.config.ui.toast.delivery,
            super::super::ToastDelivery::Herdr
        );
        assert!(loaded
            .config
            .keybinds()
            .zoom
            .bindings
            .iter()
            .any(|binding| binding.label == "prefix+z"));
    }

    #[test]
    fn load_live_config_accepts_legacy_agent_panel_scope_without_warning() {
        let loaded = load_live_config_from_str(
            r#"
[ui]
agent_panel_scope = "current"
agent_panel_sort = "priority"
"#,
        )
        .unwrap();

        assert!(loaded.diagnostics.is_empty());
        assert!(loaded.invalid_sections.is_empty());
        assert_eq!(
            loaded.config.ui.agent_panel_sort,
            super::super::AgentPanelSortConfig::Priority
        );
    }

    #[test]
    fn load_live_config_discards_ignored_keys_from_an_invalid_section() {
        let loaded = load_live_config_from_str(
            r#"
[ui]
mouse_capture = "yes"
mouse_captur = true
"#,
        )
        .unwrap();

        assert_eq!(loaded.diagnostics.len(), 1);
        assert!(loaded.diagnostics[0].contains("invalid ui config"));
        assert!(!loaded.diagnostics[0].starts_with("unknown config key"));
        assert_eq!(loaded.invalid_sections, vec!["ui"]);
    }

    #[test]
    fn startup_config_accepts_legacy_agent_panel_scope_without_warning() {
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        let path = std::env::temp_dir().join(format!(
            "herdr-config-legacy-agent-panel-scope-{}.toml",
            std::process::id()
        ));
        std::fs::write(&path, "[ui]\nagent_panel_scope = \"all\"\n").unwrap();
        std::env::set_var(CONFIG_PATH_ENV_VAR, &path);

        let loaded = Config::load();

        std::env::remove_var(CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_file(path);

        assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
    }

    #[test]
    fn startup_config_load_warns_about_unknown_top_level_sections() {
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        let path = std::env::temp_dir().join(format!(
            "herdr-config-unknown-section-{}.toml",
            std::process::id()
        ));
        std::fs::write(
            &path,
            r#"
[[plugin]]
id = "example"

[ui.toast]
delivery = "system"
"#,
        )
        .unwrap();
        std::env::set_var(CONFIG_PATH_ENV_VAR, &path);

        let loaded = Config::load();

        assert_eq!(
            loaded.diagnostics,
            vec!["unknown config section [[plugin]]; ignoring section"]
        );
        assert_eq!(
            loaded.config.ui.toast.delivery,
            super::super::ToastDelivery::System
        );

        std::env::remove_var(CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn remove_keybinding_config_sections_removes_keys_tables_only() {
        let content = r#"onboarding = false

[theme]
name = "catppuccin"

[keys]
prefix = "ctrl+a"
new_tab = "c"

[[keys.command]]
key = "g"
command = "lazygit"

[keys.indexed]
tabs = "ctrl"

[ui]
mouse_capture = false
"#;

        let (updated, removed) = remove_keybinding_config_sections(content);

        assert!(removed);
        assert!(updated.contains("onboarding = false"));
        assert!(updated.contains("[theme]\nname = \"catppuccin\""));
        assert!(updated.contains("[ui]\nmouse_capture = false"));
        assert!(!updated.contains("[keys]"));
        assert!(!updated.contains("[[keys.command]]"));
        assert!(!updated.contains("[keys.indexed]"));
        assert!(toml::from_str::<toml::Value>(&updated).is_ok());
    }

    #[test]
    fn remove_keybinding_config_sections_reports_noop_without_keys() {
        let content = "[ui]\nmouse_capture = true\n";
        let (updated, removed) = remove_keybinding_config_sections(content);
        assert!(!removed);
        assert_eq!(updated, content);
    }

    #[test]
    fn normalize_utf8_bom_removes_a_leading_bom() {
        let content = "\u{feff}onboarding = false\n[terminal]\n";
        assert_eq!(
            normalize_utf8_bom(content),
            "onboarding = false\n[terminal]\n"
        );
    }

    #[test]
    fn normalize_utf8_bom_recovers_from_a_displaced_mid_file_bom() {
        let content = "onboarding = false\n\u{feff}[terminal]\ndefault_shell = \"pwsh.exe\"\n";
        let normalized = normalize_utf8_bom(content);
        assert_eq!(
            normalized,
            "onboarding = false\n[terminal]\ndefault_shell = \"pwsh.exe\"\n"
        );
        assert!(normalized.parse::<toml::Value>().is_ok());
    }

    #[test]
    fn normalize_utf8_bom_preserves_boms_in_multiline_basic_strings() {
        let content = "[theme]\nname = \"\"\"\nfirst\n\u{feff}second\n\"\"\"\n";
        assert!(content.parse::<toml::Value>().is_ok());
        assert_eq!(normalize_utf8_bom(content), content);
    }

    #[test]
    fn normalize_utf8_bom_preserves_boms_in_multiline_literal_strings() {
        let content = "[theme]\nname = '''\nfirst\n\u{feff}second\n'''\n";
        assert!(content.parse::<toml::Value>().is_ok());
        assert_eq!(normalize_utf8_bom(content), content);
    }

    #[test]
    fn normalize_utf8_bom_preserves_string_boms_despite_other_errors() {
        let content = "[theme]\nname = \"\"\"\nfirst\n\u{feff}second\n\"\"\"\nbroken = \n";
        assert!(content.parse::<toml::Value>().is_err());
        assert_eq!(normalize_utf8_bom(content), content);
    }

    #[test]
    fn config_load_recovers_from_a_mid_file_bom() {
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        let path = std::env::temp_dir().join(format!(
            "herdr-config-mid-file-bom-{}.toml",
            std::process::id()
        ));
        std::fs::write(
            &path,
            b"onboarding = false\n\xEF\xBB\xBF[terminal]\ndefault_shell = \"pwsh.exe\"\n",
        )
        .unwrap();
        std::env::set_var(CONFIG_PATH_ENV_VAR, &path);

        let loaded = Config::load();

        std::env::remove_var(CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_file(path);

        assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
        assert_eq!(loaded.config.terminal.default_shell, "pwsh.exe");
    }
}
