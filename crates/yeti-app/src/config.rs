//! Persistent app configuration: TOML file in the OS config dir.
//!
//! - macOS: `~/Library/Application Support/com.YetiStudio.Yeti-Studio/config.toml`
//! - Windows: `%APPDATA%\YetiStudio\Yeti Studio\config\config.toml`
//!
//! Unknown/missing keys fall back to defaults (`#[serde(default)]`), so old files keep loading.
//! The software mute (`params.output_muted`) is never restored: the app always starts live.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use yeti_types::{DspParams, MonitorSettings, Preset, Theme, ViewMode};

/// Config file name inside the config dir.
pub const CONFIG_FILE: &str = "config.toml";

/// Global hotkeys (strings parsed by `global-hotkey`, e.g. `"CmdOrCtrl+Shift+M"`).
/// Empty string = disabled. Defaults are modifier combos because registered hotkeys are
/// consumed system-wide (PLAN.md §5 risk 10): a bare key would never reach the game.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HotkeyConfig {
    /// Toggle software mute of the virtual mic.
    pub mute_toggle: String,
    /// Hold to talk (mic muted while released). Disabled by default.
    pub push_to_talk: String,
    /// Show / hide the main window.
    pub show_window: String,
    /// Open / close the Mini window.
    pub toggle_mini: String,
}

/// Default mute toggle. Windows: Ctrl+Alt+M, because Ctrl+Shift+M is Microsoft Teams' own
/// mute shortcut and a registered global hotkey would swallow it system-wide.
#[cfg(windows)]
pub const DEFAULT_MUTE_HOTKEY: &str = "Ctrl+Alt+KeyM";
#[cfg(not(windows))]
pub const DEFAULT_MUTE_HOTKEY: &str = "CmdOrCtrl+Shift+M";

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            mute_toggle: DEFAULT_MUTE_HOTKEY.into(),
            push_to_talk: String::new(),
            show_window: "CmdOrCtrl+Shift+Y".into(),
            toggle_mini: "CmdOrCtrl+Shift+U".into(),
        }
    }
}

/// Everything the app persists between runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    /// Config format version (bump on breaking changes).
    pub version: u32,
    pub theme: Theme,
    /// Main window view: `Simple` or `Pro` (`Mini` is stored as `mini_open`).
    pub view: ViewMode,
    /// Whether the Mini window was open at exit.
    pub mini_open: bool,
    /// Last preset (informational; `params` is authoritative).
    pub preset: Preset,
    pub params: DspParams,
    pub monitor: MonitorSettings,
    pub hotkeys: HotkeyConfig,
    /// Closing the main window hides it to the tray / menu bar instead of quitting.
    pub close_to_tray: bool,
    /// First-run tips (disable Krisp/Zoom/Teams suppression) dismissed.
    pub tips_dismissed: bool,
    /// A/B test recording length, seconds.
    pub ab_seconds: f32,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: 1,
            theme: Theme::System,
            view: ViewMode::Simple,
            mini_open: false,
            preset: Preset::Gaming,
            params: DspParams::for_preset(Preset::Gaming),
            monitor: MonitorSettings::default(),
            hotkeys: HotkeyConfig::default(),
            close_to_tray: true,
            tips_dismissed: false,
            ab_seconds: 10.0,
        }
    }
}

/// Config load/save error.
#[derive(Debug)]
pub enum ConfigError {
    Io(std::io::Error),
    Parse(toml::de::Error),
    Serialize(toml::ser::Error),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "config I/O error: {e}"),
            Self::Parse(e) => write!(f, "config parse error: {e}"),
            Self::Serialize(e) => write!(f, "config serialize error: {e}"),
        }
    }
}

impl std::error::Error for ConfigError {}

impl AppConfig {
    /// Default config path (`None` if the OS has no home/config dir).
    pub fn default_path() -> Option<PathBuf> {
        directories::ProjectDirs::from("com", "YetiStudio", "Yeti Studio")
            .map(|d| d.config_dir().join(CONFIG_FILE))
    }

