# FileBridge agent instructions

This file is the approved architecture. Follow it. Do not re-trade decided choices unless the owner explicitly changes them.

FileBridge is a local, single-user desktop app for browsing two SSH servers at once, copying files between them, editing config files (especially Telegraf confs), and running a shell. There is no FileBridge account server.

## Ownership

- The owner approves architecture and merges. The agent does not merge.
- Work on `feat/<name>` branches, one feature per branch. Do not start a later feature inside an earlier branch.
- Do not expand scope into the "Out of scope" list.
- Do not commit binaries, real host passwords, private keys, or API keys. Tests use fixtures only.

## Stack

- Frontend: Svelte 5, Vite, TypeScript. The app uses the official Tauri SvelteKit template with `adapter-static` (SPA). Do not replace it with a second frontend setup.
- Backend: Tauri 2 (Rust). System webview. Do not pretend this is a native widget toolkit.
- Database: SQLite via `rusqlite` and a small migration runner. Not `sqlx`.
- SSH: in-process `russh` and `russh-sftp` on Tokio. Do not shell out to system OpenSSH.
- Editor: CodeMirror 6, in a real OS window.
- Terminal: xterm.js
- Crypto: `aes-gcm`, `argon2`, `zeroize`, `rand`, `keyring`
- Tests: `cargo test` and Vitest

Native feel means OS window chrome, an OS Edit menu, OS shortcuts, and a file-manager layout. It does not mean a frameless custom title bar.

## Architecture

```text
Svelte UI  --invoke/events-->  thin Tauri commands  -->  Rust domain
                                                         |            \
                                                       SQLite        russh
                                                    (app data dir)   (one client per connection)
```

- No localhost HTTP server. No REST API. IPC only.
- Components do not call `invoke` with raw strings. Use a typed wrapper in `src/lib/api.ts`.
- The frontend never opens SSH and never stores secrets.
- A pane is UI state that points at a session id. The backend does not know left from right.
- Errors are `{ code, message }`. `message` is safe to show and never contains a password, key, or passphrase.
- Commands are thin: validate, call a service, map errors. Business logic does not live in the command function.

## Data model

SQLite holds durable data only. Live sessions, pane paths, editor buffers, and PTY state die with the process, except the in-memory editor draft described below.

```text
connections
  id, name, host, port, username
  auth_method          -- password | private_key | key_file | agent
  key_path             -- local path, key_file only
  default_path, notes, created_at, updated_at, last_used_at

credential_blobs
  connection_id pk, nonce, ciphertext, updated_at
  -- ciphertext is AES-256-GCM of { password?, private_key_pem?, passphrase? }

known_hosts
  id, host, port, key_type, fingerprint, public_key, trusted_at
  unique (host, port, key_type)

app_meta
  key, value
  -- schema_version
  -- vault_mode: uninitialized | keychain | password
  -- wrapped_dek, kdf_salt, kdf_params   (password mode only)
```

- `agent` stores no secret. `key_file` stores a local path plus an optional passphrase blob.
- There is no `vault_get_credential` command. After save, the UI is told only that a credential exists. Forms show "saved / replace", never the old secret.
- One random 256-bit DEK encrypts every blob. Unique 96-bit nonce per blob.
- If the OS keychain works, store the DEK there under service `filebridge`, account `dek`. Unlock is silent.
- If the keychain is missing, first launch asks for a master password. Argon2id wraps the DEK into `app_meta`. Unlock prompts each launch.
- Hide the keychain behind a trait. Unit tests and CI inject a fake. CI must not touch the real keychain. Ubuntu runners have no Secret Service. The password path is what CI proves. Keychain unlock is a manual Windows check.
- Database file lives in the Tauri app data dir (`%APPDATA%\filebridge` on Windows), mode `0600` on Unix. Never put it in this OneDrive repo folder.
- Do not write editor drafts or remote file contents to SQLite.

## IPC

```text
vault_status() -> { mode, locked }
vault_init_password(password)          -- only when keychain is unavailable
vault_unlock(password?)

connections_list() -> ConnectionSummary[]     -- no secrets
connections_save(draft) -> Connection
connections_delete(id)

vault_set_credential(connectionId, secret)    -- crosses IPC once, then zeroized
vault_clear_credential(connectionId)
vault_has_credential(connectionId) -> bool

session_connect(connectionId) -> { sessionId }
  errors: vault_locked | host_key_unknown | host_key_changed | auth_failed | unreachable
session_trust_host(connectionId, fingerprint) -- explicit; connect cannot skip this
session_disconnect(sessionId)
session_list() -> Session[]

fs_list(sessionId, path) -> DirEntry[]
fs_stat(sessionId, path) -> Entry
fs_mkdir(sessionId, path)
fs_rename(sessionId, from, to)
fs_delete(sessionId, path)                    -- non-recursive
fs_read_text(sessionId, path) -> { content, sha256, size }
fs_write(sessionId, path, content, { overwrite, expectedSha256? })

transfer_start({ fromSession, fromPath, toSession, toPath, overwrite }) -> { transferId }
transfer_cancel(transferId)
  event transfer://progress { id, bytes, total, state }

term_open(sessionId, cols, rows) -> { termId }
term_write(termId, data)
term_resize(termId, cols, rows)
term_close(termId)
  event term://data { id, data }

editor_open(sessionId, path)                  -- Rust creates or focuses the window
```

