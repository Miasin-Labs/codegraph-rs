//! What the ecosystem-beliefs builder stores: per-crate observations of how
//! library APIs are called (the cache the builder mines from), and the
//! mined beliefs (the one artifact `analyze bugs` reads).
//!
//! An API is keyed `crate@compat::Qualified::name` (`std@1::File::create`,
//! `reqwest@0.12::ClientBuilder::build`): the crate code names, its
//! semver-compatible release line (the major, or `0.minor` below 1.0 —
//! versions a lockfile may substitute for each other), and the qualified
//! name the defining crate's index gives the item.

use serde::{Deserialize, Serialize};

/// Bump when the artifact's shape or what a belief means changes.
pub const BELIEFS_FORMAT: u32 = 1;
/// Bump when the observation code records something different: cached
/// observations of an older version are redone.
pub const OBSERVE_VERSION: u32 = 4;

/// `crate@compat::qualified` for a call into `krate` at `version`.
pub fn api_key(krate: &str, version: &str, qualified: &str) -> String {
    format!("{krate}@{}::{qualified}", compat(version))
}

/// The semver-compatible release line of `version`: `1.0.219` → `1`,
/// `0.12.4` → `0.12`, `0.0.3` → `0.0.3`, `1.101.0-nightly` → `1`.
pub fn compat(version: &str) -> String {
    let core = version.split(['+', '-']).next().unwrap_or(version).trim();
    let parts: Vec<&str> = core.split('.').collect();
    match parts.as_slice() {
        ["0", "0", ..] => core.to_string(),
        ["0", minor, ..] => format!("0.{minor}"),
        [major, ..] => (*major).to_string(),
        [] => core.to_string(),
    }
}

/// The crate part of an API key (`std` of `std@1::File::create`).
pub fn api_crate(api: &str) -> &str {
    api.split('@').next().unwrap_or(api)
}

/// The library part of an API key (`std@1`).
pub fn api_library(api: &str) -> &str {
    api.split_once("::").map_or(api, |(library, _)| library)
}

/// The qualified name part of an API key (`File::create`).
pub fn api_path(api: &str) -> &str {
    api.split_once("::").map_or("", |(_, path)| path)
}

/// The item's own name (`create` of `std@1::File::create`).
pub fn api_name(api: &str) -> &str {
    api.rsplit("::").next().unwrap_or(api)
}

/// The owner of a method (`std@1::File` of `std@1::File::create`), or the
/// library for a free function.
pub fn api_owner(api: &str) -> &str {
    api.rsplit_once("::").map_or(api, |(owner, _)| owner)
}

/// How a call site treats the API's result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UseClass {
    /// Bound, passed on, returned, tested, chained.
    Used,
    /// A bare statement: the value is dropped without a word.
    Discarded,
    /// Dropped on purpose (`let _ =`, `if let Err(_) =`, `.ok();`).
    Handled,
    /// Checked for failure, the success value dropped (`f()?;`).
    Checked,
}

/// One call of an API in one crate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SiteObs {
    /// Index into [`CrateObservations::apis`].
    pub api: u32,
    pub file: String,
    pub line: u32,
    #[serde(rename = "use")]
    pub use_class: UseClass,
    /// The object the call works on (the receiver's local, or the value a
    /// call chain builds), numbered per crate; `None` when there is none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object: Option<u32>,
    /// The object is not this function's own local, or leaves it
    /// (returned, passed on, stored): what happens to it elsewhere is not
    /// seen, so it neither supports nor departs from an ordering belief.
    #[serde(default)]
    pub escapes: bool,
    /// APIs called on the same object earlier / later in the function.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub before: Vec<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub after: Vec<u32>,
    /// In async code, the result bound to a local with an `.await` later in
    /// its block: whether it is still alive there (`Some(true)`), or was
    /// dropped first (`Some(false)`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub await_held: Option<bool>,
    /// A method call on a receiver (`v.set_len(n)`), not an associated or
    /// free fn (`Vec::with_capacity(n)`): what an object's history is made
    /// of — a constructor always comes first and says nothing.
    #[serde(default)]
    pub receiver: bool,
    /// A library constructor made the object in view, at or before this
    /// call (`let v = Vec::with_capacity(n)`): its whole history in this
    /// function is seen. An object from a parameter or a project fn is not.
    #[serde(default)]
    pub constructed: bool,
    /// Method calls (any library's) on the same object before / after it.
    #[serde(default)]
    pub methods_before: u32,
    #[serde(default)]
    pub methods_after: u32,
}