    /// Parse from TOML text, normalising values that must not be restored.
    pub fn from_toml(text: &str) -> Result<Self, ConfigError> {
        let mut cfg: Self = toml::from_str(text).map_err(ConfigError::Parse)?;
        cfg.normalise();
        Ok(cfg)
    }

    pub fn to_toml(&self) -> Result<String, ConfigError> {
        toml::to_string_pretty(self).map_err(ConfigError::Serialize)
    }

    /// Load from `path`. A missing file yields the defaults.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::from_toml(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(ConfigError::Io(e)),
        }
    }

    /// Load, falling back to defaults (with a log line) if the file is unreadable/corrupt.
    pub fn load_or_default(path: &Path) -> Self {
        Self::load(path).unwrap_or_else(|e| {
            log::warn!("{}: {e}; using defaults", path.display());
            Self::default()
        })
    }

    /// Save atomically (write `*.tmp`, then rename), creating the directory if needed.
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(ConfigError::Io)?;
        }
        let text = self.to_toml()?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text).map_err(ConfigError::Io)?;
        std::fs::rename(&tmp, path).map_err(ConfigError::Io)
    }

    fn normalise(&mut self) {
        self.params.output_muted = false;
        if self.view == ViewMode::Mini {
            self.view = ViewMode::Simple;
            self.mini_open = true;
        }
        if !self.ab_seconds.is_finite() {
            self.ab_seconds = 10.0;
        }
        self.ab_seconds = self.ab_seconds.clamp(1.0, 60.0);
        self.monitor.volume = self.monitor.volume.clamp(0.0, 1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!("yeti-app-test-{}-{name}", std::process::id()))
            .join(CONFIG_FILE)
    }

    #[test]
    fn round_trip_file() {
        let mut cfg = AppConfig {
            theme: Theme::Dark,
            view: ViewMode::Pro,
            mini_open: true,
            preset: Preset::Custom,
            close_to_tray: false,
            tips_dismissed: true,
            ab_seconds: 7.5,
            ..Default::default()
        };
        cfg.params.noise.strength = 0.42;
        cfg.params.gate.threshold_db = -61.0;
        cfg.params.echo_cancel.enabled = true;
        cfg.monitor = MonitorSettings {
            enabled: true,
            volume: 0.3,
        };
        cfg.hotkeys.push_to_talk = "Alt+KeyT".into();

        let path = temp_path("rt");
        cfg.save(&path).unwrap();
        let back = AppConfig::load(&path).unwrap();
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
        assert_eq!(back, cfg);
    }

    #[test]
    fn missing_file_gives_defaults() {
        let path = temp_path("missing");
        assert_eq!(AppConfig::load(&path).unwrap(), AppConfig::default());
    }

    #[test]
    fn partial_file_fills_defaults_and_never_restores_mute() {
        let cfg = AppConfig::from_toml(
            "theme = \"light\"\nview = \"mini\"\n[params]\noutput_muted = true\n[params.noise]\nstrength = 0.25\n",
        )
        .unwrap();
        assert_eq!(cfg.theme, Theme::Light);
        assert_eq!(cfg.view, ViewMode::Simple);
        assert!(cfg.mini_open);
        assert!(!cfg.params.output_muted);
        assert_eq!(cfg.params.noise.strength, 0.25);
        assert_eq!(cfg.params.gate, DspParams::default().gate);
        assert_eq!(cfg.hotkeys, HotkeyConfig::default());
    }

    #[test]
    fn corrupt_file_is_an_error() {
        assert!(AppConfig::from_toml("theme = [").is_err());
    }

    #[test]
    fn default_hotkeys_are_modifier_combos_and_parse() {
        let h = HotkeyConfig::default();
        for s in [&h.mute_toggle, &h.show_window, &h.toggle_mini] {
            assert!(s.contains('+'), "{s} must be a modifier combo");
            assert!(s.parse::<global_hotkey::hotkey::HotKey>().is_ok(), "{s}");
        }
        // The Windows default (cfg'd out on other platforms) must parse too.
        assert!(
            "Ctrl+Alt+KeyM"
                .parse::<global_hotkey::hotkey::HotKey>()
                .is_ok()
        );
    }
}
