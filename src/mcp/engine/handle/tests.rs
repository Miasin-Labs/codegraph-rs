use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use super::*;

const CHILD_MODE_ENV: &str = "CODEGRAPH_ENGINE_WORKER_TEST_MODE";
const FATAL_MARKER: &str = "Engine worker failed unexpectedly";

#[test]
fn engine_worker_panic_invokes_the_injected_fatal_callback_once() {
    // Given: an engine whose fatal process action is replaced by a test channel.
    let runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let _runtime = runtime.enter();
    let (fatal_tx, fatal_rx) = crossbeam_channel::bounded(2);
    let fatal = Arc::new(move |reason: &str| {
        let _ = fatal_tx.send(reason.to_string());
    });
    let engine = EngineHandle::spawn_on_with_fatal(
        MCPEngineOptions { watch: false },
        Handle::current(),
        fatal,
    );

    // When: the canonical worker panics.
    engine.panic_for_test();

    // Then: supervision reports one fatal event and never respawns the worker.
    assert_eq!(
        fatal_rx.recv_timeout(Duration::from_secs(1)).as_deref(),
        Ok("worker thread panicked")
    );
    assert!(fatal_rx.recv_timeout(Duration::from_millis(100)).is_err());
}

#[test]
fn engine_worker_normal_shutdown_does_not_invoke_the_fatal_callback() {
    // Given: an engine with an observable fatal callback.
    let runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let _runtime = runtime.enter();
    let (fatal_tx, fatal_rx) = crossbeam_channel::bounded(1);
    let fatal = Arc::new(move |reason: &str| {
        let _ = fatal_tx.send(reason.to_string());
    });
    let engine = EngineHandle::spawn_on_with_fatal(
        MCPEngineOptions { watch: false },
        Handle::current(),
        fatal,
    );

    // When: the owner requests the normal Stop path.
    engine.stop();

    // Then: worker completion is not treated as fatal.
    assert!(fatal_rx.recv_timeout(Duration::from_millis(100)).is_err());
}

#[test]
fn engine_worker_panic_fails_the_owning_process_once() {
    if child_mode() {
        return;
    }

    // Given: a subprocess owns an EngineHandle whose worker can be forced to panic.
    let output = run_child("panic");

    // When: the worker panics, the owning subprocess must terminate promptly.
    assert_eq!(output.status.code(), Some(1), "{output:?}");

    // Then: the factored fatal notification is emitted exactly once.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr.matches(FATAL_MARKER).count(), 1, "{stderr}");
}

#[test]
fn engine_worker_stop_and_disconnect_are_non_fatal() {
    if child_mode() {
        return;
    }

    // Given/When: subprocesses stop explicitly or drop their last handle.
    for mode in ["stop", "drop"] {
        let output = run_child(mode);

        // Then: both normal exits succeed without a fatal notification.
        assert!(output.status.success(), "mode={mode}: {output:?}");
        assert!(
            !String::from_utf8_lossy(&output.stderr).contains(FATAL_MARKER),
            "mode={mode}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn child_mode() -> bool {
    let Ok(mode) = std::env::var(CHILD_MODE_ENV) else {
        return false;
    };
    let runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let _runtime = runtime.enter();
    let engine = EngineHandle::spawn(MCPEngineOptions { watch: false });

    match mode.as_str() {
        "panic" => {
            engine.panic_for_test();
            std::thread::sleep(Duration::from_secs(30));
        }
        "stop" => {
            engine.stop();
            std::thread::sleep(Duration::from_millis(100));
        }
        "drop" => {
            drop(engine);
            std::thread::sleep(Duration::from_millis(100));
        }
        other => panic!("unknown child mode: {other}"),
    }
    true
}

fn run_child(mode: &str) -> Output {
    let test_name = if mode == "panic" {
        "engine_worker_panic_fails_the_owning_process_once"
    } else {
        "engine_worker_stop_and_disconnect_are_non_fatal"
    };
    let mut child = Command::new(std::env::current_exe().expect("current test executable"))
        .arg(test_name)
        .arg("--nocapture")
        .env(CHILD_MODE_ENV, mode)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn engine supervision child");
    let started = Instant::now();
    loop {
        if child.try_wait().expect("poll child").is_some() {
            return child.wait_with_output().expect("collect child output");
        }
        if started.elapsed() > Duration::from_secs(3) {
            let _ = child.kill();
            let output = child.wait_with_output().expect("collect timed-out child");
            panic!("engine supervision child timed out: {output:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
