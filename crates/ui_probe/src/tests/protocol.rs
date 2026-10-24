//! Request parsing, response encoding and the parameter checks.

use serde_json::json;

use crate::protocol::{ErrorKind, Response, check_dir, check_name, parse_request};

fn error_of(line: &str) -> (u64, ErrorKind) {
    let response = parse_request(line.as_bytes()).unwrap_err();
    assert!(!response.ok && response.result.is_none());
    (response.id, response.error.unwrap().kind)
}

#[test]
fn request_parses_with_and_without_params() {
    let with =
        parse_request(br#"{"v":1,"id":7,"method":"ui_tree","params":{"window":3}}"#).unwrap();
    assert_eq!((with.v, with.id, with.method.as_str()), (1, 7, "ui_tree"));
    assert_eq!(with.params, json!({ "window": 3 }));
    // `params` may be left out.
    let without = parse_request(br#"{"v":1,"id":8,"method":"hello"}"#).unwrap();
    assert!(without.params.is_null());
}

#[test]
fn request_errors_are_bad_request_with_the_id_when_there_is_one() {
    // The wrong version, a missing or mistyped field: the id is kept.
    assert_eq!(
        error_of(r#"{"v":2,"id":5,"method":"hello"}"#),
        (5, ErrorKind::BadRequest)
    );
    assert_eq!(error_of(r#"{"v":1,"id":6}"#), (6, ErrorKind::BadRequest));
    assert_eq!(
        error_of(r#"{"v":1,"id":9,"method":4}"#),
        (9, ErrorKind::BadRequest)
    );
    // No id, a negative id, not JSON, not UTF-8, not an object: id 0.
    for line in [
        r#"{"v":1,"method":"hello"}"#.as_bytes(),
        br#"{"v":1,"id":-1,"method":"hello"}"#,
        b"hello",
        b"",
        b"\xff\xfe",
        b"[1,2]",
        b"null",
    ] {
        let response = parse_request(line).unwrap_err();
        assert_eq!(response.id, 0, "{line:?}");
        assert_eq!(response.error.unwrap().kind, ErrorKind::BadRequest);
    }
}

#[test]
fn response_lines_are_one_json_line() {
    let ok = Response::ok(3, json!({ "pid": 1 })).to_line();
    let err = Response::err(4, ErrorKind::NoWindow).to_line();
    for line in [&ok, &err] {
        assert_eq!(line.iter().filter(|&&b| b == b'\n').count(), 1);
        assert_eq!(line.last(), Some(&b'\n'));
    }
    insta::assert_snapshot!(
        "response_lines",
        format!(
            "{}{}",
            String::from_utf8(ok).unwrap(),
            String::from_utf8(err).unwrap()
        )
    );
}

#[test]
fn every_error_kind_has_a_kebab_name_and_a_fixed_message() {
    for (kind, name) in [
        (ErrorKind::BadRequest, "bad-request"),
        (ErrorKind::UnknownMethod, "unknown-method"),
        (ErrorKind::NoWindow, "no-window"),
        (ErrorKind::Timeout, "timeout"),
        (ErrorKind::Render, "render"),
        (ErrorKind::Io, "io"),
        (ErrorKind::Quitting, "quitting"),
    ] {
        assert_eq!(kind.as_str(), name);
        let line = Response::err(1, kind).to_line();
        let value: serde_json::Value = serde_json::from_slice(&line).unwrap();
        assert_eq!(value["error"]["kind"], name);
        assert!(
            value["error"]["message"]
                .as_str()
                .is_some_and(|m| !m.is_empty())
        );
    }
}

#[test]
fn name_allows_file_name_characters_only() {
    for name in ["shot-1", "a.b_c-D9", "x", "..a", "a..", ".hidden"] {
        assert!(check_name(name), "{name}");
    }
    for name in [
        "", ".", "..", "a/b", "../x", "a b", "é", "a\n", "a\0", "a:b", "~",
    ] {
        assert!(!check_name(name), "{name:?}");
    }
}

#[test]
fn dir_must_be_absolute_and_exist() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_str().unwrap();
    assert_eq!(check_dir(dir).as_deref(), Some(tmp.path()));
    assert!(check_dir("relative/dir").is_none());
    assert!(check_dir("").is_none());
    assert!(check_dir(&format!("{dir}/missing")).is_none());
    // A file is not a directory.
    let file = tmp.path().join("f");
    std::fs::write(&file, b"").unwrap();
    assert!(check_dir(file.to_str().unwrap()).is_none());
}
