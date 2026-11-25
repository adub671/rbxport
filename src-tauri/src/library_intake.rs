//! Drop-to-library intake: files dropped from outside the app are copied (or
//! converted) into a managed library folder, the copies are imported, and an
//! original goes to the OS trash only once its copy is verified and imported.
//!
//! A file already under the library folder is imported where it is and never
//! copied or deleted; that is what keeps a drop from deleting library files.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use rbl_db::write::ImportOutcome;
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::dto::ImportReportDto;
use crate::error::{AppError, AppResult, ErrorKind};
use crate::state::AppState;

/// Staging folder under the library root, so the final rename stays on one volume.
const INCOMING: &str = ".rbxport-incoming";
const COPY_WORKERS: usize = 2;
/// A slice is imported once this many files are ready, or [`SLICE_WAIT`] after its first.
const SLICE_FILES: usize = 10;
const SLICE_WAIT: Duration = Duration::from_secs(2);
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Layout {
    /// `<root>/<YYYY-MM>/`, by import month.
    Month,
    Flat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum After {
    Keep,
    Recycle,
}

/// Preferences › Import, as the window sends them with each drop.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntakeSettings {
    pub root: PathBuf,
    pub layout: Layout,
    pub after: After,
    /// Same file name and byte size counts as already in the library.
    pub duplicates: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Copy,
    /// Not playable on every player: converted to 16-bit 44.1 kHz AIFF.
    Convert,
    /// Left for the import to report as skipped.
    NotAudio,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub path: PathBuf,
    pub size: u64,
    pub kind: SourceKind,
}

/// What is already there, read once per drop.
#[derive(Debug, Default)]
pub struct Known {
    /// Library tracks, by [`path_key`].
    pub paths: HashSet<String>,
    /// Library tracks by lower-cased file name and byte size.
    pub by_name_size: HashMap<(String, u64), PathBuf>,
    /// Library tracks by lower-cased file name alone, for converted sources,
    /// whose size says nothing about the copy's.
    pub by_name: HashMap<String, PathBuf>,
    /// The destination folder's files: name and size.
    pub folder: Vec<(String, u64)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Imported where it is and never deleted: under the root, already in
    /// the library, or not audio.
    InPlace,
    /// Probably already held: the original is kept and `of` is imported in
    /// its place, so a playlist drop still gets the track.
    Duplicate { of: PathBuf },
    Copy { to: PathBuf },
    Convert { to: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    pub source: Source,
    pub action: Action,
}

/// Where new files go this month.
#[must_use]
pub fn destination(settings: &IntakeSettings, month: &str) -> PathBuf {
    match settings.layout {
        Layout::Month => settings.root.join(month),
        Layout::Flat => settings.root.clone(),
    }
}

/// A path as compared: case and separators folded where the filesystem folds them.
#[must_use]
pub fn path_key(path: &Path) -> String {
    let text = path.to_string_lossy();
    let text = if cfg!(windows) { text.replace('/', "\\").to_lowercase() } else { text.into_owned() };
    text.trim_end_matches(std::path::MAIN_SEPARATOR).to_owned()
}

fn under(key: &str, root: &str) -> bool {
    key.strip_prefix(root)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(std::path::MAIN_SEPARATOR))
}

fn split_name(name: &str) -> (&str, Option<&str>) {
    match name.rfind('.') {
        Some(dot) if dot > 0 => (&name[..dot], Some(&name[dot + 1..])),
        _ => (name, None),
    }
}

/// Decides each source's fate from names and sizes alone. `folder` is
/// [`destination`]; `known.folder` lists what it holds now.
#[must_use]
pub fn plan(settings: &IntakeSettings, folder: &Path, sources: Vec<Source>, known: &Known) -> Vec<Planned> {
    let root = path_key(&settings.root);
    // Names in the folder, including those this plan hands out: lower-cased
    // name to the name as written and its size.
    let mut taken: HashMap<String, (String, u64)> =
        known.folder.iter().map(|(name, size)| (name.to_lowercase(), (name.clone(), *size))).collect();
    sources
        .into_iter()
        .map(|source| {
            let key = path_key(&source.path);
            let file_name = source.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if source.kind == SourceKind::NotAudio || under(&key, &root) || known.paths.contains(&key) || file_name.is_empty() {
                return Planned { source, action: Action::InPlace };
            }
            let convert = source.kind == SourceKind::Convert;
            let name = if convert { format!("{}.aiff", split_name(&file_name).0) } else { file_name };
            let lower = name.to_lowercase();
            let duplicate = if !settings.duplicates {
                None
            } else if convert {
                taken.get(&lower).map(|(name, _)| folder.join(name)).or_else(|| known.by_name.get(&lower).cloned())
            } else {
                taken
                    .get(&lower)
                    .filter(|(_, size)| *size == source.size)
                    .map(|(name, _)| folder.join(name))
                    .or_else(|| known.by_name_size.get(&(lower.clone(), source.size)).cloned())
            };
            let action = if let Some(of) = duplicate {
                Action::Duplicate { of }
            } else {
                let free = free_name(&name, &taken);
                taken.insert(free.to_lowercase(), (free.clone(), source.size));
                let to = folder.join(free);
                if convert { Action::Convert { to } } else { Action::Copy { to } }
            };
            Planned { source, action }
        })
        .collect()
}

