//! The screen's actions and key bindings (docs/design/welcome.md「Action 与
//! 快捷键」). Every action has a stable name the probe can trigger with
//! `App::build_action`. The bindings travel as a `KeyBindings` contribution;
//! `workspace` loads them into the keymap when the contribution is applied.
//! ⌘O is not here: it is `workspace::OpenFolder`, bound globally by
//! `workspace`.

use gpui::{Action, actions, register_action};
use lectern_i18n::Locale;
use lectern_ui::contrib::KeyBindings;

actions!(
    welcome,
    [
        /// ↓: the next item of the two lists taken as one sequence; the first
        /// item when nothing is selected; no change at the last item.
        SelectNext,
        /// ↑: the previous item; no change at the first item; the last item
        /// when nothing is selected.
        SelectPrevious,
        /// ↵: open the selected item.
        OpenSelected,
        /// ⌫: remove the selected recent project (a discovered folder is
        /// left alone).
        RemoveSelected,
    ]
);

/// A click on the language picker. `None` follows the system.
///
/// Written by hand, not derived: the derive builds an action with data from
/// `Deserialize` and `JsonSchema`, which `Locale` does not have. The probe
/// builds this one from `{"locale": "en"}`, `{"locale": "zh-CN"}` or
/// `{"locale": null}` (`{}` follows the system as well).
#[derive(Clone, PartialEq, Debug)]
pub struct SelectLanguage {
    pub locale: Option<Locale>,
}

impl Action for SelectLanguage {
    fn boxed_clone(&self) -> Box<dyn Action> {
        Box::new(self.clone())
    }

    fn partial_eq(&self, action: &dyn Action) -> bool {
        action
            .as_any()
            .downcast_ref::<Self>()
            .is_some_and(|other| self == other)
    }

    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn name_for_type() -> &'static str {
        Self::NAME
    }

    fn build(value: serde_json::Value) -> gpui::Result<Box<dyn Action>> {
        let locale = match value.get("locale") {
            None | Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::String(tag)) => Some(
                Locale::ALL
                    .into_iter()
                    .find(|locale| locale.tag().eq_ignore_ascii_case(tag))
                    .ok_or_else(|| gpui::private::anyhow::anyhow!("unknown locale"))?,
            ),
            Some(_) => {
                return Err(gpui::private::anyhow::anyhow!("locale is a tag or null"));
            }
        };
        Ok(Box::new(SelectLanguage { locale }))
    }
}

impl SelectLanguage {
    const NAME: &'static str = "welcome::SelectLanguage";
}

register_action!(SelectLanguage);

/// The `KeyContext` the view's root element sets.
pub(crate) const CONTEXT: &str = "welcome";
/// The predicate of the bindings: below `workspace`'s `Content` region.
const PREDICATE: &str = "Content > welcome";

/// The keystrokes are GPUI key names, not interface text.
pub(crate) fn key_bindings() -> KeyBindings {
    KeyBindings(vec![
        ("down", Box::new(SelectNext), Some(PREDICATE)),
        ("up", Box::new(SelectPrevious), Some(PREDICATE)),
        ("enter", Box::new(OpenSelected), Some(PREDICATE)),
        ("backspace", Box::new(RemoveSelected), Some(PREDICATE)),
    ])
}
