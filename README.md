# filebridge

Allows for file interaction between multiple servers and include built in file editor allowing conf files and other config files to easily be changed, save, copy, or moved. Additionally pull up the shell and run SSH commands all from a central app.

## Download

Prebuilt installers are attached to each [GitHub Release](https://github.com/Smb7/filebridge/releases). Get the latest from the [latest release page](https://github.com/Smb7/filebridge/releases/latest).

- **Windows** — `.exe` installer (NSIS)
- **macOS** — `.dmg`
- **Linux** — `.AppImage` (and `.deb`)

The binaries are unsigned, so your operating system may warn before you can run them.

- **Windows SmartScreen** — if "Windows protected your PC" appears, choose **More info** → **Run anyway**.
- **macOS Gatekeeper** — if the app is blocked, right-click it in Finder and choose **Open**, then **Open** again. You can also clear the quarantine flag with `xattr -dr com.apple.quarantine /Applications/FileBridge.app`.

Pushing a tag that matches `v*` builds these packages and creates a draft release. The tag must match the `version` in `src-tauri/tauri.conf.json` (for example, tag `v0.1.0` for version `0.1.0`).

## Development

Install [Rust](https://www.rust-lang.org/tools/install) and Node.js 22, then:

```bash
npm install
npm run tauri dev
```

Checks:

```bash
npm run format:check
npm run check
npm test
cargo fmt --check --manifest-path src-tauri/Cargo.toml
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml
```

The database is created in the OS app data directory (`%APPDATA%\filebridge` on Windows), not in this repository.

## Reviews

Pull requests run fmt, lint, test, and a three-OS compile. The required check for those is `gate`.

SquidGate scans the pull request diff with OpenCode Go. It is not a required check until its first run is green. After that, require both the `squidgate` job and the `SquidGate` check. Do not switch that workflow to `pull_request_target`.