/// `name`, or `stem (2).ext`, `stem (3).ext`… whichever is not taken.
fn free_name(name: &str, taken: &HashMap<String, (String, u64)>) -> String {
    if !taken.contains_key(&name.to_lowercase()) {
        return name.to_owned();
    }
    let (stem, ext) = split_name(name);
    // Each taken name rules out at most one candidate, so one of these is free.
    (2..=taken.len() + 2)
        .map(|n| ext.map_or_else(|| format!("{stem} ({n})"), |ext| format!("{stem} ({n}).{ext}")))
        .find(|candidate| !taken.contains_key(&candidate.to_lowercase()))
        .unwrap_or_else(|| name.to_owned())
}

/// What a drop came to, for the report dialog. Extends the import report.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntakeReportDto {
    #[serde(flatten)]
    pub import: ImportReportDto,
    /// The folder new files went to, for Show in Explorer.
    pub folder: String,
    pub copied: u32,
    pub converted: u32,
    /// Moved by a rename on the same volume, so there is no original left.
    pub moved: u32,
    /// File names judged already in the library and not copied.
    pub duplicates: Vec<String>,
    /// `old → new` for every copy that took a free name.
    pub renamed: Vec<String>,
    /// `name: reason` for every file not copied or not imported after copying.
    pub failed: Vec<String>,
    pub recycled: u32,
    /// `name: reason` for every original kept although recycling was asked for.
    pub kept: Vec<String>,
    pub cancelled: bool,
}

/// `intake:progress`, at most ten a second.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntakeProgressDto {
    pub done: u32,
    pub total: u32,
    pub bytes: u64,
    pub total_bytes: u64,
}

/// The outside world `execute` acts on, so tests can stand in for it.
pub struct Effects<'a> {
    /// Imports one slice and answers each path's outcome, in order.
    pub import: &'a mut dyn FnMut(&[PathBuf]) -> AppResult<Vec<ImportOutcome>>,
    pub recycle: &'a dyn Fn(&Path) -> Result<(), String>,
    pub progress: &'a mut dyn FnMut(IntakeProgressDto),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transfer {
    Copied,
    Converted,
    Moved,
}

/// Per planned file: how its bytes arrived, and whether the library took them.
#[derive(Debug, Default)]
struct Fate {
    transfer: Option<Transfer>,
    landed: bool,
}

fn name_of(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
}

/// Removes `.part` files a stopped or crashed intake left in `root`.
pub fn recover_incoming(root: &Path) -> std::io::Result<()> {
    let dir = root.join(INCOMING);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "part") {
            std::fs::remove_file(&path)?;
        }
    }
    // Only when empty: anything else in it is not ours to delete.
    let _ = std::fs::remove_dir(&dir);
    Ok(())
}

/// Moves `path` to the OS trash. A volume without one is an error, and the
/// file stays where it is; it is never deleted outright.
pub fn recycle(path: &Path) -> Result<(), String> {
    trash::delete(path).map_err(|e| e.to_string())
}

