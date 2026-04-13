use autotune_agent::terminal;

/// The primary guarantee the `terminal` module offers: `Guard` runs `restore`
/// on drop. We can't easily assert the side effect (restore is a no-op in
/// non-TTY contexts, which is exactly what `cargo test` runs in), but we can
/// exercise the code paths and verify they don't panic / aren't accidentally
/// pub-gated away. If these ever stop compiling the API has shifted in a
/// breaking way.
#[test]
fn guard_drops_without_panic() {
    {
        let _g = terminal::Guard::new();
    }
    // And twice in a row, to exercise the Drop path (no double-free etc).
    {
        let _g1 = terminal::Guard::new();
        let _g2 = terminal::Guard::new();
    }
}

#[test]
fn restore_is_no_op_in_non_tty() {
    // cargo test captures stdout/stderr, so stderr is not a TTY here.
    // restore() should detect that and silently return.
    terminal::restore();
    terminal::restore();
}

#[test]
fn install_panic_hook_is_idempotent() {
    terminal::install_panic_hook();
    terminal::install_panic_hook();
    terminal::install_panic_hook();
}

/// Guard correctly restores on unwinding panic (caught by catch_unwind).
#[test]
fn guard_restores_on_panic_unwind() {
    let result = std::panic::catch_unwind(|| {
        let _g = terminal::Guard::new();
        panic!("boom");
    });
    assert!(result.is_err());
    // If we got here without double-panic, Drop ran successfully during unwind.
}
