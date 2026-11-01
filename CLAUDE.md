# sonar-rollerbar

Windows utility: a global keyboard hook turns the Volume Up, Volume Down and Mute keys into changes
on a chosen audio output, with optional SteelSeries features. Security comes first because the hook
sees every keystroke on the system.

## Layout

- `src/win.rs`: the hook callback `keyboard_proc`, audio and other Win32 calls.
- `src/win/tray.rs`, `src/win/mixer.rs`: tray icon, menu and mixer window, on the `tray` thread.
- `src/win/loopback.rs`: HTTP and WebSocket limited to `127.0.0.1`.
- `src/main.rs`: safe code, covering options, output selection and the `audio` thread.
- `src/gg.rs`: the `gg` thread, for the OLED and Sonar features and the wheel's target.
- `src/gg_protocol.rs`, `src/config.rs`, `src/logfile.rs`, `src/mixer_ui.rs`: code that doesn't call
  Windows.
- `build.rs` and `assets/`: the icon and manifest, embedded with `windres`.
- `installer/sonar-rollerbar.iss`: Inno Setup script, per-user with no UAC.
- `.github/workflows/`: `ci.yml` builds and checks, `release.yml` publishes on a `v*` tag.

## Commands

Build inside WSL Debian. Running a toolchain on the Windows side triggers Smart App Control popups.

```sh
cargo clippy --locked --target x86_64-pc-windows-gnu -- -D warnings
cargo build --locked --target x86_64-pc-windows-gnu            # console and logs
cargo build --release --locked --target x86_64-pc-windows-gnu  # no console

# Tests of the code that doesn't call Windows, which run natively
rustc --edition 2024 --test src/config.rs -o /tmp/tests && /tmp/tests
rustc --edition 2024 --test src/gg_protocol.rs -o /tmp/tests && /tmp/tests
rustc --edition 2024 --test src/logfile.rs -o /tmp/tests && /tmp/tests
rustc --edition 2024 --test src/mixer_ui.rs -o /tmp/tests && /tmp/tests
```

Compile the installer from Windows after the release build:

```powershell
& "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe" /DAppVersion=0.1.1 installer\sonar-rollerbar.iss
```

The hook, the wheel, the tray and the installer can only be validated by running them on Windows.

## Rules

- All comments, docs, messages, options and the installer are written in English.
- In the hook callback, read only `vkCode` and `flags`. Pass every other key to `CallNextHookEx`
  at once, without storing it. Never print any key other than Volume Up, Volume Down and Mute, even
  in debug builds, and only with the console-only `log!` macro.
- `record!` writes to the log file and must never receive key data, not even volume keys. Keep the
  file bounded by the rotation in `src/logfile.rs`.
- The callback never calls COM, blocks or allocates: it only queues a job with `try_send`.
- Fail open: an error, a panic or a missing output lets the key through.
- Keep `unsafe` in `src/win.rs` and `src/win/`, with a `// SAFETY:` comment on every block.
- Keep `panic` on `unwind`, because the hook relies on `catch_unwind`.
- Keep `lto = "fat"` in the release profile. Without it, the executable imports `WS2_32.dll`.
- The `tray` and `gg` threads stay optional: the hook and audio threads never depend on them. The
  tray only acts on a click, and `open_in_shell` only receives the releases URL or logs folder.
- The SteelSeries options stay off by default, and with all of them off the program opens no
  connection. `win::loopback` must keep the host hard-coded to `127.0.0.1`.
- Choosing the wheel's target works from the Mute key that the `audio` thread already gets, so the
  hook reads no extra key. Play/Pause, Next and Previous stay untouched. On the Apex Pro, 2 and 3
  presses of the media button send Next and Previous, and 4 send Previous then Play/Pause, so no
  press count is free.
- The wheel's targets are ChatMix first, then the channels in mixer order. The Mic is always one.
  Master is one only with `wheel_master`, because moving it rescales every channel.
- If Sonar stops answering, the wheel must return to the output, so that keys keep working.
- No clipboard access, elevation, service or driver. Files stay in
  `%LOCALAPPDATA%\sonar-rollerbar`, apart from the sign-in shortcut.
- With the option to follow Sonar's output on, that output outranks `--device` and `--auto`.
- Don't link Sonar's Master to the output's volume. A sync was built and removed: Master rescales
  every channel, so each notch of the wheel moved the whole mix and left it offset.
- `windows` stays the only direct dependency, at an exact version. Any new crate must be added to
  the allowlist in `deny.toml` and needs a real justification.
- Pin GitHub Actions by commit SHA and build with `--locked`.

## How Sonar's volumes behave

Measured on Sonar in Classic mode through its local API:

- moving Master scales Game, Chat, Media and Aux by the same ratio, reversibly, but not the Mic;
- raising a channel above Master pushes Master up and leaves the other channels unchanged;
- lowering that channel again doesn't lower Master.

`/classicRedirections` and `/streamRedirections` give the output Sonar plays to as a Windows
endpoint ID, the same one `IMMDevice::GetId` returns. Classic mode has one route per channel: the
program uses Game's.

ChatMix is at `/v1/chatMix`, not at the `/chatMix` that community clients use, with `balance` from
-1 to 1. Sonar logs a warning above 3 calls in 500 ms there, so the `gg` thread spaces them out. The
GG window refuses software ChatMix with a headset that has its own dial, so the program must too.

The GG window only redraws through GG's `/eventing` WebSocket, which needs actions that exist in
Streamer mode only.

Keep the OLED as text on the `screened` device type, which reaches every SteelSeries screen. Images
must match one exact resolution, so a 128x40 version was built and removed.
