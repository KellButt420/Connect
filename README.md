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

To pass gamepads through, the **host** PC needs the free [ViGEmBus driver](https://github.com/nefarius/ViGEmBus/releases).
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
acting on it, both for development. `CONNECT_CONFIG_DIR` selects a separate identity, e.g. to run two
copies on one PC.
