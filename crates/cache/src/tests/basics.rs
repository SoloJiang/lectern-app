//! Construction, the UI thread check, reads and writes, namespaces, batches,
//! compression and connection settings (`new_*`, `ui_thread_*`,
//! `roundtrip_*`, `namespace_*`, `batch_*`, `compression_*`, `memory_*`).

use std::fmt::Write as _;
use std::path::Path;
use std::thread;

use super::support::{
    Blob, BlobV2, Fixture, Other, Packed, Picky, config, count, files, key, meta_bytes, noise, raw,
    sum_sizes,
};
use crate::db::ENTRY_OVERHEAD;
use crate::{
    Batch, Budgets, Cache, Config, Dropped, Entry, Key, MAX_BATCH, MAX_KEY, MAX_VALUE,
    PartitionKind, PartitionState, Written,
};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

// new_*

#[test]
fn new_does_no_io_until_the_first_read() {
    let f = Fixture::new();
    let root = f.root();
    let app = f.cache.app();
    let repo = f.repo();
    let folder = f.cache.folder(Path::new("/work/notes"));
    assert!(!root.exists());
    let stats = f.cache.stats();
    assert_eq!(stats.connections, 0);
    assert!(
        stats
            .partitions
            .iter()
            .all(|p| p.state == PartitionState::Closed)
    );
    drop((app.clone(), repo.clone(), folder.clone()));
    assert!(!root.exists());

    assert_eq!(app.get::<Blob>(&key(1)), None);
    assert!(root.join("app.sqlite").exists());
    assert!(!root.join("repo").exists());
    assert_eq!(repo.get::<Blob>(&key(1)), None);
    assert!(repo.file().exists());
    assert!(!folder.file().exists());
}

#[test]
fn new_file_layout() {
    let f = Fixture::new();
    let root = f.root();
    let name = |p: &crate::Partition| {
        p.file()
            .strip_prefix(&root)
            .expect("under the root")
            .display()
            .to_string()
    };
    let repo = f.cache.repo(Path::new("/work/lectern/.git"));
    let folder = f.cache.folder(Path::new("/work/lectern/.git"));
    let layout = format!(
        "app: {}\nrepo: {}\nfolder (same path): {}\nanother repo: {}",
        name(&f.cache.app()),
        name(&repo),
        name(&folder),
        name(&f.cache.repo(Path::new("/work/other/.git"))),
    );
    insta::assert_snapshot!(layout);
    // The same common directory, from any worktree, is the same partition.
    assert_eq!(
        f.cache.repo(Path::new("/work/lectern/.git")).file(),
        repo.file()
    );
    assert_ne!(repo.file(), folder.file());
}

#[test]
fn new_partition_records_kind_and_path() {
    let f = Fixture::new();
    let repo = f.repo();
    assert_eq!(repo.put(&key(1), &Blob(b"x".to_vec())), Written::Committed);
    let conn = raw(repo.file());
    let mut meta: Vec<(String, String)> = conn
        .prepare("SELECT name, CAST(value AS TEXT) FROM meta ORDER BY name")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for (name, value) in &mut meta {
        if name == "opened" {
            *value = "<seconds>".to_owned();
        }
    }
    insta::assert_debug_snapshot!(meta);
}

// ui_thread_*

fn on_this_thread(root: &Path) -> Cache {
    Cache::new(Config {
        ui_thread: Some(thread::current().id()),
        ..config(root, Budgets::default())
    })
}

#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "blocking call on the UI thread")]
fn ui_thread_get_panics_in_debug_builds() {
    let dir = tempfile::tempdir().unwrap();
    let cache = on_this_thread(&dir.path().join("cache"));
    let _ = cache.app().get::<Blob>(&key(1));
}

#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "blocking call on the UI thread")]
fn ui_thread_put_panics_in_debug_builds() {
    let dir = tempfile::tempdir().unwrap();
    let cache = on_this_thread(&dir.path().join("cache"));
    let _ = cache.app().put(&key(1), &Blob(vec![1]));
}

