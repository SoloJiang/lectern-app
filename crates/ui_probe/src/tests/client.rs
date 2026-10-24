//! The blocking client against a fake probe on a real socket.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::json;

use crate::client::Client;
use crate::server::SOCKET;

/// A probe that reads `answers.len()` lines and answers each with the next
/// canned line; `None` leaves the request unanswered. Returns the lines it
/// received.
fn fake_probe(
    dir: &std::path::Path,
    answers: Vec<Option<&'static str>>,
) -> JoinHandle<Vec<String>> {
    let listener = UnixListener::bind(dir.join(SOCKET)).unwrap();
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut writer = stream;
        let mut received = Vec::new();
        for answer in answers {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap() == 0 {
                break;
            }
            received.push(line.trim_end().to_owned());
            if let Some(answer) = answer {
                writeln!(writer, "{answer}").unwrap();
            }
        }
        // Stay connected until the client hangs up.
        let _ = reader.read_line(&mut String::new());
        received
    })
}

#[test]
fn call_sends_a_request_and_reads_the_reply() {
    let tmp = tempfile::tempdir().unwrap();
    let probe = fake_probe(
        tmp.path(),
        vec![
            Some(r#"{"v":1,"id":1,"ok":true,"result":{"protocol":1}}"#),
            Some(r#"{"v":1,"id":2,"ok":false,"error":{"kind":"no-window","message":"none"}}"#),
        ],
    );
    let mut client = Client::connect(tmp.path(), Duration::from_secs(10)).unwrap();

    let ok = client.call("hello", json!({})).unwrap();
    assert!(ok.ok);
    assert_eq!(ok.result, Some(json!({ "protocol": 1 })));

    let failed = client.call("ui_tree", json!({ "window": 2 })).unwrap();
    assert!(!failed.ok && failed.result.is_none());
    let error = failed.error.unwrap();
    assert_eq!(
        (error.kind.as_str(), error.message.as_str()),
        ("no-window", "none")
    );

    drop(client);
    let received = probe.join().unwrap();
    let first: serde_json::Value = serde_json::from_str(&received[0]).unwrap();
    assert_eq!(
        first,
        json!({ "v": 1, "id": 1, "method": "hello", "params": {} })
    );
    let second: serde_json::Value = serde_json::from_str(&received[1]).unwrap();
    assert_eq!(
        (second["id"].clone(), second["params"]["window"].clone()),
        (json!(2), json!(2))
    );
}

#[test]
fn raw_sends_the_line_as_it_is() {
    let tmp = tempfile::tempdir().unwrap();
    let probe = fake_probe(tmp.path(), vec![Some(r#"{"v":1,"id":0,"ok":false}"#)]);
    let mut client = Client::connect(tmp.path(), Duration::from_secs(10)).unwrap();
    let response = client.raw("not even json").unwrap();
    assert_eq!(response, r#"{"v":1,"id":0,"ok":false}"#);
    drop(client);
    assert_eq!(probe.join().unwrap(), ["not even json"]);
}

#[test]
fn a_reply_that_does_not_come_times_out() {
    let tmp = tempfile::tempdir().unwrap();
    let probe = fake_probe(tmp.path(), vec![None]);
    let mut client = Client::connect(tmp.path(), Duration::from_millis(100)).unwrap();
    let error = client.call("hello", json!({})).unwrap_err();
    assert!(
        matches!(
            error.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ),
        "{error:?}"
    );
    drop(client);
    probe.join().unwrap();
}

#[test]
fn a_closed_connection_is_an_error_and_a_missing_probe_cannot_connect() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(Client::connect(tmp.path(), Duration::from_secs(1)).is_err());

    let listener = UnixListener::bind(tmp.path().join(SOCKET)).unwrap();
    let hang_up = std::thread::spawn(move || drop(listener.accept().unwrap()));
    let mut client = Client::connect(tmp.path(), Duration::from_secs(10)).unwrap();
    hang_up.join().unwrap();
    assert!(client.call("hello", json!({})).is_err());
}
