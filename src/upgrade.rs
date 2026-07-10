//! Self-update support for the Rust CLI.

use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const REPOSITORY: &str = "Miasin-Labs/codegraph-rs";
pub const REPOSITORY_URL: &str = "https://github.com/Miasin-Labs/codegraph-rs";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallMethod {
    Cargo,
    Source { root: PathBuf },
    Unknown { executable: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Semver {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub pre: Option<String>,
}

pub fn parse_semver(value: &str) -> Option<Semver> {
    let value = value.trim().strip_prefix('v').unwrap_or(value.trim());
    let core = value.split('+').next()?;
    let (numbers, pre) = core.split_once('-').map_or((core, None), |(numbers, pre)| {
        (numbers, Some(pre.to_string()))
    });
    let mut parts = numbers.split('.');
    let version = Semver {
        major: parts.next()?.parse().ok()?,
        minor: parts.next()?.parse().ok()?,
        patch: parts.next()?.parse().ok()?,
        pre,
    };
    parts.next().is_none().then_some(version)
}

pub fn compare_versions(left: &str, right: &str) -> Option<std::cmp::Ordering> {
    let left = parse_semver(left)?;
    let right = parse_semver(right)?;
    Some(
        (left.major, left.minor, left.patch)
            .cmp(&(right.major, right.minor, right.patch))
            .then_with(|| match (&left.pre, &right.pre) {
                (None, None) => std::cmp::Ordering::Equal,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (Some(_), None) => std::cmp::Ordering::Less,
                (Some(a), Some(b)) => a.cmp(b),
            }),
    )
}

pub fn normalize_version(value: &str) -> String {
    let value = value.trim();
    if value.starts_with('v') {
        value.to_string()
    } else {
        format!("v{value}")
    }
}

pub fn is_update_available(current: &str, target: &str) -> bool {
    compare_versions(target, current)
        .map(|ordering| ordering.is_gt())
        .unwrap_or_else(|| normalize_version(current) != normalize_version(target))
}

fn source_root_from(executable: &Path) -> Option<PathBuf> {
    executable.ancestors().find_map(|candidate| {
        (candidate.join(".git").exists() && candidate.join("Cargo.toml").exists())
            .then(|| candidate.to_path_buf())
    })
}

pub fn detect_install_method(executable: &Path) -> InstallMethod {
    if let Some(root) = source_root_from(executable) {
        return InstallMethod::Source { root };
    }
    let normalized = executable.to_string_lossy().replace('\\', "/");
    if normalized.contains("/.cargo/bin/") {
        InstallMethod::Cargo
    } else {
        InstallMethod::Unknown {
            executable: executable.to_path_buf(),
        }
    }
}

fn run_capture(program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .stderr(Stdio::null())
        .output()
        .map_err(|error| error.to_string())?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
        .ok_or_else(|| format!("{program} exited with {}", output.status))
}

fn latest_from_redirect() -> Option<String> {
    let url = format!("{REPOSITORY_URL}/releases/latest");
    let headers = run_capture(
        "curl",
        &["-fsSI", "--max-time", "12", "-A", "codegraph-upgrade", &url],
    )
    .ok()?;
    headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if !name.eq_ignore_ascii_case("location") {
            return None;
        }
        let tag = value.trim().split("/releases/tag/").nth(1)?;
        Some(normalize_version(tag.trim_end_matches('/')))
    })
}

fn latest_from_api() -> Option<String> {
    let url = format!("https://api.github.com/repos/{REPOSITORY}/releases/latest");
    let body = run_capture(
        "curl",
        &[
            "-fsSL",
            "--max-time",
            "12",
            "-A",
            "codegraph-upgrade",
            "-H",
            "Accept: application/vnd.github+json",
            &url,
        ],
    )
    .ok()?;
    let value: serde_json::Value = serde_json::from_str(&body).ok()?;
    value
        .get("tag_name")
        .and_then(|tag| tag.as_str())
        .map(normalize_version)
}

pub fn resolve_latest_version() -> Result<String, String> {
    latest_from_redirect()
        .or_else(latest_from_api)
        .ok_or_else(|| {
            "could not resolve the latest release from GitHub; check the network or pin a version"
                .to_string()
        })
}

pub fn run_upgrade(version: Option<&str>, check: bool, force: bool) -> i32 {
    let current = env!("CARGO_PKG_VERSION");
    let target = match version
        .map(normalize_version)
        .or_else(|| {
            env::var("CODEGRAPH_VERSION")
                .ok()
                .map(|v| normalize_version(&v))
        })
        .map(Ok)
        .unwrap_or_else(resolve_latest_version)
    {
        Ok(target) => target,
        Err(error) => {
            eprintln!("Error: {error}");
            return 1;
        }
    };

    println!(
        "CodeGraph  current {}  {} {}",
        normalize_version(current),
        if version.is_some() {
            "target"
        } else {
            "latest"
        },
        target
    );
    let available = is_update_available(current, &target);
    if check {
        if available {
            println!(
                "An update is available: {} -> {target}",
                normalize_version(current)
            );
        } else {
            println!(
                "You are on the latest version ({}).",
                normalize_version(current)
            );
        }
        return 0;
    }
    if !available && !force && version.is_none() {
        println!("Already up to date ({}).", normalize_version(current));
        return 0;
    }

    let executable = env::current_exe().unwrap_or_else(|_| PathBuf::from("codegraph"));
    match detect_install_method(&executable) {
        InstallMethod::Cargo => {
            let status = Command::new("cargo")
                .args([
                    "install",
                    "--git",
                    REPOSITORY_URL,
                    "--tag",
                    &target,
                    "--force",
                    "codegraph-rs",
                ])
                .status();
            match status {
                Ok(status) if status.success() => {
                    println!("Upgrade complete. Refresh indexes with `codegraph sync`.");
                    0
                }
                Ok(status) => {
                    eprintln!("Error: cargo install exited with {status}");
                    1
                }
                Err(error) => {
                    eprintln!("Error: failed to launch cargo: {error}");
                    1
                }
            }
        }
        InstallMethod::Source { root } => {
            eprintln!("Running from a source checkout at {}.", root.display());
            println!("Upgrade it with: git pull && cargo build --release");
            0
        }
        InstallMethod::Unknown { executable } => {
            eprintln!(
                "Error: could not determine how CodeGraph was installed ({})",
                executable.display()
            );
            println!("Reinstall manually from {REPOSITORY_URL}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_compares_semver() {
        assert_eq!(parse_semver("v1.3.1").unwrap().minor, 3);
        assert!(is_update_available("1.3.0", "v1.3.1"));
        assert!(!is_update_available("1.3.1", "v1.3.1"));
        assert_eq!(
            compare_versions("1.0.0-rc.1", "1.0.0"),
            Some(std::cmp::Ordering::Less)
        );
    }

    #[test]
    fn detects_cargo_install_layout() {
        assert_eq!(
            detect_install_method(Path::new("/home/user/.cargo/bin/codegraph")),
            InstallMethod::Cargo
        );
    }
}
