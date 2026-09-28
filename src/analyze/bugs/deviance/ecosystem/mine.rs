//! Beliefs mined from many crates' observations: Engler's templates, with
//! the crates of the cargo cache as the population.
//!
//! Every belief is judged twice, by call sites and by crates. Sites give
//! Engler's z (agreement against a coin flip); crates keep one prolific
//! crate from voting a hundred times — each crate contributes at most
//! [`MineOptions::per_crate_cap`] sites per API, and a belief needs
//! [`MineOptions::min_crates`] crates that agree and the same share of
//! crates as of sites. The templates:
//!
//! - **ResultUsed**: callers use a status result (`Result`, `bool`).
//! - **FollowedBy / PrecededBy**: on the same object, in the same function,
//!   one of a small set of APIs follows (precedes) the call — mined from
//!   objects that stay local, with a lift over how often those APIs touch
//!   any object of the library.
//! - **PairedWith**: a crate calling `A` also calls its named release `B`
//!   (same owner, an acquire/release verb pair: `into_raw`/`from_raw`).
//! - **NotHeldAcrossAwait**: bound in async code, the result is dropped
//!   before the block's next `.await`.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::deps::beliefs::model::{
    Belief,
    BeliefKind,
    CrateObservations,
    EvidenceSite,
    SiteObs,
    Support,
    UseClass,
    api_library,
    api_name,
    api_owner,
};

/// Thresholds; the defaults are the measured ones.
#[derive(Debug, Clone, Copy)]
pub struct MineOptions {
    /// Crates that must agree.
    pub min_crates: usize,
    /// Sites a belief needs in all.
    pub min_sites: usize,
    /// Share of sites that must agree.
    pub min_ratio: f64,
    /// Share of crates that must agree.
    pub min_crate_ratio: f64,
    /// Companions this many times likelier next to the API than next to
    /// any use of its library.
    pub min_lift: f64,
    /// Sites one crate contributes per API.
    pub per_crate_cap: usize,
    /// Companions an ordering belief may name.
    pub max_companions: usize,
    pub max_evidence: usize,
}

impl Default for MineOptions {
    fn default() -> Self {
        MineOptions {
            min_crates: 5,
            min_sites: 10,
            min_ratio: 0.9,
            min_crate_ratio: 0.85,
            min_lift: 3.0,
            per_crate_cap: 20,
            max_companions: 3,
            max_evidence: 5,
        }
    }
}

/// Ecosystem beliefs rank below the project's own at equal agreement: the
/// project may use the API differently on purpose.
const WEIGHT: f64 = 0.85;

/// Engler's z for `agree` of `total`: `(2 agree - total) / sqrt(total)`.
pub fn z_score(agree: usize, total: usize) -> f64 {
    if total == 0 {
        return 0.0;
    }
    (2.0 * agree as f64 - total as f64) / (total as f64).sqrt()
}

/// How much likelier an event is under the condition than overall:
/// `(hits / cases) / base`.
pub fn lift(hits: usize, cases: usize, base: f64) -> f64 {
    if cases == 0 || base <= 0.0 {
        return 0.0;
    }
    (hits as f64 / cases as f64) / base
}

/// `z / (z + 2)`, capped, scaled by the crates' agreement.
fn confidence(z: f64, support: &Support) -> f64 {
    if z <= 0.0 || support.total_crates == 0 {
        return 0.0;
    }
    let crates = support.agree_crates as f64 / support.total_crates as f64;
    ((z / (z + 2.0)).min(0.95) * crates * WEIGHT).clamp(0.0, 0.95)
}

/// One site with its crate and the global id of its API.
struct Site<'a> {
    krate: usize,
    obs: &'a SiteObs,
    /// Global ids of the APIs before/after it on its object.
    before: Vec<u32>,
    after: Vec<u32>,
}

