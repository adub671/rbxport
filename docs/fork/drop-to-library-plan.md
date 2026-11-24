# Drop-to-library import (fork feature plan)

The owner's handoff plan, with the amendments agreed afterwards. Where the two disagree, the amendments win.

## Amendments

1. **Conversion at intake.** A source that `rbl_audio::compatibility::needs_conversion` flags (FLAC, ALAC and the like) is not copied: it is converted with `rbl_audio::compatibility::convert` to 16-bit 44.1 kHz AIFF, written to the same `.part` temp name and renamed into the month folder as `<stem>.aiff`. Tags and embedded artwork must survive; if the existing converter drops them, carry them over and test it. The converted file is what gets imported. The original is recycled under the same rule as any other original.
2. **Neutral naming.** The fork's product name and identifier already exist (`src-tauri/tauri.fork.conf.json`). Do not rename anything. The feature is compiled in every build but its preference defaults to off unless `crate::fork::BUILD`.
3. **No personal defaults in source.** The library folder has no default path. The feature stays inert until a folder is chosen in Preferences. Drive letters and user names never appear in code or tests.
4. **One intake path, callable from server mode.** The Tauri command takes paths and a target and needs no window interaction, so `POST /invoke/intake_and_import` works for a script. Do not add it to the server's refused list.
5. **Fork setup (section 3 of the original) is done.** Skip it. CI is `.github/workflows/fork-windows.yml`.

## Original plan

Goal: dragging audio files or folders onto the window copies them into a managed library folder on the user's external drive, imports the copies, queues analysis, and then moves the originals to the Recycle Bin.

### Behaviour

1. User drags files and/or folders from Explorer onto All Tracks, a playlist, or anywhere in the track list (same targets as today).
2. If "Copy into library folder" is on:
   - Expand folders with the existing `expand_import_paths` walk.
   - Plan each file (Rust, no I/O beyond metadata):
     - Already inside the library root: import in place, no copy, never delete. This is the safety rule that stops a drop from deleting library files.
     - Already in the library by exact path: `Existing` (added to the playlist if dropped on one, as today).
     - Probable duplicate: same file name and same byte size as a file already in the target month folder, or as a library track (lookup via `FileSize`): skip the copy, report "already in library", do not delete the original.
     - Name clash, different content: copy as `Name (2).ext`, `Name (3).ext`.
     - Otherwise: copy to `<root>\<YYYY-MM>\<original file name>`.
   - Copy (Rust, background job): copy to `<root>\.rbxport-incoming\<uuid>.part` on the same volume, flush, rename to the final name. Verify the size; on mismatch delete the `.part` and refuse that file.
   - Import while copying (pipelined): every 10 files or 2 s, import the finished paths with the existing `import_files_slice` batching, adding them to the drop-target playlist. Reload the library once at the end, as `importInSlices` does today.
   - Queue analysis through the existing auto-analysis path.
   - Originals: when "After import" is "Move originals to Recycle Bin", recycle only sources whose copy was verified and whose import outcome is `Added` or `Existing`. Use the OS trash (the `trash` crate), never a permanent delete. If the source volume has no Recycle Bin, keep the original and say so in the report. Empty source folders are left.
3. If the toggle is off, behaviour is exactly upstream's.
4. A drop on Playlists root or a playlist folder (`import_folder_playlist`) gets the same copy stage first.

### Settings (new Preferences pane "Import")

| Setting | Type | Default |
|---|---|---|
| Copy dropped files into the library folder | toggle | on in a fork build once a folder is set |
| Library folder | folder picker; warn if not on the library's drive | none |
| Folder for new files | `YYYY-MM` (import month) or flat | `YYYY-MM` |
| After import | Keep originals / Move originals to Recycle Bin | Move to Recycle Bin |
| Treat same name + same size as duplicate | toggle | on |
| Confirm duplicates by content (slower) | toggle | off |

Store them with the existing preferences mechanism.

### Efficiency

- All file work in Rust under the existing `blocking(...)` helper. The frontend gets progress events and the final report only.
- One plan pass, then the copy. Stat every source once.
- Same-volume rename for the final step. Clean up stray `.rbxport-incoming` folders on startup, like `file_journal::recover`.
- Copy with `backup_copy::copy_file`, two at a time (a constant).
- Source and destination on the same volume with "Move originals": `fs::rename`, report as moved.
- Duplicate check without reading audio: name + size against the month folder's entries (read once per drop) and the library by one query per slice. With "confirm by content": BLAKE3 over first 4 MiB + last 4 MiB + size.
- Reuse the existing import slices. A bounded channel feeds finished copies to the import step. Recycling waits until every slice is in.
- Progress events at most 10 per second. No polling.
- Cancellable through the existing job cancel: finish the current file, delete its `.part`, import what is copied, recycle nothing that was not imported.

### UI

- Drag-over overlay on the track list naming the destination folder, the target, and what happens to originals (`data-testid="intake-overlay"`).
- Status-bar job: "Copying 12 of 39 · 1.9 GB of 6.4 GB · 214 MB/s" with cancel.
- Report dialog: copied, converted, imported, already in library, renamed, failed (with reason), originals recycled / kept (with reason), and "Show in Explorer" for the month folder.
- All strings through `t(...)`.

### Implementation shape

- Rust: `src-tauri/src/library_intake.rs` (new): `IntakeSettings`, a pure unit-tested `plan(...)`, `execute(...)` with the bounded copy pool, `recycle(...)`, `recover_incoming(root)`.
- Command `intake_and_import(paths, target)` registered in `lib.rs` next to `import_files`, returning a report DTO that extends the import report.
- Frontend: `src/ipc/client.ts` (`intakeAndImport` + progress subscription, and the mock), a branch in `App.tsx` `importDroppedPaths`, the overlay, `IntakeReportDialog.tsx`, `src/views/settings/ImportPane.tsx`, i18n keys.
- Keep changes in new files where possible so upstream merges stay small.

### Project rules

- Stay inside `perf-budgets.json`; never raise a gate.
- Do not materialize the full library in the frontend; no idle polling.
- Never test against real user data. Tests use `RBXPORT_TEST=1` and scratch libraries (`rbl_db::fixture`).
- Workspace lints forbid `unwrap`, `expect`, `panic!` outside tests.

### Acceptance

- Dropping mixed files (AIFF/WAV/MP3/FLAC plus one `.jpg`) onto All Tracks creates the month folder with the audio (FLAC as AIFF), the library tracks, analysis queued, originals recycled, and the `.jpg` reported as skipped and untouched.
- Dropping onto a playlist also adds the tracks to it.
- Dropping a file already under the library root never copies or recycles it.
- Dropping the same files twice: the second run copies nothing and recycles nothing.
- A source volume without a Recycle Bin keeps originals and says why.
- Cancelling mid-copy leaves no `.part` files and no recycled originals for files that were not imported.
- With Library Protection on, the drop is refused exactly like upstream's import.

### Out of scope

Reorganizing existing tracks, tag editing or renaming from tags, watching a folder.
