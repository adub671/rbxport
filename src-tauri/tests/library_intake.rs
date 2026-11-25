//! Drop-to-library intake end to end: the command against a fixture library,
//! a scratch library folder and scratch originals.
#![allow(clippy::pedantic, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rbl_db::fixture::{self, Shape};
use rbl_db::{Library as Db, OpenMode};
use rbxport_lib::library_intake::{self, After, IntakeSettings, Layout};
use rbxport_lib::state::AppState;
use tauri::test::MockRuntime;
use tauri::Manager;

const FLAC: &[u8] = include_bytes!("../../crates/rbl-audio/tests/fixtures/stereo-96k.flac");

struct Shell {
    dir: tempfile::TempDir,
    app: tauri::App<MockRuntime>,
}

fn shell() -> Shell {
    let dir = tempfile::tempdir().unwrap();
    let location = fixture::build(dir.path(), Shape::default()).expect("build the fixture");
    let state = AppState::with_backups(dir.path().join("backups"));
    let db = Db::open(location.clone(), OpenMode::ReadOnly).expect("open the fixture");
    let (library, _) = rbl_index::load(&db).expect("index the fixture");
    state.set_library(library, false, db.schema().db_version, 0, location);
    let app = tauri::test::mock_app();
    app.manage(Arc::new(state));
    Shell { dir, app }
}

fn write_wav(path: &Path, frames: u32) -> String {
    let data_len = frames * 4;
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16_u32.to_le_bytes());
    out.extend_from_slice(&1_u16.to_le_bytes());
    out.extend_from_slice(&2_u16.to_le_bytes());
    out.extend_from_slice(&44_100_u32.to_le_bytes());
    out.extend_from_slice(&(44_100_u32 * 4).to_le_bytes());
    out.extend_from_slice(&4_u16.to_le_bytes());
    out.extend_from_slice(&16_u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.resize(44 + data_len as usize, 0);
    std::fs::write(path, out).unwrap();
    path.display().to_string()
}

impl Shell {
    fn intake(&self, paths: Vec<String>, settings: &IntakeSettings) -> library_intake::IntakeReportDto {
        let state = self.app.state::<Arc<AppState>>();
        tauri::async_runtime::block_on(library_intake::intake_and_import(
            self.app.handle().clone(),
            state,
            paths,
            settings.clone(),
        ))
        .unwrap()
    }

    fn library_paths(&self) -> Vec<PathBuf> {
        let library = self.app.state::<Arc<AppState>>().library().unwrap();
        (0..library.len()).map(|row| PathBuf::from(library.folder_path.get(row))).collect()
    }
}

#[test]
fn a_drop_is_copied_converted_imported_and_a_second_drop_copies_nothing() {
    let s = shell();
    let root = s.dir.path().join("Music Library");
    let downloads = s.dir.path().join("downloads");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(downloads.join("album")).unwrap();
    let a = write_wav(&downloads.join("album").join("One.wav"), 44_100);
    let b = write_wav(&downloads.join("album").join("Two.wav"), 88_200);
    let flac = downloads.join("album").join("Three.flac");
    std::fs::write(&flac, FLAC).unwrap();
    let cover = downloads.join("cover.jpg");
    std::fs::write(&cover, b"jpeg").unwrap();
    let inside = write_wav(&root.join("Already Here.wav"), 44_100);
    let settings = IntakeSettings { root: root.clone(), layout: Layout::Flat, after: After::Keep, duplicates: true };
    let before = s.library_paths().len();

    let first = s.intake(
        vec![downloads.join("album").display().to_string(), cover.display().to_string(), inside.clone()],
        &settings,
    );

    assert_eq!((first.copied, first.converted, first.import.imported), (2, 1, 4), "{first:?}");
    assert_eq!(first.import.skipped.len(), 1, "the .jpg is reported: {:?}", first.import.skipped);
    for name in ["One.wav", "Two.wav", "Three.aiff", "Already Here.wav"] {
        let path = root.join(name);
        assert!(path.is_file(), "{name} is in the library folder");
        assert!(s.library_paths().contains(&path), "{name} is in the library");
    }
    assert_eq!(s.library_paths().len(), before + 4);
    assert!(Path::new(&a).exists() && Path::new(&b).exists() && flac.exists() && cover.exists(), "originals kept");
    assert_eq!(first.recycled, 0);

    let second = s.intake(vec![downloads.join("album").display().to_string(), inside], &settings);
    assert_eq!((second.copied, second.converted, second.import.imported), (0, 0, 0), "{second:?}");
    assert_eq!(second.duplicates.len(), 3);
    assert_eq!(second.import.existing.len(), 4, "every dropped file still stands for its track");
    assert_eq!(s.library_paths().len(), before + 4);
    assert_eq!(std::fs::read_dir(&root).unwrap().filter(|e| e.as_ref().unwrap().path().is_file()).count(), 4);
}
