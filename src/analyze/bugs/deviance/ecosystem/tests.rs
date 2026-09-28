//! Mining on hand-made observations (support, lift, z, the per-crate cap,
//! each template's thresholds), and applying beliefs to a fixture project
//! whose library calls are given as an index would record them.

use std::path::Path;

use super::mine::{MineOptions, is_release_pair, lift, mine, z_score};
use super::observe::{ExternalSite, observe};
use super::{apply, is_status, parse_graph_key};
use crate::analyze::bugs::{FnSpan, Project};
use crate::deps::beliefs::model::{
    Belief,
    BeliefKind,
    BeliefSet,
    CrateObservations,
    EvidenceSite,
    SiteObs,
    Support,
    UseClass,
};

// ---------------------------------------------------------------------------
// Mining

/// Observations of one crate: `apis`, and sites as (api index, use,
/// object, after, before, await held).
struct Crate {
    obs: CrateObservations,
}

impl Crate {
    fn new(name: &str, apis: &[&str], returns: &[Option<&str>]) -> Self {
        Crate {
            obs: CrateObservations {
                version_of_observer: crate::deps::beliefs::model::OBSERVE_VERSION,
                krate: name.to_string(),
                version: "1.0.0".to_string(),
                fingerprint: String::new(),
                apis: apis.iter().map(|a| a.to_string()).collect(),
                returns: returns.iter().map(|r| r.map(str::to_string)).collect(),
                sites: Vec::new(),
            },
        }
    }

    fn site(mut self, api: u32, use_class: UseClass) -> Self {
        let line = self.obs.sites.len() as u32 + 1;
        self.obs.sites.push(SiteObs {
            api,
            file: "src/lib.rs".to_string(),
            line,
            use_class,
            object: None,
            escapes: false,
            before: Vec::new(),
            after: Vec::new(),
            await_held: None,
            receiver: true,
            constructed: true,
            methods_before: 0,
            methods_after: 0,
        });
        self
    }

    fn on_object(mut self, api: u32, object: u32, before: &[u32], after: &[u32]) -> Self {
        self = self.site(api, UseClass::Used);
        let site = self.obs.sites.last_mut().unwrap();
        site.object = Some(object);
        site.before = before.to_vec();
        site.after = after.to_vec();
        self
    }

    fn awaited(mut self, api: u32, held: bool) -> Self {
        self = self.site(api, UseClass::Used);
        self.obs.sites.last_mut().unwrap().await_held = Some(held);
        self
    }
}

const CREATE: &str = "std@1::File::create";

fn creates(name: &str, used: usize, discarded: usize) -> CrateObservations {
    let mut krate = Crate::new(name, &[CREATE], &[Some("io::Result<File>")]);
    for _ in 0..used {
        krate = krate.site(0, UseClass::Used);
    }
    for _ in 0..discarded {
        krate = krate.site(0, UseClass::Discarded);
    }
    krate.obs
}

#[test]
fn engler_z_and_lift() {
    assert!((z_score(9, 10) - 8.0 / 10f64.sqrt()).abs() < 1e-9);
    assert_eq!(z_score(5, 10), 0.0, "a coin flip");
    assert!(z_score(2, 10) < 0.0);
    assert_eq!(z_score(0, 0), 0.0);
    assert!((lift(8, 10, 0.2) - 4.0).abs() < 1e-9);
    assert_eq!(lift(1, 0, 0.5), 0.0);
}

#[test]
fn a_result_most_crates_use_is_a_belief_with_crate_and_site_support() {
    let mut crates: Vec<CrateObservations> =
        (0..7).map(|i| creates(&format!("c{i}"), 3, 0)).collect();
    crates.push(creates("sloppy", 3, 1));
    let beliefs = mine(&crates, &is_status, &MineOptions::default());
    let [belief] = beliefs.as_slice() else {
        panic!("one belief: {beliefs:?}");
    };
    assert_eq!(belief.kind, BeliefKind::ResultUsed);
    assert_eq!(
        belief.support,
        Support {
            agree_crates: 7,
            total_crates: 8,
            agree_sites: 24,
            total_sites: 25,
        }
    );
    assert!((belief.z - z_score(24, 25)).abs() < 1e-9);
    assert!(belief.confidence > 0.0 && belief.confidence <= 0.95);
    assert_eq!(
        belief.evidence.len(),
        5,
        "one example per agreeing crate, capped"
    );
    assert_eq!(belief.returns.as_deref(), Some("io::Result<File>"));

    // Two crates of eight discarding: the crates no longer agree (0.75).
    crates.push(creates("sloppier", 3, 1));
    crates.remove(0);
    assert!(mine(&crates, &is_status, &MineOptions::default()).is_empty());
}