Host keys are trust-on-first-use. `session_connect` stops on `host_key_unknown` or `host_key_changed` and shows the SHA256 fingerprint. Only `session_trust_host` records trust. Never silent trust. Never silent replace. A changed key is a louder warning.

One SSH connection per saved connection. Connecting an already-live connection returns that session. Panes on the same host share it and keep their own directories. Serialize SFTP calls per connection so two panes cannot interleave one channel. Always pass absolute remote paths. Do not depend on a server-side cwd.

## Main window

Each pane has its own header and path. There is no single path for the whole window.

```text
+-----------------------------+-----------------------------+
| prod-telegraf               | staging-telegraf            |
| ubuntu@10.0.0.8:22          | ubuntu@10.0.0.9:22          |
| [/etc/telegraf            ] | [/etc/telegraf/conf.d     ] |
| telegraf.conf               | cpu.conf                    |
+-----------------------------+-----------------------------+
| terminal            [prod-telegraf v]                     |
+-----------------------------------------------------------+
```

- Server name and `user@host:port` stay visible on both panes, including the unfocused one.
- The connection switcher is not the path field. Typing a path must never change the server.
- Path field is disabled until that pane has a session.
- First click on the path selects it all. Second click places a cursor. Enter lists that path. Escape restores the current directory. A bad path shows an error and does not change directory.
- Paste into a focused path field is text, not a file transfer.
- Single selection in v1. Single click selects. Double-click opens a directory, or opens a file in an editor window.
- File-list keys: Up/Down, Enter, Backspace for parent, F5 refresh, Tab to the other pane.
- The focused pane has a visible accent. Paste targets that pane's current directory.

Copy and drag both call `transfer_start`:

- Ctrl+C on Windows and Linux, Cmd+C on macOS, copies `{ sessionId, path, name }` inside the app. Do not put the remote file on the OS clipboard or in a temp download.
- Ctrl/Cmd+V transfers that file into the focused pane's current directory.
- Drag-and-drop between panes uses the same transfer. The drop payload is a descriptor, never file bytes.
- These shortcuts exist only on the file list. Do not register a global Ctrl+C accelerator. Terminal Ctrl+C stays shell interrupt.
- Same path prompts for a new name. An existing destination confirms overwrite. Progress is a status line, not a blocking modal, except the overwrite confirm.
- v1 transfers one file.

Ship a native Edit menu with OS undo, cut, copy, paste, and select-all roles in the scaffold. macOS webviews need it for reliable Cmd+C/V in text fields.

## Editor windows

- `editor_open` is Rust. The webview cannot create arbitrary windows or load a remote URL.
- Max 2. A third open is refused. An already-open session+path is focused, not duplicated.
- OS decorations, not a frameless frame.
- Title set from Rust: `{connection name} — {path}`, including `user@host`.
- Text only. Refuse a NUL in the first 8 KB. Refuse files over 1 MiB. No partial load.
- Save overwrite sends `expectedSha256`. A mismatch asks before replacing. Never silent overwrite.
- Save-as writes another path on the same session. Save-to-other writes the editor buffer to the other pane's session. No second download.
- Permission denied is shown as returned. No sudo retry.
- Buffer lives in the editor window and is mirrored in Rust memory on a short debounce so a reload can restore it. Do not persist that draft to disk or SQLite.
- Quit with unsaved edits prompts. If the session drops, keep the window and the buffer, and disable save until that server is connected again.

## Terminal

- One xterm.js widget. Each live session keeps its own PTY, so switching servers does not kill the other shell.
- Write terminal bytes straight to xterm.js. Do not put them in a Svelte store.
- Do not log terminal bytes. The user may type a password there.
- The terminal is the only place user text is executed as a shell. File operations must not build shell strings.

## Performance

- The UI never waits on the network. No global spinner except the host-key confirm.
- Each pane has a generation token. Drop a listing response for an old path. Keep the previous listing visible until the new one returns.
- Virtualize the file list. A large directory must not create a DOM node per file.
- Transfer bytes stay in Rust. Stream an SFTP read into an SFTP write.

## Security invariants

