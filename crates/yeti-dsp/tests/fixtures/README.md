# yeti-dsp test fixtures

- `speech_tts.wav` — 12.9 s, 48 kHz mono 16-bit. macOS `say` (default voice), generated on
  2026-11-22 with:

  ```sh
  say -o speech_tts.wav --file-format=WAVE --data-format=LEI16@48000 \
    "[[slnc 1200]] Push the button and check the left flank. [[slnc 900]] Six enemies are coming over the bridge, so stay close. [[slnc 900]] Let's share the slides after this meeting. [[slnc 1200]]"
  ```

  It is TTS, not a human recording (no real speech clip was available offline).

Noise (keyboard, fan) and the speech-like signal are synthesised deterministically in
`src/fixtures.rs`; see the limitation note there and in `docs/DSP_RESULTS.md`.
