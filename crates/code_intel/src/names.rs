//! The ⌘K name table (docs/design/code_intel.md「名字表」): the definition
//! names of the current worktree's map, deduplicated (case-sensitive), NFC,
//! in byte order, each with the set of kinds it was seen with. Built in two
//! passes: the current versions' units (one scan of `ci_ext`), filtered by
//! the map, then `ci_def_name` (name, unit, kind) in index order and in
//! pages, keeping the rows of those units, each distinct name once
//! (`Store::def_names`). This crate keeps only a `Weak`; the table
//! belongs to whoever took it from `CodeIntelEvent::Names`. While someone
//! holds it, a table behind its inputs is rebuilt in the background, at most
//! once per `max(1 s, 2 × the last build)`.

use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use lectern_executor::Priority;
use lectern_services::code_intel::{CodeIntelEvent, NameTable, Names};
use lectern_services::project::{BlobId, ProjectKey};
use unicode_normalization::{UnicodeNormalization, is_nfc};

use crate::extract::LanguageTable;
use crate::index::state::Project;
use crate::index::{Inner, lock};
use crate::map::{LiveMap, Provenance};
use crate::store::{Store, Version};

/// Rebuilds while held start at least this long after the last one did…
pub const REBUILD_MIN: Duration = Duration::from_secs(1);
/// …and at least this many times the last build's duration.
pub const REBUILD_FACTOR: u32 = 2;

/// What a table depends on (「输入代数与派生状态」): the map (its
/// provenance), the evictions, the language table and this crate's writes,
/// within one session of the project.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct NameInputs {
    pub session: u64,
    pub map: Provenance,
    pub eviction: u64,
    pub grammar: u64,
    pub writes: u64,
}

/// A project's name table state. Locked after (never with) the project's
/// state.
#[derive(Default)]
pub(crate) struct NameState {
    weak: Weak<NameTable>,
    /// Grows by one whenever the inputs are seen to differ: a table of an
    /// older generation is behind.
    generation: u64,
    inputs: Option<NameInputs>,
    building: bool,
    last_start: Option<Instant>,
    last_took: Duration,
    /// A delayed rebuild is in the watchdog's due table.
    due: bool,
    /// Builds started (tests).
    pub builds: usize,
}

impl NameState {
    /// The generation of `inputs`.
    fn current(&mut self, inputs: NameInputs) -> u64 {
        if self.inputs != Some(inputs) {
            self.inputs = Some(inputs);
            self.generation += 1;
        }
        self.generation
    }
}

/// Builds the name table of `map` from `store`, in the current versions of
/// `table`. Blocking (SQLite); executor tasks only. None when the store
/// cannot be read or `stop` says so.
pub fn build_table(
    store: &Store,
    map: &LiveMap,
    table: &LanguageTable,
    generation: u64,
    stop: &dyn Fn() -> bool,
) -> Option<NameTable> {
    let versions: Vec<Version> = table.iter().map(Version::of).collect();
    let stored = {
        let _t = crate::phases::span("names: units (SQL)");
        store.units(&versions)?
    };
    let mut units: Vec<_> = {
        let _t = crate::phases::span("names: filter by the map");
        stored
            .into_iter()
            .filter(|u| map.has(&u.oid, &u.lang.0))
            .map(|u| u.unit)
            .collect()
    };
    if stop() {
        return None;
    }
    units.sort();
    units.dedup();
    // Each distinct name once (the store's index order), NFC once per
    // name; spellings that NFC makes equal are merged by the builder.
    let mut builder = NameTable::builder(generation);
    let read = {
        let _t = crate::phases::span("names: index scan (SQL)");
        store.def_names(&units, stop, |name, kinds| {
            if is_nfc(name) {
                builder.push(name, kinds);
            } else {
                builder.push(&name.nfc().collect::<String>(), kinds);
            }
        })
    };
    if !read || stop() {
        return None;
    }
    let _t = crate::phases::span("names: build the table");
    Some(builder.build())
}

impl Inner {
    fn name_inputs(&self, project: &Project) -> Option<NameInputs> {
        let eviction = project.evictions();
        let grammar = self.grammar();
        let st = lock(&project.state);
        st.base_for?;
        Some(NameInputs {
            session: st.session,
            map: st.map.provenance,
            eviction,
            grammar,
            writes: st.gens.writes,
        })
    }

