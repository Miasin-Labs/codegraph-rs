//! Shimmer progress UI — animated phase/progress display during indexing.
//!
//! Ported from `src/ui/shimmer-progress.ts` (front-end) and
//! `src/ui/shimmer-worker.ts` (render worker). The Node `worker_threads`
//! Worker becomes a `std::thread` fed over a `crossbeam-channel`; the
//! ANSI escape logic is ported verbatim (the escape strings were already
//! inlined in the TS worker — no `sisteransi` usage to translate; the
//! `fast-string-width` dependency in package.json is unused by the UI, so
//! no string-width calculation is needed here).
//!
//! Why a dedicated thread: in Node, `process.stdout` writes from a worker
//! are proxied through the main thread's event loop — so if the main
//! thread is blocked (e.g. SQLite), the animation freezes; the TS worker
//! bypasses that with `fs.writeSync(1, ...)`. In Rust the render thread
//! writes to stdout directly (a plain syscall), which achieves the same
//! independence from the indexing thread.

use std::io::Write;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, unbounded};

use super::glyphs::{Glyphs, get_glyphs};
use super::types::{ShimmerMainMessage, ShimmerWorkerMessage};

/// Phase id → human-readable display name (mirrors `PHASE_NAMES`).
const PHASE_NAMES: &[(&str, &str)] = &[
    ("scanning", "Scanning files"),
    ("parsing", "Parsing code"),
    ("storing", "Storing data"),
    ("resolving", "Resolving refs"),
];

/// Progress callback payload (mirrors the `IndexProgress` interface
/// declared in `shimmer-progress.ts`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexProgress {
    pub phase: String,
    pub current: u64,
    pub total: u64,
}

// ============================================================
// Worker (port of shimmer-worker.ts)
// ============================================================

const ANIM_INTERVAL: i64 = 150;
const FRAMES_PER_GLYPH: i64 = 3;
/// Render tick — the TS worker re-renders on a 50 ms `setInterval`.
const TICK_MS: u64 = 50;

const RST: &str = "\x1b[0m";
const DM: &str = "\x1b[2m";
const GRN: &str = "\x1b[32m";
const BOLD: &str = "\x1b[1m";

/// Epoch milliseconds (`Date.now()` parity).
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// Write directly to stdout and flush. The TS worker uses
/// `fs.writeSync(1, ...)` to bypass the main-thread event-loop proxy; a
/// direct locked write + flush from this thread is the Rust equivalent.
fn write_stdout(s: &str) {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = lock.write_all(s.as_bytes());
    let _ = lock.flush();
}

fn anim_frame(start_time: i64) -> i64 {
    (now_ms() - start_time) / ANIM_INTERVAL
}

fn lerp(a: i64, b: i64, t: f64) -> i64 {
    (a as f64 + (b - a) as f64 * t).round() as i64
}

fn shimmer_color(frame: i64) -> String {
    let t = ((frame as f64 * 2.0 * std::f64::consts::PI / 13.0).sin() + 1.0) / 2.0;
    let r = lerp(160, 251, t);
    let g = lerp(100, 191, t);
    let b = lerp(9, 36, t);
    format!("\x1b[38;2;{r};{g};{b}m{BOLD}")
}

