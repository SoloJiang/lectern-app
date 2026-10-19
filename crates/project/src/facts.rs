//! `WorkspaceFacts` over the active trees of every open project
//! (docs/design/project.md「WorkspaceFacts」).

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};

use glob::{MatchOptions, Pattern};

use lectern_kernel::WorkspaceFacts;
use lectern_services::project::{FileKind, FileList};

#[cfg(test)]
mod tests;

/// The facts of the union of `trees`. Builds the file name and extension
/// indexes here, so call it on the executor.
pub fn workspace_facts(trees: Vec<Arc<FileList>>) -> Arc<dyn WorkspaceFacts> {
    Arc::new(ProjectFacts::new(trees))
}

/// The result cache's initial capacity.
const CACHED_PATTERNS: usize = 32;

struct ProjectFacts {
    trees: Vec<Arc<FileList>>,
    /// The last segment of every file (submodules excluded), each distinct
    /// name once.
    names: Names,
    /// What follows the last `.` of every file name.
    exts: HashSet<Box<str>>,
    /// Results by pattern; one set of facts never computes a pattern twice.
    cache: Mutex<Results>,
}

impl ProjectFacts {
    fn new(trees: Vec<Arc<FileList>>) -> ProjectFacts {
        let names = Names::build(&trees);
        let mut exts = HashSet::new();
        for tree in &trees {
            for (path, _) in files(tree) {
                if let Some((_, ext)) = file_name(path).rsplit_once('.')
                    && !exts.contains(ext)
                {
                    exts.insert(ext.into());
                }
            }
        }
        ProjectFacts {
            trees,
            names,
            exts,
            // Sized here, on the executor, for the patterns the kernel asks
            // right after the submission (one per `Workspace` plugin).
            cache: Mutex::new(Results::new()),
        }
    }

    fn compute(&self, pattern: &str) -> bool {
        // `**/x` is `x` when `x` is a file name: both match at any depth.
        let mut rest = pattern;
        while let Some(stripped) = rest.strip_prefix("**/") {
            rest = stripped;
        }
        if rest.is_empty() {
            return false;
        }
        if rest.contains('/') {
            // From the worktree root.
            let Ok(compiled) = Pattern::new(pattern) else {
                return false;
            };
            return self
                .trees
                .iter()
                .flat_map(|t| files(t))
                .any(|(path, _)| compiled.matches_with(path, OPTIONS));
        }
        // A file name, or `*.ext`, is one hash lookup. Neither can be an
        // invalid glob, so nothing is compiled for them.
        if !has_meta(rest) {
            return self.names.contains(rest);
        }
        if let Some(ext) = rest.strip_prefix("*.")
            && !has_meta(ext)
            && !ext.contains('.')
        {
            return self.exts.contains(ext);
        }
        let Ok(compiled) = Pattern::new(rest) else {
            return false;
        };
        self.names
            .iter()
            .any(|name| compiled.matches_with(name, OPTIONS))
    }
}

/// The distinct file names of some file lists, each stored once: the
/// names end to end in one buffer, and an open-addressing table (linear
/// probing, at most 3/4 full) of 64-bit slots, each a 16-bit hash tag, the
/// name's length and its offset in the buffer (0 is an empty slot; a name
/// is never empty). A lookup reads the slots, then the buffer only where
/// the tag matches: two memory reads for a hit, as many as a
/// `HashSet<Box<str>>` has, without its pointer, length and allocation
/// header for every name. Names that don't fit a slot (64 KiB or longer,
/// or past 4 GiB of names) go to `long`.
struct Names {
    slots: Box<[u64]>,
    len: usize,
    buf: String,
    long: HashSet<Box<str>>,
}

const TAG_SHIFT: u32 = 48;
const LEN_SHIFT: u32 = 32;

impl Names {
    fn build(trees: &[Arc<FileList>]) -> Names {
        let total: usize = trees.iter().map(|t| t.len()).sum();
        let mut names = Names {
            slots: vec![0; total.next_power_of_two().max(8)].into_boxed_slice(),
            len: 0,
            buf: String::new(),
            long: HashSet::new(),
        };
        for tree in trees {
            for (path, _) in files(tree) {
                names.insert(file_name(path));
            }
        }
        names.buf.shrink_to_fit();
        names
    }

    fn mask(&self) -> usize {
        self.slots.len() - 1
    }

    /// The slot holding `name`, or the empty slot where it would go.
    fn find(&self, hash: u64, name: &str) -> (usize, bool) {
        let tag = hash >> TAG_SHIFT;
        let mut i = hash as usize & self.mask();
        loop {
            let slot = self.slots[i];
            if slot == 0 {
                return (i, false);
            }
            if slot >> TAG_SHIFT == tag && self.name(slot) == name {
                return (i, true);
            }
            i = (i + 1) & self.mask();
        }
    }

