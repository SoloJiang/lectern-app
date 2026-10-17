//! `kernel/*` (docs/design/kernel.md「性能」): service lookup and event
//! dispatch on a real `Kernel` with test plugins. Record only.

use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lectern_bench::{Bench, BenchError, Ctx, Measured, Value};
use lectern_kernel::{
    Activation, Active, Context, Event, Kernel, Manifest, Plugin, PluginError, PluginSet, Service,
    ServiceRef, ServiceSet, service,
};

/// A single-provider service.
trait Probe: Send + Sync {
    fn value(&self) -> u64;
}

impl Service for dyn Probe {
    const ID: &'static str = "bench.probe";
    const MULTIPLE: bool = false;
}

/// A multi-provider service.
trait Many: Send + Sync {
    fn value(&self) -> u64;
}

impl Service for dyn Many {
    const ID: &'static str = "bench.many";
    const MULTIPLE: bool = true;
}

struct Constant(u64);

impl Probe for Constant {
    fn value(&self) -> u64 {
        self.0
    }
}

impl Many for Constant {
    fn value(&self) -> u64 {
        self.0
    }
}

/// Provides `dyn Probe` (`multiple: false`) or `dyn Many`.
struct Provider {
    multiple: bool,
    value: u64,
}

impl Plugin for Provider {
    fn manifest(&self) -> Manifest {
        Manifest {
            provides: vec![if self.multiple {
                service::<dyn Many>()
            } else {
                service::<dyn Probe>()
            }],
            requires: Vec::new(),
            activation: vec![Activation::AfterFirstFrame],
        }
    }

    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        if self.multiple {
            ctx.provide::<dyn Many>(Arc::new(Constant(self.value)))?;
        } else {
            ctx.provide::<dyn Probe>(Arc::new(Constant(self.value)))?;
        }
        Ok(Box::new(()))
    }
}

type Slot<T> = Arc<Mutex<Option<T>>>;

/// Requires the service and hands what `activate` takes out of the
/// context to the benchmark, as a plugin keeps it in its activation state.
struct Consumer<T: Send + 'static> {
    requires: lectern_kernel::ServiceKey,
    take: fn(&Context<'_>) -> T,
    slot: Slot<T>,
}

impl<T: Send + 'static> Plugin for Consumer<T> {
    fn manifest(&self) -> Manifest {
        Manifest {
            provides: Vec::new(),
            requires: vec![self.requires],
            activation: vec![Activation::AfterFirstFrame],
        }
    }

    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        *self.slot.lock().unwrap() = Some((self.take)(ctx));
        Ok(Box::new(()))
    }
}

/// A started kernel, shut down on drop.
struct Running(Arc<Kernel>);

impl Drop for Running {
    fn drop(&mut self) {
        let (done, wait) = std::sync::mpsc::channel();
        self.0.shutdown(move || {
            let _ = done.send(());
        });
        let _ = wait.recv_timeout(Duration::from_secs(10));
    }
}

