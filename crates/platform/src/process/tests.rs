use super::phys_footprint;

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

#[cfg(not(target_os = "macos"))]
#[test]
fn process_phys_footprint_is_unsupported() {
    let err = phys_footprint(std::process::id()).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
}
