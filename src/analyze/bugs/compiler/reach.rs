//! Which functions untrusted input reaches, from the index alone.
//!
//! Entry points, strongest first: functions a framework route dispatches to
//! (the index's `route` nodes — axum `.route(…, get(h))`, actix
//! `web::resource`/`#[get]`, rocket…), then functions whose parameters are
//! request extractors (`Json<T>`, `Query<T>`, `HttpRequest`…: handlers
//! registered in ways the route scan misses), and — only when a project has
//! neither — its public functions, the API a library's callers feed. From
//! them a breadth-first walk over the index's resolved call edges, at most
//! [`MAX_DEPTH`] calls deep, records each function's nearest entry and the
//! call it was reached through, so a finding can show the path.

use std::collections::{HashMap, VecDeque};
use std::sync::LazyLock;

use regex::Regex;

use crate::analyze::bugs::Project;

/// Calls followed from an entry point. Deeper than this, "reachable" says
/// little about who controls a value.
pub const MAX_DEPTH: u32 = 8;

/// What kind of entry point a function is reached from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EntryKind {
    /// A framework route's handler.
    Route,
    /// A function taking a request extractor.
    Extractor,
    /// A public function of a project with no request handlers.
    PublicApi,
}

/// One entry point.
#[derive(Debug, Clone)]
pub struct Entry {
    pub fn_id: String,
    pub kind: EntryKind,
    /// `GET /upload`, `extractor Json<…>`, `public API`.
    pub label: String,
    /// The handler's qualified name.
    pub name: String,
    pub file: String,
    pub line: u32,
}

/// How a function is reached: its nearest entry, how many calls away, and
/// the call it was reached through (`None` for the entry itself).
#[derive(Debug, Clone)]
struct Hit {
    entry: usize,
    depth: u32,
    /// The caller's id and the call into this function.
    via: Option<(String, Step)>,
}

/// One step of a path from an entry point to a function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub caller: String,
    pub callee: String,
    pub file: String,
    pub line: u32,
}

/// Reachability from the project's entry points.
pub struct Reach {
    pub entries: Vec<Entry>,
    hits: HashMap<String, Hit>,
}

/// A function's reachability, as a finding reports it.
#[derive(Debug, Clone)]
pub struct Reached<'a> {
    pub entry: &'a Entry,
    pub depth: u32,
    /// The calls from the entry to the function, entry first.
    pub path: Vec<Step>,
}

/// Parameter types that carry a request: axum/actix/warp extractors and raw
/// requests. `Path<` only in its generic form (`&Path` is `std::path`).
static EXTRACTOR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\b(?:(?:Json|Query|Form|Path|TypedHeader|Request)\s*<|(?:RawForm|RawQuery|Multipart|WebSocketUpgrade|Payload|HttpRequest)\b)",
    )
    .expect("valid extractor regex")
});

/// Parameter types that carry a library caller's data: bytes, text,
/// readers.
static INPUT_PARAM_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"&\s*(?:'\w+\s+)?(?:mut\s+)?(?:\[u8\]|str\b)|\b(?:Vec<u8>|String|Bytes|BytesMut|Cow<'?\w*,?\s*(?:str|\[u8\])>)|\b(?:Read|BufRead|AsyncRead)\b",
    )
    .expect("valid input-parameter regex")
});