/// Starts `set` and waits until `ready` holds (10 s at most).
fn start(set: PluginSet, ready: impl Fn() -> bool) -> Result<Running, BenchError> {
    let kernel = Running(Kernel::start(set));
    let started = Instant::now();
    loop {
        kernel.0.barrier();
        if ready() {
            return Ok(kernel);
        }
        if started.elapsed() > Duration::from_secs(10) {
            return Err(BenchError::new("the test plugins did not activate"));
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn filled<T>(slot: &Slot<T>) -> bool {
    slot.lock().unwrap().is_some()
}

fn take<T>(slot: &Slot<T>) -> T {
    slot.lock().unwrap().take().expect("checked by start")
}

/// `ServiceRef::get()` and one call: an admission (two atomic operations).
pub const SERVICE_GET: Bench = Bench {
    id: "kernel/service_get",
    run: service_get,
};

fn service_get(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let slot: Slot<ServiceRef<dyn Probe>> = Arc::default();
    let set = PluginSet::new()
        .with(
            "provider",
            Provider {
                multiple: false,
                value: 7,
            },
        )
        .with(
            "consumer",
            Consumer {
                requires: service::<dyn Probe>(),
                take: |ctx| ctx.service::<dyn Probe>(),
                slot: slot.clone(),
            },
        );
    let _kernel = start(set, || filled(&slot))?;
    let probe = take(&slot);
    let time = ctx.time(|| black_box(&probe).get().map(|s| s.value()));
    Ok(vec![Measured::new("kernel/service_get", Value::Time(time))])
}

/// A kernel with `providers` providers of `dyn Many` and the consumer's set.
fn many(providers: usize) -> Result<(Running, ServiceSet<dyn Many>), BenchError> {
    let slot: Slot<ServiceSet<dyn Many>> = Arc::default();
    let mut set = PluginSet::new();
    for i in 0..providers {
        set = set.with(
            format!("provider-{i}"),
            Provider {
                multiple: true,
                value: i as u64,
            },
        );
    }
    set = set.with(
        "consumer",
        Consumer {
            requires: service::<dyn Many>(),
            take: |ctx| ctx.service_set::<dyn Many>(),
            slot: slot.clone(),
        },
    );
    // The set is taken in `activate`; wait until every provider is in it.
    let check = slot.clone();
    let kernel = start(set, move || {
        check
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|s| s.current().len() == providers)
    })?;
    Ok((kernel, take(&slot)))
}

const PROVIDERS: [usize; 3] = [1, 4, 16];

/// `ServiceSet::for_each` over 1, 4 and 16 providers: one admission each,
/// no allocation.
pub const SERVICE_FOR_EACH: Bench = Bench {
    id: "kernel/service_for_each",
    run: service_for_each,
};

fn service_for_each(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let mut out = Vec::new();
    for n in PROVIDERS {
        let (_kernel, set) = many(n)?;
        let time = ctx.time(|| {
            let mut sum = 0;
            black_box(&set).for_each(|s| sum += s.value());
            sum
        });
        out.push(Measured::new(
            format!("kernel/service_for_each/{n}"),
            Value::Time(time),
        ));
    }
    Ok(out)
}

/// `ServiceSet::current()` over 1, 4 and 16 providers: `for_each` plus a
/// `Vec`.
pub const SERVICE_CURRENT: Bench = Bench {
    id: "kernel/service_current",
    run: service_current,
};

fn service_current(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let mut out = Vec::new();
    for n in PROVIDERS {
        let (_kernel, set) = many(n)?;
        let time = ctx.time(|| {
            black_box(&set)
                .current()
                .iter()
                .map(|s| s.value())
                .sum::<u64>()
        });
        out.push(Measured::new(
            format!("kernel/service_current/{n}"),
            Value::Time(time),
        ));
    }
    Ok(out)
}

struct Ping(u64);
impl Event for Ping {}

struct Outer(u64);
impl Event for Outer {}

/// Subscribes to `Ping` (adding to `seen`) and to `Outer`, whose listener
/// emits a `Ping` from inside the dispatch.
struct Listener {
    seen: Arc<AtomicU64>,
}

impl Plugin for Listener {
    fn manifest(&self) -> Manifest {
        Manifest {
            provides: Vec::new(),
            requires: Vec::new(),
            activation: vec![Activation::AfterFirstFrame],
        }
    }

    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        let seen = self.seen.clone();
        ctx.on::<Ping>(move |ping, _| {
            seen.fetch_add(ping.0, Ordering::Relaxed);
        });
        ctx.on::<Outer>(|outer, handle| handle.emitter().emit(Ping(outer.0)));
        Ok(Box::new(()))
    }
}

/// A kernel whose listener is live: emitting `Ping(1)` reaches it.
fn listening() -> Result<(Running, Arc<AtomicU64>), BenchError> {
    let seen = Arc::new(AtomicU64::new(0));
    let set = PluginSet::new().with("listener", Listener { seen: seen.clone() });
    let kernel = Running(Kernel::start(set));
    let started = Instant::now();
    while seen.load(Ordering::Relaxed) == 0 {
        if started.elapsed() > Duration::from_secs(10) {
            return Err(BenchError::new("the listener did not go live"));
        }
        kernel.0.barrier();
        kernel.0.emitter().emit(Ping(1));
    }
    Ok((kernel, seen))
}

/// One event to one listener, from the app's emitter: no allocation.
pub const EMIT: Bench = Bench {
    id: "kernel/emit",
    run: emit,
};

fn emit(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let (kernel, seen) = listening()?;
    let emitter = kernel.0.emitter();
    let before = seen.load(Ordering::Relaxed);
    let time = ctx.time(|| emitter.emit(Ping(black_box(1))));
    if seen.load(Ordering::Relaxed) == before {
        return Err(BenchError::new("no event reached the listener"));
    }
    Ok(vec![Measured::new("kernel/emit", Value::Time(time))])
}

/// One event whose listener emits another: the nested event is boxed and
/// queued until the outer dispatch ends.
pub const EMIT_NESTED: Bench = Bench {
    id: "kernel/emit_nested",
    run: emit_nested,
};

fn emit_nested(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let (kernel, seen) = listening()?;
    let emitter = kernel.0.emitter();
    let before = seen.load(Ordering::Relaxed);
    let time = ctx.time(|| emitter.emit(Outer(black_box(1))));
    if seen.load(Ordering::Relaxed) == before {
        return Err(BenchError::new("no nested event reached the listener"));
    }
    Ok(vec![Measured::new("kernel/emit_nested", Value::Time(time))])
}
