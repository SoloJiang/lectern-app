//! Test support (`test-support`): a manual clock, fake language packs, a
//! watchdog on an executor's dedicated thread, and the language fixtures.

use std::any::Any;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use lectern_executor::{Executor, Task};

use crate::LangName;
use crate::grammar::{
    GrammarChange, GrammarChangeKind, GrammarLease, GrammarSource, LoadedGrammar,
};
use crate::watchdog::{Clock, WATCHDOG_THREAD, Watchdog};

/// A clock that moves only when told to.
#[derive(Clone)]
pub struct ManualClock {
    now: Arc<Mutex<Instant>>,
}

impl ManualClock {
    pub fn new() -> ManualClock {
        ManualClock {
            now: Arc::new(Mutex::new(Instant::now())),
        }
    }

    pub fn advance(&self, by: Duration) {
        *self.now.lock().unwrap_or_else(PoisonError::into_inner) += by;
    }
}

impl Default for ManualClock {
    fn default() -> Self {
        ManualClock::new()
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Instant {
        *self.now.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A watchdog whose thread runs on `executor`'s dedicated thread
/// [`WATCHDOG_THREAD`], started on the first registration. The returned slot
/// receives the thread's task handle once it starts; the thread keeps it
/// alive too (dropping a `Task` cancels it), until it exits. As the plugin
/// will, the starter registers the watchdog's `stop` with
/// `Executor::on_shutdown` (the executor does not wait for dedicated
/// threads) and unregisters it when the thread exits.
pub fn watchdog(executor: &Executor, clock: Arc<dyn Clock>) -> (Watchdog, WatchdogTask) {
    let task = WatchdogTask(Arc::new(Mutex::new(None)));
    let slot = task.0.clone();
    let executor = executor.clone();
    let watchdog = Watchdog::new(
        clock,
        Box::new(move |body| {
            let stop = body.watchdog();
            let hook = executor.on_shutdown(Box::new(move || stop.stop()));
            let mut handle = slot.lock().unwrap_or_else(PoisonError::into_inner);
            let keep = slot.clone();
            *handle = Some(executor.run_dedicated(WATCHDOG_THREAD, move || {
                body.run();
                drop(hook);
                drop(keep);
            }));
        }),
    );
    (watchdog, task)
}

thread_local! {
    static DROPS: std::cell::RefCell<Vec<&'static str>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Records its name in this thread's drop log when dropped. Placed right
/// after a field, it marks the moment that field has been dropped (fields
/// drop in declaration order).
#[derive(Debug)]
pub struct DropTrace(pub &'static str);

impl Drop for DropTrace {
    fn drop(&mut self) {
        DROPS.with(|d| d.borrow_mut().push(self.0));
    }
}

/// Takes this thread's drop log.
pub fn take_drops() -> Vec<&'static str> {
    DROPS.with(|d| std::mem::take(&mut *d.borrow_mut()))
}

/// The watchdog thread's task handle, once started.
#[derive(Clone)]
pub struct WatchdogTask(Arc<Mutex<Option<Task<()>>>>);

impl WatchdogTask {
    pub fn started(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
    }

    /// Whether the thread exited. False before it starts.
    pub fn is_finished(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .is_some_and(Task::is_finished)
    }
}

/// Language packs in memory: install, update, roll back and remove them;
/// counts the loads of each pack's "module".
#[derive(Default)]
pub struct FakeGrammars {
    state: Mutex<FakeState>,
    loads: AtomicUsize,
}

#[derive(Default)]
struct FakeState {
    generation: u64,
    packs: HashMap<LangName, (Vec<String>, LoadedGrammar)>,
    subscribers: Vec<Arc<dyn Fn(GrammarChange) + Send + Sync>>,
}

impl FakeGrammars {
    pub fn new() -> FakeGrammars {
        FakeGrammars::default()
    }

    /// Installs (or replaces) a pack claiming `extensions`.
    pub fn install(&self, lang: &str, extensions: &[&str], grammar: LoadedGrammar) {
        let kind = {
            let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if state.packs.contains_key(&crate::lang_name(lang)) {
                GrammarChangeKind::Updated
            } else {
                GrammarChangeKind::Installed
            }
        };
        self.change(lang, kind, |packs| {
            packs.insert(
                crate::lang_name(lang),
                (
                    extensions.iter().map(|e| (*e).to_owned()).collect(),
                    grammar,
                ),
            );
        });
    }

    pub fn remove(&self, lang: &str) {
        self.change(lang, GrammarChangeKind::Removed, |packs| {
            packs.remove(&crate::lang_name(lang));
        });
    }

    /// How many times `grammar` handed out a pack.
    pub fn loads(&self) -> usize {
        self.loads.load(Ordering::Acquire)
    }

    fn change(
        &self,
        lang: &str,
        kind: GrammarChangeKind,
        f: impl FnOnce(&mut HashMap<LangName, (Vec<String>, LoadedGrammar)>),
    ) {
        let (change, subscribers) = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            f(&mut state.packs);
            state.generation += 1;
            let change = GrammarChange {
                generation: state.generation,
                lang: crate::lang_name(lang),
                kind,
            };
            (change, state.subscribers.clone())
        };
        for subscriber in subscribers {
            subscriber(change.clone());
        }
    }
}

impl GrammarSource for FakeGrammars {
    fn grammar(&self, lang: &LangName) -> Option<LoadedGrammar> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let grammar = state.packs.get(lang).map(|(_, g)| g.clone());
        if grammar.is_some() {
            self.loads.fetch_add(1, Ordering::AcqRel);
        }
        grammar
    }

    fn language_for_extension(&self, ext: &str) -> Option<LangName> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let mut claims: Vec<&LangName> = state
            .packs
            .iter()
            .filter(|(_, (exts, _))| exts.iter().any(|e| e == ext))
            .map(|(name, _)| name)
            .collect();
        claims.sort_by(|a, b| a.0.cmp(&b.0));
        claims.first().map(|n| (*n).clone())
    }

    fn generation(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .generation
    }

    fn subscribe(&self, f: Box<dyn Fn(GrammarChange) + Send + Sync>) -> Box<dyn Send> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .subscribers
            .push(Arc::from(f));
        Box::new(())
    }
}