    /// `CodeIntel::names`.
    pub(crate) fn names(self: &Arc<Self>, key: &ProjectKey) -> Names {
        let Some(project) = self.project(key) else {
            return Names::Unavailable;
        };
        let Some(inputs) = self.name_inputs(&project) else {
            return Names::Unavailable;
        };
        let now = self.watchdog.clock().now();
        let mut n = lock(&project.names);
        let generation = n.current(inputs);
        if let Some(table) = n.weak.upgrade() {
            let behind = table.generation() < generation;
            drop(n);
            if behind {
                self.names_changed(&project);
            }
            return Names::Ready(table);
        }
        if n.building {
            return Names::Building;
        }
        n.building = true;
        n.last_start = Some(now);
        n.builds += 1;
        drop(n);
        if self.spawn_names(&project) {
            Names::Building
        } else {
            lock(&project.names).building = false;
            Names::Unavailable
        }
    }

    /// An input of the name table may have changed (a batch written, a new
    /// map, evictions, the language packs): a held table behind is rebuilt
    /// now, or once the interval since the last build start has passed.
    pub(crate) fn names_changed(self: &Arc<Self>, project: &Arc<Project>) {
        let Some(inputs) = self.name_inputs(project) else {
            return;
        };
        let now = self.watchdog.clock().now();
        let mut n = lock(&project.names);
        let Some(table) = n.weak.upgrade() else {
            return;
        };
        let generation = n.current(inputs);
        if table.generation() >= generation || n.building || n.due {
            return;
        }
        let gap = REBUILD_MIN.max(n.last_took * REBUILD_FACTOR);
        let earliest = n.last_start.map_or(now, |s| s + gap);
        if now >= earliest {
            n.building = true;
            n.last_start = Some(now);
            n.builds += 1;
            drop(n);
            drop(table);
            if !self.spawn_names(project) {
                lock(&project.names).building = false;
            }
            return;
        }
        n.due = true;
        drop(n);
        drop(table);
        let (me, p) = (Arc::downgrade(self), Arc::downgrade(project));
        let scheduled = self.watchdog.schedule(
            earliest,
            Box::new(move || {
                if let (Some(me), Some(p)) = (me.upgrade(), p.upgrade()) {
                    lock(&p.names).due = false;
                    me.names_changed(&p);
                }
            }),
        );
        if !scheduled {
            lock(&project.names).due = false;
        }
    }

    fn spawn_names(self: &Arc<Self>, project: &Arc<Project>) -> bool {
        let me = self.clone();
        let p = project.clone();
        self.host
            .spawn_busy("names", Priority::Background, move || me.build_names(&p))
    }

    fn build_names(self: &Arc<Self>, project: &Arc<Project>) {
        let clock = self.watchdog.clock().clone();
        let start = clock.now();
        let inputs = self.name_inputs(project);
        let (map, ended) = {
            let st = lock(&project.state);
            (st.map.clone(), st.cancel.clone())
        };
        let table = lock(&self.langs).table.clone();
        let generation = match inputs {
            Some(inputs) => lock(&project.names).current(inputs),
            None => 0,
        };
        let open = self.is_open(project);
        let built = (inputs.is_some() && open)
            .then(|| {
                build_table(&project.store, &map, &table, generation, &|| {
                    ended.load(std::sync::atomic::Ordering::Acquire)
                })
            })
            .flatten()
            .map(Arc::new);
        let took = clock.now().saturating_duration_since(start);
        {
            let mut n = lock(&project.names);
            n.building = false;
            n.last_took = took;
            if let Some(table) = &built {
                n.weak = Arc::downgrade(table);
            }
        }
        if let Some(table) = built {
            self.host.emit(CodeIntelEvent::Names {
                project: project.key.clone(),
                generation: table.generation(),
                table,
            });
        }
        // Still held and already behind: the next one, after the interval.
        self.names_changed(project);
    }

    /// Builds of a project's name table so far (tests).
    #[cfg(test)]
    pub(crate) fn name_builds(&self, key: &ProjectKey) -> usize {
        self.project(key).map_or(0, |p| lock(&p.names).builds)
    }
}

impl LiveMap {
    /// Whether some path of the map has content `oid` in the language named
    /// `lang`. Allocates nothing.
    pub fn has(&self, oid: &BlobId, lang: &str) -> bool {
        let base = self.base.find(oid).any(|i| {
            !self.overlay.hides(i) && self.base.lang(i).is_some_and(|l| &*l.name().0 == lang)
        });
        base || self.overlay.find(oid).any(|i| {
            self.base
                .lang_of_code(self.overlay.lang_code(i))
                .is_some_and(|l| &*l.name().0 == lang)
        })
    }
}