/// Every crate's sites under global API ids.
struct Population<'a> {
    crates: &'a [CrateObservations],
    apis: Vec<&'a str>,
    returns: Vec<Option<&'a str>>,
    /// Per API: its sites (each crate's first `per_crate_cap`).
    sites: Vec<Vec<Site<'a>>>,
    /// Per API: the crates calling it.
    crates_of: Vec<BTreeSet<usize>>,
    /// Per library: (crate, object) pairs any of its APIs touches; per API:
    /// the pairs it touches.
    lib_objects: HashMap<&'a str, usize>,
    /// Per library: the crates calling any of its APIs.
    lib_crates: HashMap<&'a str, BTreeSet<usize>>,
    objects_of: Vec<BTreeSet<(usize, u32)>>,
    /// Per owner (`std@1::CString`): its APIs.
    by_owner: HashMap<&'a str, Vec<u32>>,
    /// Per API: most of its calls are made on a receiver (a method of an
    /// object, not a constructor).
    on_receiver: Vec<bool>,
}

impl<'a> Population<'a> {
    fn new(crates: &'a [CrateObservations], options: &MineOptions) -> Self {
        let mut ids: HashMap<&'a str, u32> = HashMap::new();
        let mut apis: Vec<&'a str> = Vec::new();
        let mut returns: Vec<Option<&'a str>> = Vec::new();
        let mut locals: Vec<Vec<u32>> = Vec::new();
        for krate in crates {
            let mut local = Vec::with_capacity(krate.apis.len());
            for (index, api) in krate.apis.iter().enumerate() {
                let id = *ids.entry(api.as_str()).or_insert_with(|| {
                    apis.push(api.as_str());
                    returns.push(None);
                    (apis.len() - 1) as u32
                });
                if returns[id as usize].is_none() {
                    returns[id as usize] = krate.returns.get(index).and_then(|r| r.as_deref());
                }
                local.push(id);
            }
            locals.push(local);
        }
        let mut sites: Vec<Vec<Site<'a>>> = (0..apis.len()).map(|_| Vec::new()).collect();
        let mut crates_of: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); apis.len()];
        let mut objects_of: Vec<BTreeSet<(usize, u32)>> = vec![BTreeSet::new(); apis.len()];
        let mut lib_object_sets: HashMap<&'a str, BTreeSet<(usize, u32)>> = HashMap::new();
        let mut lib_crates: HashMap<&'a str, BTreeSet<usize>> = HashMap::new();
        for (c, krate) in crates.iter().enumerate() {
            let local = &locals[c];
            let global = |id: &u32| local.get(*id as usize).copied();
            let mut taken: HashMap<u32, usize> = HashMap::new();
            for obs in &krate.sites {
                let Some(api) = global(&obs.api) else {
                    continue;
                };
                crates_of[api as usize].insert(c);
                let library = api_library(apis[api as usize]);
                lib_crates.entry(library).or_default().insert(c);
                if let Some(object) = obs.object.filter(|_| !obs.escapes) {
                    objects_of[api as usize].insert((c, object));
                    lib_object_sets
                        .entry(library)
                        .or_default()
                        .insert((c, object));
                }
                let count = taken.entry(api).or_default();
                if *count >= options.per_crate_cap {
                    continue;
                }
                *count += 1;
                sites[api as usize].push(Site {
                    krate: c,
                    obs,
                    before: obs.before.iter().filter_map(global).collect(),
                    after: obs.after.iter().filter_map(global).collect(),
                });
            }
        }
        let mut receivers: Vec<(usize, usize)> = vec![(0, 0); apis.len()];
        for (c, krate) in crates.iter().enumerate() {
            for obs in &krate.sites {
                if let Some(&api) = locals[c].get(obs.api as usize) {
                    let entry = &mut receivers[api as usize];
                    entry.0 += usize::from(obs.receiver);
                    entry.1 += 1;
                }
            }
        }
        let on_receiver = receivers
            .iter()
            .map(|&(with, all)| all > 0 && 2 * with > all)
            .collect();
        let mut by_owner: HashMap<&'a str, Vec<u32>> = HashMap::new();
        for (id, api) in apis.iter().enumerate() {
            by_owner.entry(api_owner(api)).or_default().push(id as u32);
        }
        Population {
            crates,
            by_owner,
            on_receiver,
            apis,
            returns,
            sites,
            crates_of,
            lib_objects: lib_object_sets
                .into_iter()
                .map(|(lib, set)| (lib, set.len()))
                .collect(),
            lib_crates,
            objects_of,
        }
    }

    fn evidence<'s>(
        &self,
        agreeing: impl Iterator<Item = &'s Site<'s>>,
        max: usize,
    ) -> Vec<EvidenceSite>
    where
        'a: 's,
    {
        let mut seen = BTreeSet::new();
        agreeing
            .filter(|site| seen.insert(site.krate))
            .take(max)
            .map(|site| EvidenceSite {
                krate: self.crates[site.krate].label(),
                file: site.obs.file.clone(),
                line: site.obs.line,
            })
            .collect()
    }
}

