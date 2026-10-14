//! Persistent application settings, stored as JSON in the user config
//! directory (`~/.config/pmd85/` on Linux via the `directories` crate).
//!
//! CLI arguments override the persisted values for that run; changes
//! made in the UI are written back immediately (the file is small).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use pmd85_core::Model;
use serde::{Deserialize, Serialize};

/// Settings that survive an app restart.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    /// Model to boot (`Model::from_str_loose` form, e.g. `"85-3"`).
    pub model: String,
    /// Monitor ROM file name to boot with (`None` = the model's
    /// default from the ROM directory).
    pub monitor: Option<String>,
    /// ROM module file name to attach at boot (in the ROM directory).
    pub rom_module: Option<String>,
    /// Speaker muted.
    pub mute: bool,
    /// Theme name (built-in preset or a custom theme in the themes dir).
    pub theme: String,
    /// Speed multiplier (1.0 = real time).
    pub speed: f64,
    /// Play the tape data tone through the speaker (tape monitor).
    pub tape_monitor: bool,
    /// Stop tape playback at the next file header.
    pub tape_autostop: bool,
    /// Flash-load played files: the monitor's tape read loops are
    /// intercepted and fed the block data directly (settings key kept
    /// from the old "warp loads" for compatibility).
    pub tape_warp: bool,
    /// Draw the custom titlebar (minimize/maximize/close buttons,
    /// drag) instead of the system window frame.
    pub custom_titlebar: bool,
    /// Master switch for the gamepad-to-joystick input path.
    pub joystick_enabled: bool,
    /// Per-gamepad assignment (key = the pad's UUID in hex): which
    /// joystick connector the pad drives. Pads not in the map drive
    /// GPIO/0, the connector virtually all games read.
    pub gamepad_ports: BTreeMap<String, GamepadPort>,
    /// Screen rendering: color profile and CRT simulation, applied
    /// live (no restart).
    pub screen: ScreenSettings,
}

/// Screen rendering settings: the color decoding of the attribute
/// bits and the CRT simulation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScreenSettings {
    /// Color decoding of the attribute bits.
    pub profile: ScreenProfile,
    /// Monochrome phosphor color (RGB bytes), used by the Mono
    /// profile.
    pub phosphor: [u8; 3],
    /// The CRT simulation (scanlines, glow, vignette).
    pub crt: CrtSettings,
}

impl Default for ScreenSettings {
    fn default() -> Self {
        ScreenSettings {
            profile: ScreenProfile::Rgb,
            // Today's hard-coded green.
            phosphor: [0x00, 0xD0, 0x00],
            crt: CrtSettings::default(),
        }
    }
}

/// Color decoding of the VRAM attribute bits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScreenProfile {
    /// Four colors as on the RGB output of a PMD 85-3: green, red,
    /// blue, violet on black.
    #[default]
    Rgb,
    /// Monochrome TV output, in the phosphor color.
    Mono,
    /// The ColorAce hardware mod: attributes mix across microrow
    /// pairs, 7 colors on black (see
    /// https://pmd85.borik.net/wiki/ColorAce).
    ColorAce,
}

impl ScreenProfile {
    /// The core's decode profile.
    pub fn decode(self) -> pmd85_core::vram::ColorProfile {
        match self {
            ScreenProfile::Rgb => pmd85_core::vram::ColorProfile::Rgb,
            ScreenProfile::Mono => pmd85_core::vram::ColorProfile::Mono,
            ScreenProfile::ColorAce => pmd85_core::vram::ColorProfile::ColorAce,
        }
    }
}

/// The CRT simulation, rendered on the CPU over the decoded
/// framebuffer.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CrtSettings {
    /// CRT simulation on.
    pub enabled: bool,
    /// Scanline darkness (0..=1).
    pub scanlines: f32,
    /// Phosphor glow / bloom strength (0..=1).
    pub glow: f32,
    /// Vignette darkness (0..=1).
    pub vignette: f32,
}

impl Default for CrtSettings {
    fn default() -> Self {
        CrtSettings {
            enabled: false,
            scanlines: 0.35,
            glow: 0.25,
            vignette: 0.35,
        }
    }
}

/// Which joystick connector a gamepad drives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum GamepadPort {
    /// GPIO/0 (K3, port `4Ch`) — joystick 0.
    #[default]
    Gpio0,
    /// GPIO/1 (K4, port `4Dh`) — joystick 1.
    Gpio1,
    /// Ignore the pad entirely.
    Off,
}

impl GamepadPort {
    /// The connector index, or `None` for [`GamepadPort::Off`].
    pub fn connector(self) -> Option<usize> {
        match self {
            GamepadPort::Gpio0 => Some(0),
            GamepadPort::Gpio1 => Some(1),
            GamepadPort::Off => None,
        }
    }
}

impl Default for AppSettings {
    fn default() -> Self {
        AppSettings {
            model: Model::Pmd853.name().to_string(),
            monitor: None,
            rom_module: None,
            mute: false,
            theme: crate::ui::theme::DEFAULT_THEME.to_string(),
            speed: 1.0,
            tape_monitor: true,
            tape_autostop: true,
            tape_warp: true,
            custom_titlebar: true,
            joystick_enabled: true,
            gamepad_ports: BTreeMap::new(),
            screen: ScreenSettings::default(),
        }
    }
}

