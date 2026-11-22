//! `cargo xtask <command>` — dev automation (Owner: Architect; bundle/install bodies: macOS driver engineer).
//!
//! Commands:
//! - `check-windows [--with-dfn]` : `cargo xwin check --target x86_64-pc-windows-msvc` on the
//!   portable crates (excludes `yeti-driver-mac`) with `--no-default-features`, i.e. `aec` AND
//!   `dfn` off. DFN's `tract-linalg` build script needs `llvm-lib` to cross-assemble its x86_64
//!   kernels, which is not available on this Mac; `--with-dfn` tries anyway (verify DFN on Windows).
//! - `bundle-mac [--driver-only]` : release build, assemble + ad-hoc sign + verify
//!   `dist/YetiStudioMic.driver` (then run the driver harness against the signed bundle) and
//!   `dist/Yeti Studio.app` (skipped with `--driver-only`).
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

fn target_dir() -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .map(|p| {
            if p.is_absolute() {
                p
            } else {
                workspace_root().join(p)
            }
        })
        .unwrap_or_else(|| workspace_root().join("target"))
}

/// Runs an external tool; error message on failure.
fn tool(program: &str, args: &[&str]) -> Result<(), String> {
    let mut c = Command::new(program);
    c.args(args);
    if run(c) == ExitCode::SUCCESS {
        Ok(())
    } else {
        Err(format!("{program} failed"))
    }
}

fn copy_file(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::copy(from, to)
        .map(|_| ())
        .map_err(|e| format!("copy {} -> {}: {e}", from.display(), to.display()))
}

/// Fresh `<bundle>/Contents/MacOS` (removes any previous bundle at that path).
fn fresh_bundle(bundle: &Path) -> Result<PathBuf, String> {
    if bundle.exists() {
        std::fs::remove_dir_all(bundle).map_err(|e| format!("rm {}: {e}", bundle.display()))?;
    }
    let macos = bundle.join("Contents").join("MacOS");
    std::fs::create_dir_all(&macos).map_err(|e| format!("mkdir {}: {e}", macos.display()))?;
    Ok(macos)
}

/// Ad-hoc sign, then verify signature and Info.plist.
fn sign_and_verify(bundle: &Path) -> Result<(), String> {
    let b = bundle.to_str().ok_or("non-UTF-8 path")?;
    let plist = bundle.join("Contents").join("Info.plist");
    tool(
        "plutil",
        &["-lint", plist.to_str().ok_or("non-UTF-8 path")?],
    )?;
    tool(
        "codesign",
        &["--force", "--sign", "-", "--timestamp=none", b],
    )?;
    tool("codesign", &["--verify", "--strict", "--verbose=2", b])
}

/// `dist/YetiStudioMic.driver`: release cdylib + Info.plist, ad-hoc signed, then the
/// pre-install harness is run against the signed bundle (dlopen + CFPlugIn load).
fn bundle_driver(dist: &Path) -> Result<PathBuf, String> {
    let mut c = cargo();
    c.args(["build", "--release", "-p", "yeti-driver-mac", "--lib"]);
    if run(c) != ExitCode::SUCCESS {
        return Err("driver build failed".into());
    }
    let bundle = dist.join(DRIVER_BUNDLE);
    let macos = fresh_bundle(&bundle)?;
    let exe = macos.join(DRIVER_EXECUTABLE);
    copy_file(
        &target_dir()
            .join("release")
            .join("libyeti_driver_mac.dylib"),
        &exe,
    )?;
    // Replace the absolute target/ install name with a relocatable one (before signing).
    tool(
        "install_name_tool",
        &[
            "-id",
            "@rpath/YetiStudioMic",
            exe.to_str().ok_or("non-UTF-8 path")?,
        ],
    )?;
    copy_file(
        &workspace_root().join("crates/yeti-driver-mac/Info.plist"),
        &bundle.join("Contents").join("Info.plist"),
    )?;
    sign_and_verify(&bundle)?;

    // Gate: load the signed artifact in-process like coreaudiod would (nothing installed).
    let mut t = cargo();
    t.args([
        "test",
        "--release",
        "-p",
        "yeti-driver-mac",
        "--test",
        "harness",
        "--",
        "--nocapture",
        "--test-threads=1",
    ]);
    t.env("YETI_DRIVER_DYLIB", &exe)
        .env("YETI_DRIVER_BUNDLE", &bundle);
    if run(t) != ExitCode::SUCCESS {
        return Err("driver harness failed against the signed bundle".into());
    }
    Ok(bundle)
}