#[test]
fn one_prolific_crate_cannot_outvote_the_rest() {
    // Five crates use it; one discards it 500 times. Uncapped, the sites
    // would read 15 of 515; capped at 20 per crate, 15 of 35 — still no
    // belief, but no crate's volume decides it either way.
    let mut crates: Vec<CrateObservations> =
        (0..5).map(|i| creates(&format!("c{i}"), 3, 0)).collect();
    crates.push(creates("loud", 0, 500));
    let options = MineOptions::default();
    assert!(mine(&crates, &is_status, &options).is_empty());
    // And a crate using it 500 times adds 20 sites, not 500.
    let mut crates: Vec<CrateObservations> =
        (0..5).map(|i| creates(&format!("c{i}"), 3, 0)).collect();
    crates.push(creates("busy", 500, 0));
    let beliefs = mine(&crates, &is_status, &options);
    assert_eq!(beliefs[0].support.total_sites, 15 + options.per_crate_cap);
}

#[test]
fn a_result_that_reports_no_status_makes_no_belief() {
    let crates: Vec<CrateObservations> = (0..8)
        .map(|i| {
            let mut krate = Crate::new(&format!("c{i}"), &["alloc@1::Vec::len"], &[Some("usize")]);
            for _ in 0..3 {
                krate = krate.site(0, UseClass::Used);
            }
            krate.obs
        })
        .collect();
    assert!(mine(&crates, &is_status, &MineOptions::default()).is_empty());
}

#[test]
fn a_follow_up_on_the_same_object_needs_support_and_lift() {
    // api 0 = Builder::new, 1 = Builder::build, 2 = Other::touch.
    let apis = [
        "lib@1::Builder::new",
        "lib@1::Builder::build",
        "lib@1::Other::touch",
    ];
    let returns = [Some("Builder"), Some("Client"), None];
    let crates: Vec<CrateObservations> = (0..6)
        .map(|i| {
            let mut krate = Crate::new(&format!("c{i}"), &apis, &returns);
            // Two builders, each built.
            krate = krate
                .on_object(0, 0, &[], &[1])
                .on_object(1, 0, &[0], &[])
                .on_object(0, 1, &[], &[1])
                .on_object(1, 1, &[0], &[]);
            // Ten other objects of the library, never built.
            for object in 2..12 {
                krate = krate.on_object(2, object, &[], &[]);
            }
            krate.obs
        })
        .collect();
    let beliefs = mine(&crates, &is_status, &MineOptions::default());
    let followed: Vec<&Belief> = beliefs
        .iter()
        .filter(|b| matches!(b.kind, BeliefKind::FollowedBy { .. }))
        .collect();
    let [belief] = followed.as_slice() else {
        panic!("one follow-up belief: {beliefs:?}");
    };
    assert_eq!(belief.api, "lib@1::Builder::new");
    assert_eq!(
        belief.kind,
        BeliefKind::FollowedBy {
            others: vec!["lib@1::Builder::build".to_string()]
        }
    );
    assert_eq!(belief.support.agree_sites, 12);
    assert_eq!(belief.support.total_sites, 12);
    // `build` touches 2 of the 12 objects each crate's library calls make:
    // base 1/6, so every builder being built is a lift of 6.
    assert!(
        (belief.lift.unwrap() - 6.0).abs() < 1e-9,
        "{:?}",
        belief.lift
    );
    // And the reverse: `build` is preceded by `new`.
    assert!(beliefs.iter().any(|b| b.api == "lib@1::Builder::build"
        && b.kind
            == BeliefKind::PrecededBy {
                others: vec!["lib@1::Builder::new".to_string()]
            }));
}

