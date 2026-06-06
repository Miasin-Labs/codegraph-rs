//! Message types exchanged between the shimmer progress front-end and its
//! render worker thread. Ported from `src/ui/types.ts`.
//!
//! The serde attributes preserve the TS discriminated-union wire shape
//! (`{ "type": "update", "phaseName": ... }`) for parity, even though the
//! Rust port sends these over an in-process channel rather than
//! `worker_threads.postMessage`.

use serde::{Deserialize, Serialize};

/// Messages from main thread to worker
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ShimmerWorkerMessage {
    #[serde(rename_all = "camelCase")]
    Update {
        phase: String,
        phase_name: String,
        percent: i32,
        count: u64,
    },
    FinishPhase,
    Stop,
}

/// Messages from worker to main thread
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ShimmerMainMessage {
    Stopped,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_message_wire_shape_matches_ts() {
        let msg = ShimmerWorkerMessage::Update {
            phase: "parsing".to_string(),
            phase_name: "Parsing code".to_string(),
            percent: 42,
            count: 0,
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "update");
        assert_eq!(json["phase"], "parsing");
        assert_eq!(json["phaseName"], "Parsing code");
        assert_eq!(json["percent"], 42);
        assert_eq!(json["count"], 0);

        let json = serde_json::to_value(ShimmerWorkerMessage::FinishPhase).unwrap();
        assert_eq!(json["type"], "finish-phase");

        let json = serde_json::to_value(ShimmerWorkerMessage::Stop).unwrap();
        assert_eq!(json["type"], "stop");

        let json = serde_json::to_value(ShimmerMainMessage::Stopped).unwrap();
        assert_eq!(json["type"], "stopped");
    }
}
