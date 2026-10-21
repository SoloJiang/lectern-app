//! The idle prefetch (docs/design/file_reader.md「数据流与线程」「批量线程上的
//! 先后」, ADR 0033): `Reader::prefetch` while interaction is quiet.
//!
//! A request keeps its first [`PREFETCH_MAX`] targets, replacing those of
//! the last request not started yet. One `Batch` task at a time across the
//! engine, holding a yielding `BackgroundGuard` (ADR 0033's memory-only
//! batch work), one file a turn: before the next file it goes to the end of
//! the batch queue, behind the batch work already queued, and it ends when
//! no target is left. A file up to 1 MiB gets its whole
//! spans ([`FullParse`]) under the key the first screen looks for; a larger
//! one gets its first screen's window, planned as `open` plans it and kept
//! under the target's line start (`LruKey::Screen`), after its OID is
//! hashed in 1 MiB reads (and its line checkpoints written). A worktree
//! file's identity is remembered with the OID, so opening it finds them.
//! The results go only into the LRU, as background work: nothing is opened,
//! no document is notified.
//!
//! The parse stops at its next slice boundary when the yield switch (or the
//! pause) asks, keeps its state and goes on once `proceed_item` lets it.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use lectern_git::{BlobHasher, Oid};
use lectern_kernel::BackgroundGuard;
use lectern_platform::{FileIdentity, FullRead, OpenFile};
use lectern_services::code_intel::TextOrigin;
use lectern_services::project::BlobId;
use lectern_services::reader::ReadTarget;

use crate::Lang;
use crate::git_access::blob_id;
use crate::highlight::{Budget, FullParse, Outcome, Source};
use crate::limits::{BATCH_SEGMENT, CHECKPOINT_MIN, PREFETCH_MAX, SLICE, TIER_A_MAX};
use crate::lru::{Class, LruKey, LruValue, SpansOf};
use crate::text::{IndexBuilder, Scanned, Text};
use crate::util::{lock, yield_now};

#[cfg(any(test, feature = "test-support"))]
use super::engine::Trace;
use super::engine::{EngineInner, trace};
use super::load::{self, First, Rest, SCAN_BLOCK};

/// The rows a prefetched first screen is planned for: three screens of 60
/// lines, so that its window covers what an opening viewport shows.
const ROWS: u32 = 180;

/// The targets not started yet, and whether a task is queued or running.
#[derive(Default)]
pub(crate) struct Queue {
    pending: VecDeque<(ReadTarget, Arc<Path>)>,
    busy: bool,
}

pub(crate) fn request(engine: &Arc<EngineInner>, targets: Vec<(ReadTarget, Arc<Path>)>) {
    {
        let mut queue = lock(&engine.prefetch);
        queue.pending = targets.into_iter().take(PREFETCH_MAX).collect();
        if queue.busy || queue.pending.is_empty() {
            return;
        }
        queue.busy = true;
    }
    spawn(engine);
}

fn spawn(engine: &Arc<EngineInner>) {
    let engine2 = engine.clone();
    if !engine.spawn_yielding(move |guard| async move { task(&engine2, guard).await }) {
        lock(&engine.prefetch).busy = false;
    }
}

/// The task: one target a turn, then back to the end of the batch queue
/// (`yield_now`, as code_intel's batch loop does) while targets remain.
async fn task(engine: &Arc<EngineInner>, guard: BackgroundGuard) {
    loop {
        trace!(engine, Trace::PrefetchTask);
        let next = lock(&engine.prefetch).pending.pop_front();
        if let Some((target, root)) = next {
            one(engine, &guard, &target, &root).await;
        }
        {
            let mut queue = lock(&engine.prefetch);
            if queue.pending.is_empty() {
                queue.busy = false;
                return;
            }
        }
        yield_now().await;
    }
}

/// The batch boundary: waits while paused, and while the yield switch is on
/// until a keepalive slot lets this item (of `bytes`) go on.
async fn boundary(guard: &BackgroundGuard, bytes: u64) {
    let _grant = guard.proceed_item(bytes).await;
}

/// The parse's yield probe: the yield switch, or the pause.
fn stop_asked(guard: &BackgroundGuard) -> bool {
    guard.yield_requested() || guard.pause_requested()
}

