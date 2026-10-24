//! The registry's payload check over `ui::contrib`'s types
//! (docs/design/workspace.md「`ui::contrib` 的载荷类型」): each
//! `ContributionKind` requires one type; a `SidebarEntry`'s `SidebarItem`
//! becomes the shared `Arc` that `sidebar_entries` hands out.

use std::any::Any;
use std::sync::Arc;

use lectern_services::workspace::{ContributionKind, Payload, SharedPayload};
use lectern_ui::contrib::{
    InspectorTab, KeyBindings, Overlay, Panel, Screen, SidebarItem, SidebarPanel,
};

use crate::registry::Payloads;

pub struct UiPayloads;

impl Payloads for UiPayloads {
    fn accepts(&self, kind: &ContributionKind, payload: &Payload) -> bool {
        let any: &(dyn Any + Send) = &*payload.0;
        match kind {
            ContributionKind::Screen(_) => any.is::<Screen>(),
            ContributionKind::SidebarPanel => any.is::<SidebarPanel>(),
            ContributionKind::SidebarEntry { .. } => any.is::<SidebarItem>(),
            ContributionKind::InspectorTab => any.is::<InspectorTab>(),
            ContributionKind::Panel => any.is::<Panel>(),
            ContributionKind::Overlay => any.is::<Overlay>(),
            ContributionKind::KeyBindings => any.is::<KeyBindings>(),
        }
    }

    fn share(&self, payload: Payload) -> Option<SharedPayload> {
        let item = payload.0.downcast::<SidebarItem>().ok()?;
        let shared: Arc<dyn Any + Send + Sync> = Arc::new(*item);
        Some(SharedPayload(shared))
    }
}

/// A clone of an applied contribution's payload, as its `ui::contrib`
/// type; None when it is not applied or not that type.
pub(crate) fn payload<T: Clone + 'static>(
    registry: &crate::registry::Registry,
    kind: crate::registry::Kind,
    id: &str,
) -> Option<T> {
    registry
        .with_payload(kind, id, |payload| {
            let any: &(dyn Any + Send) = &*payload.0;
            any.downcast_ref::<T>().cloned()
        })
        .flatten()
}
