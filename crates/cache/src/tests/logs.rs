//! Log records (`logs_*`, docs/design/cache.md「日志与诊断」): what the
//! failure paths write, and that keys, values and custom table parameters
//! never reach a record.

use std::fs;
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

use lectern_logging::testing::{self, Capture};

use super::support::{Blob, Fixture, files, key, noise};
use super::tables::tags;
use crate::{
    DisableReason, Dropped, Key, Param, Partition, PartitionKind, PartitionState, Written,
};

const PAGE: u64 = 4096;
const CANARY: &str = "canary-c4c3e-7f3a91";

/// Writes 200 entries of 3 KiB, each holding the canary, and closes the
/// cache so everything is checkpointed into the `.sqlite` file.
fn populate(f: &Fixture, p: &Partition) {
    let mut batch = p.batch();
    for i in 0..200 {
        let mut value = CANARY.as_bytes().to_vec();
        value.extend(noise(i, 3 << 10));
        batch.put(&Key::new().str(CANARY).u64(i), &Blob(value));
    }
    assert_eq!(batch.commit(), Written::Committed);
    f.cache.close();
    assert!(!files(p.file())[1].exists(), "checkpointed on close");
}

/// Overwrites the pages in the middle third of the file.
fn overwrite_middle_pages(file: &Path) {
    let pages = fs::metadata(file).unwrap().len() / PAGE;
    assert!(pages > 100, "{pages} pages");
    let mut out = fs::OpenOptions::new().write(true).open(file).unwrap();
    for page in pages / 3..pages * 2 / 3 {
        out.seek(SeekFrom::Start(page * PAGE)).unwrap();
        out.write_all(&noise(page, PAGE as usize)).unwrap();
    }
}

/// Reads every populated key.
fn read_all(p: &Partition) {
    for i in 0..200 {
        let _ = p.get::<Blob>(&Key::new().str(CANARY).u64(i));
    }
}

/// The records of this fixture (its temporary directory is in their paths)
/// with message `msg`. `cargo test` runs every test in one process, sharing
/// the capture.
fn records(log: &Capture, f: &Fixture, msg: &str) -> Vec<String> {
    let dir = f
        .dir
        .path()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let msg = format!("\"msg\":\"{msg}\"");
    log.lines()
        .into_iter()
        .filter(|l| l.contains(&msg) && l.contains(&dir))
        .collect()
}

/// The raw JSON text of field `name` in `line`.
fn field<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let start = line.find(&format!("\"{name}\":"))? + name.len() + 3;
    let rest = &line[start..];
    let end = if let Some(text) = rest.strip_prefix('"') {
        text.find('"')? + 2
    } else {
        rest.find([',', '}'])?
    };
    Some(&rest[..end])
}

#[test]
fn logs_hold_no_keys_values_or_parameters() {
    let log = testing::capture();
    let mut f = Fixture::new();
    // `repo/` is a file holding the canary: the directory sweep cannot list
    // it (a warn), and the file's content is not read.
    fs::create_dir_all(f.root()).unwrap();
    fs::write(f.root().join("repo"), CANARY).unwrap();

    // Keys and values holding the canary, written and read back.
    let app = f.cache.app();
    let canary_key = Key::new().str(CANARY).bytes(CANARY.as_bytes());
    let value = Blob(format!("{CANARY} file contents").into_bytes());
    assert_eq!(app.put(&canary_key, &value), Written::Committed);
    assert_eq!(app.get::<Blob>(&canary_key), Some(value));

    // Custom table parameters holding the canary: a write whose statement
    // fails (a row for a unit that does not exist) and a query with the
    // wrong number of parameters.
    let folder = f.cache.folder(Path::new("/work/notes"));
    let set = folder.tables(&tags::TABLES);
    let mut w = set.write();
    let unit = w.create(&Key::new().str(CANARY), 100);
    w.execute(
        tags::INSERT_EXT,
        &[Param::New(unit), Param::Bytes(CANARY.as_bytes())],
    );
    w.execute(
        tags::INSERT_DEF,
        &[Param::Int(9999), Param::Text(CANARY), Param::Int(1)],
    );
    assert_eq!(w.commit(), Written::Dropped(Dropped::Io));
    assert_eq!(
        set.read(|r| r.query(
            tags::DEFS,
            &[Param::Text(CANARY), Param::Text(CANARY)],
            |_| true
        )),
        None
    );

    // A corrupt file holding the canary, rebuilt in the next run.
    populate(&f, &app);
    overwrite_middle_pages(app.file());
    f.reopen();
    let app = f.cache.app();
    read_all(&app);
    assert!(f.cache.stats().rebuilt >= 1);

    for msg in [
        "cannot list partitions",
        "custom table statement failed",
        "custom table query failed",
        "partition corrupt; rebuilding",
    ] {
        assert!(
            !records(log, &f, msg).is_empty(),
            "no {msg:?} record in {:#?}",
            log.lines()
        );
    }
    log.assert_absent(CANARY);
}

#[test]
fn logs_disabled_partition_is_an_error() {
    let log = testing::capture();
    let f = Fixture::new();
    let p = f.repo();
    // First corruption in the run: random bytes, rebuilt.
    fs::create_dir_all(p.file().parent().unwrap()).unwrap();
    fs::write(p.file(), noise(3, 64 << 10)).unwrap();
    assert_eq!(p.get::<Blob>(&key(1)), None);
    // Second: disabled.
    populate(&f, &p);
    overwrite_middle_pages(p.file());
    read_all(&p);
    let stats = f.cache.stats();
    let repo = stats
        .partitions
        .iter()
        .find(|s| s.kind == PartitionKind::Repo);
    assert_eq!(
        repo.unwrap().state,
        PartitionState::Disabled {
            reason: DisableReason::Corrupt
        }
    );

    let file_name = p.file().file_name().unwrap().to_str().unwrap();
    let rebuilt = records(log, &f, "partition corrupt; rebuilding");
    assert_eq!(rebuilt.len(), 1, "{rebuilt:#?}");
    assert_eq!(field(&rebuilt[0], "lv"), Some("\"warn\""));
    assert_eq!(field(&rebuilt[0], "sqlite"), Some("26"), "SQLITE_NOTADB");

    let disabled = records(log, &f, "partition disabled");
    assert_eq!(disabled.len(), 1, "{disabled:#?}");
    let line = &disabled[0];
    assert_eq!(field(line, "lv"), Some("\"error\""));
    assert_eq!(field(line, "tg"), Some("\"cache\""));
    assert_eq!(field(line, "reason"), Some("\"corrupt\""));
    assert_eq!(field(line, "op"), Some("\"call\""));
    let code: i32 = field(line, "sqlite").unwrap().parse().unwrap();
    assert_eq!(code & 0xff, 11, "SQLITE_CORRUPT: {line}");
    assert_eq!(field(line, "kind"), Some("\"repo\""));
    assert!(
        field(line, "path")
            .unwrap()
            .ends_with(&format!("{file_name}\""))
    );
    assert_eq!(field(line, "project"), Some("\"/work/lectern/.git\""));
}