/// `n.toLocaleString()` parity — comma-grouped thousands (en-US style,
/// which is what Node's default locale renders in the published CLI).
fn format_number(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn render_bar(frame: i64, filled: i64, empty: i64, g: &Glyphs) -> String {
    if filled == 0 {
        return format!("{DM}{}{RST}", g.bar_empty.repeat(empty.max(0) as usize));
    }
    let cycle_frames = 24_i64;
    let shimmer_pos =
        ((frame % cycle_frames) as f64 / cycle_frames as f64) * (filled + 6) as f64 - 3.0;
    let shimmer_width = 3.0_f64;
    let mut bar = String::new();
    for i in 0..filled {
        let dist = (i as f64 - shimmer_pos).abs();
        let t = (1.0 - dist / shimmer_width).max(0.0);
        let r = lerp(160, 251, t);
        let gc = lerp(100, 191, t);
        let b = lerp(9, 36, t);
        bar.push_str(&format!("\x1b[38;2;{r};{gc};{b}m{BOLD}{}", g.bar_filled));
    }
    bar.push_str(&format!(
        "{RST}{DM}{}{RST}",
        g.bar_empty.repeat(empty.max(0) as usize)
    ));
    bar
}

/// Mutable render state (the TS worker's module-level `let`s).
struct WorkerState {
    current_message: String,
    current_percent: i32,
    current_count: u64,
}

fn render(state: &WorkerState, start_time: i64, g: &Glyphs) {
    if state.current_message.is_empty() {
        return;
    }
    let frame = anim_frame(start_time);
    let glyph_idx = ((frame / FRAMES_PER_GLYPH) % g.spinner.len() as i64) as usize;
    let glyph = g
        .spinner
        .get(glyph_idx)
        .or_else(|| g.spinner.first())
        .copied()
        .unwrap_or(".");
    let color = shimmer_color(frame);
    let message = &state.current_message;
    let rail = g.rail;

    let line = if state.current_percent >= 0 {
        let bar_width = 25_i64;
        let filled = (bar_width as f64 * state.current_percent as f64 / 100.0).round() as i64;
        let empty = bar_width - filled;
        format!(
            "{DM}{rail}{RST}  {color}{glyph}{RST} {message}  {}  {}%",
            render_bar(frame, filled, empty, g),
            state.current_percent
        )
    } else if state.current_count > 0 {
        format!(
            "{DM}{rail}{RST}  {color}{glyph}{RST} {message}... {} found",
            format_number(state.current_count)
        )
    } else {
        format!("{DM}{rail}{RST}  {color}{glyph}{RST} {message}...")
    };

    write_stdout(&format!("\r\x1b[K{line}"));
}

fn finish_phase(state: &mut WorkerState, g: &Glyphs) {
    if state.current_message.is_empty() {
        return;
    }
    write_stdout("\r\x1b[K");
    let mut detail = String::new();
    if state.current_percent >= 0 {
        detail = format!(" {} done", g.dash);
    } else if state.current_count > 0 {
        detail = format!(" {} {} found", g.dash, format_number(state.current_count));
    }
    write_stdout(&format!(
        "{DM}{}{RST}  {GRN}{}{RST} {}{detail}\n",
        g.rail, g.phase_done, state.current_message
    ));
    state.current_message = String::new();
    state.current_percent = -1;
    state.current_count = 0;
}

/// Render loop — independent of the indexing thread. Re-renders every
/// 50 ms (the TS `setInterval`) and reacts to channel messages.
fn worker_loop(
    start_time: i64,
    rx: Receiver<ShimmerWorkerMessage>,
    tx: Sender<ShimmerMainMessage>,
) {
    let g = get_glyphs();
    let mut state = WorkerState {
        current_message: String::new(),
        current_percent: -1,
        current_count: 0,
    };
    let tick = Duration::from_millis(TICK_MS);
    let mut next_tick = Instant::now() + tick;

    loop {
        let timeout = next_tick.saturating_duration_since(Instant::now());
        match rx.recv_timeout(timeout) {
            Ok(ShimmerWorkerMessage::Update {
                phase: _,
                phase_name,
                percent,
                count,
            }) => {
                state.current_message = phase_name;
                state.current_percent = percent;
                state.current_count = count;
            }
            Ok(ShimmerWorkerMessage::FinishPhase) => {
                finish_phase(&mut state, g);
            }
            Ok(ShimmerWorkerMessage::Stop) => {
                finish_phase(&mut state, g);
                let _ = tx.send(ShimmerMainMessage::Stopped);
                break;
            }
            Err(RecvTimeoutError::Timeout) => {
                render(&state, start_time, g);
                next_tick += tick;
                let now = Instant::now();
                if next_tick < now {
                    next_tick = now + tick;
                }
            }
            // Front-end dropped without stop() — exit quietly
            // (worker.terminate() equivalent).
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

// ============================================================
// Front-end (port of shimmer-progress.ts)
// ============================================================

/// Handle to the shimmer progress display (the TS `ShimmerProgress`
/// object literal with `onProgress`/`stop`).
pub struct ShimmerProgress {
    last_phase: String,
    tx: Sender<ShimmerWorkerMessage>,
    rx: Receiver<ShimmerMainMessage>,
    handle: Option<JoinHandle<()>>,
}

/// Spawn the render worker thread and return the progress handle
/// (mirrors `createShimmerProgress()`).
pub fn create_shimmer_progress() -> ShimmerProgress {
    let (tx_worker, rx_worker) = unbounded::<ShimmerWorkerMessage>();
    let (tx_main, rx_main) = unbounded::<ShimmerMainMessage>();
    let start_time = now_ms(); // workerData.startTime

    let handle = std::thread::Builder::new()
        .name("shimmer-worker".to_string())
        .spawn(move || worker_loop(start_time, rx_worker, tx_main))
        .ok();

    ShimmerProgress {
        last_phase: String::new(),
        tx: tx_worker,
        rx: rx_main,
        handle,
    }
}

impl ShimmerProgress {
    /// Feed an indexing progress update to the renderer.
    pub fn on_progress(&mut self, progress: &IndexProgress) {
        let phase_name = PHASE_NAMES
            .iter()
            .find(|(id, _)| *id == progress.phase)
            .map(|(_, name)| (*name).to_string())
            .unwrap_or_else(|| progress.phase.clone());

        if progress.phase != self.last_phase && !self.last_phase.is_empty() {
            let _ = self.tx.send(ShimmerWorkerMessage::FinishPhase);
        }
        self.last_phase = progress.phase.clone();

        let mut percent: i32 = -1;
        let mut count: u64 = 0;
        if progress.total > 0 {
            percent = ((progress.current as f64 / progress.total as f64) * 100.0).round() as i32;
        } else if progress.current > 0 {
            count = progress.current;
        }

        let _ = self.tx.send(ShimmerWorkerMessage::Update {
            phase: progress.phase.clone(),
            phase_name,
            percent,
            count,
        });
    }

    /// Stop the renderer: finish the current phase, wait (up to the same
    /// 2-second timeout as the TS version) for the worker's `stopped`
    /// acknowledgment, then join. On timeout the thread is detached —
    /// the closest equivalent of `worker.terminate()`.
    pub fn stop(mut self) {
        let _ = self.tx.send(ShimmerWorkerMessage::Stop);
        match self.rx.recv_timeout(Duration::from_millis(2000)) {
            Ok(ShimmerMainMessage::Stopped) => {
                if let Some(handle) = self.handle.take() {
                    let _ = handle.join();
                }
            }
            Err(_) => {
                // Timed out or worker gone — detach (terminate-equivalent).
                self.handle.take();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_number_groups_thousands_like_to_locale_string() {
        assert_eq!(format_number(0), "0");
        assert_eq!(format_number(999), "999");
        assert_eq!(format_number(1000), "1,000");
        assert_eq!(format_number(1234567), "1,234,567");
    }

    #[test]
    fn lerp_matches_math_round_behavior() {
        assert_eq!(lerp(160, 251, 0.0), 160);
        assert_eq!(lerp(160, 251, 1.0), 251);
        assert_eq!(lerp(0, 10, 0.55), 6); // 5.5 rounds half-up like Math.round
    }

    #[test]
    fn shimmer_color_emits_truecolor_escape_with_bold() {
        let c = shimmer_color(0);
        assert!(c.starts_with("\x1b[38;2;"));
        assert!(c.ends_with(BOLD));
        // frame 0 → t = 0.5 exactly
        assert_eq!(c, format!("\x1b[38;2;{};{};{}m{BOLD}", 206, 146, 23));
    }

    #[test]
    fn render_bar_with_zero_filled_is_all_empty_dim() {
        let g = &crate::ui::glyphs::ASCII_GLYPHS;
        let bar = render_bar(0, 0, 5, g);
        assert_eq!(bar, format!("{DM}-----{RST}"));
    }

    #[test]
    fn render_bar_emits_filled_then_empty_segments() {
        let g = &crate::ui::glyphs::ASCII_GLYPHS;
        let bar = render_bar(0, 3, 2, g);
        assert_eq!(bar.matches('#').count(), 3);
        assert!(bar.ends_with(&format!("{RST}{DM}--{RST}")));
    }

    #[test]
    fn phase_names_map_known_phases() {
        let lookup = |p: &str| PHASE_NAMES.iter().find(|(id, _)| *id == p).map(|(_, n)| *n);
        assert_eq!(lookup("scanning"), Some("Scanning files"));
        assert_eq!(lookup("parsing"), Some("Parsing code"));
        assert_eq!(lookup("storing"), Some("Storing data"));
        assert_eq!(lookup("resolving"), Some("Resolving refs"));
        assert_eq!(lookup("custom-phase"), None);
    }

    #[test]
    fn stop_completes_within_timeout_after_updates() {
        let mut progress = create_shimmer_progress();
        progress.on_progress(&IndexProgress {
            phase: "scanning".to_string(),
            current: 10,
            total: 0,
        });
        progress.on_progress(&IndexProgress {
            phase: "parsing".to_string(),
            current: 5,
            total: 10,
        });
        let started = Instant::now();
        progress.stop();
        assert!(started.elapsed() < Duration::from_millis(2000));
    }

    #[test]
    fn dropping_without_stop_does_not_hang() {
        let progress = create_shimmer_progress();
        drop(progress); // channel disconnect ends the worker loop
    }
}
