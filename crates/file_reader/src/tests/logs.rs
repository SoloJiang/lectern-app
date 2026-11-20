//! `logs_*`: the records of this crate (docs/design/file_reader.md「日志与
//! 诊断」) are written, with typed fields, and never hold a path, file
//! content or a position.

use lectern_logging::testing;
use lectern_services::reader::{Anchor, OpenOutcome, Reader};
use lectern_syntax::testing as syntax_testing;

use super::rig::{Rig, Tree, file};

const CANARY: &str = "canary5k2m8v1q7z";

/// The records with message `msg`.
fn records(msg: &str) -> Vec<String> {
    let msg = format!("\"msg\":\"{msg}\"");
    testing::capture()
        .lines()
        .into_iter()
        .filter(|l| l.contains(&msg))
        .collect()
}

#[test]
fn logs_hold_no_file_contents() {
    let log = testing::capture();
    let rig = Rig::new();
    let (tree, key) = rig.folder();
    let name = format!("{CANARY}/{CANARY}.ts");
    let text = format!("// {CANARY}\nconst {CANARY} = \"{CANARY}\";\n");
    tree.write(&name, text.as_bytes());
    tree.write("big.ts", &syntax_testing::ts(8 << 10, 0));

    // The reading: open, scroll, back and forward, the focus, a prefetch.
    rig.show(file(&key, &name, Anchor::Start));
    rig.show(file(&key, "big.ts", Anchor::Line { line: 2, column: 0 }));
    assert!(rig.session.back());
    assert!(rig.session.forward());
    rig.session.prefetch(vec![file(&key, &name, Anchor::Start)]);
    rig.res.settle();

    // The failure the session has: a request whose project is gone, from
    // the three ways to reach a document (a path with the canary in it).
    let stranger = Tree::new();
    let gone = stranger.folder();
    rig.session.open(file(
        &gone,
        &name,
        Anchor::Byte {
            byte: 9,
            line: 1,
            column: 0,
        },
    ));
    let request = rig.session.next_request().unwrap();
    assert_eq!(rig.session.apply(request), OpenOutcome::Cancelled);
    // Back to a position whose project closed.
    rig.host.projects.remove(&key);
    assert!(!rig.session.back());
    rig.session.close();

    let dropped =
        records("a request to show a file is dropped; its project or worktree has no root");
    // Other tests of the process may log the same message: at least these.
    let via = |kind: &str| {
        dropped
            .iter()
            .any(|l| l.contains(&format!("\"via\":\"{kind}\"")))
    };
    assert!(via("open") && via("back"), "{:#?}", log.lines());
    assert!(
        dropped
            .iter()
            .all(|l| l.contains("\"lv\":\"warn\"") && l.contains("\"tg\":\"file_reader\""))
    );
    assert!(
        dropped
            .iter()
            .any(|l| l.contains("\"origin\":\"worktree\"") && l.contains("\"ticket\":")),
    );
    // The verbose record of a queued open: the ticket and two counts.
    let queued = records("a request to open a file was queued");
    assert!(
        queued.iter().any(|l| l.contains("\"lv\":\"debug\"")
            && l.contains("\"tg\":\"file_reader\"")
            && l.contains("\"ticket\":")
            && l.contains("\"prestarted\":")
            && l.contains("\"pending\":")),
        "{queued:#?}"
    );
    // The syntax crate's record of the first screen.
    let computed = records("the first screen of an opened file was computed");
    assert!(
        computed.iter().any(|l| l.contains("\"lv\":\"debug\"")
            && l.contains("\"tg\":\"syntax\"")
            && l.contains("\"prepare_us\":")
            && l.contains("\"window_us\":")
            && l.contains("\"outcome\":")),
        "{computed:#?}"
    );
    log.assert_absent(CANARY);
}
