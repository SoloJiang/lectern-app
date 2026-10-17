//! The macOS file watching backend for `lectern-fs-watch`
//! (docs/design/fs_watch.md「platform 后端」).
//!
//! `Scope::Tree` targets get one FSEvents stream each, all scheduled on one
//! private serial dispatch queue; `Scope::Entries` targets share one kqueue
//! served by a dedicated thread that starts with the first such target.
//! Nothing is created before the first `arm`.
//!
//! Turning an FSEvents event into a [`RawEvent`](lectern_fs_watch::RawEvent)
//! is a pure function ([`convert`]) compiled on every platform so that its
//! unit tests also run on Linux; the system bindings are macOS only.

mod convert;
#[cfg(target_os = "macos")]
mod ffi;
#[cfg(target_os = "macos")]
mod kqueue;
#[cfg(target_os = "macos")]
mod stream;

#[cfg(test)]
mod tests;

#[cfg(target_os = "macos")]
pub use macos::backend;

#[cfg(target_os = "macos")]
mod macos {
    use std::io;
    use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

    use lectern_executor::{Executor, Task};
    use lectern_fs_watch::{Armed, Backend, BackendError, EventSink, RawBatch, Scope, Target};
    use rustix::io::Errno;

    use super::kqueue::Kqueue;
    use super::stream::{Queue, Stream};

    /// Where a target's events go: `EventSink::send` in production, a
    /// recorder in the tests. Called on the dispatch queue or the kqueue
    /// thread; must not block.
    pub(crate) type Deliver = Box<dyn Fn(RawBatch) + Send + Sync>;

    /// The FSEvents and kqueue backend. `app` creates it after the startup
    /// barrier is released and hands it to `Watcher::new`. Creating it does
    /// nothing: the dispatch queue is made on the first `Tree` arm, the
    /// kqueue and its thread on the first `Entries` arm (ADR 0017).
    pub fn backend(executor: Executor) -> Arc<dyn Backend> {
        Arc::new(MacBackend::new(executor))
    }

    pub(crate) struct MacBackend {
        executor: Executor,
        queue: Mutex<Option<Arc<Queue>>>,
        kqueue: Mutex<Option<(Arc<Kqueue>, Task<()>)>>,
    }

    impl MacBackend {
        pub(crate) fn new(executor: Executor) -> MacBackend {
            MacBackend {
                executor,
                queue: Mutex::new(None),
                kqueue: Mutex::new(None),
            }
        }

        /// `Backend::arm` with the delivery as a closure, so the tests can
        /// record batches without an fs_watch queue.
        pub(crate) fn arm_with(
            &self,
            target: &Target,
            deliver: Deliver,
        ) -> Result<Arc<dyn Armed>, BackendError> {
            match &target.scope {
                Scope::Tree => {
                    let stream = Stream::start(&target.path, self.queue()?, deliver)?;
                    Ok(Arc::new(stream))
                }
                Scope::Entries { names } => {
                    let watch = self.kqueue()?.arm(&target.path, names, deliver)?;
                    Ok(Arc::new(watch))
                }
            }
        }

        /// The dispatch queue all streams share, made on first use.
        fn queue(&self) -> Result<Arc<Queue>, BackendError> {
            let mut slot = lock(&self.queue);
            if let Some(queue) = &*slot {
                return Ok(queue.clone());
            }
            let queue = Arc::new(Queue::new()?);
            *slot = Some(queue.clone());
            Ok(queue)
        }

        /// The kqueue and its thread, started on first use.
        fn kqueue(&self) -> Result<Arc<Kqueue>, BackendError> {
            let mut slot = lock(&self.kqueue);
            if let Some((kqueue, _)) = &*slot
                && kqueue.is_running()
            {
                return Ok(kqueue.clone());
            }
            // A kqueue whose thread stopped (it only stops on an
            // unexpected kevent error) is replaced; its watches keep their
            // descriptors until fs_watch drops them.
            if let Some((old, task)) = slot.take() {
                old.stop();
                drop(task);
            }
            let kqueue = Arc::new(Kqueue::new()?);
            let body = kqueue.clone();
            let task = self
                .executor
                .run_dedicated("fs-watch-kqueue", move || body.run());
            if task.is_finished() {
                // The executor is shut down: no thread was started.
                return Err(BackendError::Other(
                    "could not start the kqueue thread".to_owned(),
                ));
            }
            *slot = Some((kqueue.clone(), task));
            Ok(kqueue)
        }
    }

    impl Backend for MacBackend {
        fn arm(&self, target: &Target, sink: EventSink) -> Result<Arc<dyn Armed>, BackendError> {
            self.arm_with(target, Box::new(move |batch| sink.send(batch)))
        }
    }

    impl Drop for MacBackend {
        fn drop(&mut self) {
            // Watches still armed keep the queue and the kqueue alive through
            // their own `Arc`s; the kqueue thread exits now and those watches
            // get no further events.
            if let Some((kqueue, task)) = lock(&self.kqueue).take() {
                kqueue.stop();
                drop(task);
            }
        }
    }

    pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Maps a `realpath` error.
    pub(crate) fn io_error(error: &io::Error) -> BackendError {
        match Errno::from_io_error(error) {
            Some(errno) => errno_error(errno),
            None => BackendError::Other(error.to_string()),
        }
    }

    /// Maps an `open` or `kevent` error.
    pub(crate) fn errno_error(errno: Errno) -> BackendError {
        match errno {
            Errno::NOENT | Errno::NOTDIR => BackendError::NotFound,
            Errno::ACCESS | Errno::PERM => BackendError::PermissionDenied,
            Errno::MFILE | Errno::NFILE | Errno::NOMEM => BackendError::Exhausted,
            other => BackendError::Other(io::Error::from(other).to_string()),
        }
    }
}

#[cfg(all(target_os = "macos", test))]
pub(crate) use macos::MacBackend;
#[cfg(target_os = "macos")]
pub(crate) use macos::{Deliver, errno_error, io_error, lock};