/// Copies, converts and imports `planned`, then recycles originals whose
/// copies were verified and imported. Stops between files once `cancel` is set.
#[allow(clippy::too_many_lines, reason = "one pipeline: copy pool, slice import, recycle")]
pub fn execute(
    settings: &IntakeSettings,
    folder: &Path,
    planned: &[Planned],
    cancel: &AtomicBool,
    effects: &mut Effects<'_>,
) -> AppResult<IntakeReportDto> {
    let io = |e: std::io::Error| AppError::new(ErrorKind::Internal, e.to_string());
    recover_incoming(&settings.root).map_err(io)?;
    let incoming = settings.root.join(INCOMING);
    let jobs: Vec<usize> = (0..planned.len())
        .filter(|&i| matches!(planned[i].action, Action::Copy { .. } | Action::Convert { .. }))
        .collect();
    if !jobs.is_empty() {
        std::fs::create_dir_all(folder).map_err(io)?;
        std::fs::create_dir_all(&incoming).map_err(io)?;
    }
    let mut report = IntakeReportDto {
        import: ImportReportDto { imported: 0, skipped: Vec::new(), tracks: Vec::new(), existing: Vec::new() },
        folder: folder.display().to_string(),
        copied: 0,
        converted: 0,
        moved: 0,
        duplicates: Vec::new(),
        renamed: Vec::new(),
        failed: Vec::new(),
        recycled: 0,
        kept: Vec::new(),
        cancelled: false,
    };
    let mut fates: Vec<Fate> = planned.iter().map(|_| Fate::default()).collect();
    let total = u32::try_from(jobs.len()).unwrap_or(u32::MAX);
    let total_bytes = jobs.iter().map(|&i| planned[i].source.size).sum();
    let bytes = AtomicU64::new(0);
    let mut done = 0_u32;

    // A slice that fails or refuses a file takes back what this intake put
    // in the folder for it, so nothing unimported is left behind.
    let mut import = |batch: &[usize], fates: &mut [Fate], report: &mut IntakeReportDto| {
        let paths: Vec<PathBuf> = batch.iter().map(|&i| imported_path(&planned[i])).collect();
        let outcomes = match (effects.import)(&paths) {
            Ok(outcomes) => outcomes,
            Err(e) => {
                let reason = e.message;
                for &i in batch {
                    report.failed.push(format!("{}: {reason}", name_of(&planned[i].source.path)));
                    undo(&planned[i], fates[i].transfer.take());
                }
                return;
            }
        };
        for (&i, outcome) in batch.iter().zip(outcomes) {
            if matches!(outcome, ImportOutcome::Added(_) | ImportOutcome::Existing(_)) {
                fates[i].landed = true;
            } else {
                undo(&planned[i], fates[i].transfer.take());
            }
        }
    };

    let in_place: Vec<usize> =
        (0..planned.len()).filter(|&i| planned[i].action == Action::InPlace).collect();
    if !in_place.is_empty() {
        import(&in_place, &mut fates, &mut report);
    }

    let next = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        let (tx, rx) = mpsc::sync_channel::<(usize, Result<Transfer, String>)>(COPY_WORKERS * SLICE_FILES);
        for _ in 0..COPY_WORKERS {
            let tx = tx.clone();
            let (next, jobs, bytes, incoming) = (&next, &jobs, &bytes, &incoming);
            scope.spawn(move || loop {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                let Some(&i) = jobs.get(next.fetch_add(1, Ordering::Relaxed)) else { break };
                let result = transfer(&planned[i], settings.after, incoming, cancel, bytes);
                if tx.send((i, result)).is_err() {
                    break;
                }
            });
        }
        drop(tx);
        let mut batch: Vec<usize> = Vec::new();
        let mut batch_started = Instant::now();
        let mut last_progress: Option<Instant> = None;
        loop {
            let received = rx.recv_timeout(PROGRESS_EVERY);
            let finished = matches!(received, Err(mpsc::RecvTimeoutError::Disconnected));
            if let Ok((i, result)) = received {
                done += 1;
                match result {
                    Ok(transfer) => {
                        fates[i].transfer = Some(transfer);
                        if batch.is_empty() {
                            batch_started = Instant::now();
                        }
                        batch.push(i);
                    }
                    Err(reason) => report.failed.push(format!("{}: {reason}", name_of(&planned[i].source.path))),
                }
            }
            if !batch.is_empty() && (finished || batch.len() >= SLICE_FILES || batch_started.elapsed() >= SLICE_WAIT) {
                import(&batch, &mut fates, &mut report);
                batch.clear();
            }
            if finished || last_progress.is_none_or(|at| at.elapsed() >= PROGRESS_EVERY) {
                last_progress = Some(Instant::now());
                (effects.progress)(IntakeProgressDto { done, total, bytes: bytes.load(Ordering::Relaxed), total_bytes });
            }
            if finished {
                break;
            }
        }
    });
    report.cancelled = cancel.load(Ordering::Relaxed) && (done as usize) < jobs.len();

    let duplicates: Vec<usize> =
        (0..planned.len()).filter(|&i| matches!(planned[i].action, Action::Duplicate { .. })).collect();
    if !duplicates.is_empty() {
        import(&duplicates, &mut fates, &mut report);
    }

    for (item, fate) in planned.iter().zip(&fates) {
        let name = name_of(&item.source.path);
        match (&item.action, fate.transfer) {
            (Action::Duplicate { .. }, _) => report.duplicates.push(name),
            (Action::Copy { to } | Action::Convert { to }, Some(transfer)) if fate.landed => {
                match transfer {
                    Transfer::Copied => report.copied += 1,
                    Transfer::Converted => report.converted += 1,
                    Transfer::Moved => report.moved += 1,
                }
                let expected = match &item.action {
                    Action::Convert { .. } => format!("{}.aiff", split_name(&name).0),
                    _ => name.clone(),
                };
                let given = name_of(to);
                if given != expected {
                    report.renamed.push(format!("{name} → {given}"));
                }
                if settings.after == After::Recycle && transfer != Transfer::Moved {
                    match (effects.recycle)(&item.source.path) {
                        Ok(()) => report.recycled += 1,
                        Err(reason) => report.kept.push(format!("{name}: {reason}")),
                    }
                }
            }
            _ => {}
        }
    }
    Ok(report)
}

