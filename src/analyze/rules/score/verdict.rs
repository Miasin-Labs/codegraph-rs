//! Keep or discard: a rule earns its place only if its precision on the
//! corpus beats the base rate — what firing at random would score — by a
//! margin, on enough labeled findings that the difference is not luck.

use serde::Serialize;

/// What to do with a rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Keep,
    Discard,
}

impl Decision {
    pub fn as_str(self) -> &'static str {
        match self {
            Decision::Keep => "keep",
            Decision::Discard => "discard",
        }
    }
}

/// The thresholds a rule must clear.
#[derive(Debug, Clone, Copy)]
pub struct Policy {
    /// Precision must be at least the base rate plus this (absolute).
    pub margin: f64,
    /// Labeled findings (TP + FP; differential: TP + off-fix) needed at all.
    pub min_support: usize,
    /// z of the one-sided Wilson lower bound that must exceed the base rate
    /// (1.645: 95% one-sided).
    pub z: f64,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            margin: 0.10,
            min_support: 5,
            z: 1.645,
        }
    }
}

/// The lower end of the Wilson score interval of `hits` in `n`.
pub(crate) fn wilson_lower(hits: usize, n: usize, z: f64) -> f64 {
    if n == 0 {
        return 0.0;
    }
    let n = n as f64;
    let p = hits as f64 / n;
    let z2 = z * z;
    let centre = p + z2 / (2.0 * n);
    let spread = z * (p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt();
    ((centre - spread) / (1.0 + z2 / n)).max(0.0)
}

fn pct(x: f64) -> String {
    format!("{:.1}%", x * 100.0)
}

/// Judge a rule with `hits` true positives out of `support` scored findings
/// (`findings` in all) against `base_rate`. `what` names the kind of
/// support (`labeled findings`, `differential findings`).
pub(crate) fn judge(
    findings: usize,
    hits: usize,
    support: usize,
    base_rate: Option<f64>,
    policy: &Policy,
    what: &str,
) -> (Decision, String) {
    if findings == 0 {
        return (
            Decision::Discard,
            "no findings on this corpus: nothing to measure (write a bad example from it, or \
             score on a corpus with this bug class)"
                .to_string(),
        );
    }
    let Some(base) = base_rate else {
        return (
            Decision::Discard,
            "the scored files hold no labeled rows, so there is no base rate to beat".to_string(),
        );
    };
    if support < policy.min_support {
        return (
            Decision::Discard,
            format!(
                "only {support} {what} ({hits} true), {} needed to tell it from chance",
                policy.min_support
            ),
        );
    }
    let precision = hits as f64 / support as f64;
    let lower = wilson_lower(hits, support, policy.z);
    let bar = base + policy.margin;
    if precision >= bar && lower > base {
        (
            Decision::Keep,
            format!(
                "precision {} ({hits}/{support}) beats the base rate {} by {:.1} pts, and its \
                 lower bound {} is above it",
                pct(precision),
                pct(base),
                (precision - base) * 100.0,
                pct(lower)
            ),
        )
    } else if precision < bar {
        (
            Decision::Discard,
            format!(
                "precision {} ({hits}/{support}) is not {:.0} pts above the base rate {}",
                pct(precision),
                policy.margin * 100.0,
                pct(base)
            ),
        )
    } else {
        (
            Decision::Discard,
            format!(
                "precision {} ({hits}/{support}) clears the base rate {} but its lower bound {} \
                 does not: too few {what} to trust it",
                pct(precision),
                pct(base),
                pct(lower)
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wilson_lower_bound_is_below_the_rate_and_tightens_with_n() {
        let small = wilson_lower(4, 5, 1.645);
        let large = wilson_lower(400, 500, 1.645);
        assert!(small < 0.8 && large < 0.8);
        assert!(large > small);
        assert_eq!(wilson_lower(0, 0, 1.645), 0.0);
    }

    #[test]
    fn a_rule_keeps_only_above_the_base_rate_with_support() {
        let policy = Policy::default();
        let (decision, _) = judge(20, 18, 20, Some(0.5), &policy, "labeled findings");
        assert_eq!(decision, Decision::Keep);
        let (decision, reason) = judge(20, 11, 20, Some(0.5), &policy, "labeled findings");
        assert_eq!(decision, Decision::Discard);
        assert!(reason.contains("not 10 pts above"), "{reason}");
        let (decision, reason) = judge(3, 3, 3, Some(0.5), &policy, "labeled findings");
        assert_eq!(decision, Decision::Discard);
        assert!(reason.contains("only 3"), "{reason}");
        let (decision, reason) = judge(0, 0, 0, Some(0.5), &policy, "labeled findings");
        assert_eq!(decision, Decision::Discard);
        assert!(reason.contains("no findings"), "{reason}");
        // Clears the margin, but 5 of 6 is not enough to trust over 0.6.
        let (decision, reason) = judge(6, 5, 6, Some(0.6), &policy, "labeled findings");
        assert_eq!(decision, Decision::Discard);
        assert!(reason.contains("lower bound"), "{reason}");
    }
}
