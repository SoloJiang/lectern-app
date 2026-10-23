//! A kernel running one plugin that does nothing but hand out its
//! `PluginHandle`: the activation a `BriefRules` provider registers with
//! in the tests (`ActivationToken` only comes from a running kernel). The
//! same shape as code_intel's test host.

use std::sync::{Arc, Mutex, PoisonError};

use lectern_kernel::{
    Activation, Active, Context, Kernel, Manifest, Plugin, PluginError, PluginHandle, PluginSet,
};

pub struct TestHost {
    kernel: Arc<Kernel>,
    pub handle: PluginHandle,
}

/// The host plugin's ID.
const HOST: &str = "review-test-host";

impl TestHost {
    /// Deactivates the host plugin: its token is dead from then on.
    pub fn deactivate(&self) {
        self.kernel.set_enabled(HOST, false);
        self.kernel.barrier();
    }
}

impl Drop for TestHost {
    fn drop(&mut self) {
        let (done, wait) = std::sync::mpsc::channel();
        self.kernel.shutdown(move || {
            let _ = done.send(());
        });
        let _ = wait.recv_timeout(std::time::Duration::from_secs(10));
    }
}

type Slot = Arc<Mutex<Option<PluginHandle>>>;

struct HostPlugin(Slot);

struct HostActive(Slot);

impl Active for HostActive {
    fn started(&mut self, handle: PluginHandle) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(handle);
    }
}

impl Plugin for HostPlugin {
    fn manifest(&self) -> Manifest {
        Manifest {
            provides: Vec::new(),
            requires: Vec::new(),
            activation: vec![Activation::AfterFirstFrame],
        }
    }

    fn activate(&self, _ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        Ok(Box::new(HostActive(self.0.clone())))
    }
}

/// Starts the host kernel and activates its plugin.
pub fn host() -> TestHost {
    let slot: Slot = Arc::default();
    let kernel = Kernel::start(PluginSet::new().with(HOST, HostPlugin(slot.clone())));
    kernel.barrier();
    let handle = slot
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
        .expect("started got a handle");
    TestHost { kernel, handle }
}