    fn insert(&mut self, name: &str) {
        let (Ok(len), Ok(offset)) = (u16::try_from(name.len()), u32::try_from(self.buf.len()))
        else {
            if !self.long.contains(name) {
                self.long.insert(name.into());
            }
            return;
        };
        let hash = fnv1a(name);
        let (i, found) = self.find(hash, name);
        if found {
            return;
        }
        self.buf.push_str(name);
        let tag = hash >> TAG_SHIFT;
        self.slots[i] = tag << TAG_SHIFT | u64::from(len) << LEN_SHIFT | u64::from(offset);
        self.len += 1;
        if self.len * 4 > self.slots.len() * 3 {
            self.grow();
        }
    }

    /// Twice the slots; every name placed again.
    fn grow(&mut self) {
        let doubled = vec![0; self.slots.len() * 2].into_boxed_slice();
        let old = std::mem::replace(&mut self.slots, doubled);
        for slot in old.iter().copied().filter(|&s| s != 0) {
            let mut i = fnv1a(self.name(slot)) as usize & self.mask();
            while self.slots[i] != 0 {
                i = (i + 1) & self.mask();
            }
            self.slots[i] = slot;
        }
    }

    fn contains(&self, name: &str) -> bool {
        if name.len() > usize::from(u16::MAX) {
            return self.long.contains(name);
        }
        self.find(fnv1a(name), name).1 || (!self.long.is_empty() && self.long.contains(name))
    }

    /// Every distinct name once.
    fn iter(&self) -> impl Iterator<Item = &str> + '_ {
        self.slots
            .iter()
            .filter(|&&slot| slot != 0)
            .map(|&slot| self.name(slot))
            .chain(self.long.iter().map(|n| &**n))
    }

    fn name(&self, slot: u64) -> &str {
        let offset = (slot & u64::from(u32::MAX)) as usize;
        let len = ((slot >> LEN_SHIFT) & u64::from(u16::MAX)) as usize;
        &self.buf[offset..offset + len]
    }
}

/// FNV-1a, 64-bit, then the SplitMix64 finalizer: deterministic and fast on
/// short names (they come from the user's own repositories, not from an
/// adversary). The finalizer spreads names that differ in a digit or two
/// over the whole table; FNV alone leaves the low bits that pick the slot
/// clustered, which linear probing makes slow.
fn fnv1a(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in s.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash = (hash ^ (hash >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    hash = (hash ^ (hash >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    hash ^ (hash >> 31)
}

impl WorkspaceFacts for ProjectFacts {
    fn contains(&self, pattern: &str) -> bool {
        let hash = fnv1a(pattern);
        if let Some(hit) = lock(&self.cache).get(hash, pattern) {
            return hit;
        }
        let hit = self.compute(pattern);
        lock(&self.cache).insert(hash, pattern, hit);
        hit
    }
}

/// Results by pattern, without an allocation per pattern: the patterns end
/// to end in one buffer and a list of (hash, offset, length, result),
/// searched by hash. Both are reserved when the facts are built, on the
/// executor, so the kernel's checks after a submission allocate nothing
/// until there are more than `CACHED_PATTERNS` of them.
struct Results {
    patterns: String,
    entries: Vec<(u64, usize, usize, bool)>,
}

impl Results {
    fn new() -> Results {
        Results {
            patterns: String::with_capacity(CACHED_PATTERNS * 32),
            entries: Vec::with_capacity(CACHED_PATTERNS),
        }
    }

    fn get(&self, hash: u64, pattern: &str) -> Option<bool> {
        self.entries
            .iter()
            .find(|&&(h, start, len, _)| {
                h == hash && self.patterns.get(start..start + len) == Some(pattern)
            })
            .map(|&(.., hit)| hit)
    }

    fn insert(&mut self, hash: u64, pattern: &str, hit: bool) {
        if self.get(hash, pattern).is_none() {
            self.entries
                .push((hash, self.patterns.len(), pattern.len(), hit));
            self.patterns.push_str(pattern);
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `*` and `?` stay within one segment; `**` spans any number.
const OPTIONS: MatchOptions = MatchOptions {
    case_sensitive: true,
    require_literal_separator: true,
    require_literal_leading_dot: false,
};

/// Files only: submodule entries are directories in the worktree.
fn files(tree: &FileList) -> impl Iterator<Item = (&str, FileKind)> + '_ {
    tree.iter().filter(|(_, kind)| *kind != FileKind::Submodule)
}

fn file_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

fn has_meta(s: &str) -> bool {
    s.contains(['*', '?', '[', ']'])
}