#[test]
fn ui_thread_release_behavior_does_no_io() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("cache");
    let cache = on_this_thread(&root);
    cache.set_ui_thread_panics(false);
    let app = cache.app();
    let repo = cache.repo(Path::new("/work/lectern/.git"));

    assert_eq!(app.get::<Blob>(&key(1)), None);
    assert_eq!(repo.get_many::<Blob>(&[key(1), key(2)]), vec![None, None]);
    assert_eq!(
        app.put(&key(1), &Blob(vec![1])),
        Written::Dropped(Dropped::WrongThread)
    );
    assert_eq!(
        repo.remove::<Blob>(&key(1)),
        Written::Dropped(Dropped::WrongThread)
    );
    let mut batch = app.batch();
    batch.put(&key(2), &Blob(vec![2]));
    assert_eq!(batch.commit(), Written::Dropped(Dropped::WrongThread));
    cache.close();
    assert!(!root.exists());

    let stats = cache.stats();
    assert_eq!(stats.dropped(Dropped::WrongThread), 3);
    assert_eq!(stats.misses, 3);
    assert_eq!(stats.connections, 0);

    // Any other thread may block.
    thread::scope(|s| {
        s.spawn(|| {
            assert_eq!(app.put(&key(1), &Blob(vec![1])), Written::Committed);
            assert_eq!(app.get::<Blob>(&key(1)), Some(Blob(vec![1])));
        });
    });
    assert!(root.join("app.sqlite").exists());
}

// roundtrip_*

#[test]
fn roundtrip_put_get_overwrite_remove() {
    let f = Fixture::new();
    let p = f.repo();
    assert_eq!(p.get::<Blob>(&key(1)), None);
    assert_eq!(p.put(&key(1), &Blob(b"one".to_vec())), Written::Committed);
    assert_eq!(p.get::<Blob>(&key(1)), Some(Blob(b"one".to_vec())));
    assert_eq!(p.put(&key(1), &Blob(b"uno".to_vec())), Written::Committed);
    assert_eq!(p.get::<Blob>(&key(1)), Some(Blob(b"uno".to_vec())));
    assert_eq!(p.remove::<Blob>(&key(1)), Written::Committed);
    assert_eq!(p.get::<Blob>(&key(1)), None);
    // Removing what is not there commits too.
    assert_eq!(p.remove::<Blob>(&key(9)), Written::Committed);
    assert_eq!(p.remove::<Other>(&key(9)), Written::Committed);

    let stats = f.cache.stats();
    assert_eq!((stats.hits, stats.misses, stats.written), (2, 2, 5));
}

#[test]
fn roundtrip_empty_value() {
    let f = Fixture::new();
    let p = f.cache.app();
    assert_eq!(p.put(&key(1), &Blob(Vec::new())), Written::Committed);
    assert_eq!(p.get::<Blob>(&key(1)), Some(Blob(Vec::new())));
}

#[test]
fn roundtrip_size_limits() {
    let f = Fixture::new();
    let p = f.repo();
    let largest = Blob(noise(1, MAX_VALUE));
    assert_eq!(p.put(&key(1), &largest), Written::Committed);
    assert_eq!(p.get::<Blob>(&key(1)).as_ref(), Some(&largest));

    assert_eq!(
        p.put(&key(2), &Blob(vec![0; MAX_VALUE + 1])),
        Written::Dropped(Dropped::TooLarge)
    );
    assert_eq!(p.get::<Blob>(&key(2)), None);

    let long = |len: usize| {
        // 1 tag byte + 2 length bytes + the string.
        Key::new().str(&"x".repeat(len - 3))
    };
    assert_eq!(long(MAX_KEY).len(), MAX_KEY);
    assert_eq!(p.put(&long(MAX_KEY), &Blob(vec![1])), Written::Committed);
    assert_eq!(p.get::<Blob>(&long(MAX_KEY)), Some(Blob(vec![1])));
    assert_eq!(
        p.put(&long(MAX_KEY + 1), &Blob(vec![1])),
        Written::Dropped(Dropped::TooLarge)
    );
    assert_eq!(
        p.remove::<Blob>(&long(MAX_KEY + 1)),
        Written::Dropped(Dropped::TooLarge)
    );
    assert_eq!(p.get::<Blob>(&long(MAX_KEY + 1)), None);
    assert_eq!(f.cache.stats().dropped(Dropped::TooLarge), 3);
}

