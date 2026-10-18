//! `FakeProjects` (an in-memory `dyn Projects`), `FakePower`, and a kernel
//! with one plugin whose `PluginHandle` the tests use.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use lectern_kernel::testing::TestKernel;
use lectern_kernel::{Activation, Active, Context, Kernel, Manifest, Plugin, PluginError, PluginHandle};
use lectern_services::project::{
    FileList, OpenError, OpenRequest, Package, PackageMatch, ProjectEvent, ProjectKey,
    ProjectSnapshot, Projects, ResolveError, Resolved, Scan, ScanError, WorktreeKey,
};

use crate::power::PowerSource;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Open projects' snapshots in memory. Replacing a part keeps the other
/// parts' `Arc`s, as `project` does.
#[derive(Default)]
pub struct FakeProjects {
    snapshots: Mutex<Vec<Arc<ProjectSnapshot>>>,
}

impl FakeProjects {
    pub fn new() -> Arc<FakeProjects> {
        Arc::new(FakeProjects::default())
    }

    /// Sets (or replaces) a project's snapshot and returns the event
    /// `project` would emit.
    pub fn set(&self, snapshot: ProjectSnapshot) -> ProjectEvent {
        let key = snapshot.key.clone();
        let generation = snapshot.generation;
        let mut all = lock(&self.snapshots);
        let opened = !all.iter().any(|s| s.key == key);
        all.retain(|s| s.key != key);
        all.push(Arc::new(snapshot));
        if opened {
            ProjectEvent::Opened {
                request: OpenRequest(0),
                result: Ok(key),
            }
        } else {
            ProjectEvent::Changed {
                project: key,
                generation,
                parts: lectern_services::project::ChangedParts {
                    worktrees: true,
                    active: true,
                    files: true,
                    packages: true,
                },
            }
        }
    }

    fn update(&self, key: &ProjectKey, f: impl FnOnce(&mut ProjectSnapshot)) -> u64 {
        let mut all = lock(&self.snapshots);
        let Some(at) = all.iter().position(|s| &s.key == key) else {
            return 0;
        };
        let mut snapshot = (*all[at]).clone();
        snapshot.generation += 1;
        f(&mut snapshot);
        let generation = snapshot.generation;
        all[at] = Arc::new(snapshot);
        generation
    }

    /// Replaces the active tree's file list only.
    pub fn set_files(&self, key: &ProjectKey, files: Arc<FileList>) -> ProjectEvent {
        let generation = self.update(key, |s| {
            if let Some(a) = &mut s.active {
                a.files = Scan::Ready(files);
            }
        });
        changed(key, generation, |p| p.files = true)
    }

    /// Replaces the active tree's sub-projects only.
    pub fn set_packages(&self, key: &ProjectKey, packages: Vec<Package>) -> ProjectEvent {
        let generation = self.update(key, |s| {
            if let Some(a) = &mut s.active {
                a.packages = Scan::Ready(packages.into());
            }
        });
        changed(key, generation, |p| p.packages = true)
    }

    /// A `Changed` for the worktree list (a HEAD moved) without changing
    /// the snapshot's parts.
    pub fn touch_worktrees(&self, key: &ProjectKey) -> ProjectEvent {
        let generation = self.update(key, |_| {});
        changed(key, generation, |p| p.worktrees = true)
    }

    pub fn close(&self, key: &ProjectKey) -> ProjectEvent {
        lock(&self.snapshots).retain(|s| &s.key != key);
        ProjectEvent::Closed {
            project: key.clone(),
        }
    }
}

fn changed(
    key: &ProjectKey,
    generation: u64,
    f: impl FnOnce(&mut lectern_services::project::ChangedParts),
) -> ProjectEvent {
    let mut parts = lectern_services::project::ChangedParts::default();
    f(&mut parts);
    ProjectEvent::Changed {
        project: key.clone(),
        generation,
        parts,
    }
}

impl Projects for FakeProjects {
    fn open(&self, _path: &Path) -> OpenRequest {
        OpenRequest(0)
    }

    fn close(&self, key: &ProjectKey) {
        lock(&self.snapshots).retain(|s| &s.key != key);
    }

    fn set_active_worktree(&self, _project: &ProjectKey, _worktree: &WorktreeKey) {}

    fn snapshot(&self, key: &ProjectKey) -> Option<Arc<ProjectSnapshot>> {
        lock(&self.snapshots).iter().find(|s| &s.key == key).cloned()
    }

    fn open_projects(&self) -> Vec<ProjectKey> {
        lock(&self.snapshots).iter().map(|s| s.key.clone()).collect()
    }

    fn locate(&self, _path: &Path) -> Option<lectern_services::project::Location> {
        None
    }

    fn resolve(&self, _path: &Path) -> Result<Resolved, ResolveError> {
        Err(OpenError::NotFound)
    }

    fn resolve_package(
        &self,
        _project: &ProjectKey,
        _worktree: &WorktreeKey,
        _rel: &str,
    ) -> Result<PackageMatch, ScanError> {
        Err(ScanError::RootMissing)
    }
}

/// A power source the test switches.
#[derive(Default)]
pub struct FakePower {
    battery: AtomicBool,
    subscribers: Mutex<Vec<Arc<dyn Fn(bool) + Send + Sync>>>,
}

impl FakePower {
    pub fn new(on_battery: bool) -> Arc<FakePower> {
        let power = FakePower::default();
        power.battery.store(on_battery, Ordering::Release);
        Arc::new(power)
    }

    /// Switches the source and notifies the subscribers.
    pub fn set(&self, on_battery: bool) {
        self.battery.store(on_battery, Ordering::Release);
        let subscribers = lock(&self.subscribers).clone();
        for f in subscribers {
            f(on_battery);
        }
    }

    pub fn subscribers(&self) -> usize {
        lock(&self.subscribers).len()
    }
}

impl PowerSource for FakePower {
    fn on_battery(&self) -> bool {
        self.battery.load(Ordering::Acquire)
    }

    fn subscribe(&self, f: Box<dyn Fn(bool) + Send + Sync>) -> Box<dyn Send> {
        lock(&self.subscribers).push(Arc::from(f));
        Box::new(())
    }
}

/// A kernel running one plugin that does nothing but hand out its
/// `PluginHandle`: the activation the indexer's tasks belong to.
pub struct TestHost {
    pub kernel: TestKernel,
    pub handle: PluginHandle,
}

impl TestHost {
    pub fn kernel(&self) -> Arc<Kernel> {
        self.kernel.inner().expect("a running kernel")
    }
}

struct HostPlugin(Arc<Mutex<Option<PluginHandle>>>);

struct HostActive(Arc<Mutex<Option<PluginHandle>>>);

impl Active for HostActive {
    fn started(&mut self, handle: PluginHandle) {
        *lock(&self.0) = Some(handle);
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
    let slot = Arc::new(Mutex::new(None));
    let kernel = TestKernel::new(HostPlugin(slot.clone()));
    kernel.activate().expect("the host plugin activates");
    let handle = lock(&slot).clone().expect("started got a handle");
    TestHost { kernel, handle }
}
