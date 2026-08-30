use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

use super::DAEMON_INTERNAL_ENV;
use crate::mcp::daemon_paths::get_daemon_socket_path;
use crate::mcp::proxy::{DaemonSocket, HelloConnectResult, connect_with_hello_until};
use crate::mcp::version::CODEGRAPH_PACKAGE_VERSION;

const EXISTING_DAEMON_PROBE_MS: u64 = 100;

pub(super) fn connect_or_spawn(root: &Path) -> Option<DaemonSocket> {
    let socket_path = get_daemon_socket_path(root);
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_millis(EXISTING_DAEMON_PROBE_MS);
    match connect_with_hello_until(&socket_path, CODEGRAPH_PACKAGE_VERSION, deadline) {
        HelloConnectResult::Connected(socket) => return Some(socket),
        HelloConnectResult::VersionMismatch => return None,
        HelloConnectResult::Unavailable => {}
    }
    let _ = spawn_detached(root);
    None
}

fn spawn_detached(root: &Path) -> Result<(), String> {
    let executable = std::env::current_exe()
        .map_err(|_| "cannot resolve CLI script path to spawn the daemon".to_string())?;
    let mut command = Command::new(executable);
    command
        .arg("serve")
        .arg("--mcp")
        .arg("--path")
        .arg(root)
        .env(DAEMON_INTERNAL_ENV, "1")
        .stdin(Stdio::null());

    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(crate::directory::get_codegraph_dir(root).join("daemon.log"))
        .ok();
    match log_file.and_then(|file| file.try_clone().ok().map(|clone| (file, clone))) {
        Some((stdout, stderr)) => {
            command.stdout(Stdio::from(stdout));
            command.stderr(Stdio::from(stderr));
        }
        None => {
            command.stdout(Stdio::null());
            command.stderr(Stdio::null());
        }
    }

    // SAFETY: the child hook only invokes async-signal-safe setsid before exec.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    command.spawn().map_err(|error| error.to_string())?;
    Ok(())
}
