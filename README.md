# Yeti Studio

Clean-voice software for the **[Yeti Premium USB Microphone](https://www.logitechg.com/en-us/shop/p/yeti-premium-usb-microphone.988-000100)**
(Blue Yeti, part no. 988-000100; tested with USB ID `046d:0ab7`), written in Rust.
It removes keyboard clicks, fan and wind rumble and room noise in real time and exposes the result as a
virtual microphone ("**Yeti Studio Mic**") that Discord, Zoom, Teams, OBS or any game can use.

Built for two situations: **gaming voice chat** (lowest delay) and **meetings** (echo cancellation, even loudness).

> Personal, unofficial project. Not affiliated with or endorsed by Logitech or Blue. "Blue Yeti" is a trademark of its owner.

## Features

- **AI noise removal** – [DeepFilterNet 3](https://github.com/Rikorose/DeepFilterNet) (default) or RNNoise (low CPU), with a strength slider.
- **Full voice chain** – rumble filter → noise removal → voice gate → voice EQ + de-esser → auto-level → limiter, plus WebRTC echo cancellation in the Meeting preset.
- **Presets** – *Gaming* (lowest delay) and *Meeting* (echo cancel, firmer auto-level); every stage is adjustable in the Pro view.
- **Yeti hardware control** – digital input gain (capped at 0 dB unless unlocked), hardware mute, headphone volume/mute, sample rate.
- **Virtual microphone (macOS)** – a CoreAudio AudioServerPlugIn written in Rust.
- **UI** – egui app with Simple, Pro and an always-on-top Mini window; light/dark/system themes; tray/menu-bar icon; global hotkeys (mute, push-to-talk, show window, Mini).
- **Safety and feedback** – live meters, "what your team hears" waveform, clipping warning, A/B recorder (raw vs cleaned), glitch counters.
- **Drift-compensated audio engine** – the Yeti, the virtual mic and system audio run on independent clocks; an adaptive resampler keeps them in sync.

## Status

| Platform | State |
|---|---|
| macOS (Apple Silicon, macOS 27) | Working end to end: app, voice chain, virtual mic driver. Tested with a real Yeti. |
| Windows | Compiles (cross-checked with `cargo-xwin`); **not yet run on Windows**. Uses [VB-CABLE](https://vb-audio.com/Cable/) as the virtual mic. |

Builds are **unsigned** (ad-hoc on macOS). See [Limitations](#limitations).

### Measured on macOS (Apple Silicon)

| Preset | Noise model | End-to-end delay | CPU |
|---|---|---|---|
| Gaming | DeepFilterNet | ~66 ms | ~4 % |
| Gaming | RNNoise | ~47 ms | ~1 % |
| Meeting (AEC) | DeepFilterNet | ~77 ms | ~5 % |
| Meeting (AEC) | RNNoise | ~56 ms | ~3 % |

Details: [`docs/LATENCY.md`](docs/LATENCY.md), [`docs/DSP_RESULTS.md`](docs/DSP_RESULTS.md).

## Requirements

- Rust **1.100+** (`rustup`), Xcode command-line tools on macOS.
- `meson` and `ninja` for the echo-cancellation build (`brew install meson ninja`).
- Optional: `cargo install cargo-xwin` to cross-check the Windows build from macOS.

## Build and run (macOS)

```sh
git clone git@github.com:ranjithp04/yeti_studio.git
cd yeti_studio

cargo xtask bundle-mac          # builds dist/Yeti Studio.app and dist/YetiStudioMic.driver
open "dist/Yeti Studio.app"
```

Install the virtual microphone (requires admin; restarts CoreAudio, causing a ~1 s audio dropout):

```sh
cargo xtask install-driver      # prints the exact commands, runs nothing
sudo cp -R dist/YetiStudioMic.driver /Library/Audio/Plug-Ins/HAL/
sudo chown -R root:wheel /Library/Audio/Plug-Ins/HAL/YetiStudioMic.driver
sudo killall coreaudiod
```

Then select **Yeti Studio Mic** as the microphone in your voice app, and turn off that app's own noise
suppression (Discord: Krisp off; Zoom: Low / Original sound; Teams: Off) — two suppressors in a row sound robotic.

Uninstall: `sudo rm -rf /Library/Audio/Plug-Ins/HAL/YetiStudioMic.driver && sudo killall coreaudiod`.

## Windows (untested)

1. Install [VB-CABLE](https://vb-audio.com/Cable/) and reboot.
2. `cargo build --release -p yeti-app` (add `--no-default-features` to drop DeepFilterNet if its build fails).
3. Run `target\release\yeti-studio.exe`; choose **CABLE Output** as the microphone in your voice app.

## Recommended Yeti setup

- Pattern knob on **cardioid**; talk into the front (the side with the logo and mute button), 10–15 cm away.
- Set the level with the **analog GAIN knob** (about 12 o'clock is a good start); keep the digital gain at 0 dB.
- Don't point a fan at the mic — airflow clips the input before any software can help.

## Command-line tools

| Tool | Purpose |
|---|---|
| `yeti-studio [--mock] [--mini] [--hidden] [--no-tray] [--no-hotkeys]` | The app (`--mock` runs the UI without audio). |
| `yeti-probe` | Lists the Yeti's controls; sets and restores gain. |
| `yeti-passthrough --seconds N [--preset gaming\|meeting] [--output auto\|yeti\|<device>] [--audible] [--reference] [--probe]` | Headless engine run with counters, levels and latency. |
| `yeti-render IN.wav OUT.wav [--preset ...] [--ns dfn\|rnnoise\|off] [--strength 0..1]` | Run the voice chain offline on a 48 kHz WAV. |
| `cargo xtask check-windows` | Cross-check the portable crates for `x86_64-pc-windows-msvc`. |

Diagnostics: `YETI_STATS_LOG=1` logs engine counters every 2 s; `YETI_TEST_TONE=<Hz>` replaces the mic with a test tone.

> Tip: don't judge audio quality from `ffmpeg -f avfoundation` recordings on macOS — they drop ~10 ms chunks.
> Record the virtual mic with a CoreAudio-native recorder instead.

## Project layout

```
crates/
  yeti-types       shared types: presets, DSP params, meters, events
  yeti-device      Yeti discovery and hardware control (CoreAudio / WASAPI)
  yeti-dsp         voice chain, DeepFilterNet/RNNoise/AEC, offline renderer, benchmarks
  yeti-engine      real-time engine: capture, DSP worker, drift-compensated output, loopback reference
  yeti-app         egui app (Simple / Pro / Mini, tray, hotkeys, config)
  yeti-driver-mac  macOS AudioServerPlugIn "Yeti Studio Mic"
xtask/             bundle, driver install helper, Windows check
docs/              API contracts, DSP results, latency measurements
spikes/            DeepFilterNet build spike
PLAN.md            build plan and review log
```

## Development

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo xtask check-windows
```

## Limitations

- Unsigned builds: macOS may ask again for microphone permission after a rebuild; distributing to others needs an Apple Developer ID (and notarization), and a Windows code-signing certificate.
- Windows has never been run on real hardware; DeepFilterNet's Windows build is not verified.
- The pattern knob and analog gain knob are hardware-only.
- Global hotkeys are consumed system-wide (a push-to-talk key does not reach the game).

## License

[MIT](LICENSE)