async fn one(
    engine: &Arc<EngineInner>,
    guard: &BackgroundGuard,
    target: &ReadTarget,
    root: &Arc<Path>,
) {
    let langs = engine.languages.clone();
    let Some(lang) = langs.for_path(&target.path) else {
        return;
    };
    let opened = match &target.origin {
        TextOrigin::Worktree => load::worktree_file(engine, target, root),
        TextOrigin::Dependency(dep) => load::dependency_file(engine, target, root, &dep.root),
        TextOrigin::Object => load::object(engine, target, Class::Background).map(|rest| {
            let size = match &rest {
                Rest::Memory(bytes) => bytes.len() as u64,
                Rest::Text(text) => text.len(),
                Rest::File(file) => file.meta().size,
            };
            (rest, size, None)
        }),
    };
    let Ok((rest, size, identity)) = opened else {
        return;
    };
    boundary(guard, size).await;
    if !lang.prepare() {
        return;
    }
    if size <= TIER_A_MAX {
        whole(engine, guard, target, root, lang, rest, identity).await;
    } else {
        screen(engine, guard, target, root, lang, rest, identity).await;
    }
}

/// The OID of a file whose identity was hashed before (an earlier prefetch
/// or opening): a repeated prefetch reads and hashes nothing again.
fn known(
    engine: &EngineInner,
    target: &ReadTarget,
    root: &Arc<Path>,
    identity: Option<FileIdentity>,
) -> Option<BlobId> {
    match target.origin {
        TextOrigin::Object => None,
        TextOrigin::Worktree | TextOrigin::Dependency(_) => {
            engine.known_oid(root, &target.path, identity?)
        }
    }
}

/// The OID of content read whole: an object's is `expect`; a file's is
/// hashed and remembered with its identity, so the first screen trusts it.
fn oid_of(
    engine: &EngineInner,
    target: &ReadTarget,
    root: &Arc<Path>,
    identity: Option<FileIdentity>,
    bytes: &[u8],
) -> Option<BlobId> {
    match target.origin {
        TextOrigin::Object => target.expect,
        TextOrigin::Worktree | TextOrigin::Dependency(_) => {
            let oid = blob_id(&Oid::for_blob(engine.hash_kind(target), bytes));
            engine.remember(root, &target.path, identity?, oid);
            Some(oid)
        }
    }
}

/// Up to 1 MiB: the whole file's spans.
async fn whole(
    engine: &Arc<EngineInner>,
    guard: &BackgroundGuard,
    target: &ReadTarget,
    root: &Arc<Path>,
    lang: &Lang,
    rest: Rest,
    identity: Option<FileIdentity>,
) {
    // Already there: looked up as background work, so it stays in the
    // class it has.
    let done = known(engine, target, root, identity).or(match target.origin {
        TextOrigin::Object => target.expect,
        _ => None,
    });
    if let Some(oid) = done
        && engine
            .lru
            .spans(&load::whole_key(oid, lang), Class::Background)
            .is_some()
    {
        return;
    }
    let bytes: Arc<[u8]> = match rest {
        Rest::File(file) => match file.read_all(TIER_A_MAX) {
            Ok(FullRead::File { bytes, meta }) if Some(meta.identity()) == identity => bytes.into(),
            _ => return,
        },
        Rest::Memory(bytes) => bytes,
        Rest::Text(text) => text.shared_bytes().clone(),
    };
    let size = bytes.len() as u64;
    let Some(oid) = oid_of(engine, target, root, identity, &bytes) else {
        return;
    };
    let key = load::whole_key(oid, lang);
    if engine.lru.spans(&key, Class::Background).is_some() {
        return;
    }
    let text = Text::shared(bytes);
    // Not text: the document shows it plain.
    if !text.index().facts().is_text() {
        return;
    }
    trace!(
        engine,
        Trace::PrefetchParse {
            path: target.path.clone()
        }
    );
    let src: Arc<dyn Source> = Arc::new(text);
    let mut parse = FullParse::new(lang, src);
    let never = AtomicBool::new(false);
    loop {
        let outcome = {
            let probe = || stop_asked(guard);
            parse.step(Budget {
                budget: SLICE,
                cancel: &never,
                yield_probe: Some(&probe),
            })
        };
        match outcome {
            Outcome::Done(spans) => {
                engine
                    .lru
                    .insert(key, LruValue::Spans(Arc::new(spans)), Class::Background);
                trace!(
                    engine,
                    Trace::PrefetchDone {
                        path: target.path.clone()
                    }
                );
                return;
            }
            Outcome::TimedOut => {}
            Outcome::Yielded => {
                trace!(
                    engine,
                    Trace::PrefetchYielded {
                        path: target.path.clone()
                    }
                );
                boundary(guard, size).await;
            }
            Outcome::Cancelled | Outcome::Unavailable => return,
        }
    }
}