#[test]
fn what_usually_happens_to_a_new_object_is_no_protocol() {
    // `HashSet::new` is followed by `insert` everywhere — but a set filled
    // with `extend` is no bug: a constructor gets no follow-up belief.
    let apis = [
        "std@1::HashSet::new",
        "std@1::HashSet::insert",
        "std@1::Other::touch",
    ];
    let crates: Vec<CrateObservations> = (0..6)
        .map(|i| {
            let mut krate = Crate::new(&format!("c{i}"), &apis, &[None, None, None]);
            for object in 0..3 {
                krate = krate
                    .on_object(0, object, &[], &[1])
                    .on_object(1, object, &[0], &[]);
                let ctor = krate.obs.sites.len() - 2;
                krate.obs.sites[ctor].receiver = false;
            }
            for object in 3..12 {
                krate = krate.on_object(2, object, &[], &[]);
            }
            krate.obs
        })
        .collect();
    let beliefs = mine(&crates, &is_status, &MineOptions::default());
    assert!(
        !beliefs
            .iter()
            .any(|b| b.api == "std@1::HashSet::new"
                && matches!(b.kind, BeliefKind::FollowedBy { .. })),
        "{beliefs:?}"
    );
    // The method's belief stays: `insert` is preceded by `new`? No — a
    // constructor is no precursor either.
    assert!(
        !beliefs.iter().any(|b| b.api == "std@1::HashSet::insert"
            && matches!(b.kind, BeliefKind::PrecededBy { .. }))
    );
}

#[test]
fn a_constructor_is_no_precursor() {
    // `set_len` always comes after `with_capacity` (a constructor) and after
    // `as_mut_ptr` only half the time: no belief — a constructor initialises
    // nothing `set_len` exposes.
    let apis = [
        "alloc@1::Vec::with_capacity",
        "alloc@1::Vec::set_len",
        "alloc@1::Vec::as_mut_ptr",
    ];
    let crates: Vec<CrateObservations> = (0..6)
        .map(|i| {
            let mut krate = Crate::new(&format!("c{i}"), &apis, &[None, None, None]);
            for object in 0..4 {
                let written = object % 2 == 0;
                krate = krate.on_object(0, object, &[], &[1]);
                krate.obs.sites.last_mut().unwrap().receiver = false;
                if written {
                    krate =
                        krate
                            .on_object(2, object, &[0], &[1])
                            .on_object(1, object, &[0, 2], &[]);
                } else {
                    krate = krate.on_object(1, object, &[0], &[]);
                }
            }
            // Other vectors of the library, to give the lift a base.
            for object in 4..12 {
                krate = krate.on_object(0, object, &[], &[]);
            }
            krate.obs
        })
        .collect();
    let beliefs = mine(&crates, &is_status, &MineOptions::default());
    assert!(
        !beliefs
            .iter()
            .any(|b| b.api == "alloc@1::Vec::set_len"
                && matches!(b.kind, BeliefKind::PrecededBy { .. })),
        "{beliefs:?}"
    );
}

#[test]
fn a_companion_everywhere_in_the_library_has_no_lift() {
    // `touch` follows `new` always — but touches every object of the
    // library: lift 1, no belief.
    let apis = ["lib@1::Builder::new", "lib@1::Other::touch"];
    let crates: Vec<CrateObservations> = (0..6)
        .map(|i| {
            let mut krate = Crate::new(&format!("c{i}"), &apis, &[None, None]);
            for object in 0..4 {
                krate = krate
                    .on_object(0, object, &[], &[1])
                    .on_object(1, object, &[0], &[]);
            }
            krate.obs
        })
        .collect();
    let beliefs = mine(&crates, &is_status, &MineOptions::default());
    assert!(
        !beliefs
            .iter()
            .any(|b| matches!(b.kind, BeliefKind::FollowedBy { .. })),
        "{beliefs:?}"
    );
}

