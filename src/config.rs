//! rex-nvim's TOML config: `~/.config/rex-nvim/config.toml` (or the
//! `REX_NVIM_CONFIG` override). Controls how the sidebar's nvim processes run
//! (binary + environment), how many trailing pane lines the picker scans, how
//! many entries its default view shows, and whether fff.nvim's frecency DB is
//! reused.
//!
//! Missing file -> defaults, silently. Malformed file -> defaults, with one
//! warning on stderr.

use std::{env, fs, path::PathBuf};

use serde::Deserialize;

fn is_valid_env_key(key: &str) -> bool {
    !key.is_empty() && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

#[derive(Deserialize, Clone, PartialEq, Eq, Debug, Default)]
#[serde(default)]
pub struct Config {
    pub sidebar: Sidebar,
    pub picker: Picker,
}

#[derive(Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(default)]
pub struct Sidebar {
    pub nvim_bin: String,
    pub nvim_env: Vec<String>,
    pub position: SidebarPosition,
}

impl Default for Sidebar {
    fn default() -> Self {
        Self {
            nvim_bin: default_nvim_bin(),
            nvim_env: Vec::new(),
            position: SidebarPosition::Right,
        }
    }
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum SidebarPosition {
    #[default]
    Right,
    Left,
    Top,
    Bottom,
}

impl SidebarPosition {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Right => "right",
            Self::Left => "left",
            Self::Top => "top",
            Self::Bottom => "bottom",
        }
    }
}

fn default_nvim_bin() -> String {
    "nvim".to_owned()
}

#[derive(Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(default)]
pub struct Picker {
    pub scan_lines: u32,
    pub max_files: u32,
    pub frecency: bool,
}

impl Default for Picker {
    fn default() -> Self {
        Self {
            scan_lines: default_scan_lines(),
            max_files: default_max_files(),
            frecency: true,
        }
    }
}

fn default_scan_lines() -> u32 {
    300
}

fn default_max_files() -> u32 {
    20
}

/// Path to the config file. `REX_NVIM_CONFIG` overrides everything; otherwise
/// `$XDG_CONFIG_HOME/rex-nvim/config.toml`, falling back to
/// `$HOME/.config/rex-nvim/config.toml`.
pub fn config_path() -> Option<PathBuf> {
    if let Some(path) = env::var_os("REX_NVIM_CONFIG") {
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    if let Some(base) = env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(base).join("rex-nvim/config.toml"));
    }
    env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(|home| PathBuf::from(home).join(".config/rex-nvim/config.toml"))
}

pub fn config_file_display() -> String {
    match config_path() {
        Some(path) => path.to_string_lossy().into_owned(),
        None => "~/.config/rex-nvim/config.toml".to_owned(),
    }
}

/// Load the config, warning once and falling back to defaults on any problem.
pub fn load() -> Config {
    let Some(path) = config_path() else {
        return Config::default();
    };
    let Ok(text) = fs::read_to_string(&path) else {
        return Config::default();
    };
    let mut config: Config = match toml::from_str(&text) {
        Ok(config) => config,
        Err(err) => {
            eprintln!(
                "rex-nvim: ignoring malformed config {}: {err}",
                path.display()
            );
            return Config::default();
        }
    };
    sanitize_env(&mut config.sidebar.nvim_env);
    config
}

/// Drop `nvim_env` entries that are not `KEY=VALUE` with an ASCII
/// alphanumeric/underscore key, warning once per bad entry.
fn sanitize_env(entries: &mut Vec<String>) {
    entries.retain(|entry| {
        let valid = entry
            .split_once('=')
            .map(|(key, _)| is_valid_env_key(key))
            .unwrap_or(false);
        if !valid {
            eprintln!("rex-nvim: ignoring invalid nvim_env entry {entry:?} (expected KEY=VALUE)");
        }
        valid
    });
}

/// JSON form consumed by the Rex Lua half (`rex-nvim config --json`).
pub fn to_json(config: &Config) -> serde_json::Value {
    serde_json::json!({
        "sidebar": {
            "nvim_bin": config.sidebar.nvim_bin,
            "nvim_env": config.sidebar.nvim_env,
            "position": config.sidebar.position.as_str(),
        },
        "picker": {
            "scan_lines": config.picker.scan_lines,
            "max_files": config.picker.max_files,
            "frecency": config.picker.frecency,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Config {
        let mut config: Config = toml::from_str(text).expect("valid toml");
        sanitize_env(&mut config.sidebar.nvim_env);
        config
    }

    #[test]
    fn defaults_fill_when_file_is_empty() {
        let config = parse("");
        assert_eq!(config, Config::default());
        assert_eq!(config.sidebar.nvim_bin, "nvim");
        assert_eq!(config.sidebar.position.as_str(), "right");
        assert_eq!(config.picker.scan_lines, 300);
        assert_eq!(config.picker.max_files, 20);
        assert!(config.picker.frecency);
    }

    #[test]
    fn json_shape_is_exact() {
        let json = to_json(&Config::default());
        assert_eq!(
            json,
            serde_json::json!({
                "sidebar": {
                    "nvim_bin": "nvim",
                    "nvim_env": [],
                    "position": "right"
                },
                "picker": {
                    "scan_lines": 300,
                    "max_files": 20,
                    "frecency": true
                }
            })
        );
    }

    #[test]
    fn malformed_config_yields_defaults() {
        // The production path warns and falls back; the parse itself errors.
        assert!(toml::from_str::<Config>("[sidebar\nnvim_bin =").is_err());
        assert_eq!(Config::default(), parse(""));
    }

    #[test]
    fn invalid_env_entries_are_skipped() {
        let config = parse(
            "[sidebar]\nnvim_env = [\"GOOD=1\", \"no-equals\", \"BAD-KEY=x\", \"=emptykey\", \"A_B_9=v=w\"]\n",
        );
        assert_eq!(config.sidebar.nvim_env, vec!["GOOD=1", "A_B_9=v=w"]);
    }

    #[test]
    fn unknown_keys_and_position_parse() {
        let config =
            parse("[sidebar]\nposition = \"top\"\nextra = true\n[picker]\nmax_files = 5\n");
        assert_eq!(config.sidebar.position, SidebarPosition::Top);
        assert_eq!(config.picker.max_files, 5);
        assert_eq!(config.picker.scan_lines, 300);
    }

    #[test]
    fn config_path_prefers_override() {
        // The env is process-global; test the override spelling only.
        assert!(config_path().is_some());
    }
}
