# Security policy

sonar-rollerbar installs a global keyboard hook, so it runs in the path of every keystroke on your
system, passwords included. It must be possible to audit it and to confirm that it does nothing
except change volumes.

## Reporting a vulnerability

Please **don't** open a public issue. Use GitHub's
[private vulnerability reporting](https://github.com/BastienPoP/sonar-rollerbar/security/advisories/new)
form instead. This is a hobby project, so responses are best effort.

Only the latest release is supported.

## What the hook can and cannot see

A `WH_KEYBOARD_LL` hook is called by Windows for each key press and release on every keyboard. That
access is why this tool deserves scrutiny, and it is limited as follows.

| Step | What happens |
| --- | --- |
| Key event arrives | [`keyboard_proc`](src/win.rs) is called by Windows. |
| Fields read | Only `vkCode` and `flags`. `scanCode`, `time` and `dwExtraInfo` are never read. |
| Not Volume Up, Down or Mute | The key goes straight to `CallNextHookEx`. Nothing is copied, stored or logged. |
| Volume Up, Down or Mute | A job is queued for the `audio` thread and the key is swallowed if it can be handled. |
| Audio thread | It changes the chosen output with `IAudioEndpointVolume`. It never sees any other key. |

Debug builds print those three keys to their own console. They never print any other key, and
neither build writes keystrokes anywhere.

## Threads

The program splits its work so that a failure in one part can't reach the hook.

| Thread | Role | Sees keys |
| --- | --- | --- |
| main | The keyboard hook | Yes, as described above |
| `audio` | Volume and mute of the chosen output | Only that one of the three volume keys was pressed |
| `tray` | Icon, menu and mixer window | No |
| `gg` | Optional SteelSeries features | Only that the wheel turned or was clicked, while it points at Sonar |

The `tray` thread acts only when you click, and its menu can pause the program, change the options
and step, add or remove the sign-in shortcut, show a message box and quit. **Check for updates**
opens the fixed releases page in your default browser and **Open logs** opens the logs folder. These
two are the only things the program ever opens or starts.

## Network access

The program never connects to anything outside your computer. Its only network use is local, for
the optional SteelSeries features, which are all off by default.

| Option | What it talks to | What it sends |
| --- | --- | --- |
| Keyboard OLED | GameSense | The output name and volume, to display them |
| Sonar mixer | GG and Sonar | Reads and sets Sonar's volumes, mutes and ChatMix |
| Follow Sonar's output | GG and Sonar | Reads which output Sonar plays to |
| Choosing what the wheel controls | GG and Sonar | Reads and sets Sonar's volumes, mutes and ChatMix |

- With all of them off, no connection is opened.
- Requests can't leave your computer: `127.0.0.1` is hard-coded in `src/win/loopback.rs` and isn't a
  parameter, no proxy is used, and CI checks that there is only one such call.
- Addresses from GG's `coreProps.json` are refused unless they are on `127.0.0.1`.
- GG's self-signed certificate is accepted on that local connection.
- **Check for updates** only opens a page in your browser. The program itself connects to nothing
  and doesn't learn whether an update exists.

No key data is ever sent: Sonar and GameSense only receive the resulting levels.

## What it does not do

- It does not read the clipboard, other processes' memory or the screen.
- It does not swallow any key other than Volume Up, Volume Down and Mute.
- It has no telemetry and no automatic updates.

## Files it writes

Everything is in `%LOCALAPPDATA%\sonar-rollerbar`, apart from the optional sign-in shortcut.

- `config.ini` holds the options and the wheel step of each output. Output choices stay as
  command-line arguments.
- The log holds program events: start and stop, output changes, pause, and errors. An entry can
  contain the name of an audio output. It never holds keys, not even the volume keys: only the
  debug console can show those.
- The log is bounded: at 512 KB it is rotated and one previous file is kept, about 1 MB in total.
- Uninstalling removes all of it.

## Failure behaviour

The hook fails open, so a problem can't trap your keyboard input.

- A panic in the callback is caught by `catch_unwind` and the key passes through.
- If no selected output is available, or the program is paused, the key passes through.
- If the audio thread stops or falls behind its bounded queue, the key passes through.
- The callback never calls COM, waits or allocates, so slow audio calls can't stall system input.
- If the `tray` or `gg` thread fails, or GG isn't running, the hook and the audio thread aren't
  affected.
- If Sonar stops answering while the wheel points at one of its channels, the wheel returns to your
  output.

Two limits remain. If an output disappears in the instant before a key arrives, that key may be
swallowed without effect: the output is rechecked every 2 seconds and on every key. With the option
to choose what the wheel controls, a click is swallowed before the program knows whether it starts a
double click, and a double click has no effect if GG isn't running.

## Privileges

- The program and its installer run as the current user. Neither requests elevation or UAC.
- There is no service or driver.
- Start at sign-in uses a shortcut in your Startup folder, created only if you ask for it in the
  installer or the tray menu.
- The installer adds no background task.

Because the install folder is in your profile, other programs running as you could replace the
executable. Those programs could also install their own hook, so this gives them nothing new.

## Supply chain

- `windows` is the only direct dependency, at an exact version, and `Cargo.lock` is committed.
- `deny.toml` is an allowlist: CI fails on any other crate, licence or source.
- CI runs `cargo deny` and `cargo audit`, and checks with `cargo tree` that nothing outside the
  `windows` crates ships in the executable.
- CI fails if the release executable imports a network library other than `winhttp.dll`.
- All `unsafe` code is in `src/win.rs` and `src/win/`, and `#![deny(unsafe_code)]` rejects it
  anywhere else.
- `build.rs` only runs at build time, to embed the icon and manifest with `windres`. It adds nothing
  else to the executable.
- Releases are built only by GitHub Actions, with actions pinned by commit SHA and builds run with
  `--locked`. Job permissions are limited to what each step needs.
- Dependabot proposes updates after a 7-day cooldown, and each one must pass the checks above.

## Verifying a download

Compare the hash with the one in the release notes, then check where the file was built:

```powershell
Get-FileHash .\sonar-rollerbar-setup-v0.1.0.exe -Algorithm SHA256
gh attestation verify .\sonar-rollerbar-setup-v0.1.0.exe --repo BastienPoP/sonar-rollerbar
```

To audit the code, start with `keyboard_proc` and `should_swallow` in `src/win.rs`, then
`src/win/loopback.rs`, and list what the release executable imports:

```sh
x86_64-w64-mingw32-objdump -p sonar-rollerbar.exe | grep 'DLL Name'
```

Only the release build removes the unused network code of the Rust standard library. Debug builds
also list `WS2_32.dll`, although they never call it, and are not distributed.

## Scope

In scope:

- logging or storing any key, or reading, copying or storing any key other than Volume Up, Volume
  Down and Mute;
- swallowing any other key;
- any network access beyond `127.0.0.1`, or any connection with all options off;
- any clipboard access;
- any file write other than those listed above;
- opening or starting anything other than the two items listed above;
- any elevation, or persistence other than the shortcut you opted into;
- stalling system input;
- raising your output without you moving the wheel or the mixer's Output slider.

Out of scope:

- the executable and installer being unsigned, which triggers Windows warnings (see the README);
- issues in SteelSeries software, which should be reported to SteelSeries;
- attacks that already require running code as your user.