/// The path the library is given for a planned file.
fn imported_path(item: &Planned) -> PathBuf {
    match &item.action {
        Action::InPlace => item.source.path.clone(),
        Action::Duplicate { of } => of.clone(),
        Action::Copy { to } | Action::Convert { to } => to.clone(),
    }
}

/// Takes back a transfer the library did not take: a copy is deleted, a move is put back.
fn undo(item: &Planned, transfer: Option<Transfer>) {
    let (Action::Copy { to } | Action::Convert { to }) = &item.action else { return };
    let result = match transfer {
        Some(Transfer::Moved) => std::fs::rename(to, &item.source.path),
        Some(Transfer::Copied | Transfer::Converted) => std::fs::remove_file(to),
        None => return,
    };
    if let Err(e) = result {
        tracing::warn!(path = %to.display(), error = %e, "could not take back an unimported intake copy");
    }
}

/// Brings one file into the folder: renamed when it can be and recycling
/// was asked for, else staged as `.part`, verified, and renamed into place.
fn transfer(item: &Planned, after: After, incoming: &Path, cancel: &AtomicBool, bytes: &AtomicU64) -> Result<Transfer, String> {
    let (to, convert) = match &item.action {
        Action::Copy { to } => (to, false),
        Action::Convert { to } => (to, true),
        Action::InPlace | Action::Duplicate { .. } => return Err("nothing to copy".into()),
    };
    let source = &item.source;
    // A rename replaces an existing file; the plan's names can be stale.
    if std::fs::symlink_metadata(to).is_ok() {
        return Err(format!("{} is already in the folder", name_of(to)));
    }
    if !convert && after == After::Recycle && std::fs::rename(&source.path, to).is_ok() {
        bytes.fetch_add(source.size, Ordering::Relaxed);
        return Ok(Transfer::Moved);
    }
    let part = incoming.join(format!("{}.part", uuid::Uuid::new_v4()));
    let staged = if convert { convert_into(&source.path, &part, cancel) } else { copy_into(source, &part, cancel, bytes) };
    let published = staged.and_then(|()| {
        if std::fs::symlink_metadata(to).is_ok() {
            return Err(format!("{} is already in the folder", name_of(to)));
        }
        std::fs::rename(&part, to).map_err(|e| e.to_string())
    });
    if let Err(reason) = published {
        let _ = std::fs::remove_file(&part);
        return Err(reason);
    }
    if convert {
        bytes.fetch_add(source.size, Ordering::Relaxed);
        Ok(Transfer::Converted)
    } else {
        Ok(Transfer::Copied)
    }
}

fn copy_into(source: &Source, part: &Path, cancel: &AtomicBool, bytes: &AtomicU64) -> Result<(), String> {
    let copied = crate::backup_copy::copy_file(&source.path, part, &mut |n| {
        if cancel.load(Ordering::Relaxed) {
            return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "stopped"));
        }
        bytes.fetch_add(n, Ordering::Relaxed);
        Ok(())
    })
    .map_err(|e| e.to_string())?;
    let written = std::fs::metadata(part).map_err(|e| e.to_string())?.len();
    if copied != source.size || written != source.size {
        return Err(format!("the copy is {written} bytes, the original {} bytes", source.size));
    }
    Ok(())
}

fn convert_into(source: &Path, part: &Path, cancel: &AtomicBool) -> Result<(), String> {
    use rbl_audio::compatibility::{convert_observed, Format};
    convert_observed(source, part, Format::Aiff, &mut |_, _| !cancel.load(Ordering::Relaxed))
        .map_err(|e| e.to_string())?;
    carry_tags(source, part).map_err(|e| format!("its tags could not be carried over: {e}"))
}

/// The converter writes bare audio; the source's tags and artwork go into
/// the AIFF's `ID3v2` chunk, which is what an AIFF's tags are read from.
fn carry_tags(source: &Path, target: &Path) -> lofty::error::Result<()> {
    use lofty::config::{ParseOptions, ParsingMode, WriteOptions};
    use lofty::prelude::*;
    let tagged = lofty::probe::Probe::open(source)?
        .options(ParseOptions::new().read_properties(false).parsing_mode(ParsingMode::Relaxed))
        .read()?;
    let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) else { return Ok(()) };
    let mut tag = tag.clone();
    tag.re_map(lofty::tag::TagType::Id3v2);
    tag.save_to_path(target, WriteOptions::default())
}

/// Library files by path, and by name and size, from the loaded index.
fn known(library: &rbl_index::Library, folder: &Path) -> Known {
    let mut known = Known::default();
    for row in 0..library.len() {
        let path = PathBuf::from(library.folder_path.get(row));
        let name = name_of(&path).to_lowercase();
        let size = library.file_size.get(row).copied().unwrap_or(0);
        known.paths.insert(path_key(&path));
        known.by_name_size.entry((name.clone(), size)).or_insert_with(|| path.clone());
        known.by_name.entry(name).or_insert(path);
    }
    if let Ok(entries) = std::fs::read_dir(folder) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                if meta.is_file() {
                    known.folder.push((entry.file_name().to_string_lossy().into_owned(), meta.len()));
                }
            }
        }
    }
    known
}