#[test]
fn roundtrip_get_many_keeps_the_order_of_keys() {
    let f = Fixture::new();
    let p = f.repo();
    for i in [1, 3, 4, 7] {
        assert_eq!(p.put(&key(i), &Blob(vec![i as u8])), Written::Committed);
    }
    let keys: Vec<Key> = [7, 0, 1, 2, 3, 4, 8, 7].into_iter().map(key).collect();
    let found: Vec<Option<u8>> = p
        .get_many::<Blob>(&keys)
        .into_iter()
        .map(|v| v.map(|b| b.0[0]))
        .collect();
    assert_eq!(
        found,
        vec![
            Some(7),
            None,
            Some(1),
            None,
            Some(3),
            Some(4),
            None,
            Some(7)
        ]
    );
    assert_eq!(p.get_many::<Blob>(&[]), Vec::new());
    let stats = f.cache.stats();
    assert_eq!((stats.hits, stats.misses), (5, 3));
}

#[test]
fn roundtrip_key_encoding_is_injective() {
    let keys = [
        ("empty", Key::new()),
        ("str ab, str c", Key::new().str("ab").str("c")),
        ("str a, str bc", Key::new().str("a").str("bc")),
        ("str abc", Key::new().str("abc")),
        ("bytes abc", Key::new().bytes(b"abc")),
        ("u32 0x61626364", Key::new().u32(0x6162_6364)),
        ("bytes abcd", Key::new().bytes(b"abcd")),
        ("u64 1", Key::new().u64(1)),
        ("u32 1", Key::new().u32(1)),
        ("bytes 0x00 * 130", Key::new().bytes(&[0; 130])),
        (
            "oid, u32, str",
            Key::new().bytes(&[0xAB; 20]).u32(2).str("histogram"),
        ),
    ];
    let mut out = String::new();
    for (name, key) in &keys {
        let _ = writeln!(out, "{name}: {}", hex(key.as_bytes()));
    }
    insta::assert_snapshot!(out);
    for (i, (_, a)) in keys.iter().enumerate() {
        for (_, b) in &keys[i + 1..] {
            assert_ne!(a, b);
        }
    }
}

#[test]
fn roundtrip_meta_bytes_track_entry_sizes() {
    let f = Fixture::new();
    let p = f.repo();
    let size = |k: &Key, v: usize| (k.len() + v) as u64 + ENTRY_OVERHEAD;
    assert_eq!(p.put(&key(1), &Blob(vec![0; 100])), Written::Committed);
    assert_eq!(p.put(&key(2), &Blob(vec![0; 200])), Written::Committed);
    assert_eq!(p.put(&key(1), &Blob(vec![0; 50])), Written::Committed);
    let conn = raw(p.file());
    let expected = size(&key(1), 50) + size(&key(2), 200);
    assert_eq!(meta_bytes(&conn), expected);
    assert_eq!(sum_sizes(&conn), expected);
    assert_eq!(p.remove::<Blob>(&key(2)), Written::Committed);
    assert_eq!(meta_bytes(&conn), size(&key(1), 50));
    let stats = f.cache.stats();
    let repo = stats
        .partitions
        .iter()
        .find(|s| s.kind == PartitionKind::Repo)
        .unwrap();
    assert_eq!(repo.bytes, size(&key(1), 50));
    assert_eq!(repo.state, PartitionState::Open);
}

