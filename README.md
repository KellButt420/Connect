# Connect

Peer-to-peer remote desktop for gaming, built on [iroh](https://iroh.computer). Each install has a
User-ID. Give it to someone and they can connect to your PC from anywhere: direct P2P when
possible, through a relay when not. You allow or deny every connection.

## Status

| | |
|---|---|
| Works | User-ID, connect by ID across networks, Allow / Deny prompt, "Always allow" list, live view of the host's screen (up to 60 fps) |
| Not yet | Remote mouse / keyboard / gamepad (the toggles are shown but inactive), audio, hardware video encoding |
| Platforms | Windows can host and connect. Linux can connect only |

## Download

Grab the latest build from [Releases](../../releases):

- `connect.exe`: Windows 10 / 11. It's unsigned, so SmartScreen asks first: **More info → Run anyway**.
  Allow it through the firewall so direct connections work.
- `connect-linux-x86_64`: Linux with glibc 2.39+ (Ubuntu 24.04, Fedora 40, Debian 13 or newer).

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
connect host [--test-pattern]       share this PC (Windows); Allow/Deny is asked on the terminal
connect join <USER-ID> [--seconds N]
connect id                          print this PC's User-ID
```

`CONNECT_CONFIG_DIR` selects a separate identity, e.g. to run two copies on one PC.
