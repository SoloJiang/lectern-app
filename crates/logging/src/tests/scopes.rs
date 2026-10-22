//! The thread's scope stack (docs/design/logging.md「作用域与关联 ID」):
//! overlay scopes, `within`, and guards that are forgotten, dropped out of
//! order or escape a `within`.

use std::panic::{self, AssertUnwindSafe};

use crate::scope::MAX_DEPTH;
use crate::{Ctx, PluginTag, current, scope, within};

fn act(n: u64) -> Ctx {
    Ctx {
        act: Some(n),
        ..Ctx::default()
    }
}

fn task(n: u64) -> Ctx {
    Ctx {
        task: Some(n),
        ..Ctx::default()
    }
}

/// Runs `f` on a fresh thread, so a test's leftover depth cannot reach
/// another test sharing this process.
fn on_thread(f: impl FnOnce() + Send + 'static) {
    std::thread::spawn(f).join().unwrap();
}

#[test]
fn overlay_scopes_nest_and_restore() {
    on_thread(|| {
        let plugin = Ctx {
            plugin: Some(PluginTag::new("p")),
            act: Some(1),
            ..Ctx::default()
        };
        {
            let _outer = scope(plugin);
            {
                let _inner = scope(task(9));
                assert_eq!(
                    current(),
                    Ctx {
                        task: Some(9),
                        ..plugin
                    }
                );
            }
            assert_eq!(current(), plugin);
        }
        assert_eq!(current(), Ctx::default());
    });
}

#[test]
fn within_replaces_the_scope_and_restores_it() {
    on_thread(|| {
        let _outer = scope(act(1));
        let entered = Ctx {
            review: Some(7),
            ..task(3)
        };
        within(&entered, || {
            // Replaced, not overlaid: no `act` from outside.
            assert_eq!(current(), entered);
            let _inner = scope(act(2));
            assert_eq!(
                current(),
                Ctx {
                    act: Some(2),
                    ..entered
                }
            );
        });
        assert_eq!(current(), act(1));
        // Nested `within`s restore in order.
        within(&task(4), || {
            within(&task(5), || assert_eq!(current(), task(5)));
            assert_eq!(current(), task(4));
        });
        assert_eq!(current(), act(1));
    });
}

#[test]
fn within_restores_when_unwinding() {
    on_thread(|| {
        let _outer = scope(act(1));
        let result = panic::catch_unwind(|| {
            within(&task(3), || {
                let _inner = scope(act(2));
                panic!("inside");
            })
        });
        assert!(result.is_err());
        assert_eq!(current(), act(1));
    });
}

#[test]
fn a_forgotten_scope_does_not_outlive_its_within() {
    on_thread(|| {
        let entered = task(3);
        // Leaving `within` finds the forgotten guard's depth: a nesting
        // error, reported in debug builds after the depth is lowered.
        let left = panic::catch_unwind(AssertUnwindSafe(|| {
            within(&entered, || std::mem::forget(scope(act(2))));
        }));
        assert_eq!(left.is_err(), cfg!(debug_assertions));
        assert_eq!(current(), Ctx::default());
    });
}

#[test]
fn a_scope_escaping_its_within_cannot_bring_back_the_frame() {
    on_thread(|| {
        let escaped = {
            let entered = task(3);
            let mut escaped = None;
            let _ = panic::catch_unwind(AssertUnwindSafe(|| {
                within(&entered, || escaped = Some(scope(act(2))));
            }));
            escaped.unwrap()
        };
        // `entered` is gone. Dropping the guard only lowers the depth, so
        // the frame that pointed at it never comes back.
        assert_eq!(current(), Ctx::default());
        let dropped = panic::catch_unwind(AssertUnwindSafe(|| drop(escaped)));
        assert_eq!(dropped.is_err(), cfg!(debug_assertions));
        assert_eq!(current(), Ctx::default());
        let _later = scope(act(5));
        assert_eq!(current(), act(5));
    });
}

#[test]
fn scopes_beyond_the_limit_change_nothing() {
    fn nest(level: usize) {
        let _s = scope(task(level as u64));
        let expected = task(level.min(MAX_DEPTH) as u64);
        assert_eq!(current(), expected);
        if level <= MAX_DEPTH + 2 {
            nest(level + 1);
        }
        assert_eq!(current(), expected);
    }
    on_thread(|| {
        nest(1);
        assert_eq!(current(), Ctx::default());
        // A `within` past the limit runs `f` without entering.
        fn deep(level: usize) {
            let _s = scope(task(level as u64));
            if level < MAX_DEPTH {
                deep(level + 1);
            } else {
                within(&act(9), || assert_eq!(current(), task(MAX_DEPTH as u64)));
            }
        }
        deep(1);
        assert_eq!(current(), Ctx::default());
    });
}