impl Reach {
    /// Entry points of `project` and everything they reach.
    pub fn compute(project: &Project) -> Self {
        let entries = entries(project);
        let mut calls: HashMap<&str, Vec<usize>> = HashMap::new();
        for (index, site) in project.call_sites().iter().enumerate() {
            if !site.in_test {
                calls
                    .entry(site.caller_id.as_str())
                    .or_default()
                    .push(index);
            }
        }
        // Multi-source BFS, entries in strength order, so a function gets
        // its strongest nearest entry.
        let mut hits: HashMap<String, Hit> = HashMap::new();
        let mut queue = VecDeque::new();
        for (index, entry) in entries.iter().enumerate() {
            if !hits.contains_key(&entry.fn_id) {
                hits.insert(
                    entry.fn_id.clone(),
                    Hit {
                        entry: index,
                        depth: 0,
                        via: None,
                    },
                );
                queue.push_back(entry.fn_id.clone());
            }
        }
        while let Some(id) = queue.pop_front() {
            let (entry, depth) = {
                let hit = &hits[&id];
                (hit.entry, hit.depth)
            };
            if depth >= MAX_DEPTH {
                continue;
            }
            for &site in calls.get(id.as_str()).into_iter().flatten() {
                let site = &project.call_sites()[site];
                if hits.contains_key(&site.callee_id) {
                    continue;
                }
                hits.insert(
                    site.callee_id.clone(),
                    Hit {
                        entry,
                        depth: depth + 1,
                        via: Some((
                            id.clone(),
                            Step {
                                caller: site.caller.clone(),
                                callee: site.callee_qualified.clone(),
                                file: site.file.clone(),
                                line: site.line,
                            },
                        )),
                    },
                );
                queue.push_back(site.callee_id.clone());
            }
        }
        Self { entries, hits }
    }

    /// How `fn_id` is reached, with the call path, or `None`.
    pub fn reached(&self, fn_id: &str) -> Option<Reached<'_>> {
        let hit = self.hits.get(fn_id)?;
        let mut path = Vec::new();
        let mut at = fn_id;
        while let Some(Hit {
            via: Some((caller, step)),
            ..
        }) = self.hits.get(at)
        {
            path.push(step.clone());
            at = caller;
        }
        path.reverse();
        Some(Reached {
            entry: &self.entries[hit.entry],
            depth: hit.depth,
            path,
        })
    }

    /// Whether the project has request handlers (else entries are its
    /// public API).
    #[cfg(test)]
    pub fn has_handlers(&self) -> bool {
        self.entries.iter().any(|e| e.kind != EntryKind::PublicApi)
    }
}

/// The project's entry points: route handlers, then extractor-taking
/// functions, else public functions.
fn entries(project: &Project) -> Vec<Entry> {
    let spans: HashMap<&str, &crate::analyze::bugs::FnSpan> = project
        .files()
        .iter()
        .flat_map(|file| project.functions_in(file))
        .map(|span| (span.id.as_str(), span))
        .collect();
    // Free functions by name, for route handlers the index resolved to a
    // method: the route scan keeps only a handler path's last segment
    // (`calendar_api::get_events` → `get_events`), which can land on an
    // unrelated `CalendarManager::get_events(&self, …)`. A framework cannot
    // dispatch to a `self` method, so such a target stands for the free
    // function of that name (up to [`MAX_NAMESAKES`] of them).
    const MAX_NAMESAKES: usize = 3;
    let mut free_fns: HashMap<&str, Vec<&crate::analyze::bugs::FnSpan>> = HashMap::new();
    for span in spans.values() {
        if span.kind == "function" && !span.is_test {
            free_fns.entry(span.name.as_str()).or_default().push(span);
        }
    }
    let mut entries = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for route in project.routes() {
        let Some(span) = spans.get(route.handler_id.as_str()) else {
            continue;
        };
        if span.is_test {
            continue;
        }
        let takes_self = span.kind == "method"
            && span.signature.as_deref().is_some_and(|sig| {
                sig.trim_start_matches('(')
                    .trim_start()
                    .trim_start_matches(['&', ' '])
                    .trim_start_matches("mut ")
                    .starts_with("self")
            });
        let handlers: Vec<&crate::analyze::bugs::FnSpan> = if takes_self {
            match free_fns.get(span.name.as_str()) {
                Some(named) if named.len() <= MAX_NAMESAKES => named.clone(),
                _ => Vec::new(),
            }
        } else {
            vec![*span]
        };
        for handler in handlers {
            if !seen.insert((handler.id.as_str(), route.route.as_str())) {
                continue;
            }
            entries.push(Entry {
                fn_id: handler.id.clone(),
                kind: EntryKind::Route,
                label: route.route.clone(),
                name: handler.qualified_name.clone(),
                file: route.file.clone(),
                line: route.line,
            });
        }
    }
    entries.sort_by(|a, b| (&a.file, a.line, &a.name).cmp(&(&b.file, b.line, &b.name)));
    let mut extractors: Vec<&crate::analyze::bugs::FnSpan> = spans
        .values()
        .filter(|span| !span.is_test)
        .filter(|span| {
            span.signature
                .as_deref()
                .and_then(|sig| sig.split("->").next())
                .is_some_and(|params| EXTRACTOR_RE.is_match(params))
        })
        .copied()
        .collect();
    extractors.sort_by(|a, b| (&a.file, a.start_line).cmp(&(&b.file, b.start_line)));
    for span in extractors {
        let params = span.signature.as_deref().unwrap_or_default();
        let found = EXTRACTOR_RE
            .find(params)
            .map(|m| {
                m.as_str()
                    .trim_end_matches([',', ')', '<'])
                    .trim()
                    .to_string()
            })
            .unwrap_or_default();
        entries.push(Entry {
            fn_id: span.id.clone(),
            kind: EntryKind::Extractor,
            label: format!("takes {found}"),
            name: span.qualified_name.clone(),
            file: span.file.clone(),
            line: span.start_line,
        });
    }
    if entries.is_empty() {
        // A library's untrusted input arrives through public functions that
        // take bytes, text or a reader (parsers, decoders); through the rest
        // of its API when it has none of those.
        let public: Vec<&crate::analyze::bugs::FnSpan> = spans
            .values()
            .filter(|span| !span.is_test && project.is_public(&span.id))
            .copied()
            .collect();
        let takes_input = |span: &&crate::analyze::bugs::FnSpan| {
            span.signature
                .as_deref()
                .and_then(|sig| sig.split("->").next())
                .is_some_and(|params| INPUT_PARAM_RE.is_match(params))
        };
        let mut chosen: Vec<&crate::analyze::bugs::FnSpan> =
            public.iter().copied().filter(takes_input).collect();
        let label = if chosen.is_empty() {
            chosen = public;
            "public API"
        } else {
            "public API taking bytes, text or a reader"
        };
        chosen.sort_by(|a, b| (&a.file, a.start_line).cmp(&(&b.file, b.start_line)));
        for span in chosen {
            entries.push(Entry {
                fn_id: span.id.clone(),
                kind: EntryKind::PublicApi,
                label: label.to_string(),
                name: span.qualified_name.clone(),
                file: span.file.clone(),
                line: span.start_line,
            });
        }
    }
    entries
}