#[test]
fn named_release_pairs_across_a_crate() {
    assert!(is_release_pair("into_raw", "from_raw"));
    assert!(is_release_pair("into_raw_fd", "from_raw_fd"));
    assert!(is_release_pair("leak", "from_raw"));
    assert!(is_release_pair("lock", "unlock"));
    assert!(!is_release_pair("into_raw", "from_utf8"));
    assert!(!is_release_pair("into_bytes", "from_raw"));
    assert!(!is_release_pair("into_parts", "from_parts"), "a conversion");
    assert!(!is_release_pair("push", "pop"));

    let apis = [
        "std@1::CString::into_raw",
        "std@1::CString::from_raw",
        "std@1::CString::new",
    ];
    let mut crates: Vec<CrateObservations> = (0..5)
        .map(|i| {
            Crate::new(&format!("c{i}"), &apis, &[None, None, None])
                .site(0, UseClass::Used)
                .site(1, UseClass::Used)
                .site(2, UseClass::Used)
                .obs
        })
        .collect();
    crates.push(
        Crate::new("leaky", &apis, &[None, None, None])
            .site(0, UseClass::Used)
            .obs,
    );
    let beliefs = mine(&crates, &is_status, &MineOptions::default());
    let pairs: Vec<&Belief> = beliefs
        .iter()
        .filter(|b| matches!(b.kind, BeliefKind::PairedWith { .. }))
        .collect();
    let [pair] = pairs.as_slice() else {
        panic!("one pair: {beliefs:?}");
    };
    assert_eq!(pair.api, "std@1::CString::into_raw");
    assert_eq!(
        (pair.support.agree_crates, pair.support.total_crates),
        (5, 6)
    );
    // `new` is no release of anything, and `from_raw` is not paired back.
    assert!(
        !beliefs.iter().any(|b| b.api == "std@1::CString::from_raw"
            && matches!(b.kind, BeliefKind::PairedWith { .. }))
    );
}

#[test]
fn a_guard_dropped_before_await_is_a_belief_and_holders_depart() {
    let apis = ["std@1::Mutex::lock"];
    let mut crates: Vec<CrateObservations> = (0..6)
        .map(|i| {
            Crate::new(
                &format!("c{i}"),
                &apis,
                &[Some("LockResult<MutexGuard<'_, T>>")],
            )
            .awaited(0, false)
            .awaited(0, false)
            .obs
        })
        .collect();
    let beliefs = mine(&crates, &is_status, &MineOptions::default());
    assert!(
        beliefs
            .iter()
            .any(|b| b.kind == BeliefKind::NotHeldAcrossAwait && b.support.agree_sites == 12),
        "{beliefs:?}"
    );
    // The async lock whose guard is meant to be held: no belief.
    crates = (0..6)
        .map(|i| {
            Crate::new(&format!("c{i}"), &["tokio@1::Mutex::lock"], &[None])
                .awaited(0, true)
                .awaited(0, false)
                .obs
        })
        .collect();
    assert!(
        !mine(&crates, &is_status, &MineOptions::default())
            .iter()
            .any(|b| b.kind == BeliefKind::NotHeldAcrossAwait)
    );
}

#[test]
fn graph_keys_name_package_and_version() {
    assert_eq!(
        parse_graph_key("crates/tokio-util-0.7.15"),
        Some(("tokio-util", "0.7.15"))
    );
    assert_eq!(
        parse_graph_key("crates/std-1.101.0-nightly"),
        Some(("std", "1.101.0-nightly"))
    );
    assert_eq!(
        parse_graph_key("crates/x25519-dalek-2.0.1"),
        Some(("x25519-dalek", "2.0.1"))
    );
    assert_eq!(parse_graph_key("/home/me/linked"), None);
}

// ---------------------------------------------------------------------------
// Observing and applying

const PROJECT: &str = r#"use std::ffi::CString;
use std::fs::File;
use std::process::Command;
use std::sync::Mutex;

fn bad_create(path: &str) {
    File::create(path);
}

fn old_create(path: &str) -> std::io::Result<()> {
    try!(File::create(path));
    Ok(())
}

fn good_create(path: &str) -> std::io::Result<()> {
    let _file = File::create(path)?;
    Ok(())
}

async fn holds(m: &Mutex<u32>) {
    let guard = m.lock().unwrap();
    tick().await;
    drop(guard);
}

async fn scoped(m: &Mutex<u32>) {
    {
        let guard = m.lock().unwrap();
        let _ = *guard;
    }
    tick().await;
}

async fn releases(m: &Mutex<u32>) {
    let guard = m.lock().unwrap();
    drop(guard);
    tick().await;
}

