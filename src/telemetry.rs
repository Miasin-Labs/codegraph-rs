//! Anonymous, aggregate usage telemetry with explicit local controls.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const TELEMETRY_ENDPOINT: &str = "https://telemetry.getcodegraph.com/v1/events";
pub const TELEMETRY_DOCS: &str =
    "https://github.com/Miasin-Labs/codegraph-rs/blob/main/TELEMETRY.md";
const SCHEMA_VERSION: u32 = 2;
const MAX_BUFFER_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetryDecision {
    DoNotTrack,
    Environment,
    Config,
    Default,
}

impl TelemetryDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DoNotTrack => "DO_NOT_TRACK environment variable",
            Self::Environment => "CODEGRAPH_TELEMETRY environment variable",
            Self::Config => "your saved choice",
            Self::Default => "default",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelemetryStatus {
    pub enabled: bool,
    pub decided_by: TelemetryDecision,
    pub machine_id: Option<String>,
    pub config_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Config {
    enabled: bool,
    machine_id: String,
    consent_source: String,
    #[serde(default)]
    first_run_notice_shown: bool,
    updated_at: String,
}

#[derive(Debug, Clone)]
pub struct Telemetry {
    dir: PathBuf,
}

impl Default for Telemetry {
    fn default() -> Self {
        let dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".codegraph");
        Self { dir }
    }
}

impl Telemetry {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn config_path(&self) -> PathBuf {
        self.dir.join("telemetry.json")
    }

    pub fn queue_path(&self) -> PathBuf {
        self.dir.join("telemetry-queue.jsonl")
    }

    fn read_config(&self) -> Option<Config> {
        let value = fs::read_to_string(self.config_path()).ok()?;
        serde_json::from_str(&value).ok()
    }