/// Where persistent state lives. `None` when the platform does not
/// provide a home directory (rare); settings are then simply not
/// persisted.
pub fn config_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "pmd85").map(|dirs| dirs.config_dir().to_path_buf())
}

/// Subdirectory holding custom theme files.
pub fn themes_dir() -> Option<PathBuf> {
    config_dir().map(|d| d.join("themes"))
}

/// The part of the session that is not the tape image itself:
/// what the tape editor looked like when the app exited.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionState {
    /// File the tape was last saved to (`None` = untitled).
    pub tape_path: Option<PathBuf>,
    /// Unsaved edits since the last load/save.
    pub tape_dirty: bool,
    /// Selected file in the tape editor.
    pub selected: Option<usize>,
}

impl SessionState {
    /// Load from `session.json` in `dir`, falling back to defaults
    /// (and logging) on a missing or corrupt file.
    pub fn load(dir: &Path) -> Self {
        let path = dir.join("session.json");
        match fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<SessionState>(&text) {
                Ok(state) => return state,
                Err(e) => log::warn!("cannot parse {}: {e}", path.display()),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => log::warn!("cannot read {}: {e}", path.display()),
        }
        SessionState::default()
    }

    /// Write to `session.json` in `dir`. Failures are logged, not fatal.
    pub fn save(&self, dir: &Path) {
        let path = dir.join("session.json");
        match serde_json::to_string_pretty(self) {
            Ok(text) => {
                if let Err(e) = fs::create_dir_all(dir).and_then(|()| fs::write(&path, text)) {
                    log::warn!("cannot write {}: {e}", path.display());
                }
            }
            Err(e) => log::warn!("cannot serialize the session: {e}"),
        }
    }
}

impl AppSettings {
    /// Load from `settings.json` in `dir`, falling back to defaults
    /// (and logging) on a missing or corrupt file.
    pub fn load(dir: &Path) -> Self {
        let path = dir.join("settings.json");
        match fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<AppSettings>(&text) {
                Ok(settings) => return settings,
                Err(e) => log::warn!("cannot parse {}: {e}", path.display()),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => log::warn!("cannot read {}: {e}", path.display()),
        }
        AppSettings::default()
    }

    /// Write to `settings.json` in `dir`. Failures are logged, not fatal.
    pub fn save(&self, dir: &Path) {
        let path = dir.join("settings.json");
        match serde_json::to_string_pretty(self) {
            Ok(text) => {
                if let Err(e) = fs::create_dir_all(dir).and_then(|()| fs::write(&path, text)) {
                    log::warn!("cannot write {}: {e}", path.display());
                }
            }
            Err(e) => log::warn!("cannot serialize settings: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("pmd85-settings-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn defaults_are_valid() {
        let s = AppSettings::default();
        assert_eq!(Model::from_str_loose(&s.model), Some(Model::Pmd853));
        assert_eq!(s.speed, 1.0);
        assert!(!s.mute);
        assert!(s.tape_monitor);
        assert!(s.tape_autostop);
        assert!(s.tape_warp);
        assert_eq!(s.screen, ScreenSettings::default());
        assert!(!s.screen.crt.enabled, "CRT off out of the box");
    }

    #[test]
    fn round_trip_through_disk() {
        let dir = test_dir("roundtrip");
        let s = AppSettings {
            model: "85-2A".into(),
            monitor: Some("monit2A.rom".into()),
            rom_module: Some("basic2A.rmm".into()),
            mute: true,
            theme: "Amber Terminal".into(),
            speed: 5.0,
            tape_monitor: false,
            tape_autostop: false,
            tape_warp: false,
            custom_titlebar: false,
            joystick_enabled: false,
            gamepad_ports: [(
                "00112233445566778899aabbccddeeff".to_string(),
                GamepadPort::Gpio1,
            )]
            .into_iter()
            .collect(),
            screen: ScreenSettings {
                profile: ScreenProfile::ColorAce,
                phosphor: [0xE8, 0xA0, 0x20],
                crt: CrtSettings {
                    enabled: true,
                    scanlines: 0.6,
                    glow: 0.4,
                    vignette: 0.2,
                },
            },
        };
        s.save(&dir);
        let loaded = AppSettings::load(&dir);
        assert_eq!(loaded, s);
        assert_eq!(Model::from_str_loose(&loaded.model), Some(Model::Pmd852a));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_gives_defaults() {
        let dir = test_dir("missing");
        assert_eq!(AppSettings::load(&dir), AppSettings::default());
    }

    #[test]
    fn corrupt_file_gives_defaults() {
        let dir = test_dir("corrupt");
        fs::write(dir.join("settings.json"), "{not json").unwrap();
        assert_eq!(AppSettings::load(&dir), AppSettings::default());
    }

    #[test]
    fn partial_file_keeps_defaults_for_missing_fields() {
        let dir = test_dir("partial");
        fs::write(dir.join("settings.json"), r#"{"mute": true}"#).unwrap();
        let s = AppSettings::load(&dir);
        assert!(s.mute);
        assert_eq!(Model::from_str_loose(&s.model), Some(Model::Pmd853));
        assert_eq!(s.speed, 1.0);
        assert_eq!(s.monitor, None, "monitor defaults to the model's own");
        // New settings keep their defaults when absent from the file.
        assert!(s.tape_monitor);
        assert_eq!(s.screen.profile, ScreenProfile::Rgb);
        assert!(!s.screen.crt.enabled);
        assert!(s.tape_autostop);
        assert!(s.tape_warp);
    }
}