fn fill(n: usize) -> usize {
    let mut v: Vec<u8> = Vec::with_capacity(n);
    unsafe { v.set_len(n) };
    v.len()
}

fn fill_initialized(n: usize, src: &[u8]) -> usize {
    let mut v: Vec<u8> = Vec::with_capacity(n);
    unsafe {
        std::ptr::copy_nonoverlapping(src.as_ptr(), v.as_mut_ptr(), n);
        v.set_len(n);
    }
    v.len()
}

fn returned(n: usize) -> Vec<u8> {
    let mut v: Vec<u8> = Vec::with_capacity(n);
    unsafe { v.set_len(n) };
    v
}

fn finished(dir: &str) -> bool {
    let mut cmd = Command::new("ls");
    cmd.args([dir]);
    cmd.status().is_ok()
}

fn abandoned(dir: &str) {
    let mut cmd = Command::new("ls");
    cmd.args([dir]);
    println!("never run");
}

fn debug_arm(f: &mut std::fmt::Formatter<'_>, x: Option<u8>) -> std::fmt::Result {
    let mut d = f.debug_struct("X");
    match x {
        Some(v) => d.field("x", &v),
        None => d.field("x", &"none"),
    }
    .finish()
}

fn hand_out(s: &str) -> *mut i8 {
    CString::new(s).unwrap().into_raw()
}
"#;

/// `(line, col)` of the `nth` occurrence of `needle`.
fn at(source: &str, needle: &str, nth: usize) -> (u32, u32) {
    let offset = source
        .match_indices(needle)
        .nth(nth)
        .unwrap_or_else(|| panic!("no {needle} #{nth}"))
        .0;
    let before = &source[..offset];
    let line = before.matches('\n').count() as u32 + 1;
    let col = (offset - before.rfind('\n').map_or(0, |nl| nl + 1)) as u32;
    (line, col)
}

fn external(needle: &str, nth: usize, api: &str) -> ExternalSite {
    let (line, col) = at(PROJECT, needle, nth);
    ExternalSite {
        file: "src/lib.rs".to_string(),
        line,
        col,
        api: api.to_string(),
        name: api.rsplit("::").next().unwrap().to_string(),
    }
}

/// Every `fn` of the fixture as the index would record it.
fn functions() -> Vec<FnSpan> {
    let lines: Vec<&str> = PROJECT.lines().collect();
    let mut spans = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let Some(rest) = line
            .strip_prefix("fn ")
            .or_else(|| line.strip_prefix("async fn "))
        else {
            continue;
        };
        let name = rest.split('(').next().unwrap().to_string();
        let end = (i..lines.len()).find(|&j| lines[j] == "}").unwrap();
        spans.push(FnSpan {
            id: name.clone(),
            name: name.clone(),
            qualified_name: name,
            kind: "function".to_string(),
            file: "src/lib.rs".to_string(),
            start_line: i as u32 + 1,
            end_line: end as u32 + 1,
            start_col: 0,
            signature: None,
            return_type: None,
            is_test: false,
        });
    }
    spans
}

fn belief(api: &str, kind: BeliefKind, returns: Option<&str>) -> Belief {
    Belief {
        api: api.to_string(),
        kind,
        returns: returns.map(str::to_string),
        support: Support {
            agree_crates: 40,
            total_crates: 41,
            agree_sites: 180,
            total_sites: 185,
        },
        z: 12.9,
        lift: Some(7.5),
        confidence: 0.7,
        evidence: vec![EvidenceSite {
            krate: "tempfile@3.10.1".to_string(),
            file: "src/file.rs".to_string(),
            line: 12,
        }],
    }
}

fn beliefs() -> BeliefSet {
    BeliefSet {
        format: crate::deps::beliefs::model::BELIEFS_FORMAT,
        beliefs: vec![
            belief(CREATE, BeliefKind::ResultUsed, Some("io::Result<File>")),
            belief("std@1::Mutex::lock", BeliefKind::NotHeldAcrossAwait, None),
            belief(
                "alloc@1::Vec::set_len",
                BeliefKind::PrecededBy {
                    others: vec![
                        "alloc@1::Vec::as_mut_ptr".to_string(),
                        "alloc@1::Vec::spare_capacity_mut".to_string(),
                    ],
                },
                None,
            ),
            belief(
                "core@1::DebugStruct::field",
                BeliefKind::FollowedBy {
                    others: vec!["core@1::DebugStruct::finish".to_string()],
                },
                None,
            ),
            belief(
                "std@1::Command::args",
                BeliefKind::FollowedBy {
                    others: vec![
                        "std@1::Command::output".to_string(),
                        "std@1::Command::spawn".to_string(),
                    ],
                },
                None,
            ),
            belief(
                "alloc@1::CString::into_raw",
                BeliefKind::PairedWith {
                    other: "alloc@1::CString::from_raw".to_string(),
                },
                None,
            ),
        ],
        ..BeliefSet::default()
    }
}

