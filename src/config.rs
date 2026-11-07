use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tab {
    #[default]
    Manual,
    Automatic,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub remember_tab: bool,
    pub last_tab: Tab,
    pub remember_devices: bool,
    pub manual_left: Option<String>,
    pub manual_right: Option<String>,
    pub auto_left: Option<String>,
    pub auto_right: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            remember_tab: true,
            last_tab: Tab::Manual,
            remember_devices: true,
            manual_left: None,
            manual_right: None,
            auto_left: None,
            auto_right: None,
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let path = Self::path();
        let Ok(data) = fs::read_to_string(&path) else {
            return Self::default();
        };
        serde_json::from_str(&data).unwrap_or_default()
    }

    pub fn save(&self) {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = fs::write(path, json);
        }
    }

    pub fn path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("pvz2-save-sync")
            .join("config.json")
    }
}
