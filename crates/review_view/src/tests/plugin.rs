//! The plugin (`plugins_register_and_deactivate_cleanly`).

use gpui::TestAppContext;

use crate::tests::support::Rig;

#[gpui::test]
fn plugins_register_and_deactivate_cleanly(cx: &mut TestAppContext) {
    let rig = Rig::inactive(cx);
    assert!(
        rig.contributions.registered().is_empty(),
        "nothing before activation"
    );
    rig.kernel.activate().expect("the plugin activates");
    rig.settle(cx);
    assert_eq!(
        rig.contributions.registered(),
        [
            "screen:review",
            "inspector-tab:files",
            "key-bindings:review"
        ]
    );
    assert_eq!(rig.contributions.released(), 0);
    let keys = rig.contributions.keys();
    assert!(keys.contains(&(
        "j".to_owned(),
        "review::NextHunk".to_owned(),
        Some("Content > review".to_owned())
    )));
    assert!(keys.contains(&(
        "v".to_owned(),
        "review::MarkSeenAndNext".to_owned(),
        Some("Inspector > review-progress".to_owned())
    )));
    // The layout the design gives the screen.
    let _ = rig.contributions.screen();
    let _ = rig.contributions.tab();

    rig.kernel.deactivate();
    rig.settle(cx);
    assert_eq!(rig.contributions.released(), 3, "the registrations drop");
}

#[gpui::test]
fn the_plugin_requires_the_three_services(cx: &mut TestAppContext) {
    use lectern_kernel::Plugin;
    let _ = cx;
    let manifest = crate::plugins(lectern_executor::Executor::manual())
        .0
        .manifest();
    let ids: Vec<&str> = manifest.requires.iter().map(|key| key.id()).collect();
    assert_eq!(ids, ["contributions", "review", "session-source"]);
    assert!(manifest.provides.is_empty());
}