fn fixture_sites() -> Vec<ExternalSite> {
    vec![
        external("File::create(path);", 0, CREATE),
        external("File::create(path)?", 0, CREATE),
        external("File::create(path));", 0, CREATE),
        external("m.lock()", 0, "std@1::Mutex::lock"),
        external("m.lock()", 1, "std@1::Mutex::lock"),
        external("m.lock()", 2, "std@1::Mutex::lock"),
        external("Vec::with_capacity", 0, "alloc@1::Vec::with_capacity"),
        external("v.set_len", 0, "alloc@1::Vec::set_len"),
        external("Vec::with_capacity", 1, "alloc@1::Vec::with_capacity"),
        external("v.as_mut_ptr()", 0, "alloc@1::Vec::as_mut_ptr"),
        external("v.set_len", 1, "alloc@1::Vec::set_len"),
        external("Vec::with_capacity", 2, "alloc@1::Vec::with_capacity"),
        external("v.set_len", 2, "alloc@1::Vec::set_len"),
        external(
            "CString::new(s).unwrap().into_raw()",
            0,
            "alloc@1::CString::into_raw",
        ),
        external("d.field(\"x\", &v)", 0, "core@1::DebugStruct::field"),
        external("Command::new(\"ls\")", 0, "std@1::Command::new"),
        external("cmd.args", 0, "std@1::Command::args"),
        external("cmd.status()", 0, "std@1::Command::status"),
        external("Command::new(\"ls\")", 1, "std@1::Command::new"),
        external("cmd.args", 1, "std@1::Command::args"),
    ]
}

fn run(project_source: &str, sites: &[ExternalSite]) -> Vec<(String, u32)> {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), project_source).unwrap();
    let mut project = Project::from_parts(
        dir.path(),
        vec!["src/lib.rs".to_string()],
        functions(),
        Vec::new(),
    );
    let observed = observe(&mut project, sites);
    let mut found: Vec<(String, u32)> = apply::findings(&beliefs(), &observed, &is_status)
        .into_iter()
        .map(|finding| (finding.rule.to_string(), finding.line))
        .collect();
    found.sort();
    found
}

#[test]
fn project_calls_that_depart_from_ecosystem_beliefs_are_findings() {
    let line = |needle: &str, nth: usize| at(PROJECT, needle, nth).0;
    let found = run(PROJECT, &fixture_sites());
    let mut expected = vec![
        // A bare `File::create(path);` — not the `?`-checked one.
        (
            "ecosystem-result-discarded".to_string(),
            line("File::create(path);", 0),
        ),
        // The guard alive at `tick().await` — not the one dropped first.
        (
            "ecosystem-held-across-await".to_string(),
            line("m.lock()", 0),
        ),
        // `set_len` on a local never written through — not after
        // `as_mut_ptr`, and not on a vector handed back to the caller.
        (
            "ecosystem-missing-precursor".to_string(),
            line("v.set_len", 0),
        ),
        // A command configured and never run — not the one finished with
        // `status`, a way the belief's companions do not name.
        (
            "ecosystem-missing-follow-up".to_string(),
            line("cmd.args", 1),
        ),
        // `into_raw` with no `from_raw` anywhere in the project.
        ("ecosystem-missing-pair".to_string(), line("into_raw()", 0)),
    ];
    expected.sort();
    assert_eq!(found, expected);
}

