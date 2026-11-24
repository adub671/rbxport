# This fork

A personal fork of [chrisle/rbxport](https://github.com/chrisle/rbxport). It installs beside stock rbxport and adds what one library needs. Upstream stays the source of everything else.

## What differs

| Change | Where | Why |
| --- | --- | --- |
| Fork build flag | [`src-tauri/src/fork.rs`](src-tauri/src/fork.rs), set by `RBXPORT_FORK` at compile time | Everything below is off in a build without it |
| Own name and identifier | [`src-tauri/tauri.fork.conf.json`](src-tauri/tauri.fork.conf.json) | Installs as `rbxport-fork` next to stock rbxport; no silent-update task, no updater artifacts |
| No self-update | [`src-tauri/src/update.rs`](src-tauri/src/update.rs) | The release feed's builds are upstream's. A check answers "nothing on offer" |
| Server mode | [`src-tauri/src/server.rs`](src-tauri/src/server.rs) | The app's commands over loopback HTTP, for scripts and agents |
| Hosted Windows build | [`.github/workflows/fork-windows.yml`](.github/workflows/fork-windows.yml) | Upstream's release workflows need its runners and signing keys |

Logs, backups and the analysis journal stay in the same `rbxport` data folder stock uses. Preferences are separate, because they belong to the identifier.

## Server mode

Off unless `fork-server.json` exists in the app's data folder (`%APPDATA%\rbxport\` on Windows):

```json
{ "port": 48650, "token": "<at least 24 characters>" }
```

The app then listens on `127.0.0.1:48650`. Every request carries `Authorization: Bearer <token>`.

```bash
curl -H "Authorization: Bearer $TOKEN" http://127.0.0.1:48650/version
curl -H "Authorization: Bearer $TOKEN" -d '{}' http://127.0.0.1:48650/invoke/library_summary
```

`POST /invoke/<command>` takes a JSON object of the command's arguments in camelCase. The commands are the ones in the `generate_handler!` list in [`src-tauri/src/lib.rs`](src-tauri/src/lib.rs), and the argument names are each Rust function's parameters. A command's own error comes back as HTTP 422 with `{kind, message}`.

An edit made this way takes the app's one edit path: Library Protection and the read-only refusals apply, and the window redraws. Plugin commands and the update commands are refused.

Only one of rekordbox, stock rbxport and this fork should have the library open at a time.

## Branches and builds

- `main` mirrors upstream. Do not commit to it.
- `fork` is this fork's default branch. Features land there through pull requests, and each push builds an unsigned Windows installer as the `rbxport-fork-windows` workflow artifact. Windows SmartScreen warns about an unknown publisher; that is expected.
- To take upstream changes: `git fetch upstream && git push origin upstream/main:main`, then merge `main` into `fork`.

A local build of the fork: `RBXPORT_FORK=1 pnpm tauri build --config src-tauri/tauri.fork.conf.json`.

## License

GPL-2.0-or-later, as upstream. A build shared with anyone else must come with this source.
