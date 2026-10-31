//! From GPUI's events to the terminal's `Input` (docs/design/terminal_view.md
//! 「键盘」「鼠标与滚轮」). Pure functions; the pane sends what they return.

use gpui::{Bounds, Keystroke, Modifiers, MouseButton as GpuiButton, Pixels, Point, ScrollDelta};
use lectern_services::terminal::{Key, KeyAction, KeyInput, Mods, MouseButton, ScrollTarget};

/// What a key press means for the terminal.
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum Mapped {
    /// `Input::Key`.
    Key(KeyInput),
    /// `Input::ScrollTo`: ⇧PageUp, ⇧PageDown, ⇧Home, ⇧End.
    ScrollTo(ScrollTarget),
}

pub(crate) fn mods_of(modifiers: &Modifiers) -> Mods {
    Mods {
        shift: modifiers.shift,
        control: modifiers.control,
        option: modifiers.alt,
        command: modifiers.platform,
    }
}

/// The key table of the design. None: the key is not the terminal's to
/// handle here (a ⌘ key bubbles; a printable character, with or without ⇧,
/// and an ⌥ character reach the terminal through the input method).
pub(crate) fn map_key(keystroke: &Keystroke, action: KeyAction) -> Option<Mapped> {
    let modifiers = &keystroke.modifiers;
    if modifiers.platform {
        return None;
    }
    if modifiers.shift && !modifiers.control && !modifiers.alt {
        let target = match keystroke.key.as_str() {
            "pageup" => Some(ScrollTarget::PageUp),
            "pagedown" => Some(ScrollTarget::PageDown),
            "home" => Some(ScrollTarget::Top),
            "end" => Some(ScrollTarget::Bottom),
            _ => None,
        };
        if let Some(target) = target {
            return Some(Mapped::ScrollTo(target));
        }
    }
    let key = named_key(&keystroke.key).or_else(|| {
        // ⌃space and ⌃ with a character. ⌥ with a character is the input
        // method's (⌥e e is é).
        if !modifiers.control {
            return None;
        }
        if keystroke.key == "space" {
            return Some(Key::Space);
        }
        let mut chars = keystroke.key.chars();
        match (chars.next(), chars.next()) {
            (Some(ch), None) => Some(Key::Char(ch)),
            _ => None,
        }
    })?;
    Some(Mapped::Key(KeyInput {
        key,
        mods: mods_of(modifiers),
        action,
        text: None,
    }))
}

fn named_key(name: &str) -> Option<Key> {
    Some(match name {
        "enter" => Key::Enter,
        "tab" => Key::Tab,
        "backspace" => Key::Backspace,
        "escape" => Key::Escape,
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "insert" => Key::Insert,
        "delete" => Key::Delete,
        _ => {
            let number = name.strip_prefix('f')?.parse::<u8>().ok()?;
            if !(1..=20).contains(&number) || name.starts_with("f0") {
                return None;
            }
            Key::F(number)
        }
    })
}

pub(crate) fn button_of(button: GpuiButton) -> Option<MouseButton> {
    match button {
        GpuiButton::Left => Some(MouseButton::Left),
        GpuiButton::Right => Some(MouseButton::Right),
        GpuiButton::Middle => Some(MouseButton::Middle),
        GpuiButton::Navigate(_) => None,
    }
}

/// The cell (column, row) under `position`, counted from 0 and clamped into
/// a grid of `cols` by `rows` cells whose top left corner is the origin of
/// `grid`.
pub(crate) fn cell_at(
    position: Point<Pixels>,
    grid: &Bounds<Pixels>,
    cell: gpui::Size<Pixels>,
    cols: u16,
    rows: u16,
) -> (u16, u16) {
    let index = |offset: Pixels, size: Pixels, count: u16| {
        let at = (offset / size).floor();
        (at.max(0.0) as u32).min(u32::from(count.saturating_sub(1))) as u16
    };
    (
        index(position.x - grid.origin.x, cell.width, cols),
        index(position.y - grid.origin.y, cell.height, rows),
    )
}

/// Which way a drag that left the grid scrolls: -1 above it (earlier lines),
/// 1 below it, 0 while the pointer is within its rows.
pub(crate) fn drag_direction(position: Point<Pixels>, grid: &Bounds<Pixels>) -> i32 {
    if position.y < grid.origin.y {
        -1
    } else if position.y > grid.origin.y + grid.size.height {
        1
    } else {
        0
    }
}

/// Whole lines of a wheel or trackpad event. `rest` carries the part of a
/// line that has not been scrolled yet. GPUI's positive `y` shows earlier
/// content; `ScrollInput::lines` is positive towards newer content, so the
/// sign is turned over here.
pub(crate) fn wheel_lines(rest: &mut f32, delta: &ScrollDelta, line_height: Pixels) -> i32 {
    let lines = match delta {
        ScrollDelta::Lines(lines) => -lines.y,
        ScrollDelta::Pixels(pixels) => -(pixels.y / line_height),
    };
    *rest += lines;
    let whole = rest.trunc();
    *rest -= whole;
    whole as i32
}
