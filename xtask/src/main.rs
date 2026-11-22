//! `cargo xtask <command>` — dev automation (Owner: Architect; bundle/install bodies: macOS driver engineer).
//!
//! Commands:
//! - `check-windows [--with-dfn]` : `cargo xwin check --target x86_64-pc-windows-msvc` on the
//!   portable crates (excludes `yeti-driver-mac`) with `--no-default-features`, i.e. `aec` AND
//!   `dfn` off. DFN's `tract-linalg` build script needs `llvm-lib` to cross-assemble its x86_64
//!   kernels, which is not available on this Mac; `--with-dfn` tries anyway (verify DFN on Windows).
//! - `bundle-mac`     : build release + assemble `dist/Yeti Studio.app` and `dist/YetiStudioMic.driver` (stub).
//! - `install-driver` : PRINT the sudo commands to install the driver. Never runs them.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const WIN_TARGET: &str = "x86_64-pc-windows-msvc";
/// Crates that must cross-check for Windows. `yeti-driver-mac` is macOS-only.
const PORTABLE: &[&str] = &[
    "yeti-types",
    "yeti-device",
    "yeti-dsp",
    "yeti-engine",
    "yeti-app",
    "xtask",
];
const DRIVER_BUNDLE: &str = "YetiStudioMic.driver";
const HAL_DIR: &str = "/Library/Audio/Plug-Ins/HAL";

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a parent dir")
        .to_path_buf()
}

fn cargo() -> Command {
    let mut c = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    c.current_dir(workspace_root());
    c
}

fn run(mut c: Command) -> ExitCode {
    eprintln!("$ {c:?}");
    match c.status() {
        Ok(s) if s.success() => ExitCode::SUCCESS,
        Ok(s) => {
            eprintln!("command failed: {s}");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("failed to spawn: {e}");
            ExitCode::FAILURE
        }
    }
}

fn check_windows(with_dfn: bool) -> ExitCode {
    let mut c = cargo();
    c.args([
        "xwin",
        "check",
        "--target",
        WIN_TARGET,
        "--no-default-features",
    ]);
    for p in PORTABLE {
        c.args(["-p", p]);
    }
    // AEC is never enabled here (off for Windows until built on real Windows).
    if with_dfn {
        c.args(["--features", "yeti-dsp/dfn"]);
    }
    run(c)
}

fn bundle_mac() -> ExitCode {
    if !cfg!(target_os = "macos") {
        eprintln!("bundle-mac only runs on macOS");
        return ExitCode::FAILURE;
    }
    let mut c = cargo();
    c.args([
        "build",
        "--release",
        "-p",
        "yeti-app",
        "-p",
        "yeti-driver-mac",
    ]);
    let code = run(c);
    if code != ExitCode::SUCCESS {
        return code;
    }
    // TODO(Phase 5, macOS driver engineer): assemble bundles
    //   dist/Yeti Studio.app/Contents/{Info.plist,MacOS/yeti-studio}
    //   dist/YetiStudioMic.driver/Contents/{Info.plist,MacOS/YetiStudioMic}
    //   then `codesign -s - --force --deep` + `plutil -lint`.
    eprintln!("bundle-mac: release binaries built; bundle assembly not implemented yet (Phase 5).");
    ExitCode::SUCCESS
}

fn install_driver() -> ExitCode {
    let bundle = workspace_root().join("dist").join(DRIVER_BUNDLE);
    println!("# Run these yourself (requires admin). xtask never runs sudo.");
    println!("# Installing restarts coreaudiod: all audio drops out for ~1 s.");
    println!("sudo rm -rf '{HAL_DIR}/{DRIVER_BUNDLE}'");
    println!("sudo cp -R '{}' '{HAL_DIR}/'", bundle.display());
    println!("sudo chown -R root:wheel '{HAL_DIR}/{DRIVER_BUNDLE}'");
    println!("sudo killall coreaudiod");
    println!("# Verify: system_profiler SPAudioDataType | grep -A3 'Yeti Studio Mic'");
    println!("# Uninstall: sudo rm -rf '{HAL_DIR}/{DRIVER_BUNDLE}' && sudo killall coreaudiod");
    if !bundle.exists() {
        eprintln!(
            "note: {} does not exist yet — run `cargo xtask bundle-mac` first.",
            bundle.display()
        );
    }
    ExitCode::SUCCESS
}

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("check-windows") => check_windows(std::env::args().any(|a| a == "--with-dfn")),
        Some("bundle-mac") => bundle_mac(),
        Some("install-driver") => install_driver(),
        _ => {
            eprintln!("usage: cargo xtask <check-windows [--with-dfn]|bundle-mac|install-driver>");
            ExitCode::FAILURE
        }
    }
}
