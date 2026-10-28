//! Tests (docs/design/review_view.md「测试」). The pure ones are plain
//! `#[test]`s; the rest run in GPUI's test platform with a real kernel
//! (`support::Rig`) and `testing::FakeReview` in place of the review service.

mod anchors;
mod brief;
mod fakes;
mod highlight;
mod items;
mod labels;
mod logs;
mod needs;
mod plugin;
mod redraw;
mod screen;
mod scroll;
mod step2;
mod support;
mod tree;
