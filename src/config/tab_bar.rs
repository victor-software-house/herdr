use serde::{Deserialize, Serialize};
use unicode_width::UnicodeWidthStr;

pub(crate) const DEFAULT_TAB_BAR_COMMAND_INTERVAL_SECONDS: u64 = 5;
pub(crate) const DEFAULT_TAB_BAR_COMMAND_TIMEOUT_SECONDS: u64 = 2;
pub(crate) const MAX_TAB_BAR_COMMAND_INTERVAL_SECONDS: u64 = 31_536_000;
pub(crate) const MAX_TAB_BAR_COMMAND_TIMEOUT_SECONDS: u64 = 3_600;
pub(crate) const MAX_TAB_BAR_RIGHT_ENTRIES: usize = 16;

fn default_datetime_format() -> String {
    "%H:%M".to_string()
}

fn default_command_interval_seconds() -> u64 {
    DEFAULT_TAB_BAR_COMMAND_INTERVAL_SECONDS
}

fn default_command_timeout_seconds() -> u64 {
    DEFAULT_TAB_BAR_COMMAND_TIMEOUT_SECONDS
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum TabBarRightEntryConfig {
    Zoom,
    Hostname,
    Datetime {
        #[serde(default = "default_datetime_format")]
        format: String,
    },
    Text {
        text: String,
    },
    Command {
        command: String,
        #[serde(default = "default_command_interval_seconds")]
        interval_seconds: u64,
        #[serde(default = "default_command_timeout_seconds")]
        timeout_seconds: u64,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TabLabelAlignmentConfig {
    Left,
    #[default]
    Center,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct TabStripText(String);

impl TabStripText {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    fn width(&self) -> u16 {
        UnicodeWidthStr::width(self.0.as_str()).min(u16::MAX as usize) as u16
    }
}

impl<'de> Deserialize<'de> for TabStripText {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if value.is_empty() || value.chars().any(char::is_control) {
            return Err(serde::de::Error::custom(
                "tab strip text must be non-empty printable text",
            ));
        }
        Ok(Self(value))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TabStripControlConfig {
    pub width: u16,
    pub label: TabStripText,
}

impl Default for TabStripControlConfig {
    fn default() -> Self {
        Self {
            width: 3,
            label: TabStripText(" + ".into()),
        }
    }
}

impl TabStripControlConfig {
    fn validate(&self, path: &str) -> Option<String> {
        if !(1..=16).contains(&self.width) {
            return Some(format!("{path}.width must be between 1 and 16"));
        }
        (self.label.width() > self.width).then(|| {
            format!(
                "{path}.label is {} columns wide but width is {}",
                self.label.width(),
                self.width
            )
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TabStripScrollConfig {
    pub width: u16,
    pub left: TabStripText,
    pub right: TabStripText,
}

impl Default for TabStripScrollConfig {
    fn default() -> Self {
        Self {
            width: 3,
            left: TabStripText(" < ".into()),
            right: TabStripText(" > ".into()),
        }
    }
}

impl TabStripScrollConfig {
    fn validate(&self) -> Option<String> {
        if !(1..=16).contains(&self.width) {
            return Some("ui.tab_strip.scroll.width must be between 1 and 16".into());
        }
        for (name, label) in [("left", &self.left), ("right", &self.right)] {
            if label.width() > self.width {
                return Some(format!(
                    "ui.tab_strip.scroll.{name} is {} columns wide but scroll width is {}",
                    label.width(),
                    self.width
                ));
            }
        }
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TabStripConfig {
    pub min_tab_width: u16,
    pub label_padding_left: u16,
    pub label_padding_right: u16,
    pub label_alignment: TabLabelAlignmentConfig,
    pub gap: u16,
    pub scroll: TabStripScrollConfig,
    pub new_tab: TabStripControlConfig,
    pub overflow_indicator: TabStripText,
    pub drop_indicator: TabStripText,
    pub status_gap: u16,
}

impl Default for TabStripConfig {
    fn default() -> Self {
        Self {
            min_tab_width: 8,
            label_padding_left: 2,
            label_padding_right: 2,
            label_alignment: TabLabelAlignmentConfig::Center,
            gap: 1,
            scroll: TabStripScrollConfig::default(),
            new_tab: TabStripControlConfig::default(),
            overflow_indicator: TabStripText("…".into()),
            drop_indicator: TabStripText("│".into()),
            status_gap: 1,
        }
    }
}

impl TabStripConfig {
    pub(crate) fn invalid_diagnostic(&self) -> Option<String> {
        if !(1..=64).contains(&self.min_tab_width) {
            return Some("ui.tab_strip.min_tab_width must be between 1 and 64".into());
        }
        if self.label_padding_left > 32 || self.label_padding_right > 32 {
            return Some("ui.tab_strip label padding may be at most 32 columns per side".into());
        }
        if self.gap > 16 || self.status_gap > 16 {
            return Some("ui.tab_strip gap and status_gap may be at most 16 columns".into());
        }
        if self.overflow_indicator.width() != 1 || self.drop_indicator.width() != 1 {
            return Some(
                "ui.tab_strip overflow_indicator and drop_indicator must each be one column wide"
                    .into(),
            );
        }
        self.scroll
            .validate()
            .or_else(|| self.new_tab.validate("ui.tab_strip.new_tab"))
    }

    pub(crate) fn minimum_overflow_width(&self) -> u16 {
        self.min_tab_width
            .saturating_add(self.new_tab.width)
            .saturating_add(self.scroll.width.saturating_mul(2))
    }
}

pub(crate) fn parse_tab_bar_datetime_format(
    value: &str,
) -> Result<time::format_description::OwnedFormatItem, String> {
    if value.is_empty() {
        return Err("datetime format is empty".into());
    }
    let format = time::format_description::parse_strftime_owned(value)
        .map_err(|err| format!("invalid datetime format: {err}"))?;
    time::PrimitiveDateTime::MIN
        .format(&format)
        .map_err(|err| format!("unsupported datetime format: {err}"))?;
    Ok(format)
}

pub(crate) fn tab_bar_right_diagnostics(entries: &[TabBarRightEntryConfig]) -> Vec<String> {
    let mut diagnostics = Vec::new();
    if entries.len() > MAX_TAB_BAR_RIGHT_ENTRIES {
        diagnostics.push(format!(
            "ui.tab_bar_right may contain at most {MAX_TAB_BAR_RIGHT_ENTRIES} entries; ignoring extras"
        ));
    }

    for (index, entry) in entries.iter().enumerate().take(MAX_TAB_BAR_RIGHT_ENTRIES) {
        match entry {
            TabBarRightEntryConfig::Datetime { format } => {
                if format.is_empty() {
                    diagnostics.push(format!(
                        "ui.tab_bar_right[{index}] datetime format is empty; hiding entry"
                    ));
                } else if let Err(err) = parse_tab_bar_datetime_format(format) {
                    diagnostics.push(format!("ui.tab_bar_right[{index}] has {err}; hiding entry"));
                }
            }
            TabBarRightEntryConfig::Command {
                command,
                interval_seconds,
                timeout_seconds,
            } => {
                if command.trim().is_empty() {
                    diagnostics.push(format!(
                        "ui.tab_bar_right[{index}] command is empty; hiding entry"
                    ));
                }
                if *interval_seconds == 0 {
                    diagnostics.push(format!(
                        "ui.tab_bar_right[{index}] interval_seconds must be at least 1; hiding entry"
                    ));
                }
                if *interval_seconds > MAX_TAB_BAR_COMMAND_INTERVAL_SECONDS {
                    diagnostics.push(format!(
                        "ui.tab_bar_right[{index}] interval_seconds may be at most {MAX_TAB_BAR_COMMAND_INTERVAL_SECONDS}; hiding entry"
                    ));
                }
                if *timeout_seconds == 0 {
                    diagnostics.push(format!(
                        "ui.tab_bar_right[{index}] timeout_seconds must be at least 1; hiding entry"
                    ));
                }
                if *timeout_seconds > MAX_TAB_BAR_COMMAND_TIMEOUT_SECONDS {
                    diagnostics.push(format!(
                        "ui.tab_bar_right[{index}] timeout_seconds may be at most {MAX_TAB_BAR_COMMAND_TIMEOUT_SECONDS}; hiding entry"
                    ));
                }
            }
            TabBarRightEntryConfig::Zoom
            | TabBarRightEntryConfig::Hostname
            | TabBarRightEntryConfig::Text { .. } => {}
        }
    }

    diagnostics
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_strip_defaults_match_upstream_chrome() {
        let strip = TabStripConfig::default();
        assert_eq!(strip.min_tab_width, 8);
        assert_eq!(strip.label_padding_left, 2);
        assert_eq!(strip.label_padding_right, 2);
        assert_eq!(strip.label_alignment, TabLabelAlignmentConfig::Center);
        assert_eq!(strip.gap, 1);
        assert_eq!(strip.scroll.width, 3);
        assert_eq!(strip.scroll.left.as_str(), " < ");
        assert_eq!(strip.scroll.right.as_str(), " > ");
        assert_eq!(strip.new_tab.width, 3);
        assert_eq!(strip.new_tab.label.as_str(), " + ");
        assert_eq!(strip.status_gap, 1);
        assert!(strip.invalid_diagnostic().is_none());
    }

    #[test]
    fn tab_strip_parses_custom_bounded_fields() {
        let strip: TabStripConfig = toml::from_str(
            r#"
min_tab_width = 5
label_padding_left = 1
label_padding_right = 0
label_alignment = "left"
gap = 0
overflow_indicator = "~"
drop_indicator = "!"
status_gap = 2

[scroll]
width = 2
left = "<<"
right = ">>"

[new_tab]
width = 1
label = "+"
"#,
        )
        .unwrap();
        assert_eq!(strip.label_alignment, TabLabelAlignmentConfig::Left);
        assert_eq!(strip.scroll.left.as_str(), "<<");
        assert_eq!(strip.new_tab.label.as_str(), "+");
        assert!(strip.invalid_diagnostic().is_none());
    }

    #[test]
    fn tab_strip_diagnostics_reject_invalid_geometry() {
        let mut strip: TabStripConfig = toml::from_str("min_tab_width = 0\n").unwrap();
        assert!(strip
            .invalid_diagnostic()
            .unwrap()
            .contains("min_tab_width"));

        strip = toml::from_str("[scroll]\nwidth = 1\nleft = '<<'\n").unwrap();
        assert!(strip.invalid_diagnostic().unwrap().contains("scroll.left"));

        strip = toml::from_str("overflow_indicator = 'wide'\n").unwrap();
        assert!(strip.invalid_diagnostic().unwrap().contains("one column"));

        strip = toml::from_str("[new_tab]\nwidth = 1\nlabel = ' + '\n").unwrap();
        assert!(strip
            .invalid_diagnostic()
            .unwrap()
            .contains("new_tab.label"));
    }

    #[test]
    fn tab_strip_rejects_empty_or_control_text() {
        for input in [
            "overflow_indicator = ''\n",
            "drop_indicator = \"\\u0007\"\n",
        ] {
            assert!(toml::from_str::<TabStripConfig>(input).is_err());
        }
    }

    #[test]
    fn tab_bar_entries_parse_with_command_defaults() {
        #[derive(Deserialize)]
        struct Wrapper {
            entries: Vec<TabBarRightEntryConfig>,
        }

        let parsed: Wrapper = toml::from_str(
            r#"
entries = [
  { type = "zoom" },
  { type = "hostname" },
  { type = "datetime", format = "%H:%M" },
  { type = "text", text = "prod" },
  { type = "command", command = "status.sh" },
]
"#,
        )
        .expect("parse tab bar entries");

        assert_eq!(parsed.entries.len(), 5);
        assert!(matches!(
            &parsed.entries[4],
            TabBarRightEntryConfig::Command {
                interval_seconds: DEFAULT_TAB_BAR_COMMAND_INTERVAL_SECONDS,
                timeout_seconds: DEFAULT_TAB_BAR_COMMAND_TIMEOUT_SECONDS,
                ..
            }
        ));
    }

    #[test]
    fn diagnostics_reject_invalid_datetime_and_command_schedules() {
        let entries = vec![
            TabBarRightEntryConfig::Datetime {
                format: "%Q".into(),
            },
            TabBarRightEntryConfig::Datetime {
                format: "%z".into(),
            },
            TabBarRightEntryConfig::Command {
                command: String::new(),
                interval_seconds: 0,
                timeout_seconds: 0,
            },
            TabBarRightEntryConfig::Command {
                command: "status.sh".into(),
                interval_seconds: MAX_TAB_BAR_COMMAND_INTERVAL_SECONDS + 1,
                timeout_seconds: MAX_TAB_BAR_COMMAND_TIMEOUT_SECONDS + 1,
            },
        ];

        let diagnostics = tab_bar_right_diagnostics(&entries).join("\n");
        assert!(diagnostics.contains("invalid datetime format"));
        assert!(diagnostics.contains("unsupported datetime format"));
        assert!(diagnostics.contains("command is empty"));
        assert!(diagnostics.contains("interval_seconds must be at least 1"));
        assert!(diagnostics.contains("interval_seconds may be at most"));
        assert!(diagnostics.contains("timeout_seconds must be at least 1"));
        assert!(diagnostics.contains("timeout_seconds may be at most"));
        assert!(parse_tab_bar_datetime_format("").is_err());
    }
}