#[cfg(test)]
pub(super) mod tests {
    use std::path::Path;

    use super::*;
    use crate::analyze::bugs::project::RouteHandler;
    use crate::analyze::bugs::{CallSite, FnSpan};

    pub(crate) fn span(id: &str, file: &str, start: u32, end: u32, sig: &str) -> FnSpan {
        FnSpan {
            id: id.into(),
            name: id.into(),
            qualified_name: format!("m::{id}"),
            kind: "function".into(),
            file: file.into(),
            start_line: start,
            end_line: end,
            start_col: 0,
            signature: Some(sig.into()),
            return_type: None,
            is_test: false,
        }
    }

    pub(crate) fn call(from: &str, to: &str, file: &str, line: u32) -> CallSite {
        CallSite {
            caller_id: from.into(),
            caller: format!("m::{from}"),
            file: file.into(),
            line,
            col: 4,
            callee_id: to.into(),
            callee_name: to.into(),
            callee_qualified: format!("m::{to}"),
            callee_kind: "function".into(),
            callee_signature: None,
            callee_return_type: None,
            callee_file: file.into(),
            callee_line: 1,
            in_test: false,
        }
    }

    #[test]
    fn routes_reach_through_calls_and_record_the_path() {
        let functions = vec![
            span("upload", "src/api.rs", 1, 10, "(body: Vec<u8>) -> u8"),
            span("parse", "src/api.rs", 12, 20, "(b: &[u8]) -> u8"),
            span("header", "src/api.rs", 22, 30, "(b: &[u8]) -> u8"),
            span("offline", "src/tool.rs", 1, 5, "() -> ()"),
        ];
        let calls = vec![
            call("upload", "parse", "src/api.rs", 3),
            call("parse", "header", "src/api.rs", 14),
        ];
        let project = Project::from_parts(
            Path::new("/p"),
            vec!["src/api.rs".into(), "src/tool.rs".into()],
            functions,
            calls,
        )
        .with_entries(
            vec![RouteHandler {
                handler_id: "upload".into(),
                route: "POST /upload".into(),
                file: "src/main.rs".into(),
                line: 7,
            }],
            &["offline"],
        );
        let reach = Reach::compute(&project);
        assert!(reach.has_handlers());
        assert_eq!(
            reach.entries.len(),
            1,
            "public fns are not entries next to routes"
        );
        let header = reach.reached("header").unwrap();
        assert_eq!(header.entry.label, "POST /upload");
        assert_eq!(header.depth, 2);
        assert_eq!(
            header
                .path
                .iter()
                .map(|s| (s.caller.as_str(), s.callee.as_str(), s.line))
                .collect::<Vec<_>>(),
            vec![("m::upload", "m::parse", 3), ("m::parse", "m::header", 14)]
        );
        assert_eq!(reach.reached("upload").unwrap().depth, 0);
        assert!(reach.reached("offline").is_none());
    }

