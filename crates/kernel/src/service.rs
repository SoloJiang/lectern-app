//! Service interfaces and their runtime keys.

use std::any::TypeId;
use std::fmt;

/// A service interface, implemented by the `services` crate for each trait
/// object:
///
/// ```
/// # use lectern_kernel::Service;
/// trait SessionSource: Send + Sync {}
/// impl Service for dyn SessionSource {
///     const ID: &'static str = "session-source";
///     const MULTIPLE: bool = true;
/// }
/// ```
///
/// Service methods report failures with `Err` and must not panic: callers hold
/// a trait object and the kernel does not wrap each call in `catch_unwind`.
/// Implementations may be dropped on any thread, so their `Drop` must not
/// panic either.
pub trait Service: Send + Sync + 'static {
    /// Stable string ID used by logs, manifests and the future process plugin
    /// protocol.
    const ID: &'static str;
    /// Whether several implementations may be active at once (session sources,
    /// language navigation). When false a second provider is rejected.
    const MULTIPLE: bool;
}

/// Runtime key of a service: its `TypeId`, stable ID and `MULTIPLE`.
///
/// The associated constants cannot be recovered from a `TypeId`, so
/// [`service`] stores `MULTIPLE` in the key for [`PluginSet::validate`](crate::PluginSet::validate).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ServiceKey {
    type_id: TypeId,
    id: &'static str,
    multiple: bool,
}

impl ServiceKey {
    /// The stable string ID.
    pub fn id(&self) -> &'static str {
        self.id
    }

    /// Whether the service allows several providers.
    pub fn multiple(&self) -> bool {
        self.multiple
    }

    pub(crate) fn type_id(&self) -> TypeId {
        self.type_id
    }
}

impl fmt::Debug for ServiceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id)
    }
}

impl fmt::Display for ServiceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id)
    }
}

/// The key of service `S`.
pub fn service<S: Service + ?Sized>() -> ServiceKey {
    ServiceKey {
        type_id: TypeId::of::<S>(),
        id: S::ID,
        multiple: S::MULTIPLE,
    }
}