/// A pack grammar built from a core grammar crate, for [`FakeGrammars`]:
/// the Java grammar and queries under another name, in the `jvm` group, with
/// a lease on `module`.
pub fn java_pack(sha: u8, module: Arc<dyn Any + Send + Sync>) -> LoadedGrammar {
    LoadedGrammar {
        lease: GrammarLease::new(module),
        language: tree_sitter_java::LANGUAGE.into(),
        tags_query: Arc::from(include_str!("../queries/java/tags.scm")),
        locals_query: Some(Arc::from(include_str!("../queries/java/locals.scm"))),
        artifact_sha256: [sha; 32],
        compat_group: Some(Arc::from("jvm")),
    }
}

/// The extraction fixtures: (language, file name, content), one or more per
/// core language (`tests/fixtures/extract/<language>/`).
pub const FIXTURES: &[(&str, &str, &str)] = &[
    (
        "ts",
        "shapes.ts",
        include_str!("../tests/fixtures/extract/ts/shapes.ts"),
    ),
    (
        "tsx",
        "panel.tsx",
        include_str!("../tests/fixtures/extract/tsx/panel.tsx"),
    ),
    (
        "js",
        "store.js",
        include_str!("../tests/fixtures/extract/js/store.js"),
    ),
    (
        "js",
        "legacy.cjs",
        include_str!("../tests/fixtures/extract/js/legacy.cjs"),
    ),
    (
        "rust",
        "queue.rs",
        include_str!("../tests/fixtures/extract/rust/queue.rs"),
    ),
    (
        "java",
        "Inventory.java",
        include_str!("../tests/fixtures/extract/java/Inventory.java"),
    ),
    (
        "python",
        "billing.py",
        include_str!("../tests/fixtures/extract/python/billing.py"),
    ),
    (
        "go",
        "units.go",
        include_str!("../tests/fixtures/extract/go/units.go"),
    ),
];

/// About `len` bytes of a language's fixtures, repeated (for throughput).
pub fn corpus(lang: &str, len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len + 4096);
    let samples: Vec<&str> = FIXTURES
        .iter()
        .filter(|(l, _, _)| *l == lang)
        .map(|(_, _, s)| *s)
        .collect();
    assert!(!samples.is_empty(), "no fixture for {lang}");
    while out.len() < len {
        for sample in &samples {
            out.extend_from_slice(sample.as_bytes());
            out.push(b'\n');
        }
    }
    out
}

/// Pathological input for the deadline tests: deep nesting and a very long
/// line, about `len` bytes of JavaScript.
pub fn pathological_js(len: usize) -> Vec<u8> {
    let depth = 2_000;
    let mut out = Vec::with_capacity(len + 2 * depth + 64);
    out.extend_from_slice(b"const x = ");
    out.extend(std::iter::repeat_n(b'[', depth));
    while out.len() < len {
        out.extend_from_slice(b"f(a, b), g[c], {k: [1, 2, (3)]}, ");
    }
    out.extend(std::iter::repeat_n(b']', depth));
    out.extend_from_slice(b";\n");
    out
}