    #[test]
    fn a_route_resolved_to_a_self_method_stands_for_the_free_function() {
        let mut method = span(
            "m_get_events",
            "src/calendar.rs",
            1,
            9,
            "(&self, id: &str) -> u8",
        );
        method.kind = "method".into();
        method.name = "get_events".into();
        let mut handler = span(
            "h_get_events",
            "src/api.rs",
            1,
            9,
            "(Path(id): Path<String>) -> u8",
        );
        handler.name = "get_events".into();
        let project = Project::from_parts(
            Path::new("/p"),
            vec!["src/api.rs".into(), "src/calendar.rs".into()],
            vec![method, handler],
            vec![],
        )
        .with_entries(
            vec![RouteHandler {
                handler_id: "m_get_events".into(),
                route: "GET /{id}/events".into(),
                file: "src/lib.rs".into(),
                line: 40,
            }],
            &[],
        );
        let reach = Reach::compute(&project);
        let routes: Vec<&str> = reach
            .entries
            .iter()
            .filter(|e| e.kind == EntryKind::Route)
            .map(|e| e.fn_id.as_str())
            .collect();
        assert_eq!(routes, ["h_get_events"]);
        assert!(reach.reached("m_get_events").is_none());
    }

    #[test]
    fn extractor_signatures_are_entries_and_libraries_fall_back_to_public_fns() {
        let functions = vec![
            span(
                "create",
                "src/h.rs",
                1,
                5,
                "(Json(body): Json<Item>) -> StatusCode",
            ),
            span("open", "src/h.rs", 7, 9, "(path: &Path) -> File"),
        ];
        let project = Project::from_parts(
            Path::new("/p"),
            vec!["src/h.rs".into()],
            functions.clone(),
            vec![],
        );
        let reach = Reach::compute(&project);
        assert_eq!(reach.entries.len(), 1, "`&Path` is not an extractor");
        assert_eq!(reach.entries[0].kind, EntryKind::Extractor);
        assert_eq!(reach.entries[0].label, "takes Json");

        let library = Project::from_parts(
            Path::new("/p"),
            vec!["src/h.rs".into()],
            vec![functions[1].clone()],
            vec![],
        )
        .with_entries(vec![], &["open"]);
        let reach = Reach::compute(&library);
        assert!(!reach.has_handlers());
        assert_eq!(reach.entries[0].kind, EntryKind::PublicApi);
        assert_eq!(reach.entries[0].label, "public API");

        // A public parser is where a library's input comes in; the rest of
        // its API is then not an entry point.
        let parser = Project::from_parts(
            Path::new("/p"),
            vec!["src/h.rs".into()],
            vec![
                functions[1].clone(),
                span("decode", "src/h.rs", 11, 20, "(data: &'a [u8]) -> Item"),
                span("from_reader", "src/h.rs", 21, 30, "<R: Read>(r: R) -> Item"),
            ],
            vec![],
        )
        .with_entries(vec![], &["open", "decode", "from_reader"]);
        let reach = Reach::compute(&parser);
        let names: Vec<&str> = reach.entries.iter().map(|e| e.fn_id.as_str()).collect();
        assert_eq!(names, ["decode", "from_reader"]);
        assert_eq!(
            reach.entries[0].label,
            "public API taking bytes, text or a reader"
        );
    }
}