const APP_BUNDLE: &str = "Yeti Studio.app";
const APP_EXECUTABLE: &str = "yeti-studio";
const DRIVER_EXECUTABLE: &str = "YetiStudioMic";

fn app_info_plist() -> String {
    let version = env!("CARGO_PKG_VERSION");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleDevelopmentRegion</key>
	<string>en</string>
	<key>CFBundleExecutable</key>
	<string>{APP_EXECUTABLE}</string>
	<key>CFBundleIdentifier</key>
	<string>local.yetistudio.app</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleName</key>
	<string>Yeti Studio</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>{version}</string>
	<key>CFBundleVersion</key>
	<string>1</string>
	<key>LSMinimumSystemVersion</key>
	<string>14.2</string>
	<key>NSHighResolutionCapable</key>
	<true/>
	<key>NSMicrophoneUsageDescription</key>
	<string>Yeti Studio cleans up the voice from your Blue Yeti microphone.</string>
	<key>NSAudioCaptureUsageDescription</key>
	<string>Yeti Studio uses system audio as the echo-cancellation reference in Meeting mode.</string>
</dict>
</plist>
"#
    )
}

/// `dist/Yeti Studio.app`: release `yeti-studio` binary + Info.plist, ad-hoc signed.
fn bundle_app(dist: &Path) -> Result<PathBuf, String> {
    let mut c = cargo();
    c.args([
        "build",
        "--release",
        "-p",
        "yeti-app",
        "--bin",
        APP_EXECUTABLE,
    ]);
    if run(c) != ExitCode::SUCCESS {
        return Err("app build failed".into());
    }
    let bundle = dist.join(APP_BUNDLE);
    let macos = fresh_bundle(&bundle)?;
    copy_file(
        &target_dir().join("release").join(APP_EXECUTABLE),
        &macos.join(APP_EXECUTABLE),
    )?;
    let plist = bundle.join("Contents").join("Info.plist");
    std::fs::write(&plist, app_info_plist())
        .map_err(|e| format!("write {}: {e}", plist.display()))?;
    sign_and_verify(&bundle)?;
    Ok(bundle)
}

/// `bundle-mac [--driver-only]`
fn bundle_mac(driver_only: bool) -> ExitCode {
    if !cfg!(target_os = "macos") {
        eprintln!("bundle-mac only runs on macOS");
        return ExitCode::FAILURE;
    }
    let dist = workspace_root().join("dist");
    let result = bundle_driver(&dist).and_then(|drv| {
        let app = if driver_only {
            None
        } else {
            Some(bundle_app(&dist)?)
        };
        Ok((drv, app))
    });
    match result {
        Ok((drv, app)) => {
            eprintln!("bundle-mac: OK\n  {}", drv.display());
            if let Some(app) = app {
                eprintln!("  {}", app.display());
            }
            eprintln!("Install the driver with the commands from `cargo xtask install-driver`.");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("bundle-mac: {e}");
            ExitCode::FAILURE
        }
    }
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
        Some("bundle-mac") => bundle_mac(std::env::args().any(|a| a == "--driver-only")),
        Some("install-driver") => install_driver(),
        _ => {
            eprintln!(
                "usage: cargo xtask <check-windows [--with-dfn]|bundle-mac [--driver-only]|install-driver>"
            );
            ExitCode::FAILURE
        }
    }
}