    pub fn status(&self) -> TelemetryStatus {
        let config = self.read_config();
        let machine_id = config.as_ref().map(|config| config.machine_id.clone());
        if env_truthy("DO_NOT_TRACK") {
            return TelemetryStatus {
                enabled: false,
                decided_by: TelemetryDecision::DoNotTrack,
                machine_id,
                config_path: self.config_path(),
            };
        }
        if let Ok(value) = std::env::var("CODEGRAPH_TELEMETRY") {
            if !value.is_empty() {
                return TelemetryStatus {
                    enabled: !matches!(value.to_ascii_lowercase().as_str(), "0" | "false"),
                    decided_by: TelemetryDecision::Environment,
                    machine_id,
                    config_path: self.config_path(),
                };
            }
        }
        if let Some(config) = config {
            return TelemetryStatus {
                enabled: config.enabled,
                decided_by: TelemetryDecision::Config,
                machine_id: Some(config.machine_id),
                config_path: self.config_path(),
            };
        }
        TelemetryStatus {
            enabled: true,
            decided_by: TelemetryDecision::Default,
            machine_id: None,
            config_path: self.config_path(),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.status().enabled
    }

    pub fn set_enabled(&self, enabled: bool, source: &str) {
        let existing = self.read_config();
        let config = Config {
            enabled,
            machine_id: existing
                .as_ref()
                .map(|config| config.machine_id.clone())
                .unwrap_or_else(machine_id),
            consent_source: source.to_string(),
            first_run_notice_shown: true,
            updated_at: iso_now(),
        };
        let _ = write_private_json(&self.config_path(), &config);
        if !enabled {
            self.delete_buffered_data();
        }
    }

    pub fn record_usage(&self, kind: &str, name: &str, ok: bool) {
        if !self.is_enabled() {
            return;
        }
        let Some(kind) = safe_identifier(kind, 32) else {
            return;
        };
        let Some(name) = safe_identifier(name, 64) else {
            return;
        };
        let line = json!({
            "v": SCHEMA_VERSION,
            "d": utc_day(),
            "k": kind,
            "n": name,
            "c": 1,
            "e": if ok { 0 } else { 1 },
        });
        self.append_line(&line);
    }

    pub fn record_lifecycle(&self, event: &str, props: Value) {
        if !self.is_enabled() {
            return;
        }
        let Some(event) = safe_identifier(event, 32) else {
            return;
        };
        let Some(props) = sanitize_props(props) else {
            return;
        };
        self.append_line(&json!({
            "v": SCHEMA_VERSION,
            "ev": event,
            "ts": iso_now(),
            "props": props,
        }));
    }

    fn append_line(&self, line: &Value) {
        let Ok(serialized) = serde_json::to_string(line) else {
            return;
        };
        if fs::create_dir_all(&self.dir).is_err() {
            return;
        }
        let path = self.queue_path();
        let mut contents = fs::read(&path).unwrap_or_default();
        contents.extend_from_slice(serialized.as_bytes());
        contents.push(b'\n');
        if contents.len() > MAX_BUFFER_BYTES {
            let excess = contents.len() - MAX_BUFFER_BYTES;
            let cut = contents[excess..]
                .iter()
                .position(|byte| *byte == b'\n')
                .map(|offset| excess + offset + 1)
                .unwrap_or(contents.len());
            contents.drain(..cut);
        }
        let _ = write_private_bytes(&path, &contents);
    }

    fn delete_buffered_data(&self) {
        let _ = fs::remove_file(self.queue_path());
        if let Ok(entries) = fs::read_dir(&self.dir) {
            for entry in entries.flatten() {
                if entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("telemetry-queue.sending.")
                {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
    }

    /// Flush completed-day rollups and lifecycle events. All failures are silent;
    /// unsent lines are restored for a future process.
    pub fn flush(&self) {
        if !self.is_enabled() || !command_exists("curl") {
            return;
        }
        let queue = self.queue_path();
        let claim = self.dir.join(format!(
            "telemetry-queue.sending.{}.jsonl",
            std::process::id()
        ));
        if fs::rename(&queue, &claim).is_err() {
            return;
        }
        let raw = fs::read_to_string(&claim).unwrap_or_default();
        let today = utc_day();
        let mut keep = Vec::new();
        let mut counts: BTreeMap<(String, String, String), (u64, u64)> = BTreeMap::new();
        let mut events = Vec::new();
        for line in raw.lines() {
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if value.get("v").and_then(Value::as_u64) != Some(SCHEMA_VERSION as u64) {
                continue;
            }
            if let Some(event) = value.get("ev").and_then(Value::as_str) {
                events.push(json!({
                    "event": event,
                    "ts": value.get("ts").cloned().unwrap_or(Value::Null),
                    "props": value.get("props").cloned().unwrap_or_else(|| json!({})),
                }));
                continue;
            }
            let day = value.get("d").and_then(Value::as_str).unwrap_or_default();
            if day >= today.as_str() {
                keep.push(value);
                continue;
            }
            let kind = value.get("k").and_then(Value::as_str).unwrap_or_default();
            let name = value.get("n").and_then(Value::as_str).unwrap_or_default();
            let entry = counts
                .entry((day.to_string(), kind.to_string(), name.to_string()))
                .or_default();
            entry.0 += value.get("c").and_then(Value::as_u64).unwrap_or(0);
            entry.1 += value.get("e").and_then(Value::as_u64).unwrap_or(0);
        }
        for ((day, kind, name), (count, errors)) in counts {
            events.push(json!({
                "event": "usage_rollup",
                "ts": format!("{day}T12:00:00Z"),
                "props": { "kind": kind, "name": name, "count": count, "error_count": errors },
            }));
        }
        if events.is_empty() {
            restore_queue(&claim, &queue, &keep);
            return;
        }

        let config = self.ensure_notice_config();
        let endpoint = std::env::var("CODEGRAPH_TELEMETRY_ENDPOINT")
            .unwrap_or_else(|_| TELEMETRY_ENDPOINT.to_string());
        let body = json!({
            "machine_id": config.machine_id,
            "codegraph_version": env!("CARGO_PKG_VERSION"),
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "schema_version": SCHEMA_VERSION,
            "events": events,
        });
        let success = Command::new("curl")
            .args([
                "-fsS",
                "--max-time",
                "1.5",
                "-H",
                "content-type: application/json",
                "--data-binary",
                "@-",
                &endpoint,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .and_then(|mut child| {
                if let Some(stdin) = child.stdin.as_mut() {
                    let _ = serde_json::to_writer(stdin, &body);
                }
                child.wait()
            })
            .is_ok_and(|status| status.success());
        if success {
            restore_queue(&claim, &queue, &keep);
        } else {
            let all = raw
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .collect::<Vec<_>>();
            restore_queue(&claim, &queue, &all);
        }
    }

    fn ensure_notice_config(&self) -> Config {
        if let Some(mut config) = self.read_config() {
            if !config.first_run_notice_shown {
                config.first_run_notice_shown = true;
                config.updated_at = iso_now();
                let _ = write_private_json(&self.config_path(), &config);
                eprintln!(
                    "codegraph collects anonymous usage stats (no code, paths, or names); `codegraph telemetry off` disables. Details: {TELEMETRY_DOCS}"
                );
            }
            return config;
        }
        let config = Config {
            enabled: true,
            machine_id: machine_id(),
            consent_source: "default-notice".into(),
            first_run_notice_shown: true,
            updated_at: iso_now(),
        };
        let _ = write_private_json(&self.config_path(), &config);
        eprintln!(
            "codegraph collects anonymous usage stats (no code, paths, or names); `codegraph telemetry off` disables. Details: {TELEMETRY_DOCS}"
        );
        config
    }
}

fn env_truthy(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| {
        !value.is_empty() && !matches!(value.to_ascii_lowercase().as_str(), "0" | "false")
    })
}

fn safe_identifier(value: &str, max: usize) -> Option<String> {
    (!value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')))
    .then(|| value.to_string())
}

fn sanitize_props(props: Value) -> Option<Value> {
    let object = props.as_object()?;
    let mut sanitized = serde_json::Map::new();
    for (key, value) in object {
        let key = safe_identifier(key, 32)?;
        let value = match value {
            Value::String(value) => Value::String(safe_identifier(value, 64)?),
            Value::Bool(_) | Value::Number(_) | Value::Null => value.clone(),
            Value::Array(_) | Value::Object(_) => return None,
        };
        sanitized.insert(key, value);
    }
    Some(Value::Object(sanitized))
}

fn command_exists(command: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| {
            let candidate = dir.join(command);
            candidate.is_file()
        })
    })
}

fn restore_queue(claim: &Path, queue: &Path, values: &[Value]) {
    if !values.is_empty() {
        if let Some(parent) = queue.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        if let Ok(mut file) = options.open(queue) {
            for value in values {
                let _ = serde_json::to_writer(&mut file, value);
                let _ = file.write_all(b"\n");
            }
        }
    }
    let _ = fs::remove_file(claim);
}

fn write_private_bytes(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut options = OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)
}

fn write_private_json(path: &Path, value: &impl Serialize) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut options = OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")
}

fn machine_id() -> String {
    let mut bytes = [0u8; 16];
    let random = File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .is_ok();
    if !random {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        SystemTime::now().hash(&mut hasher);
        std::process::id().hash(&mut hasher);
        std::thread::current().id().hash(&mut hasher);
        let first = hasher.finish();
        first.hash(&mut hasher);
        let second = hasher.finish();
        bytes[..8].copy_from_slice(&first.to_be_bytes());
        bytes[8..].copy_from_slice(&second.to_be_bytes());
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}

fn unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month as u32, day as u32)
}

