//! Which rows and texts the view asks for (`needs_*`).

use lectern_services::review::Need;

use crate::items::Item;
use crate::needs::{CAP, compute, prompt_key};
use crate::testing::{agent_key, snapshot_fixture};
use crate::tests::items::build;

#[test]
fn needs_cover_the_visible_range_and_one_screen_each_way() {
    let (snapshot, _) = snapshot_fixture(6, 2, 8);
    let items = build(&snapshot, &Default::default(), true, false);
    // Items: brief, turn, prompt, text, (step, placeholder) × 3, tool, turn,
    // prompt, text, (step, placeholder) × 3, tool.
    let needs = compute(&items, &snapshot, &(8..12), CAP);
    let rows = needs
        .iter()
        .filter(|need| matches!(need, Need::Rows(_)))
        .count();
    let texts = needs
        .iter()
        .filter(|need| matches!(need, Need::Text(_)))
        .count();
    // 4 visible, 4 before, 4 after: items 4..16.
    let expected_rows = items[4..16]
        .iter()
        .filter(|item| matches!(item, Item::Placeholder { .. }))
        .count();
    assert_eq!(rows, expected_rows);
    assert!(texts >= 2, "the texts of the turns in the range: {needs:?}");
}

#[test]
fn needs_the_visible_ones_come_first() {
    let (snapshot, _) = snapshot_fixture(6, 1, 8);
    let items = build(&snapshot, &Default::default(), true, false);
    // Visible: the third step's placeholder only.
    let placeholders: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, item)| matches!(item, Item::Placeholder { .. }))
        .map(|(ix, _)| ix)
        .collect();
    let visible = placeholders[2]..placeholders[2] + 1;
    let needs = compute(&items, &snapshot, &visible, CAP);
    let Some(Need::Rows(first)) = needs.first() else {
        panic!("a rows need first: {needs:?}");
    };
    assert_eq!(Some(first), items[placeholders[2]].rows_key());
}

#[test]
fn needs_the_same_key_is_asked_for_once() {
    let (snapshot, _) = snapshot_fixture(2, 1, 40);
    let rows = crate::tests::items::rows_map(snapshot_fixture(2, 1, 40).1);
    let items = build(&snapshot, &rows, true, false);
    let needs = compute(&items, &snapshot, &(0..items.len()), CAP);
    let mut unique = needs.clone();
    unique.dedup();
    assert_eq!(needs.len(), unique.len());
    let step_rows = needs
        .iter()
        .filter(|need| matches!(need, Need::Rows(_)))
        .count();
    assert_eq!(step_rows, 2, "one key per step, however many rows it has");
}

#[test]
fn needs_are_capped() {
    let (snapshot, _) = snapshot_fixture(40, 1, 4);
    let items = build(&snapshot, &Default::default(), true, false);
    let needs = compute(&items, &snapshot, &(0..items.len()), 10);
    assert_eq!(needs.len(), 10);
}

#[test]
fn needs_name_the_prompt_and_the_agent_text_of_a_turn() {
    let (snapshot, _) = snapshot_fixture(2, 1, 4);
    let items = build(&snapshot, &Default::default(), true, false);
    let needs = compute(&items, &snapshot, &(0..items.len()), CAP);
    assert!(needs.contains(&Need::Text(prompt_key(&snapshot, 0).unwrap())));
    assert!(needs.contains(&Need::Text(agent_key(0))));
}

#[test]
fn needs_an_empty_list_asks_for_nothing() {
    let (snapshot, _) = snapshot_fixture(1, 1, 4);
    assert!(compute(&[], &snapshot, &(0..0), CAP).is_empty());
}
