//! The canary test (docs/design/review_view.md「日志与诊断」,
//! `logs_hold_no_contents`): paths, a session title, prompts, the agent's
//! texts, a tool name, diff lines, the arguments and the reason of a brief
//! entry and of an analysis carry a canary through the whole flow of step 1
//! at trace level, and no record carries it.

use std::sync::Arc;
use std::time::Duration;

use gpui::TestAppContext;
use lectern_services::changes::ChangeError;
use lectern_services::project::BlobId;
use lectern_services::review::{
    AnalysisState, BriefItem, BriefRule, LoadState, ReviewError, ReviewTarget,
};
use lectern_services::usage::{RowKey, UsageQuery, UsageSnapshot};
use lectern_ui::motion::CHECK_STROKE;

use crate::items::ItemKey;
use crate::testing::{FIRST_ID, agent_key, prompt_key, session_key, snapshot_fixture_tagged};
use crate::tests::step2::{locate, session_usage};
use crate::tests::support::Rig;

const CANARY: &str = "review-canary-3f8a91c2";

#[gpui::test]
fn logs_hold_no_contents(cx: &mut TestAppContext) {
    let capture = lectern_logging::testing::capture();
    lectern_logging::testing::set_callsite_limit(1_000_000);
    let rig = Rig::new(cx);
    rig.enable_handoff(cx);
    locate(&rig);
    // The session title is the canary too.
    let mut titled = crate::tests::support::summary(session_key(), &format!("Fix the {CANARY}"));
    titled.place = lectern_services::session::Place::Located(lectern_services::project::Location {
        project: lectern_services::project::ProjectKey::Folder {
            root: Arc::from(std::path::Path::new("/fake")),
        },
        worktree: Some(lectern_services::project::WorktreeKey(Arc::from(
            std::path::Path::new("/fake"),
        ))),
        rel: Arc::from(""),
        package: lectern_services::project::PackageMatch::None,
    });
    rig.sessions.set(vec![titled]);
    let UsageSnapshot::Session(mut usage) = session_usage() else {
        unreachable!()
    };
    let mut usage_rows = usage.rows.to_vec();
    if let RowKey::Branch { branch, .. } = &mut usage_rows[1].key {
        branch.name = Arc::from(format!("refs/heads/{CANARY}"));
    }
    usage.rows = Arc::from(usage_rows);
    rig.usage.set(
        UsageQuery::Session {
            session: session_key(),
        },
        UsageSnapshot::Session(usage),
    );
    let (mut snapshot, mut rows) = snapshot_fixture_tagged(3, 2, 8, CANARY);
    // Blobs with OIDs, so that the sides are shared and highlighted.
    for (n, (_, rows)) in rows.iter_mut().enumerate() {
        rows.old.oid = Some(BlobId::Sha1([n as u8 + 1; 20]));
        rows.new.oid = Some(BlobId::Sha1([n as u8 + 101; 20]));
    }
    let mut brief = (*snapshot.brief).clone();
    let mut items = brief.items.to_vec();
    items.push(BriefItem {
        rule: BriefRule::Provided {
            provider: Arc::from(format!("provider-{CANARY}")),
            rule: Arc::from(format!("rule-{CANARY}")),
            args: Arc::from(vec![(
                Arc::<str>::from(format!("arg-{CANARY}")),
                Arc::<str>::from(format!("value-{CANARY}")),
            )]),
        },
        critical: false,
        file: None,
        line: None,
        count: 1,
    });
    brief.items = Arc::from(items);
    brief.analysis = AnalysisState::Failed(Arc::from(format!("reason {CANARY}")));
    snapshot.brief = Arc::new(brief);
    rig.review.set_all_rows(FIRST_ID, rows);
    rig.review.set_auto_prepare(true);
    for turn in 0..2 {
        rig.review
            .set_text(FIRST_ID, prompt_key(turn), &format!("prompt text {CANARY}"));
        rig.review
            .set_text(FIRST_ID, agent_key(turn), &format!("agent said {CANARY}"));
    }
    let target = ReviewTarget::Session(session_key());
    let opened = rig.open(cx, target.clone(), Some(snapshot.clone()));
    opened.draw(cx);
    rig.settle(cx);
    opened.draw(cx);
    // The first frame with rows reports the milestone.
    let mut visual = opened.visual(cx);
    visual.update(|window, cx| window.simulate_next_frame(cx));
    visual.run_until_parked();

    // The highlight tasks run on the executor.
    rig.run_tasks(cx);
    opened.draw(cx);

    // J, V (and the scroll after the check), Looks good, Esc.
    visual.simulate_keystrokes("j v");
    opened.advance(
        cx,
        CHECK_STROKE.delay + CHECK_STROKE.duration + Duration::from_micros(1),
    );
    visual.simulate_keystrokes("escape");
    visual.dispatch_action(crate::actions::LooksGood);
    visual.run_until_parked();

    // The second step: a row of it is handed to the reader and the code
    // navigation, the mode changes, a fold is looked for, an analysis is
    // asked for.
    let row = opened.read(cx, |model| {
        model
            .items
            .iter()
            .find(|item| matches!(item, crate::items::Item::Row { .. }))
            .map(crate::items::Item::key)
    });
    if let Some(row) = row {
        opened.update(cx, |model, cx| model.select(Some(row), cx));
    }
    visual.dispatch_action(crate::actions::OpenFile);
    visual.dispatch_action(crate::actions::OpenContext);
    visual.dispatch_action(crate::actions::ToggleDiffMode);
    visual.dispatch_action(crate::actions::ExpandFold);
    visual.dispatch_action(crate::actions::Analyze);
    visual.dispatch_action(crate::actions::ToggleDiffMode);
    visual.run_until_parked();
    opened.update(cx, |model, cx| model.select(Some(ItemKey::Brief), cx));
    rig.usage.set(
        UsageQuery::Session {
            session: session_key(),
        },
        session_usage(),
    );
    rig.handoff
        .inner()
        .expect("the kernel runs")
        .emitter()
        .emit(lectern_services::usage::UsageEvent::Changed {
            query: UsageQuery::Session {
                session: session_key(),
            },
            generation: 3,
        });
    rig.settle(cx);
    opened.draw(cx);
    assert!(!rig.reader.opened().is_empty(), "the reader was asked");
    assert!(!rig.code_intel.focus().is_empty());

    // A new generation, a stale one, and a failed state with a reason.
    let mut next = snapshot.clone();
    next.generation = 40;
    next.unclaimed = 2;
    rig.review.set_snapshot(FIRST_ID, next);
    rig.review.announce(FIRST_ID);
    rig.settle(cx);
    let mut failed = snapshot;
    failed.generation = 41;
    failed.state = LoadState::Failed(ReviewError::Changes(ChangeError::Io(Arc::from(format!(
        "read error {CANARY}"
    )))));
    rig.review.set_snapshot(FIRST_ID, failed);
    rig.review.announce(FIRST_ID);
    rig.settle(cx);
    opened.draw(cx);

    // The screen goes away.
    let window = opened.window;
    drop(opened);
    window
        .update(cx, |host, _, cx| {
            host.clear_child();
            cx.notify();
        })
        .unwrap();
    cx.run_until_parked();

    capture.assert_absent(CANARY);
    let lines = capture.lines();
    let has = |msg: &str| {
        lines
            .iter()
            .any(|line| line.contains(&format!("\"msg\":\"{msg}\"")))
    };
    assert!(has("review screen opened"), "{lines:#?}");
    assert!(has("diff shown"));
    assert!(has("review screen closed"));
    assert!(has("review failed shown"));
    assert!(has("highlight"));
    assert!(has("focus set"));
    assert!(has("usage applied"));
    let closed = lines
        .iter()
        .find(|line| line.contains("\"msg\":\"review screen closed\""))
        .unwrap();
    assert!(closed.contains("\"marked\":1"), "{closed}");
    assert!(closed.contains("\"looks_good\":true"), "{closed}");
    let failed = lines
        .iter()
        .find(|line| line.contains("\"msg\":\"review failed shown\""))
        .unwrap();
    assert!(failed.contains("\"error\":\"io\""), "{failed}");
}
