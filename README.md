# Connect

Peer-to-peer remote desktop for gaming, built on [iroh](https://iroh.computer). Click **Generate
Account** to get a User-ID. Give it to someone and they can connect to your PC from anywhere:
direct P2P when possible, through a relay when not. You allow or deny every connection, and choose
whether they may use your mouse, keyboard and gamepad. You can switch each one on or off at any time.

## Status

| | |
|---|---|
| Works | Generate Account, connect by User-ID across networks, Allow / Deny with per-device permissions, "Always allow" list, the host's screen at its display's refresh rate (60–240 Hz), remote mouse (normal or captured for games), keyboard by scancode, up to 4 gamepads as virtual Xbox controllers |
| Not yet | Audio, hardware (GPU) video encoding, the mouse pointer drawn in the stream, picking a monitor |
| Platforms | Windows can host and connect. Linux can connect only |

## Download

Grab the latest build from [Releases](../../releases):

- `connect.exe`: Windows 10 / 11. It's unsigned, so SmartScreen asks first: **More info → Run anyway**.
  Allow it through the firewall so direct connections work.
- `connect-linux-x86_64`: Linux with glibc 2.39+ (Ubuntu 24.04, Fedora 40, Debian 13 or newer).

Connect updates itself: when a new release is out it shows **Update available**, and **Update now**
downloads it, checks it against the release's checksums, and restarts into it. Settings → Updates
checks on demand.

Gamepads work on the host through the ViGEmBus driver, which is built into `connect.exe`. The
welcome screen offers to set it up, and so does the Share page. Windows asks for permission once.
The remote user's controller shows up as an Xbox 360 controller in the next free slot: player 2
if the host has one plugged in, player 1 if not. The driver stays installed if Connect is deleted;
remove it under Windows *Installed apps* ("ViGEm Bus Driver").

The host's own mouse and keyboard always win: while the host is using one, the remote user's
input on it pauses (and anything they held is released) until a second after the host stops.

Windows won't let a normal program control one running as administrator, so to control such
programs, run Connect as administrator on the host.

## While connected

| Keys | |
|---|---|
| Ctrl+Alt+M | Capture the mouse for games (raw movement, hidden pointer), and release it again |
| Ctrl+Alt+F | Full screen |
| Ctrl+Alt+S | Stats: connection path, round trip, fps, bitrate, latency |

All other keys, including F-keys and Esc, go to the host.

## Build

Windows builds are cross-compiled from Linux (Ubuntu 24.04):

```sh
sudo apt install gcc-mingw-w64-x86-64 g++-mingw-w64-x86-64
rustup target add x86_64-pc-windows-gnu
cargo build --release --target x86_64-pc-windows-gnu -p connect   # Windows
cargo build --release -p connect                                    # Linux
cargo test --workspace
```

## Headless commands

For testing without a display:

```text
connect host [--test-pattern] [--print-input]
                                    share this PC (Windows); Allow/Deny is asked on the terminal
connect join <USER-ID> [--seconds N]
connect id                          print this PC's User-ID
```

`--test-pattern` shares a generated image and `--print-input` prints the remote input instead of
acting on it, both for development. On Windows the app has no console window, so start these as
`start /wait connect.exe id` for the terminal to wait for them. `CONNECT_CONFIG_DIR` selects a separate identity, e.g. to run two
copies on one PC.

## Third-party components

`connect.exe` includes the installer for [ViGEmBus](https://github.com/nefarius/ViGEmBus) 1.22.0
(BSD-3-Clause, © Nefarius Software Solutions e.U.); see `crates/host/vendor/vigembus/`.