#[test]
fn roundtrip_decode_failure_is_a_miss_and_deleted_by_the_next_write() {
    let f = Fixture::new();
    let p = f.repo();
    assert_eq!(p.put(&key(1), &Picky(vec![0xFF, 1])), Written::Committed);
    assert_eq!(p.put(&key(2), &Picky(vec![1])), Written::Committed);
    assert_eq!(p.get::<Picky>(&key(1)), None);
    assert_eq!(p.get::<Picky>(&key(1)), None);
    assert_eq!(p.get::<Picky>(&key(2)), Some(Picky(vec![1])));
    let conn = raw(p.file());
    assert_eq!(count(&conn, &Picky::NAMESPACE), 2);
    let stats = f.cache.stats();
    assert_eq!((stats.hits, stats.misses, stats.decode_failures), (1, 2, 2));
    // Read twice, remembered once.
    assert_eq!(p.slot.pending_bad(), 1);

    assert_eq!(p.put(&key(3), &Other(vec![3])), Written::Committed);
    assert_eq!(count(&conn, &Picky::NAMESPACE), 1);
    assert_eq!(p.slot.pending_bad(), 0);
    assert_eq!(meta_bytes(&conn), sum_sizes(&conn));
}

#[test]
fn roundtrip_survives_the_next_run() {
    let mut f = Fixture::new();
    assert_eq!(f.repo().put(&key(1), &Blob(vec![1])), Written::Committed);
    assert_eq!(
        f.cache.app().put(&key(1), &Blob(vec![2])),
        Written::Committed
    );
    f.cache.close();
    assert_eq!(f.cache.stats().connections, 0);
    // Closed partitions open again.
    assert_eq!(f.repo().get::<Blob>(&key(1)), Some(Blob(vec![1])));
    f.reopen();
    assert_eq!(f.repo().get::<Blob>(&key(1)), Some(Blob(vec![1])));
    assert_eq!(f.cache.app().get::<Blob>(&key(1)), Some(Blob(vec![2])));
    assert_eq!(f.cache.stats().rebuilt, 0);
}

#[test]
fn roundtrip_handles_outliving_the_cache_do_nothing() {
    let f = Fixture::new();
    let p = f.repo();
    assert_eq!(p.put(&key(1), &Blob(vec![1])), Written::Committed);
    drop(f.cache);
    assert_eq!(p.get::<Blob>(&key(1)), None);
    assert_eq!(
        p.put(&key(2), &Blob(vec![2])),
        Written::Dropped(Dropped::Unavailable)
    );
    // The connections closed with the cache: the WAL is checkpointed away.
    assert!(!files(p.file())[1].exists());
}

// namespace_*

#[test]
fn namespace_isolates_equal_keys() {
    let f = Fixture::new();
    let p = f.repo();
    assert_eq!(p.put(&key(1), &Blob(b"blob".to_vec())), Written::Committed);
    assert_eq!(
        p.put(&key(1), &Other(b"other".to_vec())),
        Written::Committed
    );
    assert_eq!(p.get::<Blob>(&key(1)), Some(Blob(b"blob".to_vec())));
    assert_eq!(p.get::<Other>(&key(1)), Some(Other(b"other".to_vec())));
    assert_eq!(p.remove::<Other>(&key(1)), Written::Committed);
    assert_eq!(p.get::<Blob>(&key(1)), Some(Blob(b"blob".to_vec())));
    assert_eq!(p.get::<Other>(&key(1)), None);
}

