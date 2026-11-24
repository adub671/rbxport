# This fork

A personal fork of [chrisle/rbxport](https://github.com/chrisle/rbxport). It installs beside stock rbxport and adds what one library needs. Upstream stays the source of everything else.

## What differs

| Change | Where | Why |
| --- | --- | --- |
| Fork build flag | [`src-tauri/src/fork.rs`](src-tauri/src/fork.rs), set by `RBXPORT_FORK` at compile time | Everything below is off in a build without it |
| Own name and identifier | [`src-tauri/tauri.fork.conf.json`](src-tauri/tauri.fork.conf.json) | Installs as `rbxport-fork` next to stock rbxport; no silent-update task, no updater artifacts |
| No self-update | [`src-tauri/src/update.rs`](src-tauri/src/update.rs) | The release feed's builds are upstream's. A check answers "nothing on offer" |
| Server mode | [`src-tauri/src/server.rs`](src-tauri/src/server.rs) | The app's commands over loopback HTTP, for scripts and agents |
| Drop into a library folder | [`src-tauri/src/library_intake.rs`](src-tauri/src/library_intake.rs), Preferences › Import | Dropped files are copied (FLAC and the like converted to AIFF) into a chosen folder, imported, and the originals recycled. On by default in a fork build once a folder is chosen; `POST /invoke/intake_and_import` takes `{paths, settings}` |
| Hosted Windows build | [`.github/workflows/fork-windows.yml`](.github/workflows/fork-windows.yml) | Upstream's release workflows need its runners and signing keys |
| Date Added edit | `TrackField::DateAdded` in [`crates/rbl-db/src/write.rs`](crates/rbl-db/src/write.rs) | `set_track_field` takes the field `dateAdded`. It is compiled into every build, and no screen sends it |
| Forward-slash track paths | `folder_path` in [`crates/rbl-db/src/write.rs`](crates/rbl-db/src/write.rs), `relocate_track` in [`src-tauri/src/commands.rs`](src-tauri/src/commands.rs) | On Windows an import or a relocate stores `FolderPath` as rekordbox does, `F:/music/a.aiff`, and a row stored with `\` counts as the same file. `relocate_track` accepts a track's own file and stores its path again in that spelling. It is compiled into every build |

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

These two calls change a track's Date Added and read it back. The value must be a real date written `YYYY-MM-DD`, and Undo in the app restores the old one.

```bash
curl -H "Authorization: Bearer $TOKEN" -d '{"tracks":["<track id>"],"field":"dateAdded","value":"2024-03-09"}' http://127.0.0.1:48650/invoke/set_track_field
curl -H "Authorization: Bearer $TOKEN" -d '{"track":"<track id>"}' http://127.0.0.1:48650/invoke/track_details
```

`POST /invoke/<command>` takes a JSON object of the command's arguments in camelCase. The commands are the ones in the `generate_handler!` list in [`src-tauri/src/lib.rs`](src-tauri/src/lib.rs), and the argument names are each Rust function's parameters. A command's own error comes back as HTTP 422 with `{kind, message}`.

An edit made this way takes the app's one edit path: Library Protection and the read-only refusals apply, and the window redraws. Plugin commands, the update commands, `format_device`, and the commands that create or switch the library are refused.

Only one of rekordbox, stock rbxport and this fork should have the library open at a time.

## Branches and builds

- `main` mirrors upstream. Do not commit to it.
- `fork` is this fork's default branch. Features land there through pull requests, and each push builds an unsigned Windows installer as the `rbxport-fork-windows` workflow artifact. Windows SmartScreen warns about an unknown publisher; that is expected.
- To take upstream changes: `git fetch upstream && git push origin upstream/main:main`, then merge `main` into `fork`.

A local build of the fork: `RBXPORT_FORK=1 pnpm tauri build --config src-tauri/tauri.fork.conf.json`.

## License

GPL-2.0-or-later, as upstream. A build shared with anyone else must come with this source.
