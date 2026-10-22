//! Filters (docs/design/logging.md「过滤」) and the process-wide active one.
//!
//! ```text
//! filter    := directive ("," directive)*
//! directive := level | target "=" level | "@" plugin "=" level
//! ```

use std::fmt;
#[cfg(any(feature = "sink", feature = "test-support"))]
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};

use log::{Level, LevelFilter};

/// A parsed filter. `Display` writes it back in canonical form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filter {
    /// A bare level: Lectern's targets only. None keeps them at info.
    base: Option<LevelFilter>,
    /// `target=level`, by crate short name; later duplicates replace earlier ones.
    targets: Vec<(Box<str>, LevelFilter)>,
    /// `@plugin=level`.
    plugins: Vec<(Box<str>, LevelFilter)>,
    /// Third-party targets not named: warn. Only the test capture raises it.
    third_party: LevelFilter,
}

impl Default for Filter {
    /// The default filter: Lectern's targets at info, third-party at warn.
    fn default() -> Filter {
        Filter {
            base: Some(LevelFilter::Info),
            targets: Vec::new(),
            plugins: Vec::new(),
            third_party: LevelFilter::Warn,
        }
    }
}

impl Filter {
    /// Parses `s`. Directives it cannot read are skipped and their indices
    /// (in the comma-separated list) returned; empty ones are ignored.
    pub fn parse(s: &str) -> (Filter, Vec<usize>) {
        let mut filter = Filter {
            base: None,
            ..Filter::default()
        };
        let mut skipped = Vec::new();
        for (i, raw) in s.split(',').enumerate() {
            let d = raw.trim();
            if d.is_empty() {
                continue;
            }
            let ok = if let Some(rest) = d.strip_prefix('@') {
                rest.split_once('=')
                    .and_then(|(name, level)| Some((plugin_name(name)?, parse_level(level)?)))
                    .map(|(name, level)| upsert(&mut filter.plugins, name, level))
                    .is_some()
            } else if let Some((name, level)) = d.split_once('=') {
                target_name(name)
                    .zip(parse_level(level))
                    .map(|(name, level)| upsert(&mut filter.targets, name, level))
                    .is_some()
            } else {
                parse_level(d)
                    .map(|level| filter.base = Some(level))
                    .is_some()
            };
            if !ok {
                skipped.push(i);
            }
        }
        (filter, skipped)
    }

    /// Everything at trace, third-party targets included (`testing::capture`).
    #[cfg(feature = "test-support")]
    pub(crate) fn everything() -> Filter {
        Filter {
            base: Some(LevelFilter::Trace),
            targets: Vec::new(),
            plugins: Vec::new(),
            third_party: LevelFilter::Trace,
        }
    }

    /// The level for a Lectern target (a crate's short name).
    pub(crate) fn lectern(&self, target: &str) -> LevelFilter {
        self.target(target)
            .unwrap_or(self.base.unwrap_or(LevelFilter::Info))
    }

    /// The level for a third-party target (first module path segment).
    #[cfg(any(feature = "sink", feature = "test-support"))]
    pub(crate) fn third_party(&self, target: &str) -> LevelFilter {
        self.target(target).unwrap_or(self.third_party)
    }

    fn target(&self, target: &str) -> Option<LevelFilter> {
        self.targets
            .iter()
            .find(|(t, _)| **t == *target)
            .map(|(_, l)| *l)
    }

    pub(crate) fn plugin(&self, id: &str) -> Option<LevelFilter> {
        self.plugins
            .iter()
            .find(|(p, _)| **p == *id)
            .map(|(_, l)| *l)
    }

    /// The most detailed level any record can pass: `log::max_level()`.
    #[cfg(any(feature = "sink", feature = "test-support"))]
    pub(crate) fn max(&self) -> LevelFilter {
        let mut max = self.base.unwrap_or(LevelFilter::Info).max(self.third_party);
        for (_, l) in self.targets.iter().chain(&self.plugins) {
            max = max.max(*l);
        }
        max
    }
}

impl fmt::Display for Filter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut sep = "";
        if let Some(base) = self.base {
            write!(f, "{}", level_name(base))?;
            sep = ",";
        }
        for (t, l) in &self.targets {
            write!(f, "{sep}{t}={}", level_name(*l))?;
            sep = ",";
        }
        for (p, l) in &self.plugins {
            write!(f, "{sep}@{p}={}", level_name(*l))?;
            sep = ",";
        }
        Ok(())
    }
}

