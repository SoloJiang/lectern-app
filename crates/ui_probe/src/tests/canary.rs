//! `canary_stays_out_of_probe_logs` (docs/design/ui_probe.md「日志与诊断」):
//! a label, a business key and an attribute value that hold the canary, and
//! a method that is the canary, leave no trace in the log.

use std::sync::Arc;

use gpui::{
    AnyElement, Context, IntoElement, ParentElement, Render, Styled, TestAppContext, Window, div,
    px,
};
use image::RgbaImage;
use lectern_i18n::{Locale, Strings};
use lectern_ui::semantic::{NodeId, Role, Semantic, semantic};
use lectern_ui::{Appearance, Text, init_fixed};
use serde_json::json;

use super::server::{FakeHost, request};
use crate::annotate::{annotate, write_png};
use crate::server::{Outcome, Session};
use crate::tree::snapshot;

const CANARY: &str = "canary-7f3a91e2-probe";

struct Scene;

impl Render for Scene {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let list = NodeId::new("probe", "list", None);
        let item = list.item(CANARY);
        let text = Text::verbatim(Arc::<str>::from(CANARY));
        let node: AnyElement = semantic(
            Semantic::new(list, Role::List),
            div().w(px(100.0)).h(px(40.0)).child(semantic(
                Semantic::new(item, Role::ListItem)
                    .label(&text)
                    .attr("note", CANARY.to_owned()),
                div().w(px(100.0)).h(px(20.0)),
            )),
        )
        .into_any_element();
        node
    }
}

#[gpui::test]
fn canary_stays_out_of_probe_logs(cx: &mut TestAppContext) {
    let logs = lectern_logging::testing::capture();
    cx.update(|cx| init_fixed(cx, Strings::of(Locale::En), Appearance::Light, false));
    let (_, cx) = cx.add_window_view(|_, _| Scene);
    cx.run_until_parked();

    let tree = cx.update(snapshot);
    let json = tree.to_json();
    assert!(json.contains(CANARY), "the tree itself shows the canary");

    // The picture and the file.
    let tmp = tempfile::tempdir().unwrap();
    let mut image = RgbaImage::new(64, 64);
    annotate(&mut image, &tree);
    write_png(&image, &tmp.path().join("shot.png")).unwrap();

    // The requests: a method that is the canary, one that works and one that
    // is not JSON.
    let session = Session::new(tmp.path().to_path_buf());
    let host = FakeHost::answering(Outcome::Value(json!({ "protocol": 1 })));
    let line = request(1, CANARY, json!({ "note": CANARY }));
    let processed = session.process(&host, 1, line.as_bytes());
    let response: serde_json::Value = serde_json::from_slice(&processed.line).unwrap();
    assert_eq!(response["error"]["kind"], "unknown-method");
    session.process(&host, 1, request(2, "hello", json!({})).as_bytes());
    session.process(&host, 1, format!("{CANARY} is not json").as_bytes());

    let lines = logs.lines();
    assert!(
        lines.iter().any(|l| l.contains("probe request failed")),
        "the failed requests were logged: {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("\"probe request\"") || l.contains("probe request\"")),
        "the request that worked was logged: {lines:?}"
    );
    logs.assert_absent(CANARY);
}