- Parameterized SQL only, in the repository layer.
- Remote paths are SFTP strings. Never pass them to local `std::fs`. `..` is a legal remote path. Reject NUL.
- Do not interpolate a remote path, filename, host, or username into a shell command. This includes any future sudo-save work.
- Validate host, port (1–65535), username, and display name before save.
- `zeroize` secret material on drop. Do not log `vault_set_credential` payloads or fields named password, key, passphrase, or authorization.
- Auth failure is `auth_failed` without echoing the secret.
- Tauri capabilities are custom and tight. No shell plugin. No broad fs plugin. No updater. CSP `default-src 'self'`. No remote scripts. `withGlobalTauri` off. DevTools off in release. Dialog only if key-file import needs a native picker, and then scoped.
- Main and editor windows may load only the app's own pages.
- Do not place remote file contents on the OS clipboard.

## Tests

Test pure logic, SQL repositories against a temp database, crypto round-trips, input validation, transfer planning, session reuse, the editor-window cap, and frontend state rules. Do not open real SSH sockets. Do not snapshot the webview.

A feature is not done without tests for the logic it adds.

## CI

`ci.yml` on pull requests and pushes to `main`. The required check is a `gate` job that needs:

- `cargo fmt --check` and Prettier check
- `cargo clippy -- -D warnings` and `svelte-check`
- `cargo test` and Vitest
- `cargo audit` failing on known vulnerabilities in direct Rust deps
- compile (`cargo build` and `vite build`) on `ubuntu-latest`, `windows-latest`, and `macos-latest`

Do not build installers on pull requests.

`squidgate.yml` on `pull_request` (`opened`, `synchronize`, `reopened`) only. Never `pull_request_target`.

- Job permissions only: `contents: read`, `pull-requests: write`, `checks: write`
- Pin `SquidSec/SquidGate` to the immutable build tag that exists when the workflow is written. Not floating `@v1`. The tag seen during planning was `v1.0.0-build.28`; verify before writing the workflow.
- `llm-provider: opencode-go`
- `llm-model: glm-5.3-flash`
- `llm-api-key: ${{ secrets.LLM_API_KEY }}` — already set in GitHub Actions secrets. Never write the key into the repo or the workflow.
- Do not set `llm-base-url`. OpenCode Go ignores it.
- `block_on: high`, secrets and injection categories on, `fail_on_error: true`
- Required checks, after the owner enables them: `gate`, `squidgate`, and the `SquidGate` check run

Pull request diffs are sent to OpenCode Go. That is accepted. Do not put real credentials in a diff.

Also add Dependabot for Actions, Cargo, and npm. Pin third-party actions by commit SHA.

`release.yml` on tags `v*`, via `tauri-apps/tauri-action`: Windows NSIS `.exe`, macOS `.dmg`, Linux `.AppImage` (and `.deb` if the action emits it). Unsigned. Document SmartScreen and Gatekeeper warnings. README links to GitHub Release assets. Do not commit binaries. No updater plugin.

The owner configures branch protection: PR required, owner approval, no direct push, no force-push. Do not turn SquidGate on as a required check until the first run is green.

## Build order

Implement in this order. Each row is its own branch and PR.

| Branch | Lands |
|---|---|
| `feat/scaffold` | Tauri 2 + Svelte, migration runner, native window chrome, native Edit menu, fmt/lint/test/build CI |
| `feat/ci-security` | SquidGate workflow and policy. Owner enables the required check after the first green run |
| `feat/connection-crud` | Saved connections, no secrets |
| `feat/credential-vault` | Hybrid vault and unlock |
| `feat/known-hosts` | Fingerprint prompt, no silent replace |
| `feat/ssh-connect` | Connect, reuse a live session, serialize SFTP |
| `feat/remote-browser` | Server header, path field, virtualized list, keyboard, stale-listing guard |
| `feat/dual-pane` | Second pane, focus accent, independent directories |
| `feat/file-ops` | mkdir, rename, confirmed non-recursive delete |
| `feat/transfer` | Drag-and-drop and Ctrl/Cmd+C/V, progress, cancel, overwrite confirm |
| `feat/editor` | Two native editor windows, save targets, in-memory draft |
| `feat/terminal` | One xterm, one PTY per live session, server selector |
| `feat/release` | Tag workflow and README download links |

`feat/ssh-connect` may split agent auth to `feat/ssh-agent` if Windows agent support blows the review. Password, stored key, and key file still ship first.

## Out of scope

Do not build these unless the owner opens a new branch for one of them:

- sudo or any privileged write
- ProxyJump, `~/.ssh/config`, SSH certificates
- local filesystem pane
- OS-clipboard file transfer
- recursive directory copy (`feat/transfer-tree`)
- multi-select
- code signing, notarization, auto-update
- binary editor
- syncing or multi-user accounts
- machine-to-machine recovery of a keychain-mode vault
