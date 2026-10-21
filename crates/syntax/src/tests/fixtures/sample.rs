//! A synthetic Rust sample.
use std::collections::HashMap;

/// The largest batch.
pub const MAX_BATCH: usize = 1_024;

#[derive(Debug, Clone)]
pub struct Cache<'a> {
    names: HashMap<&'a str, u32>,
}

impl<'a> Cache<'a> {
    pub fn insert(&mut self, name: &'a str) -> Option<u32> {
        let next = self.names.len() as u32 + 1;
        println!("insert {name}\n");
        self.names.insert(name, next)
    }
}

fn main() {
    let mut cache = Cache { names: HashMap::new() };
    match cache.insert("a") {
        Some(id) if id > 0 => {}
        _ => unreachable!(),
    }
}