#[test]
fn namespace_new_version_hides_and_gradually_deletes_old_entries() {
    let f = Fixture::new();
    let p = f.repo();
    let mut batch = p.batch();
    for i in 0..600 {
        batch.put(&key(i), &Blob(vec![1; 100]));
    }
    assert_eq!(batch.commit(), Written::Committed);
    assert_eq!(p.get::<BlobV2>(&key(1)), None);
    let conn = raw(p.file());
    let before = meta_bytes(&conn);

    // The first write of version 2 retires version 1 and deletes 256 of its
    // entries in the same transaction.
    assert_eq!(p.put(&key(1), &BlobV2(vec![2])), Written::Committed);
    let mut left = vec![count(&conn, &Blob::NAMESPACE)];
    let mut bytes = vec![meta_bytes(&conn)];
    for i in 0..3 {
        assert_eq!(p.put(&key(1000 + i), &Other(vec![3])), Written::Committed);
        left.push(count(&conn, &Blob::NAMESPACE));
        bytes.push(meta_bytes(&conn));
    }
    assert_eq!(left, vec![344, 88, 0, 0]);
    assert!(bytes[0] < before && bytes[1] < bytes[0] && bytes[2] < bytes[1]);
    assert_eq!(meta_bytes(&conn), sum_sizes(&conn));
    assert_eq!(p.get::<Blob>(&key(599)), None);
    assert_eq!(p.get::<BlobV2>(&key(1)), Some(BlobV2(vec![2])));
    let v1_entry = key(0).len() as u64 + 100 + ENTRY_OVERHEAD;
    assert_eq!(f.cache.stats().evicted_bytes, 600 * v1_entry);
}

#[test]
fn namespace_older_program_sees_only_its_own_version() {
    let f = Fixture::new();
    let p = f.repo();
    assert_eq!(p.put(&key(1), &BlobV2(b"new".to_vec())), Written::Committed);
    // The older program, with version 1 of the same namespace.
    assert_eq!(p.get::<Blob>(&key(1)), None);
    assert_eq!(p.put(&key(2), &Blob(b"old".to_vec())), Written::Committed);
    assert_eq!(p.get::<BlobV2>(&key(2)), None);
    let conn = raw(p.file());
    let rows: Vec<(String, u32, bool)> = conn
        .prepare("SELECT name, version, retired FROM namespace ORDER BY name, version")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    // A version lower than one already seen is retired from the start.
    assert_eq!(
        rows,
        vec![
            ("test.blob".to_owned(), 1, true),
            ("test.blob".to_owned(), 2, false)
        ]
    );
}

// batch_*

#[test]
fn batch_becomes_visible_at_once() {
    let f = Fixture::new();
    let p = f.repo();
    assert_eq!(p.put(&key(0), &Blob(vec![0])), Written::Committed);
    let mut batch = p.batch();
    for i in 1..=100 {
        batch.put(&key(i), &Blob(vec![i as u8]));
    }
    batch.put(&key(1), &Other(vec![9]));
    batch.remove::<Blob>(&key(0));
    for i in 1..=100 {
        assert_eq!(p.get::<Blob>(&key(i)), None);
    }
    assert_eq!(batch.commit(), Written::Committed);
    for i in 1..=100 {
        assert_eq!(p.get::<Blob>(&key(i)), Some(Blob(vec![i as u8])));
    }
    assert_eq!(p.get::<Other>(&key(1)), Some(Other(vec![9])));
    assert_eq!(p.get::<Blob>(&key(0)), None);
    assert_eq!(f.cache.stats().written, 2);
    let conn = raw(p.file());
    assert_eq!(meta_bytes(&conn), sum_sizes(&conn));
}