fn utc_day() -> String {
    let (year, month, day) = civil_from_days(unix_seconds().div_euclid(86_400));
    format!("{year:04}-{month:02}-{day:02}")
}

fn iso_now() -> String {
    let seconds = unix_seconds();
    let (year, month, day) = civil_from_days(seconds.div_euclid(86_400));
    let within = seconds.rem_euclid(86_400);
    let hour = within / 3_600;
    let minute = within % 3_600 / 60;
    let second = within % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn explicit_off_deletes_buffered_data() {
        let dir = tempdir().unwrap();
        let telemetry = Telemetry::new(dir.path());
        telemetry.record_usage("cli_command", "status", true);
        assert!(telemetry.queue_path().exists());
        telemetry.set_enabled(false, "cli");
        assert!(!telemetry.queue_path().exists());
        assert!(!telemetry.status().enabled);
    }

    #[test]
    fn rejects_names_that_could_leak_paths_or_prompts() {
        let dir = tempdir().unwrap();
        let telemetry = Telemetry::new(dir.path());
        telemetry.record_usage("cli_command", "/home/user/secret.rs", true);
        assert!(!telemetry.queue_path().exists());
    }

    #[test]
    fn rejects_lifecycle_properties_that_could_leak_paths() {
        let dir = tempdir().unwrap();
        let telemetry = Telemetry::new(dir.path());
        telemetry.record_lifecycle("index_complete", json!({ "project": "/home/user/private" }));
        assert!(!telemetry.queue_path().exists());
    }

    #[test]
    fn machine_ids_have_uuid_v4_shape() {
        let id = machine_id();
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4");
        assert!(matches!(&id[19..20], "8" | "9" | "a" | "b"));
    }
}
