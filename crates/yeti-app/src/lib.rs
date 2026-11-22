//! Yeti Studio desktop app (PLAN.md Phase 3).
//!
//! - [`ui`]: platform-free view logic (Simple / Pro / Mini) bound to `Arc<dyn Engine>`;
//!   tested with `egui_kittest` (`tests/views.rs`).
//! - [`shell`]: eframe `App` owning windows, close-to-tray, Mini viewport (always on top).
//! - [`tray`], [`hotkeys`]: tray / menu-bar icon and global hotkeys (main thread, created
//!   after the event loop started; handlers call `request_repaint()`).
//! - [`config`]: TOML config in the OS config dir. [`theme`]: palette + Light/Dark/System.

pub mod config;
pub mod hotkeys;
pub mod shell;
pub mod theme;
pub mod tray;
pub mod ui;
pub mod widgets;

pub use config::AppConfig;
pub use shell::{DesktopApp, ShellOptions};
pub use ui::{Command, YetiUi};