#[test]
fn batch_last_write_of_a_key_wins() {
    let f = Fixture::new();
    let p = f.repo();
    let mut batch = p.batch();
    batch
        .put(&key(1), &Blob(b"a".to_vec()))
        .put(&key(1), &Blob(b"b".to_vec()))
        .put(&key(2), &Blob(b"c".to_vec()))
        .remove::<Blob>(&key(2))
        .remove::<Blob>(&key(3))
        .put(&key(3), &Blob(b"d".to_vec()));
    assert_eq!(batch.commit(), Written::Committed);
    assert_eq!(p.get::<Blob>(&key(1)), Some(Blob(b"b".to_vec())));
    assert_eq!(p.get::<Blob>(&key(2)), None);
    assert_eq!(p.get::<Blob>(&key(3)), Some(Blob(b"d".to_vec())));
    let conn = raw(p.file());
    assert_eq!(meta_bytes(&conn), sum_sizes(&conn));
}

#[test]
fn batch_over_the_limits_is_dropped_whole() {
    let f = Fixture::new();
    let p = f.repo();
    let mut batch = p.batch();
    for i in 0..3 {
        batch.put(&key(i), &Blob(vec![0; 6 << 20]));
    }
    assert!(batch.bytes() > MAX_BATCH);
    assert_eq!(batch.commit(), Written::Dropped(Dropped::TooLarge));

    let mut batch = p.batch();
    batch
        .put(&key(10), &Blob(vec![1]))
        .put(&key(11), &Blob(vec![0; MAX_VALUE + 1]));
    assert_eq!(batch.commit(), Written::Dropped(Dropped::TooLarge));
    for i in [0, 1, 2, 10, 11] {
        assert_eq!(p.get::<Blob>(&key(i)), None);
    }
    assert_eq!(f.cache.stats().dropped(Dropped::TooLarge), 2);
}

#[test]
fn batch_counts_keys_and_values() {
    let f = Fixture::new();
    let p = f.repo();
    let mut batch: Batch = p.batch();
    assert_eq!(batch.bytes(), 0);
    batch.put(&key(1), &Blob(vec![0; 100]));
    batch.remove::<Blob>(&key(2));
    assert_eq!(batch.bytes(), (key(1).len() + 100 + key(2).len()) as u64);
    // An empty batch commits without opening anything.
    assert_eq!(p.batch().commit(), Written::Committed);
    assert!(!p.file().exists());
}

#[test]
fn batch_is_send() {
    fn send<T: Send>() {}
    fn send_sync<T: Send + Sync>() {}
    send::<Batch>();
    send_sync::<crate::Partition>();
    send_sync::<Cache>();
}

// compression_*

