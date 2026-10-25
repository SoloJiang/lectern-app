use super::phys_footprint;

#[cfg(target_os = "macos")]
use std::process::{Child, Command};
#[cfg(target_os = "macos")]
use std::time::{Duration, SystemTime};

#[cfg(target_os = "macos")]
use super::{children, start_time};

/// A `sleep` child that is killed and reaped when the guard is dropped, also
/// when an assertion fails.
#[cfg(target_os = "macos")]
struct Sleeper(Child);

#[cfg(target_os = "macos")]
impl Sleeper {
    fn spawn() -> Self {
        Sleeper(Command::new("sleep").arg("5").spawn().unwrap())
    }

    fn pid(&self) -> u32 {
        self.0.id()
    }
}

#[cfg(target_os = "macos")]
impl Drop for Sleeper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(target_os = "macos")]
#[test]
fn process_phys_footprint_follows_written_memory() {
    let pid = std::process::id();
    let before = phys_footprint(pid).unwrap();
    assert!(before > 0);
    let mut block = vec![0u8; 64 << 20];
    // Write every page: pages never written are not in the footprint.
    for page in block.chunks_mut(4096) {
        page[0] = 1;
    }
    let after = phys_footprint(pid).unwrap();
    std::hint::black_box(&block);
    assert!(after >= before + (60 << 20), "{before} -> {after}");
}

#[cfg(target_os = "macos")]
#[test]
fn process_phys_footprint_of_a_missing_process_is_an_error() {
    // Above the largest pid macOS assigns (99 998).
    assert!(phys_footprint(4_000_000).is_err());
}

#[cfg(target_os = "macos")]
#[test]
fn process_start_time_of_a_spawned_child_is_between_the_spawn_calls() {
    let margin = Duration::from_millis(1);
    let before = SystemTime::now();
    let child = Sleeper::spawn();
    let after = SystemTime::now();
    let started = start_time(child.pid()).unwrap();
    assert!(started + margin >= before, "{started:?} < {before:?}");
    assert!(started <= after + margin, "{started:?} > {after:?}");
}

#[cfg(target_os = "macos")]
#[test]
fn process_start_time_of_this_process_is_in_the_past() {
    let test_started = SystemTime::now();
    let started = start_time(std::process::id()).unwrap();
    assert!(started < test_started, "{started:?} >= {test_started:?}");
}

#[cfg(target_os = "macos")]
#[test]
fn process_start_time_is_the_same_on_every_call() {
    let pid = std::process::id();
    assert_eq!(start_time(pid).unwrap(), start_time(pid).unwrap());
}

#[cfg(target_os = "macos")]
#[test]
fn process_children_lists_spawned_children() {
    let me = std::process::id();
    let mut first = Sleeper::spawn();
    let second = Sleeper::spawn();
    let listed = children(me).unwrap();
    assert!(listed.contains(&first.pid()), "{listed:?}");
    assert!(listed.contains(&second.pid()), "{listed:?}");
    assert!(listed.is_sorted(), "{listed:?}");
    // `proc_listchildpids` returns a number of pids, not bytes: no padding
    // of zeros is left in the result, and no pid is listed twice.
    assert!(!listed.contains(&0), "{listed:?}");
    assert!(
        listed.windows(2).all(|pair| pair[0] < pair[1]),
        "{listed:?}"
    );
    assert!(!listed.contains(&me), "{listed:?}");

    first.0.kill().unwrap();
    first.0.wait().unwrap();
    let listed = children(me).unwrap();
    assert!(!listed.contains(&first.pid()), "{listed:?}");
    assert!(listed.contains(&second.pid()), "{listed:?}");
}

#[cfg(target_os = "macos")]
#[test]
fn process_children_of_a_process_without_children_is_empty() {
    // A fresh child of ours has no children of its own.
    let child = Sleeper::spawn();
    assert_eq!(children(child.pid()).unwrap(), Vec::<u32>::new());
}

#[cfg(target_os = "macos")]
#[test]
fn process_missing_pid_is_an_error() {
    let mut child = Sleeper::spawn();
    let pid = child.pid();
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    assert!(phys_footprint(pid).is_err());
    assert!(start_time(pid).is_err());
    // `proc_listchildpids` cannot tell "no children" from "no such
    // process": an error or an empty list.
    assert!(children(pid).map_or(true, |listed| listed.is_empty()));
}

#[cfg(target_os = "macos")]
#[test]
fn process_pid_beyond_the_pid_type_is_invalid_input() {
    let pid = u32::MAX;
    for kind in [
        phys_footprint(pid).unwrap_err().kind(),
        start_time(pid).unwrap_err().kind(),
        children(pid).unwrap_err().kind(),
    ] {
        assert_eq!(kind, std::io::ErrorKind::InvalidInput);
    }
}

#[cfg(not(target_os = "macos"))]
#[test]
fn process_is_unsupported_elsewhere() {
    use super::{children, start_time};
    use std::io::ErrorKind;

    let pid = std::process::id();
    assert_eq!(
        phys_footprint(pid).unwrap_err().kind(),
        ErrorKind::Unsupported
    );
    assert_eq!(start_time(pid).unwrap_err().kind(), ErrorKind::Unsupported);
    assert_eq!(children(pid).unwrap_err().kind(), ErrorKind::Unsupported);
}
