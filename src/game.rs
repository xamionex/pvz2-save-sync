//! The games and mods whose saves can be synced.
//!
//! All of them keep their save in
//! `/sdcard/Android/data/<package>/files/No_Backup`, so a game is fully
//! described by its Android package name.
//!
//! To add another mod to the picker, add one entry to [`PRESETS`].

/// Config id of the "Custom package" entry in the picker.
pub const CUSTOM_ID: &str = "custom";

/// Config id used when nothing valid is stored yet. This is the game the app
/// supported before the picker existed, so old configs keep working unchanged.
pub const DEFAULT_ID: &str = "reflourished";

#[derive(Clone, Copy, Debug)]
pub struct Preset {
    /// Stable id stored in the config file. Never change an id once released.
    pub id: &'static str,
    /// Name shown in the picker and in messages.
    pub label: &'static str,
    pub package: &'static str,
}

pub const PRESETS: &[Preset] = &[
    Preset {
        id: "base_international",
        label: "Base game (International)",
        package: "com.ea.game.pvz2_row",
    },
    Preset {
        id: "base_north_america",
        label: "Base game (North America)",
        package: "com.ea.game.pvz2_na",
    },
    Preset {
        id: DEFAULT_ID,
        label: "PvZ2 Reflourished",
        package: "com.ea.game.pvz2_rfl",
    },
    Preset {
        id: "altverz",
        label: "PvZ2 AltverZ",
        package: "com.ea.game.pvz2_auz",
    },
    Preset {
        id: "ultimatum",
        label: "PvZ2 Ultimatum",
        package: "com.ea.game.pvz2_ul",
    },
];

/// A validated game. The only ways to build one are [`Game::from_preset`] and
/// [`Game::custom`], so the package name is always safe to place in a shell
/// command (letters, digits, underscores and dots only).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Game {
    label: String,
    package: String,
}

impl Game {
    pub fn from_preset(preset: &Preset) -> Self {
        Self {
            label: preset.label.to_string(),
            package: preset.package.to_string(),
        }
    }

    pub fn custom(input: &str) -> Result<Self, String> {
        let package = validate_package(input)?;
        Ok(Self {
            label: package.clone(),
            package,
        })
    }

    /// Name for the UI and for messages.
    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn package(&self) -> &str {
        &self.package
    }

    /// Where the game keeps its save on the device.
    pub fn save_dir(&self) -> String {
        format!("/sdcard/Android/data/{}/files/No_Backup", self.package)
    }
}

pub fn find_preset(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|preset| preset.id == id)
}

fn default_preset() -> &'static Preset {
    find_preset(DEFAULT_ID).unwrap_or(&PRESETS[0])
}

/// Turn the stored picker choice into a game.
///
/// An id that no longer exists falls back to the default game; an invalid
/// custom package is reported so the UI can show why.
pub fn resolve(id: &str, custom_package: &str) -> Result<Game, String> {
    if id == CUSTOM_ID {
        return Game::custom(custom_package);
    }
    Ok(Game::from_preset(
        find_preset(id).unwrap_or_else(default_preset),
    ))
}

/// Check an Android package name and return it trimmed.
///
/// Rules follow Android's: at least two dot separated parts, each starting
/// with a letter and containing only letters, digits and underscores. This
/// also keeps anything shell-special out of the commands run on the device.
pub fn validate_package(input: &str) -> Result<String, String> {
    let package = input.trim();
    if package.is_empty() {
        return Err("Enter the game's package name, for example com.example.game.".to_string());
    }

    let parts: Vec<&str> = package.split('.').collect();
    if parts.len() < 2 {
        return Err(
            "A package name needs at least two parts separated by dots, like com.example.game."
                .to_string(),
        );
    }

    for part in parts {
        if part.is_empty() {
            return Err(
                "A package name cannot start or end with a dot or contain two dots in a row."
                    .to_string(),
            );
        }
        let mut characters = part.chars();
        let starts_with_letter = characters
            .next()
            .is_some_and(|character| character.is_ascii_alphabetic());
        let rest_is_valid =
            characters.all(|character| character.is_ascii_alphanumeric() || character == '_');
        if !starts_with_letter || !rest_is_valid {
            return Err(format!(
                "\"{part}\" is not valid: each part must start with a letter and use only letters, digits and underscores."
            ));
        }
    }

    Ok(package.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn presets_are_valid_and_unique() {
        let mut ids = HashSet::new();
        let mut packages = HashSet::new();
        for preset in PRESETS {
            assert!(ids.insert(preset.id), "duplicate id {}", preset.id);
            assert!(
                packages.insert(preset.package),
                "duplicate package {}",
                preset.package
            );
            assert_ne!(preset.id, CUSTOM_ID);
            assert_eq!(
                validate_package(preset.package).as_deref(),
                Ok(preset.package),
                "invalid package for {}",
                preset.id
            );
        }
    }

    #[test]
    fn default_preset_is_reflourished() {
        assert!(find_preset(DEFAULT_ID).is_some());
        assert_eq!(default_preset().package, "com.ea.game.pvz2_rfl");
    }

    #[test]
    fn accepts_valid_packages_and_trims() {
        assert_eq!(
            validate_package("com.foo.bar").as_deref(),
            Ok("com.foo.bar")
        );
        assert_eq!(
            validate_package("  org.example.game_2 ").as_deref(),
            Ok("org.example.game_2")
        );
        assert_eq!(validate_package("net.x").as_deref(), Ok("net.x"));
    }

    #[test]
    fn rejects_invalid_packages() {
        for bad in [
            "",
            "   ",
            "single",
            "com..x",
            ".com.x",
            "com.x.",
            "com.1x",
            "com.x y",
            "com.x-y",
            "com.x;reboot",
            "com.x'y",
            "com.x$(id)",
            "com.x/../y",
        ] {
            assert!(validate_package(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn builds_the_save_directory_from_the_package() {
        let game = Game::custom("com.foo.bar").unwrap();
        assert_eq!(game.package(), "com.foo.bar");
        assert_eq!(game.label(), "com.foo.bar");
        assert_eq!(
            game.save_dir(),
            "/sdcard/Android/data/com.foo.bar/files/No_Backup"
        );
    }

    #[test]
    fn resolves_presets_custom_and_unknown_ids() {
        let base = resolve("base_international", "").unwrap();
        assert_eq!(base.package(), "com.ea.game.pvz2_row");

        let custom = resolve(CUSTOM_ID, " com.foo.bar ").unwrap();
        assert_eq!(custom.package(), "com.foo.bar");

        assert!(resolve(CUSTOM_ID, "nonsense").is_err());

        let fallback = resolve("removed_in_a_later_version", "").unwrap();
        assert_eq!(fallback.package(), "com.ea.game.pvz2_rfl");
    }
}