fn codecs(conn: &rusqlite::Connection) -> Vec<(i64, i64)> {
    conn.prepare("SELECT codec, length(value) FROM entry ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn compression_stores_large_values_compressed() {
    let f = Fixture::new();
    let p = f.cache.app();
    let text: Vec<u8> = (0..64 << 10).map(|i| b"fn main() {}\n"[i % 13]).collect();
    assert_eq!(p.put(&key(1), &Packed(text.clone())), Written::Committed);
    assert_eq!(p.get::<Packed>(&key(1)), Some(Packed(text.clone())));
    let conn = raw(p.file());
    let stored = codecs(&conn);
    assert_eq!(stored[0].0, 1);
    assert!(stored[0].1 < text.len() as i64 / 10);
    // The budget counts the stored bytes.
    assert_eq!(
        meta_bytes(&conn),
        (key(1).len() as u64) + stored[0].1 as u64 + ENTRY_OVERHEAD
    );
}

#[test]
fn compression_keeps_small_and_incompressible_values_raw() {
    let f = Fixture::new();
    let p = f.cache.app();
    let small = vec![b'a'; (4 << 10) - 1];
    let random = noise(7, 64 << 10);
    let text = vec![b'a'; 64 << 10];
    assert_eq!(p.put(&key(1), &Packed(small.clone())), Written::Committed);
    assert_eq!(p.put(&key(2), &Packed(random.clone())), Written::Committed);
    // Compression::None never compresses.
    assert_eq!(p.put(&key(3), &Blob(text.clone())), Written::Committed);
    assert_eq!(p.get::<Packed>(&key(1)), Some(Packed(small.clone())));
    assert_eq!(p.get::<Packed>(&key(2)), Some(Packed(random.clone())));
    assert_eq!(p.get::<Blob>(&key(3)), Some(Blob(text.clone())));
    let conn = raw(p.file());
    assert_eq!(
        codecs(&conn),
        vec![
            (0, small.len() as i64),
            (0, random.len() as i64),
            (0, text.len() as i64)
        ]
    );
}

#[test]
fn compression_broken_frame_is_a_decode_failure() {
    let f = Fixture::new();
    let p = f.cache.app();
    assert_eq!(
        p.put(&key(1), &Packed(vec![b'a'; 64 << 10])),
        Written::Committed
    );
    let conn = raw(p.file());
    conn.execute("UPDATE entry SET value = x'28b52ffd00'", [])
        .unwrap();
    assert_eq!(p.get::<Packed>(&key(1)), None);
    assert_eq!(f.cache.stats().decode_failures, 1);
}

// memory_*

const PRAGMAS: [&str; 12] = [
    "cache_size",
    "mmap_size",
    "busy_timeout",
    "journal_mode",
    "synchronous",
    "journal_size_limit",
    "trusted_schema",
    "cell_size_check",
    "query_only",
    "auto_vacuum",
    "application_id",
    "user_version",
];

fn pragmas(conn: &rusqlite::Connection) -> String {
    let mut out = String::new();
    for name in PRAGMAS {
        let value: rusqlite::types::Value = conn
            .query_row(&format!("PRAGMA {name}"), [], |r| r.get(0))
            .unwrap();
        let _ = writeln!(out, "{name} = {value:?}");
    }
    out
}

#[test]
fn memory_connection_settings() {
    let f = Fixture::new();
    let p = f.cache.app();
    assert_eq!(p.put(&key(1), &Blob(vec![1])), Written::Committed);
    let (writer, reader) = p
        .slot
        .with_connections(|w, r| (pragmas(w), r.map(pragmas)))
        .unwrap();
    assert_eq!(reader, None, "the read connection opens on the first read");
    assert_eq!(f.cache.stats().connections, 1);
    assert_eq!(p.get::<Blob>(&key(1)), Some(Blob(vec![1])));
    let (_, reader) = p
        .slot
        .with_connections(|w, r| (pragmas(w), r.map(pragmas)))
        .unwrap();
    insta::assert_snapshot!("writer", writer);
    insta::assert_snapshot!("reader", reader.unwrap());
    assert_eq!(f.cache.stats().connections, 2);
}

#[test]
fn memory_page_cache_follows_the_budget() {
    let f = Fixture::with_budgets(Budgets {
        page_cache: 2 << 20,
        ..Budgets::default()
    });
    let p = f.repo();
    assert_eq!(p.get::<Blob>(&key(1)), None);
    let sizes = p
        .slot
        .with_connections(|w, r| {
            let size = |c: &rusqlite::Connection| -> i64 {
                c.query_row("PRAGMA cache_size", [], |r| r.get(0)).unwrap()
            };
            (size(w), r.map(size))
        })
        .unwrap();
    assert_eq!(sizes, (-2048, Some(-2048)));
}

#[test]
fn memory_connections_are_counted_and_closed() {
    let f = Fixture::new();
    let app = f.cache.app();
    let repo = f.repo();
    assert_eq!(app.get::<Blob>(&key(1)), None);
    assert_eq!(repo.put(&key(1), &Blob(vec![1])), Written::Committed);
    assert_eq!(f.cache.stats().connections, 3);
    assert_eq!(repo.get::<Blob>(&key(1)), Some(Blob(vec![1])));
    assert_eq!(f.cache.stats().connections, 4);
    f.cache.close();
    let stats = f.cache.stats();
    assert_eq!(stats.connections, 0);
    assert!(
        stats
            .partitions
            .iter()
            .all(|p| p.state == PartitionState::Closed)
    );
}
