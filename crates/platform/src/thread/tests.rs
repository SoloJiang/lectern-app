//! Tests of the thread QoS. The read-back runs on macOS only.

#[cfg(target_os = "macos")]
#[test]
fn thread_set_user_interactive_sets_the_qos() {
    // A new thread, so that no other test sees the changed QoS.
    std::thread::spawn(|| {
        super::set_user_interactive().unwrap();
        let mut class = libc::qos_class_t::QOS_CLASS_UNSPECIFIED;
        let mut relative_priority: libc::c_int = 1;
        // SAFETY: `class` and `relative_priority` are valid for writes for
        // the call; the thread is the calling thread.
        let rc = unsafe {
            libc::pthread_get_qos_class_np(
                libc::pthread_self(),
                &raw mut class,
                &raw mut relative_priority,
            )
        };
        assert_eq!(rc, 0);
        assert!(matches!(
            class,
            libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE
        ));
        assert_eq!(relative_priority, 0);
    })
    .join()
    .unwrap();
}

#[cfg(target_os = "macos")]
#[test]
fn thread_set_user_interactive_can_be_repeated() {
    std::thread::spawn(|| {
        super::set_user_interactive().unwrap();
        super::set_user_interactive().unwrap();
    })
    .join()
    .unwrap();
}

#[cfg(not(target_os = "macos"))]
#[test]
fn thread_qos_is_unsupported_elsewhere() {
    let err = super::set_user_interactive().unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
}
