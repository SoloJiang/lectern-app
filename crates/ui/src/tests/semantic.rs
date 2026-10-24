//! `node_id_*` (docs/design/ui.md「测试」): the format, `child`, `item`
//! and the debug-build check. Moved up from step 4 with `NodeId` itself,
//! which the controls' constructors take.

use gpui::ElementId;

use crate::semantic::{Label, NodeId, Role, Semantic, State};
use crate::{Text, strings};

#[test]
fn node_id_formats_view_area_and_key() {
    let plain = NodeId::new("review", "files", None);
    assert_eq!(plain.as_str(), "review.files");
    let keyed = NodeId::new("review", "files", Some("src/main.rs"));
    assert_eq!(keyed.as_str(), "review.files.item[src/main.rs]");
    assert_eq!(
        keyed.child("seen").as_str(),
        "review.files.item[src/main.rs].seen"
    );
    assert_eq!(
        plain.item("a b/c-d_e.rs").as_str(),
        "review.files.item[a b/c-d_e.rs]"
    );
    // A key may hold `]` and `.`, only not `].` together.
    assert_eq!(
        plain.item("x[1]").child("edit").as_str(),
        "review.files.item[x[1]].edit"
    );
    assert_eq!(
        ElementId::from(&plain),
        ElementId::Name("review.files".into())
    );
}

#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "not [a-z0-9-]+")]
fn node_id_rejects_an_invalid_view_in_debug_builds() {
    let _ = NodeId::new("Review", "files", None);
}

#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "newline")]
fn node_id_rejects_a_key_with_a_newline_in_debug_builds() {
    let _ = NodeId::new("review", "files", Some("a\nb"));
}

#[test]
fn semantic_builder_takes_label_state_and_attrs() {
    let strings = lectern_i18n::Strings::of(lectern_i18n::Locale::En);
    let text = Text::key(&strings, lectern_i18n::keys::common::OK);
    let spec = Semantic::new(NodeId::new("welcome", "ok", None), Role::Button)
        .label(&text)
        .state(State {
            disabled: true,
            ..State::default()
        })
        .attr("tier", "B");
    assert_eq!(
        spec.label,
        Some(Label {
            key: Some(lectern_i18n::keys::common::OK.name()),
            text: text.shared(),
        })
    );
    assert!(spec.state.disabled);
    assert_eq!(spec.attrs, vec![("tier", "B".into())]);
    let verbatim = Text::verbatim(String::from("src/main.rs"));
    assert_eq!(Label::of(&verbatim).key, None);
}

#[gpui::test]
fn semantic_label_of_a_key_names_the_message(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        crate::init_fixed(
            cx,
            lectern_i18n::Strings::of(lectern_i18n::Locale::En),
            crate::Appearance::Light,
            false,
        );
        let text = Text::key(&strings(cx), lectern_i18n::keys::common::OK);
        assert_eq!(Label::of(&text).key, Some("common-ok"));
    });
}