fn source(path: PathBuf) -> Source {
    let size = std::fs::metadata(&path).map_or(0, |m| m.len());
    let kind = if !rbl_db::import::is_audio(&path) {
        SourceKind::NotAudio
    } else if rbl_audio::compatibility::needs_conversion(&path).unwrap_or(false) {
        SourceKind::Convert
    } else {
        SourceKind::Copy
    };
    Source { path, size, kind }
}

fn merge(into: &mut ImportReportDto, slice: ImportReportDto) {
    into.imported += slice.imported;
    into.skipped.extend(slice.skipped);
    into.tracks.extend(slice.tracks);
    into.existing.extend(slice.existing);
}

// ponytail: one flag and one intake at a time, app-wide; per-job ids if two
// windows or scripts ever need to copy at once.
static CANCEL: AtomicBool = AtomicBool::new(false);
static RUNNING: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

/// Whether "Copy dropped files into the library folder" starts out on.
#[tauri::command]
pub fn intake_default_enabled() -> bool {
    crate::fork::BUILD
}

/// Stops the running intake between files; what is already copied is imported.
#[tauri::command]
pub fn cancel_intake() {
    CANCEL.store(true, Ordering::Relaxed);
}

/// Copies `paths` (files or folders) into `settings.root`, imports the
/// copies a slice at a time as they land, and recycles the originals as
/// `settings.after` says. Adding to a playlist stays with the caller, which
/// gets every track id the drop stands for in `tracks` and `existing`.
#[tauri::command]
pub async fn intake_and_import<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, Arc<AppState>>,
    paths: Vec<String>,
    settings: IntakeSettings,
) -> AppResult<IntakeReportDto> {
    let writing = Arc::clone(&state);
    let emitter = app.clone();
    let report = crate::commands::blocking("intake_and_import", move || {
        let Some(_running) = RUNNING.try_lock() else {
            return Err(AppError::new(ErrorKind::Conflict, "Another drop is still being copied into the library folder."));
        };
        CANCEL.store(false, Ordering::Relaxed);
        if !settings.root.is_dir() {
            return Err(AppError::new(ErrorKind::NotFound, "The library folder is not there. Choose it again in Preferences › Import."));
        }
        // Nothing is copied into a library that cannot take it.
        writing.write(|_| Ok(())).map_err(crate::commands::write_error)?;
        let month = rbl_core::time::local_date().get(..7).unwrap_or_default().to_owned();
        let folder = destination(&settings, &month);
        let library = writing.library()?;
        let known = known(&library, &folder);
        let sources = crate::commands::expand_import_paths(&paths).into_iter().map(source).collect();
        let planned = plan(&settings, &folder, sources, &known);
        let mut imported = ImportReportDto { imported: 0, skipped: Vec::new(), tracks: Vec::new(), existing: Vec::new() };
        let mut report = execute(&settings, &folder, &planned, &CANCEL, &mut Effects {
            import: &mut |files| {
                let (slice, outcomes) = crate::commands::import_paths(&writing, files)?;
                merge(&mut imported, slice);
                Ok(outcomes)
            },
            recycle: &recycle,
            progress: &mut |progress| {
                let _ = tauri::Emitter::emit(&emitter, "intake:progress", progress);
            },
        })?;
        report.import = imported;
        Ok(report)
    })
    .await?;
    if report.import.imported > 0 {
        crate::commands::reload(app.clone(), Arc::clone(&state)).await?;
        crate::commands::announce_history(&app, &state);
    }
    Ok(report)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn settings(root: &str) -> IntakeSettings {
        IntakeSettings { root: PathBuf::from(root), layout: Layout::Month, after: After::Recycle, duplicates: true }
    }

    fn src(path: &str, size: u64, kind: SourceKind) -> Source {
        Source { path: PathBuf::from(path), size, kind }
    }

    fn actions(planned: &[Planned]) -> Vec<Action> {
        planned.iter().map(|p| p.action.clone()).collect()
    }

    const ROOT: &str = "/lib";
    const FOLDER: &str = "/lib/2026-11";

    #[test]
    fn new_files_go_to_the_month_folder_or_the_root() {
        let mut s = settings(ROOT);
        assert_eq!(destination(&s, "2026-11"), PathBuf::from(FOLDER));
        s.layout = Layout::Flat;
        assert_eq!(destination(&s, "2026-11"), PathBuf::from(ROOT));
    }

    #[test]
    fn a_new_file_is_copied_under_its_own_name() {
        let planned = plan(&settings(ROOT), Path::new(FOLDER), vec![src("/in/a.mp3", 10, SourceKind::Copy)], &Known::default());
        assert_eq!(actions(&planned), [Action::Copy { to: PathBuf::from("/lib/2026-11/a.mp3") }]);
    }

    #[test]
    fn a_file_under_the_root_is_imported_in_place_never_copied() {
        let sources = vec![
            src("/lib/2025-01/a.mp3", 10, SourceKind::Copy),
            src("/lib/x.flac", 10, SourceKind::Convert),
            src("/lib", 0, SourceKind::Copy),
        ];
        let planned = plan(&settings(ROOT), Path::new(FOLDER), sources, &Known::default());
        assert_eq!(actions(&planned), [Action::InPlace, Action::InPlace, Action::InPlace]);
    }

    #[test]
    fn a_sibling_folder_sharing_the_roots_prefix_is_not_under_it() {
        let planned = plan(&settings(ROOT), Path::new(FOLDER), vec![src("/library/a.mp3", 10, SourceKind::Copy)], &Known::default());
        assert_eq!(actions(&planned), [Action::Copy { to: PathBuf::from("/lib/2026-11/a.mp3") }]);
    }

    #[test]
    fn a_file_the_library_has_by_exact_path_is_imported_in_place() {
        let known = Known { paths: HashSet::from([path_key(Path::new("/music/a.mp3"))]), ..Known::default() };
        let planned = plan(&settings(ROOT), Path::new(FOLDER), vec![src("/music/a.mp3", 10, SourceKind::Copy)], &known);
        assert_eq!(actions(&planned), [Action::InPlace]);
    }

    #[test]
    fn a_file_that_is_not_audio_is_left_for_the_import_to_skip() {
        let planned = plan(&settings(ROOT), Path::new(FOLDER), vec![src("/in/cover.jpg", 10, SourceKind::NotAudio)], &Known::default());
        assert_eq!(actions(&planned), [Action::InPlace]);
    }

    #[test]
    fn same_name_and_size_is_a_duplicate_in_the_folder_or_the_library() {
        let known = Known {
            folder: vec![("A.mp3".into(), 10)],
            by_name_size: HashMap::from([(("b.mp3".into(), 20), PathBuf::from("/old/b.mp3"))]),
            ..Known::default()
        };
        let sources = vec![src("/in/a.mp3", 10, SourceKind::Copy), src("/in/B.mp3", 20, SourceKind::Copy)];
        let planned = plan(&settings(ROOT), Path::new(FOLDER), sources, &known);
        assert_eq!(
            actions(&planned),
            [
                Action::Duplicate { of: PathBuf::from("/lib/2026-11/A.mp3") },
                Action::Duplicate { of: PathBuf::from("/old/b.mp3") },
            ]
        );
    }

    #[test]
    fn the_same_file_twice_in_one_drop_is_copied_once() {
        let sources = vec![src("/in/1/a.mp3", 10, SourceKind::Copy), src("/in/2/a.mp3", 10, SourceKind::Copy)];
        let planned = plan(&settings(ROOT), Path::new(FOLDER), sources, &Known::default());
        assert_eq!(
            actions(&planned),
            [
                Action::Copy { to: PathBuf::from("/lib/2026-11/a.mp3") },
                Action::Duplicate { of: PathBuf::from("/lib/2026-11/a.mp3") },
            ]
        );
    }

    #[test]
    fn a_name_clash_with_other_content_takes_the_next_free_suffix() {
        let known = Known { folder: vec![("a.mp3".into(), 10), ("a (2).mp3".into(), 11), ("noext".into(), 1)], ..Known::default() };
        let sources = vec![
            src("/in/a.mp3", 99, SourceKind::Copy),
            src("/in/x/a.mp3", 98, SourceKind::Copy),
            src("/in/noext", 5, SourceKind::Copy),
        ];
        let planned = plan(&settings(ROOT), Path::new(FOLDER), sources, &known);
        assert_eq!(
            actions(&planned),
            [
                Action::Copy { to: PathBuf::from("/lib/2026-11/a (3).mp3") },
                Action::Copy { to: PathBuf::from("/lib/2026-11/a (4).mp3") },
                Action::Copy { to: PathBuf::from("/lib/2026-11/noext (2)") },
            ]
        );
    }

    #[test]
    fn with_duplicate_detection_off_a_match_is_renamed_not_skipped() {
        let mut s = settings(ROOT);
        s.duplicates = false;
        let known = Known { folder: vec![("a.mp3".into(), 10)], ..Known::default() };
        let planned = plan(&s, Path::new(FOLDER), vec![src("/in/a.mp3", 10, SourceKind::Copy)], &known);
        assert_eq!(actions(&planned), [Action::Copy { to: PathBuf::from("/lib/2026-11/a (2).mp3") }]);
    }

    #[test]
    fn a_source_that_needs_conversion_becomes_an_aiff_and_matches_by_name() {
        let known = Known {
            by_name: HashMap::from([("held.aiff".into(), PathBuf::from("/old/Held.aiff"))]),
            folder: vec![("Taken.aiff".into(), 123)],
            ..Known::default()
        };
        let sources = vec![
            src("/in/New.Mix.flac", 10, SourceKind::Convert),
            src("/in/held.flac", 10, SourceKind::Convert),
            src("/in/taken.m4a", 10, SourceKind::Convert),
        ];
        let planned = plan(&settings(ROOT), Path::new(FOLDER), sources, &known);
        assert_eq!(
            actions(&planned),
            [
                Action::Convert { to: PathBuf::from("/lib/2026-11/New.Mix.aiff") },
                Action::Duplicate { of: PathBuf::from("/old/Held.aiff") },
                Action::Duplicate { of: PathBuf::from("/lib/2026-11/Taken.aiff") },
            ]
        );
    }

    const FLAC: &[u8] = include_bytes!("../../crates/rbl-audio/tests/fixtures/stereo-96k.flac");

    /// A scratch root and a scratch source folder beside it.
    struct Scratch {
        _dir: tempfile::TempDir,
        root: PathBuf,
        input: PathBuf,
    }

    fn scratch() -> Scratch {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("library");
        let input = dir.path().join("downloads");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&input).unwrap();
        Scratch { root, input, _dir: dir }
    }

    impl Scratch {
        fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
            let path = self.input.join(name);
            std::fs::write(&path, bytes).unwrap();
            path
        }

        fn settings(&self, after: After) -> IntakeSettings {
            IntakeSettings { root: self.root.clone(), layout: Layout::Flat, after, duplicates: true }
        }
    }

    fn sourced(path: &Path, kind: SourceKind) -> Source {
        Source { path: path.to_path_buf(), size: std::fs::metadata(path).unwrap().len(), kind }
    }

    /// Runs `execute` with an import that refuses any name containing "bad"
    /// and a recycle bin that is a list.
    fn run(settings: &IntakeSettings, planned: &[Planned], cancel: &AtomicBool) -> (IntakeReportDto, Vec<Vec<PathBuf>>, Vec<PathBuf>) {
        let mut slices = Vec::new();
        let recycled = std::cell::RefCell::new(Vec::new());
        let mut events = 0;
        let report = execute(settings, &settings.root, planned, cancel, &mut Effects {
            import: &mut |paths| {
                slices.push(paths.to_vec());
                Ok(paths
                    .iter()
                    .map(|p| {
                        if name_of(p).contains("bad") {
                            ImportOutcome::Refused("unreadable".into())
                        } else {
                            ImportOutcome::Added(p.display().to_string())
                        }
                    })
                    .collect())
            },
            recycle: &|path| {
                recycled.borrow_mut().push(path.to_path_buf());
                Ok(())
            },
            progress: &mut |_| events += 1,
        })
        .unwrap();
        assert!(events >= 1, "progress is reported at least once");
        (report, slices, recycled.into_inner())
    }

    fn no_parts(root: &Path) -> bool {
        std::fs::read_dir(root.join(INCOMING)).map_or(true, |entries| {
            entries.flatten().all(|e| e.path().extension().is_none_or(|ext| ext != "part"))
        })
    }

    #[test]
    fn copies_are_verified_imported_and_originals_kept_when_asked() {
        let s = scratch();
        let good = s.file("good.wav", &[1; 5000]);
        let bad = s.file("bad.wav", &[2; 300]);
        let settings = s.settings(After::Keep);
        let planned = plan(&settings, &s.root, vec![sourced(&good, SourceKind::Copy), sourced(&bad, SourceKind::Copy)], &Known::default());
        let (report, slices, recycled) = run(&settings, &planned, &AtomicBool::new(false));

        assert_eq!(std::fs::read(s.root.join("good.wav")).unwrap(), [1; 5000]);
        assert!(!s.root.join("bad.wav").exists(), "a copy the library refused is taken back");
        assert!(good.exists() && bad.exists(), "originals are kept");
        assert_eq!(report.copied, 1);
        assert_eq!(recycled, Vec::<PathBuf>::new());
        assert_eq!(slices.concat().len(), 2);
        assert!(no_parts(&s.root));
    }

    #[test]
    fn on_one_volume_a_recycled_original_is_moved_and_put_back_if_refused() {
        let s = scratch();
        let good = s.file("good.wav", &[1; 100]);
        let bad = s.file("bad.wav", &[2; 100]);
        let settings = s.settings(After::Recycle);
        let planned = plan(&settings, &s.root, vec![sourced(&good, SourceKind::Copy), sourced(&bad, SourceKind::Copy)], &Known::default());
        let (report, _, recycled) = run(&settings, &planned, &AtomicBool::new(false));

        assert_eq!(report.moved, 1);
        assert!(!good.exists() && s.root.join("good.wav").exists());
        assert!(bad.exists() && !s.root.join("bad.wav").exists(), "a refused move is put back");
        assert_eq!(recycled, Vec::<PathBuf>::new(), "a moved file has no original left to recycle");
    }

    #[test]
    fn only_verified_imported_originals_are_recycled_and_never_in_root_files() {
        let s = scratch();
        let flac = s.file("tune.flac", FLAC);
        let broken = s.file("bad.flac", b"not audio at all");
        let inside = s.root.join("held.wav");
        std::fs::write(&inside, [3; 10]).unwrap();
        let duplicate = s.file("held.wav", &[3; 10]);
        let settings = s.settings(After::Recycle);
        let known = known_folder(&s.root);
        let sources = vec![
            sourced(&flac, SourceKind::Convert),
            sourced(&broken, SourceKind::Convert),
            sourced(&inside, SourceKind::Copy),
            sourced(&duplicate, SourceKind::Copy),
        ];
        let planned = plan(&settings, &s.root, sources, &known);
        assert_eq!(planned[3].action, Action::Duplicate { of: inside.clone() });
        let (report, _, recycled) = run(&settings, &planned, &AtomicBool::new(false));

        assert_eq!(recycled, std::slice::from_ref(&flac), "only the converted, imported original");
        assert_eq!(report.converted, 1);
        assert!(s.root.join("tune.aiff").exists());
        assert_eq!(report.failed.len(), 1, "{:?}", report.failed);
        assert!(broken.exists() && inside.exists() && duplicate.exists());
        assert_eq!(report.duplicates, ["held.wav"]);
        assert!(no_parts(&s.root));
    }

    fn known_folder(folder: &Path) -> Known {
        let mut known = Known::default();
        for entry in std::fs::read_dir(folder).unwrap().flatten() {
            known.folder.push((entry.file_name().to_string_lossy().into_owned(), entry.metadata().unwrap().len()));
        }
        known
    }

    #[test]
    fn a_cancelled_intake_copies_and_recycles_nothing_but_imports_in_place_files() {
        let s = scratch();
        let a = s.file("a.wav", &[1; 100]);
        let inside = s.root.join("in.wav");
        std::fs::write(&inside, [3; 10]).unwrap();
        let settings = s.settings(After::Recycle);
        let planned = plan(&settings, &s.root, vec![sourced(&a, SourceKind::Copy), sourced(&inside, SourceKind::Copy)], &Known::default());
        let (report, slices, recycled) = run(&settings, &planned, &AtomicBool::new(true));

        assert!(report.cancelled);
        assert!(a.exists() && !s.root.join("a.wav").exists());
        assert_eq!(slices, [vec![inside]]);
        assert_eq!(recycled, Vec::<PathBuf>::new());
        assert!(no_parts(&s.root));
    }

    #[test]
    fn stray_part_files_are_cleared_and_nothing_else() {
        let s = scratch();
        let incoming = s.root.join(INCOMING);
        std::fs::create_dir_all(&incoming).unwrap();
        std::fs::write(incoming.join("x.part"), b"half").unwrap();
        std::fs::write(incoming.join("keep.txt"), b"mine").unwrap();
        recover_incoming(&s.root).unwrap();
        assert!(!incoming.join("x.part").exists());
        assert!(incoming.join("keep.txt").exists());
        std::fs::remove_file(incoming.join("keep.txt")).unwrap();
        recover_incoming(&s.root).unwrap();
        assert!(!incoming.exists(), "an emptied staging folder goes");
    }

    #[test]
    fn a_conversion_keeps_the_sources_tags_and_artwork() {
        use lofty::config::WriteOptions;
        use lofty::picture::{MimeType, Picture, PictureType};
        use lofty::prelude::*;
        use lofty::tag::{Tag, TagType};
        let s = scratch();
        let flac = s.file("tagged.flac", FLAC);
        let mut tag = Tag::new(TagType::VorbisComments);
        tag.insert_text(ItemKey::TrackTitle, "Title".into());
        tag.insert_text(ItemKey::TrackArtist, "Artist".into());
        tag.insert_text(ItemKey::InitialKey, "Am".into());
        tag.insert_text(ItemKey::Label, "Label".into());
        let art = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 1, 2, 3, 4];
        tag.push_picture(Picture::new_unchecked(PictureType::CoverFront, Some(MimeType::Png), None, art.clone()));
        tag.save_to_path(&flac, WriteOptions::default()).unwrap();

        let part = s.root.join("x.part");
        convert_into(&flac, &part, &AtomicBool::new(false)).unwrap();
        let aiff = s.root.join("tagged.aiff");
        std::fs::rename(&part, &aiff).unwrap();
        let tags = rbl_db::import::read_tags(&aiff).unwrap();
        assert_eq!((tags.title.as_str(), tags.artist.as_str(), tags.key.as_str(), tags.label.as_str()), ("Title", "Artist", "Am", "Label"));
        assert_eq!(tags.sample_rate, 44_100);
        assert_eq!(rbl_db::import::read_artwork(&aiff).unwrap(), Some(art));
    }
}
