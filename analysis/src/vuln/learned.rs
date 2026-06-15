//! The self-learning store: inferred predicate roles persisted with confidence
//! that strengthens as independent mechanisms agree and decays when stale.
//!
//! This is what makes the engine improve over time rather than recompute from
//! scratch. Each mechanism — frequency mining, name lexicon, fix-history, an
//! LLM verdict — calls [`LearnedStore::observe`] with the role it inferred and
//! its origin. Confidence combines by **noisy-OR** (`1 − ∏(1 − cᵢ)`), the
//! correct way to fuse independent evidence: two mechanisms that each say
//! "0.6 this is a guard" yield 0.84, not 0.6. [`LearnedStore::decay`] lets a
//! periodic pass forget roles that stop being reinforced.
//!
//! Keyed by `qualified_name` (stable across re-index) rather than the hashed
//! `NodeId`, so the learned memory survives reindexing and file moves that
//! preserve the symbol path. Serializes as JSON for on-disk persistence by the
//! root crate.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::InferenceOrigin;

/// One learned role for one symbol, with fused confidence and provenance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LearnedRole {
    pub qualified_name: String,
    /// `"source" | "sink" | "guard" | "sanitizer"`.
    pub role: String,
    /// Fused confidence in `[0, 1]`.
    pub confidence: f64,
    /// Origin ids that have contributed (deduped, sorted).
    pub origins: Vec<String>,
    /// Number of observations fused in.
    pub support: u32,
    /// Graph revision at last reinforcement (for staleness/decay policy).
    pub last_seen_rev: u64,
}

/// Persistent, confidence-weighted memory of inferred predicate roles.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LearnedStore {
    /// Keyed by `"{role}\u{1f}{qualified_name}"`.
    roles: HashMap<String, LearnedRole>,
    /// Bumped on schema changes to the stored shape.
    pub format_version: u32,
}

fn key(role: &str, qualified_name: &str) -> String {
    format!("{role}\u{1f}{qualified_name}")
}

impl LearnedStore {
    pub fn new() -> Self {
        Self {
            roles: HashMap::new(),
            format_version: 1,
        }
    }

    /// Fuse one observation. Confidence combines by noisy-OR with whatever is
    /// already known; origin is recorded; support and `last_seen_rev` advance.
    pub fn observe(
        &mut self,
        qualified_name: &str,
        role: &str,
        origin: InferenceOrigin,
        confidence: f64,
        rev: u64,
    ) {
        let confidence = confidence.clamp(0.0, 1.0);
        let entry = self
            .roles
            .entry(key(role, qualified_name))
            .or_insert_with(|| LearnedRole {
                qualified_name: qualified_name.to_owned(),
                role: role.to_owned(),
                confidence: 0.0,
                origins: Vec::new(),
                support: 0,
                last_seen_rev: 0,
            });
        // noisy-OR: 1 - (1-old)*(1-new)
        entry.confidence = 1.0 - (1.0 - entry.confidence) * (1.0 - confidence);
        entry.support += 1;
        entry.last_seen_rev = entry.last_seen_rev.max(rev);
        let origin_id = origin.id().to_owned();
        if !entry.origins.contains(&origin_id) {
            entry.origins.push(origin_id);
            entry.origins.sort();
        }
    }

    /// Current fused confidence for a `(qualified_name, role)`, or `0.0`.
    pub fn confidence(&self, qualified_name: &str, role: &str) -> f64 {
        self.roles
            .get(&key(role, qualified_name))
            .map(|r| r.confidence)
            .unwrap_or(0.0)
    }

    /// Multiply every confidence by `factor` (in `(0, 1]`) — periodic forgetting
    /// so roles that stop being reinforced fade. Entries that fall below
    /// `floor` are dropped entirely.
    pub fn decay(&mut self, factor: f64, floor: f64) {
        let factor = factor.clamp(0.0, 1.0);
        for r in self.roles.values_mut() {
            r.confidence *= factor;
        }
        self.roles.retain(|_, r| r.confidence >= floor);
    }

    /// Top-`n` symbols for a role, highest confidence first.
    pub fn top(&self, role: &str, n: usize) -> Vec<&LearnedRole> {
        let mut v: Vec<&LearnedRole> = self.roles.values().filter(|r| r.role == role).collect();
        v.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.qualified_name.cmp(&b.qualified_name))
        });
        v.truncate(n);
        v
    }

    /// All roles meeting a confidence threshold, for a given role kind.
    pub fn confident(&self, role: &str, min_confidence: f64) -> Vec<&LearnedRole> {
        let mut v: Vec<&LearnedRole> = self
            .roles
            .values()
            .filter(|r| r.role == role && r.confidence >= min_confidence)
            .collect();
        v.sort_by(|a, b| a.qualified_name.cmp(&b.qualified_name));
        v
    }

    pub fn len(&self) -> usize {
        self.roles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.roles.is_empty()
    }

    /// Serialize for on-disk persistence.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Load from JSON; an empty/corrupt payload yields a fresh store rather
    /// than an error, so a damaged cache never blocks analysis.
    pub fn from_json(s: &str) -> Self {
        serde_json::from_str(s).unwrap_or_else(|_| LearnedStore::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noisy_or_fuses_independent_evidence() {
        let mut s = LearnedStore::new();
        s.observe(
            "crate::check_access",
            "guard",
            InferenceOrigin::Frequency,
            0.6,
            1,
        );
        s.observe(
            "crate::check_access",
            "guard",
            InferenceOrigin::Name,
            0.6,
            2,
        );
        let c = s.confidence("crate::check_access", "guard");
        // 1 - (1-0.6)*(1-0.6) = 0.84
        assert!((c - 0.84).abs() < 1e-9, "got {c}");
        let role = &s.top("guard", 1)[0];
        assert_eq!(role.support, 2);
        assert_eq!(
            role.origins,
            vec!["frequency".to_string(), "name".to_string()]
        );
        assert_eq!(role.last_seen_rev, 2);
    }

    #[test]
    fn decay_forgets_unreinforced_roles() {
        let mut s = LearnedStore::new();
        s.observe("crate::a", "sink", InferenceOrigin::Llm, 0.5, 1);
        s.decay(0.5, 0.3); // 0.25 < floor 0.3 -> dropped
        assert_eq!(s.confidence("crate::a", "sink"), 0.0);
        assert!(s.is_empty());

        let mut s2 = LearnedStore::new();
        s2.observe("crate::b", "sink", InferenceOrigin::Llm, 0.9, 1);
        s2.decay(0.9, 0.1); // 0.81 kept
        assert!((s2.confidence("crate::b", "sink") - 0.81).abs() < 1e-9);
    }

    #[test]
    fn round_trips_through_json() {
        let mut s = LearnedStore::new();
        s.observe(
            "crate::sanitize",
            "sanitizer",
            InferenceOrigin::Name,
            0.7,
            3,
        );
        let json = s.to_json().unwrap();
        let back = LearnedStore::from_json(&json);
        assert!((back.confidence("crate::sanitize", "sanitizer") - 0.7).abs() < 1e-9);
    }

    #[test]
    fn corrupt_json_yields_fresh_store() {
        assert!(LearnedStore::from_json("{not json").is_empty());
    }
}