/// How one crate calls library APIs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrateObservations {
    pub version_of_observer: u32,
    /// The observed crate (a user of the APIs) and its version.
    pub krate: String,
    pub version: String,
    /// Changes whenever the crate's shard or a graph it resolved into did.
    pub fingerprint: String,
    /// API keys, indexed by [`SiteObs::api`].
    pub apis: Vec<String>,
    /// Each API's declared return type (parallel to `apis`).
    pub returns: Vec<Option<String>>,
    pub sites: Vec<SiteObs>,
}

impl CrateObservations {
    /// `name@version`, as evidence cites the crate.
    pub fn label(&self) -> String {
        format!("{}@{}", self.krate, self.version)
    }
}

/// How many agree with a belief, in crates and in call sites.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Support {
    pub agree_crates: usize,
    pub total_crates: usize,
    pub agree_sites: usize,
    pub total_sites: usize,
}

/// What the ecosystem believes about one API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum BeliefKind {
    /// Callers use the result (bind, test, pass, `?`, `unwrap`).
    ResultUsed,
    /// On the same object, one of `others` follows the call.
    FollowedBy { others: Vec<String> },
    /// On the same object, one of `others` comes before the call.
    PrecededBy { others: Vec<String> },
    /// A crate that calls the API also calls `other` (its named release:
    /// `CString::into_raw` → `CString::from_raw`).
    PairedWith { other: String },
    /// In async code, the result is dropped before the next `.await`.
    NotHeldAcrossAwait,
}

impl BeliefKind {
    /// The finding rule a departure from this belief reports under.
    pub fn rule(&self) -> &'static str {
        match self {
            Self::ResultUsed => "ecosystem-result-discarded",
            Self::FollowedBy { .. } => "ecosystem-missing-follow-up",
            Self::PrecededBy { .. } => "ecosystem-missing-precursor",
            Self::PairedWith { .. } => "ecosystem-missing-pair",
            Self::NotHeldAcrossAwait => "ecosystem-held-across-await",
        }
    }
}

/// A crate that agrees with a belief, and one place it does.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceSite {
    /// `name@version`.
    pub krate: String,
    pub file: String,
    pub line: u32,
}

/// One mined belief.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Belief {
    pub api: String,
    #[serde(flatten)]
    pub kind: BeliefKind,
    /// The API's declared return type, when its index records one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub returns: Option<String>,
    pub support: Support,
    /// Engler's z over call sites (agreement against a coin flip).
    pub z: f64,
    /// How much likelier the companion is next to the API than anywhere
    /// the library is used (ordering and pair beliefs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lift: Option<f64>,
    /// 0..1: how strongly the ecosystem backs a departure being a bug.
    pub confidence: f64,
    pub evidence: Vec<EvidenceSite>,
}

/// The artifact: every belief strong enough to report against.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeliefSet {
    pub format: u32,
    pub built_at_ms: i64,
    /// The `rustc -V` whose `std`/`core`/`alloc` sources were resolved into.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toolchain: Option<String>,
    /// Crates whose observations were mined.
    pub crates: usize,
    /// Distinct APIs called by at least one of them.
    pub apis: usize,
    pub sites: usize,
    pub beliefs: Vec<Belief>,
}

impl BeliefSet {
    /// The beliefs about `api`.
    pub fn about<'a>(&'a self, api: &'a str) -> impl Iterator<Item = &'a Belief> + 'a {
        self.beliefs.iter().filter(move |belief| belief.api == api)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_keys_name_the_compatible_release_line() {
        assert_eq!(compat("1.0.219"), "1");
        assert_eq!(compat("0.12.4"), "0.12");
        assert_eq!(compat("0.0.3"), "0.0.3");
        assert_eq!(compat("1.101.0-nightly"), "1");
        assert_eq!(compat("0.4.1+git.abcdef123456"), "0.4");
        let key = api_key("reqwest", "0.12.9", "ClientBuilder::build");
        assert_eq!(key, "reqwest@0.12::ClientBuilder::build");
        assert_eq!(api_crate(&key), "reqwest");
        assert_eq!(api_library(&key), "reqwest@0.12");
        assert_eq!(api_path(&key), "ClientBuilder::build");
        assert_eq!(api_name(&key), "build");
        assert_eq!(api_owner(&key), "reqwest@0.12::ClientBuilder");
    }
}
