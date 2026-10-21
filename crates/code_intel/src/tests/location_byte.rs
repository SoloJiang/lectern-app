//! `location_byte_*` (「字节偏移」, ADR 0034): every candidate's and
//! reference's `Location::byte` is the offset of the name's first byte in
//! the blob its OID names, for the worktree's files (from the index, the
//! current text parsed or kept, references) and the object database's;
//! None when the bytes extracted only became that content through
//! `canonical_oid` (a CRLF checkout). The window units' group is in
//! `window_*` (`window_byte_offsets_are_in_the_whole_file`); version 2's
//! tables are rebuilt in `store_*`'s file
//! (`location_byte_version_2_tables_are_rebuilt`).

use lectern_services::code_intel::{Location, QueryKind, TextOrigin, TextRef};

use super::goto::indexed;
use super::support::{CHANGED, Harness, pos_in};

const LIB: &str = "export function helper(a: number): number {\n  return a + 1;\n}\n\
                   export const other = 2;\n";
const MAIN: &str = "import { helper } from './lib';\n\
                    export function run(): number {\n  const local = helper(1);\n  \
                    return local + helper(2);\n}\n";

/// The bytes of the blob `location` names: the worktree's file (its OID
/// checked) or the object database's.
fn bytes_of(h: &Harness, location: &Location, texts: &[&str]) -> Vec<u8> {
    texts
        .iter()
        .map(|t| t.as_bytes().to_vec())
        .find(|b| h.git.oid(b) == location.oid)
        .unwrap_or_else(|| panic!("no text with {:?}", location.oid))
}

/// `location.byte` is set and the name starts there.
fn assert_points_at(h: &Harness, location: &Location, name: &str, texts: &[&str]) {
    let bytes = bytes_of(h, location, texts);
    let at = location.byte.expect("a byte offset") as usize;
    assert!(
        bytes[at..].starts_with(name.as_bytes()),
        "{}:{} byte {at} is not {name:?}",
        location.path,
        location.line
    );
    // The same place as the line and column.
    let line_start = bytes
        .split_inclusive(|b| *b == b'\n')
        .take(location.line as usize)
        .map(<[u8]>::len)
        .sum::<usize>();
    assert_eq!(at, line_start + location.column as usize);
}

#[test]
fn location_byte_points_at_the_names() {
    let h = indexed(&[("lib.ts", LIB), ("main.ts", MAIN)]);
    let texts = [LIB, MAIN];
    // Step 3: the index's candidate in another file.
    let r = h.goto("main.ts", "helper", 1);
    let lib = r
        .candidates
        .iter()
        .find(|c| &*c.definition.location.path == "lib.ts")
        .expect("lib.ts's helper");
    assert_points_at(&h, &lib.definition.location, "helper", &texts);
    // Step 1: the current text's local binding, parsed cold and then kept.
    for _ in 0..2 {
        let r = h.goto("main.ts", "local", 1);
        let local = &r.candidates[r.jump.expect("a jump") as usize];
        assert_points_at(&h, &local.definition.location, "local", &texts);
    }
    // A definition's own name, and references to it in both files.
    let r = h.goto("lib.ts", "helper", 0);
    let def = r.candidates[0].definition.clone();
    assert_points_at(&h, &def.location, "helper", &texts);
    let r = h.ask(QueryKind::References { of: def });
    let all: Vec<_> = r.callers.iter().chain(r.same_name.iter()).collect();
    assert!(all.len() >= 2, "{:?}", r);
    for reference in all {
        assert_points_at(&h, &reference.location, "helper", &texts);
    }
}

#[test]
fn location_byte_points_into_the_object_database() {
    let old = "export function run() {\n  const limit = 1;\n  return limit;\n}\n";
    let h = indexed(&[("a.ts", old)]);
    h.git.add(
        "main",
        &[("a.ts", b"export function run() { return 0; }\n")],
    );
    h.git.commit("main");
    h.refresh_files();
    let at = TextRef {
        worktree: Some(h.wt()),
        path: "a.ts".into(),
        oid: h.git.oid(old.as_bytes()),
        origin: TextOrigin::Object,
    };
    let position = pos_in(old.as_bytes(), "limit", 1);
    let r = h.ask(QueryKind::Definition { at, position });
    let def = r.candidates[0].definition.clone();
    assert_eq!(def.location.origin, TextOrigin::Object);
    assert_points_at(&h, &def.location, "limit", &[old]);
    let r = h.ask(QueryKind::References { of: def });
    assert!(!r.callers.is_empty());
    for reference in r.callers.iter() {
        assert_points_at(&h, &reference.location, "limit", &[old]);
    }
}

#[test]
fn location_byte_is_none_for_a_converted_checkout() {
    let h = Harness::new();
    h.git.add(
        "main",
        &[("lib.ts", LIB.as_bytes()), ("main.ts", MAIN.as_bytes())],
    );
    h.git.commit("main");
    // A CRLF checkout of lib.ts: its raw bytes are not the index's blob,
    // `canonical_oid` makes them that content.
    h.git.eol("lib.ts");
    h.git
        .write("main", "lib.ts", LIB.replace('\n', "\r\n").as_bytes());
    h.open();
    h.emit(&[("lib.ts", CHANGED)]);
    h.settle();
    assert!(h.hinted("lib.ts"), "indexed under the index's OID");
    let r = h.goto("main.ts", "helper", 1);
    let lib = r
        .candidates
        .iter()
        .find(|c| &*c.definition.location.path == "lib.ts")
        .expect("lib.ts's helper");
    assert_eq!(lib.definition.location.oid, h.git.oid(LIB.as_bytes()));
    assert_eq!(lib.definition.location.byte, None, "the view goes by line");
    assert_eq!(lib.definition.location.line, 0);
    // The current text read and converted: no offsets either; main.ts,
    // read as it is, keeps its own.
    let r = h.goto("lib.ts", "helper", 0);
    let def = r.candidates[0].definition.clone();
    assert_eq!(def.location.byte, None);
    let r = h.ask(QueryKind::References { of: def });
    for reference in r.callers.iter().chain(r.same_name.iter()) {
        if &*reference.location.path == "lib.ts" {
            assert_eq!(reference.location.byte, None);
        } else {
            assert_points_at(&h, &reference.location, "helper", &[MAIN]);
        }
    }
}