#[test]
fn a_release_called_anywhere_answers_a_pair_belief() {
    let source = format!(
        "{PROJECT}\nfn take_back(p: *mut i8) {{\n    unsafe {{ drop(Vec::from_raw_parts(p, 1, 1)) }};\n}}\n"
    );
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), &source).unwrap();
    let mut project = Project::from_parts(
        dir.path(),
        vec!["src/lib.rs".to_string()],
        functions(),
        Vec::new(),
    );
    // Released through `from_raw_parts`, unresolved: the name answers.
    let observed = observe(&mut project, &fixture_sites());
    let rules: Vec<String> = apply::findings(&beliefs(), &observed, &is_status)
        .into_iter()
        .map(|finding| finding.rule.to_string())
        .collect();
    assert!(
        !rules.iter().any(|rule| rule == "ecosystem-missing-pair"),
        "{rules:?}"
    );
}

#[test]
fn a_raw_pointer_given_to_an_owner_is_released() {
    // `alloc` whose pointer `Box::from_raw` takes over: no `dealloc`
    // needed. `Box::from_raw` is left unresolved; its name answers.
    let source = "use std::alloc::{alloc, Layout};\n\
                  fn make() -> Box<u64> {\n    \
                  let layout = Layout::new::<u64>();\n    \
                  unsafe { Box::from_raw(alloc(layout) as *mut u64) }\n}\n";
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), source).unwrap();
    let (line, col) = at(source, "alloc(layout)", 0);
    let site = ExternalSite {
        file: "src/lib.rs".to_string(),
        line,
        col,
        api: "alloc@1::alloc".to_string(),
        name: "alloc".to_string(),
    };
    let mut project = Project::from_parts(
        dir.path(),
        vec!["src/lib.rs".to_string()],
        Vec::new(),
        Vec::new(),
    );
    let observed = observe(&mut project, &[site]);
    let mut pair = belief(
        "alloc@1::alloc",
        BeliefKind::PairedWith {
            other: "alloc@1::dealloc".to_string(),
        },
        Some("*mut u8"),
    );
    let set = BeliefSet {
        beliefs: vec![pair.clone()],
        ..BeliefSet::default()
    };
    assert!(apply::findings(&set, &observed, &is_status).is_empty());
    // Without the raw-pointer return, the named release is required.
    pair.returns = None;
    let set = BeliefSet {
        beliefs: vec![pair],
        ..BeliefSet::default()
    };
    assert_eq!(apply::findings(&set, &observed, &is_status).len(), 1);
}

#[test]
fn observation_records_objects_order_and_escapes() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), PROJECT).unwrap();
    let mut project = Project::from_parts(
        Path::new(dir.path()),
        vec!["src/lib.rs".to_string()],
        functions(),
        Vec::new(),
    );
    let observed = observe(&mut project, &fixture_sites());
    let api = |name: &str| {
        observed
            .apis
            .iter()
            .position(|known| known == name)
            .unwrap() as u32
    };
    let site = |needle: &str, nth: usize| {
        let (line, col) = at(PROJECT, needle, nth);
        observed
            .sites
            .iter()
            .find(|site| site.obs.line == line && site.col == col)
            .unwrap_or_else(|| panic!("no site at {needle}"))
    };
    let initialized = site("v.set_len", 1);
    assert_eq!(
        initialized.obs.before,
        vec![
            api("alloc@1::Vec::with_capacity"),
            api("alloc@1::Vec::as_mut_ptr")
        ]
    );
    assert!(!initialized.obs.escapes);
    assert_eq!(initialized.function.as_deref(), Some("fill_initialized"));
    assert!(site("v.set_len", 2).obs.escapes, "`v` is returned");
    assert_eq!(
        site("File::create(path);", 0).obs.use_class,
        UseClass::Discarded
    );
    assert_eq!(site("File::create(path)?", 0).obs.use_class, UseClass::Used);
    assert_eq!(
        site("File::create(path));", 0).obs.use_class,
        UseClass::Checked,
        "`try!(..);` checks it"
    );
    assert_eq!(site("m.lock()", 0).obs.await_held, Some(true));
    assert_eq!(
        site("m.lock()", 1).obs.await_held,
        Some(false),
        "its block ends before the await"
    );
    assert_eq!(site("m.lock()", 2).obs.await_held, Some(false));
    assert!(site("v.set_len", 0).obs.receiver);
    assert!(!site("Vec::with_capacity", 0).obs.receiver);
    assert!(observed.called_names.contains("tick"));
}
