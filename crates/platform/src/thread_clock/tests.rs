use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use super::ThreadCpuClock;

#[test]
fn thread_cpu_clock_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<ThreadCpuClock>();
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn thread_cpu_busy_thread_grows() {
    let stop = Arc::new(AtomicBool::new(false));
    let (sender, receiver) = mpsc::channel();
    let busy = {
        let stop = Arc::clone(&stop);
        thread::spawn(move || {
            sender.send(ThreadCpuClock::current().unwrap()).unwrap();
            let mut spins = 0u64;
            while !stop.load(Ordering::Relaxed) {
                spins = std::hint::black_box(spins.wrapping_add(1));
            }
            spins
        })
    };
    // Read on this thread, not the one measured.
    let clock = receiver.recv().unwrap();
    let start = clock.cpu_time().unwrap();
    // Generous: other builds may share the machine's CPUs.
    let deadline = Instant::now() + Duration::from_secs(20);
    let grown = loop {
        let now = clock.cpu_time().unwrap();
        if now >= start + Duration::from_millis(100) {
            break now - start;
        }
        assert!(
            Instant::now() < deadline,
            "the busy thread's CPU time grew only {:?}",
            now - start
        );
        thread::sleep(Duration::from_millis(5));
    };
    stop.store(true, Ordering::Relaxed);
    busy.join().unwrap();
    eprintln!("busy thread: {grown:?} of CPU time");
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn thread_cpu_blocked_thread_barely_moves() {
    let (clock_sender, clock_receiver) = mpsc::channel();
    let (wake, wait) = mpsc::channel::<()>();
    let blocked = thread::spawn(move || {
        clock_sender
            .send(ThreadCpuClock::current().unwrap())
            .unwrap();
        // Blocks until the test is done.
        wait.recv().unwrap();
    });
    let clock = Arc::new(clock_receiver.recv().unwrap());
    // Let it reach `recv` before the first reading.
    thread::sleep(Duration::from_millis(20));
    let start = clock.cpu_time().unwrap();
    // Burn CPU on another thread meanwhile: only the blocked thread counts.
    let wall = Instant::now();
    let mut spins = 0u64;
    while wall.elapsed() < Duration::from_millis(300) {
        spins = std::hint::black_box(spins.wrapping_add(1));
    }
    // Readable from a third thread too.
    let reader = Arc::clone(&clock);
    let end = thread::spawn(move || reader.cpu_time().unwrap())
        .join()
        .unwrap();
    wake.send(()).unwrap();
    blocked.join().unwrap();
    let used = end - start;
    assert!(
        used < Duration::from_millis(20),
        "a blocked thread used {used:?} of CPU time in 300 ms"
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn thread_cpu_after_the_thread_exits_is_an_error() {
    let clock = thread::spawn(|| ThreadCpuClock::current().unwrap())
        .join()
        .unwrap();
    // `join` can return a moment before the kernel has released the thread.
    let deadline = Instant::now() + Duration::from_secs(2);
    while clock.cpu_time().is_ok() {
        assert!(
            Instant::now() < deadline,
            "an exited thread's clock still reads"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn thread_cpu_clocks_are_released() {
    // Each clock holds a Mach port reference on macOS, released on drop: far
    // more clocks than a port's 65 535 user references still succeed.
    for _ in 0..70_000 {
        let clock = ThreadCpuClock::current().unwrap();
        clock.cpu_time().unwrap();
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
#[test]
fn thread_cpu_is_unsupported_elsewhere() {
    let error = ThreadCpuClock::current().unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
}