fn upsert(list: &mut Vec<(Box<str>, LevelFilter)>, name: String, level: LevelFilter) {
    match list.iter_mut().find(|(n, _)| **n == *name) {
        Some(entry) => entry.1 = level,
        None => list.push((name.into_boxed_str(), level)),
    }
}

fn parse_level(s: &str) -> Option<LevelFilter> {
    let s = s.trim();
    [
        LevelFilter::Off,
        LevelFilter::Error,
        LevelFilter::Warn,
        LevelFilter::Info,
        LevelFilter::Debug,
        LevelFilter::Trace,
    ]
    .into_iter()
    .find(|l| level_name(*l).eq_ignore_ascii_case(s))
}

pub(crate) fn level_name(level: LevelFilter) -> &'static str {
    match level {
        LevelFilter::Off => "off",
        LevelFilter::Error => "error",
        LevelFilter::Warn => "warn",
        LevelFilter::Info => "info",
        LevelFilter::Debug => "debug",
        LevelFilter::Trace => "trace",
    }
}

/// A crate short name: `[a-z0-9_]`, `-` read as `_`, at most 64 bytes.
fn target_name(s: &str) -> Option<String> {
    let s = s.trim().to_ascii_lowercase().replace('-', "_");
    (!s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'))
    .then_some(s)
}

/// A plugin ID: `[a-z0-9._-]`, at most 64 bytes.
fn plugin_name(s: &str) -> Option<String> {
    let s = s.trim().to_ascii_lowercase();
    (!s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b)))
    .then_some(s)
}

/// The active filter with its generation; call sites cache their level per
/// generation.
pub(crate) struct Active {
    pub(crate) filter: Filter,
    pub(crate) generation: u64,
}

static ACTIVE: AtomicPtr<Active> = AtomicPtr::new(std::ptr::null_mut());
/// The generation of [`ACTIVE`]; 0 before any filter.
pub(crate) static GENERATION: AtomicU64 = AtomicU64::new(0);
/// Whether the active filter has `@plugin` directives (the scope is read).
pub(crate) static PLUGINS: AtomicBool = AtomicBool::new(false);
#[cfg(any(feature = "sink", feature = "test-support"))]
static SWAP: Mutex<()> = Mutex::new(());

pub(crate) fn active() -> Option<&'static Active> {
    // SAFETY: ACTIVE is null or a pointer from `Box::leak` in `activate`,
    // never freed, so the reference is valid for 'static.
    unsafe { ACTIVE.load(Ordering::Acquire).as_ref() }
}

/// Makes `filter` the active one: call sites recompute their level on their
/// next call. Earlier filters are never freed, so readers take no lock; a
/// filter changes on user actions only (startup, the verbose switch and its
/// expiry), a few hundred bytes each time.
#[cfg(any(feature = "sink", feature = "test-support"))]
pub(crate) fn activate(filter: Filter) {
    let _guard = SWAP.lock().unwrap_or_else(|e| e.into_inner());
    let generation = GENERATION.load(Ordering::Acquire) + 1;
    let max = filter.max();
    PLUGINS.store(!filter.plugins.is_empty(), Ordering::Release);
    let active = Box::leak(Box::new(Active { filter, generation }));
    ACTIVE.store(active, Ordering::Release);
    GENERATION.store(generation, Ordering::Release);
    log::set_max_level(max);
}

/// No filter: nothing passes (tests reinstalling the logger).
#[cfg(all(test, feature = "sink"))]
pub(crate) fn deactivate() {
    let _guard = SWAP.lock().unwrap_or_else(|e| e.into_inner());
    log::set_max_level(LevelFilter::Off);
    PLUGINS.store(false, Ordering::Release);
    ACTIVE.store(std::ptr::null_mut(), Ordering::Release);
    GENERATION.fetch_add(1, Ordering::AcqRel);
}

/// Whether an `@plugin` directive lets `level` through for the plugin of
/// the current scope.
pub(crate) fn plugin_allows(level: Level) -> bool {
    let Some(plugin) = crate::scope::current().plugin else {
        return false;
    };
    active()
        .and_then(|a| a.filter.plugin(plugin.as_str()))
        .is_some_and(|l| level <= l)
}

#[cfg(all(test, feature = "sink"))]
pub(crate) fn snapshot_of(s: &str) -> String {
    let (f, skipped) = Filter::parse(s);
    format!(
        "{s:?} -> {:?} skipped {skipped:?} max {}",
        f.to_string(),
        level_name(f.max())
    )
}
