use crate::game::{self, Game};
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

/// One picker selection: a preset id from `game::PRESETS` or `game::CUSTOM_ID`,
/// plus the package name that is used when it is `game::CUSTOM_ID`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GameChoice {
    pub id: String,
    pub custom_package: String,
}

impl Default for GameChoice {
    fn default() -> Self {
        Self {
            id: game::DEFAULT_ID.to_string(),
            custom_package: String::new(),
        }
    }
}

impl GameChoice {
    pub fn resolve(&self) -> Result<Game, String> {
        game::resolve(&self.id, &self.custom_package)
    }

    /// Replace an id that no longer exists with the default.
    fn normalize(&mut self) {
        normalize_game_id(&mut self.id);
    }
}

fn normalize_game_id(id: &mut String) {
    if id != game::CUSTOM_ID && game::find_preset(id).is_none() {
        *id = game::DEFAULT_ID.to_string();
    }
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
    /// Game id of the Automatic tab: a preset id from `game::PRESETS` or
    /// `game::CUSTOM_ID`. (Kept under this name so earlier configs still load.)
    pub game: String,
    /// Package name used when `game` is `game::CUSTOM_ID`.
    pub custom_package: String,
    /// Game of the Manual tab's source and target. `None` only until `load`
    /// fills them in, which is also how configs saved before these existed
    /// are recognised.
    pub manual_left_game: Option<GameChoice>,
    pub manual_right_game: Option<GameChoice>,
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
            game: game::DEFAULT_ID.to_string(),
            custom_package: String::new(),
            // Unset until `normalize` fills them in (see there).
            manual_left_game: None,
            manual_right_game: None,
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let path = Self::path();
        let mut config = match fs::read_to_string(&path) {
            Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
            Err(_) => Self::default(),
        };
        config.normalize();
        config
    }

    /// The Automatic tab's selection.
    pub fn auto_choice(&self) -> GameChoice {
        GameChoice {
            id: self.game.clone(),
            custom_package: self.custom_package.clone(),
        }
    }

    pub fn set_auto_choice(&mut self, choice: GameChoice) {
        self.game = choice.id;
        self.custom_package = choice.custom_package;
    }

    /// Make every selection valid: ids that no longer exist (for example from
    /// a newer version of the app) fall back to the default, and Manual sides
    /// without a selection of their own start from the Automatic one, so
    /// someone who already picked a game keeps it. After that the three are
    /// independent.
    fn normalize(&mut self) {
        normalize_game_id(&mut self.game);
        let inherited = self.auto_choice();
        for side in [&mut self.manual_left_game, &mut self.manual_right_game] {
            side.get_or_insert_with(|| inherited.clone()).normalize();
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_saved_before_the_game_picker_still_loads() {
        let old = r#"{
            "remember_tab": true,
            "last_tab": "automatic",
            "remember_devices": true,
            "manual_left": "192.168.240.112:5555",
            "manual_right": null,
            "auto_left": null,
            "auto_right": null
        }"#;
        let mut config: Config = serde_json::from_str(old).unwrap();
        config.normalize();

        assert_eq!(config.game, game::DEFAULT_ID);
        assert_eq!(config.custom_package, "");
        assert_eq!(config.last_tab, Tab::Automatic);
        assert_eq!(config.manual_left.as_deref(), Some("192.168.240.112:5555"));
    }

    #[test]
    fn unknown_game_ids_fall_back_but_custom_is_kept() {
        let mut config = Config {
            game: "removed_in_a_later_version".to_string(),
            ..Config::default()
        };
        config.normalize();
        assert_eq!(config.game, game::DEFAULT_ID);

        let mut config = Config {
            game: game::CUSTOM_ID.to_string(),
            custom_package: "com.foo.bar".to_string(),
            ..Config::default()
        };
        config.normalize();
        assert_eq!(config.game, game::CUSTOM_ID);
        assert_eq!(config.custom_package, "com.foo.bar");
    }

    #[test]
    fn manual_sides_start_from_the_earlier_shared_choice_then_diverge() {
        // Saved by the previous version: one game for everything.
        let previous = r#"{ "game": "base_international", "custom_package": "" }"#;
        let mut config: Config = serde_json::from_str(previous).unwrap();
        assert!(config.manual_left_game.is_none());
        config.normalize();

        let left = config.manual_left_game.clone().unwrap();
        let right = config.manual_right_game.clone().unwrap();
        assert_eq!(left.id, "base_international");
        assert_eq!(right.id, "base_international");

        // Changing one side afterwards touches neither the other side nor Automatic.
        config.manual_left_game = Some(GameChoice {
            id: game::CUSTOM_ID.to_string(),
            custom_package: "com.foo.bar".to_string(),
        });
        assert_eq!(
            config.manual_right_game.as_ref().unwrap().id,
            "base_international"
        );
        assert_eq!(config.game, "base_international");

        // And once saved, the sides are not re-inherited on the next load.
        let json = serde_json::to_string(&config).unwrap();
        let mut reloaded: Config = serde_json::from_str(&json).unwrap();
        reloaded.normalize();
        assert_eq!(
            reloaded.manual_left_game.unwrap().custom_package,
            "com.foo.bar"
        );
        assert_eq!(reloaded.manual_right_game.unwrap().id, "base_international");
    }

    #[test]
    fn new_configs_start_on_the_default_game_everywhere() {
        let mut config = Config::default();
        config.normalize();
        assert_eq!(config.auto_choice(), GameChoice::default());
        assert_eq!(config.manual_left_game, Some(GameChoice::default()));
        assert_eq!(config.manual_right_game, Some(GameChoice::default()));
        assert_eq!(
            GameChoice::default().resolve().unwrap().package(),
            "com.ea.game.pvz2_rfl"
        );
    }

    #[test]
    fn unknown_ids_in_manual_sides_fall_back() {
        let mut config = Config {
            manual_left_game: Some(GameChoice {
                id: "removed_in_a_later_version".to_string(),
                custom_package: String::new(),
            }),
            ..Config::default()
        };
        config.normalize();
        assert_eq!(config.manual_left_game.unwrap().id, game::DEFAULT_ID);
    }

    #[test]
    fn game_choice_round_trips_through_json() {
        let config = Config {
            game: game::CUSTOM_ID.to_string(),
            custom_package: "org.example.game".to_string(),
            ..Config::default()
        };
        let json = serde_json::to_string(&config).unwrap();
        let loaded: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.game, game::CUSTOM_ID);
        assert_eq!(loaded.custom_package, "org.example.game");
    }
}
