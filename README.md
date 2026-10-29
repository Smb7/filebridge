# filebridge

Allows for file interaction between multiple servers and include built in file editor allowing conf files and other config files to easily be changed, save, copy, or moved. Additionally pull up the shell and run SSH commands all from a central app.

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

The database is created in the OS app data directory, not in this repository.

## Reviews

Pull requests run fmt, lint, test, and a three-OS compile. The required check for those is `gate`.

SquidGate scans the pull request diff with OpenCode Go. It is not a required check until its first run is green. After that, require both the `squidgate` job and the `SquidGate` check. Do not switch that workflow to `pull_request_target`.