/// Mine every belief `crates` support. `is_status` says whether a declared
/// return type reports success or failure (the only results `ResultUsed`
/// is kept for).
pub fn mine(
    crates: &[CrateObservations],
    is_status: &dyn Fn(&str) -> bool,
    options: &MineOptions,
) -> Vec<Belief> {
    let population = Population::new(crates, options);
    let mut beliefs = Vec::new();
    for api in 0..population.apis.len() as u32 {
        if population.crates_of[api as usize].len() < options.min_crates {
            continue;
        }
        beliefs.extend(result_used(&population, api, is_status, options));
        beliefs.extend(ordering(&population, api, Direction::After, options));
        beliefs.extend(ordering(&population, api, Direction::Before, options));
        beliefs.extend(paired(&population, api, options));
        beliefs.extend(not_held_across_await(&population, api, options));
    }
    beliefs.sort_by(|a, b| {
        a.api
            .cmp(&b.api)
            .then_with(|| a.kind.rule().cmp(b.kind.rule()))
    });
    beliefs
}

/// Agreement by crate: a crate agrees when at least half its sites do.
fn crate_support<'s>(sites: &[&'s Site<'s>], agrees: impl Fn(&Site<'_>) -> bool) -> Support {
    let mut per_crate: BTreeMap<usize, (usize, usize)> = BTreeMap::new();
    let mut agree_sites = 0;
    for site in sites {
        let entry = per_crate.entry(site.krate).or_default();
        entry.1 += 1;
        if agrees(site) {
            entry.0 += 1;
            agree_sites += 1;
        }
    }
    Support {
        agree_crates: per_crate
            .values()
            .filter(|(agree, total)| 2 * agree >= *total)
            .count(),
        total_crates: per_crate.len(),
        agree_sites,
        total_sites: sites.len(),
    }
}

fn strong(support: &Support, options: &MineOptions) -> bool {
    support.total_sites >= options.min_sites
        && support.agree_crates >= options.min_crates
        && support.agree_sites as f64 >= options.min_ratio * support.total_sites as f64
        && support.agree_crates as f64 >= options.min_crate_ratio * support.total_crates as f64
}

