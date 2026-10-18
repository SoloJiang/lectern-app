//! A bounded job queue.

use std::collections::VecDeque;
use std::fmt;

pub const DEFAULT_CAPACITY: usize = 64;
static NAME: &str = "queue";

macro_rules! ensure {
    ($cond:expr) => {
        if !$cond {
            return Err(QueueError::Full);
        }
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueError {
    Full,
    Closed,
}

pub struct Queue<T> {
    items: VecDeque<T>,
    pub capacity: usize,
    closed: bool,
}

struct Stats(u64, u64);

pub trait Drain<T> {
    fn drain_into(&mut self, out: &mut Vec<T>) -> usize;
    fn is_drained(&self) -> bool {
        self.remaining() == 0
    }
    fn remaining(&self) -> usize;
}

impl<T> Queue<T> {
    pub fn new(capacity: usize) -> Self {
        Queue {
            items: VecDeque::with_capacity(capacity),
            capacity,
            closed: false,
        }
    }

    pub fn push(&mut self, item: T) -> Result<(), QueueError> {
        ensure!(self.items.len() < self.capacity);
        self.items.push_back(item);
        Ok(())
    }

    fn compact<F: Fn(&T) -> bool>(&mut self, keep: F, limit: usize) {
        let before = self.items.len();
        self.items.retain(|item| keep(item));
        let removed = before - self.items.len();
        debug_assert!(removed <= limit);
    }

    pub(crate) fn close(&mut self) {
        self.closed = true;
    }
}

impl<T> Drain<T> for Queue<T> {
    fn drain_into(&mut self, out: &mut Vec<T>) -> usize {
        let n = self.items.len();
        out.extend(self.items.drain(..));
        n
    }

    fn remaining(&self) -> usize {
        self.items.len()
    }
}

impl fmt::Display for QueueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{NAME}: {self:?}")
    }
}

pub mod metrics {
    pub fn record(name: &str, value: u64) {
        let line = format!("{name}={value}");
        super::emit(&line);
    }

    fn reset() {}
}

fn emit(line: &str) {
    let trimmed = line.trim();
    println!("{trimmed}");
}

pub fn run(jobs: Vec<u32>) -> Result<usize, QueueError> {
    let mut queue = Queue::new(DEFAULT_CAPACITY);
    for job in jobs {
        queue.push(job)?;
    }
    let stats = Stats(0, 0);
    let mut out = Vec::new();
    metrics::record("jobs", stats.0);
    Ok(queue.drain_into(&mut out))
}