/// Over 1 MiB: the OID (hashed in 1 MiB reads for a file), then the first
/// screen's window.
async fn screen(
    engine: &Arc<EngineInner>,
    guard: &BackgroundGuard,
    target: &ReadTarget,
    root: &Arc<Path>,
    lang: &Lang,
    rest: Rest,
    identity: Option<FileIdentity>,
) {
    let (oid, size, _share) = match (&target.origin, rest) {
        (TextOrigin::Object, rest) => {
            let Some(oid) = target.expect else {
                return;
            };
            let bytes = match rest {
                Rest::Memory(bytes) => bytes,
                Rest::Text(text) => text.shared_bytes().clone(),
                Rest::File(_) => return,
            };
            let size = bytes.len() as u64;
            // The first screen below takes the content from here, not from
            // the object database again.
            (oid, size, Some(engine.shares.register(oid, bytes)))
        }
        (TextOrigin::Worktree | TextOrigin::Dependency(_), Rest::File(file)) => {
            let size = file.meta().size;
            let Some(identity) = identity else {
                return;
            };
            // Hashed before with this identity (its checkpoints written
            // then): not read again.
            if let Some(oid) = known(engine, target, root, Some(identity)) {
                (oid, size, None)
            } else {
                trace!(
                    engine,
                    Trace::PrefetchHash {
                        path: target.path.clone()
                    }
                );
                let Some((oid, scanned)) = stream(engine, guard, &file, identity, target).await
                else {
                    return;
                };
                drop(file);
                if size >= CHECKPOINT_MIN {
                    engine.store_checkpoints(&target.project, &oid, &scanned.checkpoints());
                }
                engine.remember(root, &target.path, identity, oid);
                (oid, size, None)
            }
        }
        _ => return,
    };
    screen_window(engine, guard, target, root, lang, oid, size).await;
}

/// The first screen's window of content whose OID is `oid`, kept under
/// the target's line start unless it is there already.
async fn screen_window(
    engine: &Arc<EngineInner>,
    guard: &BackgroundGuard,
    target: &ReadTarget,
    root: &Arc<Path>,
    lang: &Lang,
    oid: BlobId,
    size: u64,
) {
    trace!(
        engine,
        Trace::PrefetchParse {
            path: target.path.clone()
        }
    );
    let never = AtomicBool::new(false);
    loop {
        let first = {
            let probe = || stop_asked(guard);
            let budget = Budget {
                budget: BATCH_SEGMENT,
                cancel: &never,
                yield_probe: Some(&probe),
            };
            // Found in the LRU as background work: a prefetch never makes
            // its own entries the user's.
            load::first_screen(
                engine,
                target,
                root,
                target.at,
                target.expect,
                ROWS,
                budget,
                Class::Background,
            )
        };
        let First::Shown(shown) = first else {
            return;
        };
        if shown.yielded {
            // A window is parsed in one call: once let go on, again.
            trace!(
                engine,
                Trace::PrefetchYielded {
                    path: target.path.clone()
                }
            );
            boundary(guard, size).await;
            continue;
        }
        let Some(window) = shown.window.clone() else {
            return;
        };
        let key = LruKey::Screen {
            of: SpansOf::new(oid, lang),
            target_line_start: shown.anchor.line_start,
        };
        // A window the first screen took from there is left as it is.
        let kept = engine
            .lru
            .spans(&key, Class::Background)
            .is_some_and(|kept| Arc::ptr_eq(&kept, &window));
        if !kept {
            engine
                .lru
                .insert(key, LruValue::Spans(window), Class::Background);
        }
        trace!(
            engine,
            Trace::PrefetchDone {
                path: target.path.clone()
            }
        );
        return;
    }
}

/// A file's raw OID and line index, in 1 MiB reads on its `OpenFile` (tier
/// C's scan), stopping between reads while the switches ask. None when the
/// file changed meanwhile.
async fn stream(
    engine: &EngineInner,
    guard: &BackgroundGuard,
    file: &OpenFile,
    identity: FileIdentity,
    target: &ReadTarget,
) -> Option<(BlobId, Scanned)> {
    let size = file.meta().size;
    let mut hasher = BlobHasher::new(engine.hash_kind(target), size);
    let mut builder = IndexBuilder::new();
    let mut at = 0;
    while at < size {
        if stop_asked(guard) {
            boundary(guard, size).await;
        }
        let end = (at + SCAN_BLOCK).min(size);
        let read = file.read_ranges(std::slice::from_ref(&(at..end))).ok()?;
        let piece = read.pieces.into_iter().next().unwrap_or_default();
        if read.meta.identity() != identity || piece.len() as u64 != end - at {
            return None;
        }
        hasher.update(&piece);
        builder.feed(&piece);
        at = end;
    }
    let oid = blob_id(&hasher.finish()?);
    Some((oid, builder.finish()))
}