fn result_used(
    population: &Population<'_>,
    api: u32,
    is_status: &dyn Fn(&str) -> bool,
    options: &MineOptions,
) -> Option<Belief> {
    let returns = population.returns[api as usize]?;
    if !is_status(returns) {
        return None;
    }
    let sites: Vec<&Site<'_>> = population.sites[api as usize]
        .iter()
        .filter(|site| site.obs.use_class != UseClass::Checked)
        .collect();
    let support = crate_support(&sites, |site| site.obs.use_class == UseClass::Used);
    // A crate departs only by a bare discard; `let _ =` is a choice.
    let discarding: BTreeSet<usize> = sites
        .iter()
        .filter(|site| site.obs.use_class == UseClass::Discarded)
        .map(|site| site.krate)
        .collect();
    let support = Support {
        agree_crates: support.total_crates - discarding.len(),
        ..support
    };
    if !strong(&support, options) {
        return None;
    }
    let z = z_score(support.agree_sites, support.total_sites);
    Some(Belief {
        api: population.apis[api as usize].to_string(),
        kind: BeliefKind::ResultUsed,
        returns: Some(returns.to_string()),
        support,
        z,
        lift: None,
        confidence: confidence(z, &support),
        evidence: population.evidence(
            sites
                .iter()
                .copied()
                .filter(|site| site.obs.use_class == UseClass::Used),
            options.max_evidence,
        ),
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Direction {
    Before,
    After,
}

/// The APIs on a site's object after or before it.
fn side<'s>(site: &'s Site<'_>, direction: Direction) -> &'s [u32] {
    match direction {
        Direction::After => &site.after,
        Direction::Before => &site.before,
    }
}

/// `FollowedBy`/`PrecededBy`: the fewest companions (at most
/// `max_companions`, each with its own lift) whose union covers the
/// required share of the API's local-object sites.
fn ordering(
    population: &Population<'_>,
    api: u32,
    direction: Direction,
    options: &MineOptions,
) -> Option<Belief> {
    // A follow-up protocol is a method's (`field` → `finish`): what usually
    // happens to a new object (`HashSet::new` → `insert`) is a habit, not
    // a protocol — the object may be filled with `extend` just as well.
    if direction == Direction::After && !population.on_receiver[api as usize] {
        return None;
    }
    // A precursor is only seen for an object made in view.
    let sites: Vec<&Site<'_>> = population.sites[api as usize]
        .iter()
        .filter(|site| {
            site.obs.object.is_some()
                && !site.obs.escapes
                && (direction == Direction::After || site.obs.constructed)
        })
        .collect();
    if sites.len() < options.min_sites {
        return None;
    }
    let library = api_library(population.apis[api as usize]);
    let lib_objects = population.lib_objects.get(library).copied().unwrap_or(0);
    if lib_objects == 0 {
        return None;
    }
    let base = |set: &[u32]| -> f64 {
        let objects: BTreeSet<(usize, u32)> = set
            .iter()
            .flat_map(|b| population.objects_of[*b as usize].iter().copied())
            .collect();
        objects.len() as f64 / lib_objects as f64
    };
    let mut hits: BTreeMap<u32, usize> = BTreeMap::new();
    for site in &sites {
        for &b in side(site, direction) {
            *hits.entry(b).or_default() += 1;
        }
    }
    // Candidates: calls on the object (a constructor always comes first and
    // is no step of a protocol — `with_capacity` does not initialise what
    // `set_len` exposes), each more likely here than anywhere in the library.
    let mut candidates: Vec<(u32, usize)> = hits
        .into_iter()
        .filter(|&(b, count)| {
            population.on_receiver[b as usize]
                && api_library(population.apis[b as usize]) == library
                && lift(count, sites.len(), base(&[b])) >= options.min_lift
        })
        .collect();
    candidates.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut chosen: Vec<u32> = Vec::new();
    let mut covered = vec![false; sites.len()];
    while chosen.len() < options.max_companions {
        let best = candidates
            .iter()
            .filter(|(b, _)| !chosen.contains(b))
            .map(|&(b, _)| {
                let gain = sites
                    .iter()
                    .enumerate()
                    .filter(|(i, site)| !covered[*i] && side(site, direction).contains(&b))
                    .count();
                (b, gain)
            })
            .filter(|&(_, gain)| gain > 0)
            .max_by(|x, y| x.1.cmp(&y.1).then(y.0.cmp(&x.0)));
        let Some((b, _)) = best else {
            break;
        };
        chosen.push(b);
        for (i, site) in sites.iter().enumerate() {
            covered[i] |= side(site, direction).contains(&b);
        }
        if covered.iter().filter(|c| **c).count() as f64 >= options.min_ratio * sites.len() as f64 {
            break;
        }
    }
    if chosen.is_empty() {
        return None;
    }
    let agrees = |site: &Site<'_>| side(site, direction).iter().any(|b| chosen.contains(b));
    let support = crate_support(&sites, agrees);
    if !strong(&support, options) {
        return None;
    }
    let set_lift = lift(support.agree_sites, support.total_sites, base(&chosen));
    if set_lift < options.min_lift {
        return None;
    }
    let names: Vec<String> = chosen
        .iter()
        .map(|b| population.apis[*b as usize].to_string())
        .collect();
    let z = z_score(support.agree_sites, support.total_sites);
    Some(Belief {
        api: population.apis[api as usize].to_string(),
        kind: match direction {
            Direction::After => BeliefKind::FollowedBy { others: names },
            Direction::Before => BeliefKind::PrecededBy { others: names },
        },
        returns: population.returns[api as usize].map(str::to_string),
        support,
        z,
        lift: Some(set_lift),
        confidence: confidence(z, &support),
        evidence: population.evidence(
            sites.iter().copied().filter(|site| agrees(site)),
            options.max_evidence,
        ),
    })
}

/// Acquire/release verbs whose objects are the same remaining words:
/// `into_raw`/`from_raw`, `lock`/`unlock`, `acquire`/`release`.
const RELEASE_PAIRS: &[(&str, &str)] = &[
    ("into", "from"),
    ("leak", "from"),
    ("lock", "unlock"),
    ("acquire", "release"),
    ("begin", "commit"),
    ("begin", "end"),
    ("enter", "exit"),
    ("alloc", "dealloc"),
    ("alloc", "free"),
    ("open", "close"),
    ("register", "unregister"),
    ("register", "deregister"),
    ("subscribe", "unsubscribe"),
    ("attach", "detach"),
    ("connect", "disconnect"),
    ("start", "stop"),
    ("start", "finish"),
    ("init", "deinit"),
    ("create", "destroy"),
];

/// Whether `b` releases what `a` acquires, by name: one verb pair, the
/// other words equal (`into_raw`/`from_raw`, `into_raw_fd`/`from_raw_fd`),
/// or `leak`/`from_raw` (`Box::leak` is undone by `Box::from_raw`).
/// `into`/`from` pair only over a raw resource: `into_parts`/`from_parts`
/// is a conversion, not a release (and `push`/`pop` is no pair at all — a
/// heap drained by `into_sorted_vec` leaks nothing).
pub fn is_release_pair(a: &str, b: &str) -> bool {
    let a_words: Vec<&str> = a.split('_').filter(|w| !w.is_empty()).collect();
    let b_words: Vec<&str> = b.split('_').filter(|w| !w.is_empty()).collect();
    RELEASE_PAIRS.iter().any(|&(acquire, release)| {
        let (Some(i), Some(j)) = (
            a_words.iter().position(|w| *w == acquire),
            b_words.iter().position(|w| *w == release),
        ) else {
            return false;
        };
        let mut a_rest = a_words.clone();
        a_rest.remove(i);
        let mut b_rest = b_words.clone();
        b_rest.remove(j);
        let raw = b_rest.contains(&"raw");
        if matches!(acquire, "into" | "leak") && !raw {
            return false;
        }
        a_rest == b_rest || (acquire == "leak" && a_rest.is_empty() && b_rest == ["raw"])
    })
}

fn paired(population: &Population<'_>, api: u32, options: &MineOptions) -> Vec<Belief> {
    let name = population.apis[api as usize];
    let owner = api_owner(name);
    let with_a = &population.crates_of[api as usize];
    let library = api_library(name);
    let lib_crates = population.lib_crates.get(library).map_or(0, BTreeSet::len);
    let mut out = Vec::new();
    let siblings = population
        .by_owner
        .get(owner)
        .map_or(&[][..], Vec::as_slice);
    for &b in siblings {
        let b = b as usize;
        let b_name = population.apis[b];
        if b as u32 == api || !is_release_pair(api_name(name), api_name(b_name)) {
            continue;
        }
        let with_b = &population.crates_of[b];
        let both = with_a.intersection(with_b).count();
        let support = Support {
            agree_crates: both,
            total_crates: with_a.len(),
            agree_sites: both,
            total_sites: with_a.len(),
        };
        if support.agree_crates < options.min_crates
            || (support.agree_crates as f64) < 0.8 * support.total_crates as f64
        {
            continue;
        }
        let base = with_b.len() as f64 / lib_crates.max(1) as f64;
        let pair_lift = lift(both, with_a.len(), base);
        let z = z_score(both, with_a.len());
        let evidence = population.evidence(
            population.sites[b]
                .iter()
                .filter(|site| with_a.contains(&site.krate)),
            options.max_evidence,
        );
        out.push(Belief {
            api: name.to_string(),
            kind: BeliefKind::PairedWith {
                other: (*b_name).to_string(),
            },
            returns: population.returns[api as usize].map(str::to_string),
            support,
            z,
            lift: Some(pair_lift),
            confidence: confidence(z, &support),
            evidence,
        });
    }
    out
}

fn not_held_across_await(
    population: &Population<'_>,
    api: u32,
    options: &MineOptions,
) -> Option<Belief> {
    let sites: Vec<&Site<'_>> = population.sites[api as usize]
        .iter()
        .filter(|site| site.obs.await_held.is_some())
        .collect();
    let support = crate_support(&sites, |site| site.obs.await_held == Some(false));
    // A crate that ever holds it across an await departs.
    let holding: BTreeSet<usize> = sites
        .iter()
        .filter(|site| site.obs.await_held == Some(true))
        .map(|site| site.krate)
        .collect();
    let support = Support {
        agree_crates: support.total_crates - holding.len(),
        ..support
    };
    if !strong(&support, options) {
        return None;
    }
    let z = z_score(support.agree_sites, support.total_sites);
    Some(Belief {
        api: population.apis[api as usize].to_string(),
        kind: BeliefKind::NotHeldAcrossAwait,
        returns: population.returns[api as usize].map(str::to_string),
        support,
        z,
        lift: None,
        confidence: confidence(z, &support),
        evidence: population.evidence(
            sites
                .iter()
                .copied()
                .filter(|site| site.obs.await_held == Some(false)),
            options.max_evidence,
        ),
    })
}

/// What the population says about one API, strong or not — for `deps
/// beliefs show`: how its result is used, what follows and precedes it on
/// its object (top companions with their share and lift), and how often
/// it is held across an `.await`.
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiProfile {
    pub api: String,
    pub crates: usize,
    pub sites: usize,
    pub used: usize,
    pub discarded: usize,
    pub handled: usize,
    pub checked: usize,
    pub local_object_sites: usize,
    /// `(api, share of local-object sites, lift)`.
    pub followed_by: Vec<(String, f64, f64)>,
    pub preceded_by: Vec<(String, f64, f64)>,
    pub await_sites: usize,
    pub await_held: usize,
}

/// The profile of `api` over `crates` (`None` when no crate calls it).
pub fn profile(
    crates: &[CrateObservations],
    api: &str,
    options: &MineOptions,
) -> Option<ApiProfile> {
    let population = Population::new(crates, options);
    let id = population.apis.iter().position(|known| *known == api)? as u32;
    let sites = &population.sites[id as usize];
    let count = |class: UseClass| sites.iter().filter(|s| s.obs.use_class == class).count();
    let local: Vec<&Site<'_>> = sites
        .iter()
        .filter(|site| site.obs.object.is_some() && !site.obs.escapes)
        .collect();
    let library = api_library(api);
    let lib_objects = population
        .lib_objects
        .get(library)
        .copied()
        .unwrap_or(0)
        .max(1);
    let companions = |direction: Direction| -> Vec<(String, f64, f64)> {
        let mut hits: BTreeMap<u32, usize> = BTreeMap::new();
        for site in &local {
            for b in side(site, direction) {
                *hits.entry(*b).or_default() += 1;
            }
        }
        let mut rows: Vec<(String, f64, f64)> = hits
            .into_iter()
            .map(|(b, count)| {
                let base = population.objects_of[b as usize].len() as f64 / lib_objects as f64;
                (
                    population.apis[b as usize].to_string(),
                    count as f64 / local.len().max(1) as f64,
                    lift(count, local.len(), base),
                )
            })
            .collect();
        rows.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        rows.truncate(8);
        rows
    };
    Some(ApiProfile {
        api: api.to_string(),
        crates: population.crates_of[id as usize].len(),
        sites: sites.len(),
        used: count(UseClass::Used),
        discarded: count(UseClass::Discarded),
        handled: count(UseClass::Handled),
        checked: count(UseClass::Checked),
        local_object_sites: local.len(),
        followed_by: companions(Direction::After),
        preceded_by: companions(Direction::Before),
        await_sites: sites.iter().filter(|s| s.obs.await_held.is_some()).count(),
        await_held: sites
            .iter()
            .filter(|s| s.obs.await_held == Some(true))
            .count(),
    })
}
