//! Which functions untrusted input reaches, from the index alone. Computed
//! once per [`Project`] ([`Project::reach`]) and shared by the compiler
//! detector (its confidence) and the rules engine (the `reached-from`
//! predicate).
//!
//! Entry points, strongest first ([`EntryKind`]):
//!
//! - **route**: functions a framework route dispatches to (the index's
//!   `route` nodes, for every framework it extracts them for: axum
//!   `.route(…, get(h))`, actix `web::resource`/`#[get]`, rocket, Flask,
//!   FastAPI, Django, express…);
//! - **extractor**: functions whose parameters are request types (Rust
//!   `Json<T>`, `Query<T>`, `Multipart`, `HttpRequest`…; Go `*http.Request`,
//!   `gin.Context`…; Python `HttpRequest`; TS `NextRequest`, `req: Request`),
//!   handlers registered in ways the route scan misses;
//! - **listener**: functions that accept connections, i.e. call `accept()`
//!   (no arguments), `incoming()` or Go's `Accept()` on something the index
//!   leaves to a library (a `TcpListener`, a socket);
//! - **message**: functions taking a queue or bus message (lapin
//!   `Delivery`, rdkafka messages, `async_nats::Message`, `rumqttc::Publish`,
//!   Kafka `ConsumerRecord`);
//! - **public-api**: only when a project has none of the above, its public
//!   functions taking bytes, text or a reader (else all of them): the API a
//!   library's callers feed.
//!
//! From the entries of each kind, a breadth-first walk over the index's
//! resolved, non-test call edges, at most [`MAX_DEPTH`] calls deep, records
//! each function's nearest entry and the call it was reached through, so a
//! finding can show the path. One walk per kind: O(kinds × (reached
//! functions + their calls)) per project; a lookup is O(path). A function
//! that no resolved chain of calls links to an entry is **not reached**:
//! unknown counts as not reached (a callback the index cannot follow, a
//! dynamic dispatch it did not resolve, an entry kind it does not know).

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::LazyLock;

use regex::Regex;

use crate::analyze::bugs::{FnSpan, Project};
use crate::types::Language;

/// Calls followed from an entry point. Deeper than this, "reachable" says
/// little about who controls a value.
pub const MAX_DEPTH: u32 = 8;

/// What kind of entry point a function is reached from, strongest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EntryKind {
    /// A framework route's handler.
    Route,
    /// A function taking a request type.
    Extractor,
    /// A function accepting network connections.
    Listener,
    /// A function taking a queue or bus message.
    Message,
    /// A public function of a project with none of the above.
    PublicApi,
}

impl EntryKind {
    pub const ALL: [EntryKind; 5] = [
        EntryKind::Route,
        EntryKind::Extractor,
        EntryKind::Listener,
        EntryKind::Message,
        EntryKind::PublicApi,
    ];

    /// The kinds that make code run in a server that handles untrusted
    /// input (`reached-from: server`).
    pub const SERVER: [EntryKind; 4] = [
        EntryKind::Route,
        EntryKind::Extractor,
        EntryKind::Listener,
        EntryKind::Message,
    ];

    /// The name a rule's `reached-from` uses.
    pub fn as_str(self) -> &'static str {
        match self {
            EntryKind::Route => "route",
            EntryKind::Extractor => "extractor",
            EntryKind::Listener => "listener",
            EntryKind::Message => "message",
            EntryKind::PublicApi => "public-api",
        }
    }

    /// The kind a rule names `name`, or `None`.
    pub fn parse(name: &str) -> Option<EntryKind> {
        Some(match name {
            "route" | "route-handler" => EntryKind::Route,
            "extractor" | "request-extractor" => EntryKind::Extractor,
            "listener" => EntryKind::Listener,
            "message" | "message-handler" => EntryKind::Message,
            "public-api" => EntryKind::PublicApi,
            _ => return None,
        })
    }

    fn index(self) -> usize {
        self as usize
    }
}

/// One entry point.
#[derive(Debug, Clone)]
pub struct Entry {
    pub fn_id: String,
    pub kind: EntryKind,
    /// `GET /upload`, `takes Json`, `calls accept`, `public API`.
    pub label: String,
    /// The handler's qualified name.
    pub name: String,
    pub file: String,
    pub line: u32,
}

/// How a function is reached from the entries of one kind: its nearest
/// entry, how many calls away, and the call it was reached through (`None`
/// for the entry itself).
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
    /// Per [`EntryKind`] (by its index): function id → how it is reached.
    hits: Vec<HashMap<String, Hit>>,
    /// (file, owning type) → its methods' ids and qualified names, for
    /// [`Reach::reached_via_type`].
    methods: HashMap<(String, String), Vec<(String, String)>>,
}

/// The type a method (or associated function) belongs to: its qualified
/// name without the last segment (`FeedManager` of `FeedManager::new`).
fn owner(span: &FnSpan) -> Option<&str> {
    if span.kind != "method" {
        return None;
    }
    span.qualified_name
        .rsplit_once("::")
        .map(|(owner, _)| owner)
}

/// A function's reachability, as a finding reports it.
#[derive(Debug, Clone)]
pub struct Reached<'a> {
    pub entry: &'a Entry,
    pub depth: u32,
    /// The calls from the entry to the function, entry first.
    pub path: Vec<Step>,
}

impl Reached<'_> {
    /// The entry, as evidence names it: ``route `GET /x` (`api::upload`)``.
    pub fn entry_text(&self) -> String {
        let entry = self.entry;
        match entry.kind {
            EntryKind::Route => format!("route `{}` (`{}`)", entry.label, entry.name),
            EntryKind::Extractor => format!("request handler `{}` ({})", entry.name, entry.label),
            EntryKind::Listener => format!("listener `{}` ({})", entry.name, entry.label),
            EntryKind::Message => format!("message handler `{}` ({})", entry.name, entry.label),
            EntryKind::PublicApi => format!("public `{}`", entry.name),
        }
    }

    /// `, 2 calls away` (empty for the entry itself).
    pub fn distance_text(&self) -> String {
        match self.depth {
            0 => String::new(),
            1 => ", 1 call away".to_string(),
            n => format!(", {n} calls away"),
        }
    }
}

/// Parameter types that carry a request in Rust: axum/actix/warp/rocket
/// extractors and raw requests. `Path<` only in its generic form (`&Path`
/// is `std::path`). Not rmcp's MCP tool requests (`Parameters<T>`,
/// `RequestContext<RoleServer>`): a stdio MCP server's one client is the
/// local agent, and its `call_tool` dispatches to a CLI's whole surface
/// (testsprite's `git` calls, 2026-09).
static EXTRACTOR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\b(?:(?:Json|Query|Form|Path|TypedHeader|Request)\s*<|(?:RawForm|RawQuery|Multipart|WebSocketUpgrade|Payload|HttpRequest)\b)",
    )
    .expect("valid extractor regex")
});

/// Parameter types that carry a request in other languages: Go net/http,
/// gin, echo, fiber; Django, Starlette/FastAPI; Next, fastify, express.
static OTHER_REQUEST_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\*http\.Request\b|\b(?:gin\.Context|echo\.Context|fiber\.Ctx|HttpRequest|WSGIRequest|ASGIRequest|NextRequest|FastifyRequest)\b|\breq(?:uest)?\s*:\s*(?:express\.)?Request\b",
    )
    .expect("valid request regex")
});

/// Parameter types of a queue or bus message handler.
static MESSAGE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\b(?:lapin::message::)?Delivery\b|\b(?:BorrowedMessage|OwnedMessage|ConsumerRecord)\b|\b(?:async_)?nats::Message\b|\brumqttc::(?:v5::mqttbytes::v5::)?Publish\b",
    )
    .expect("valid message regex")
});

/// Parameter types that carry a library caller's data: bytes, text,
/// readers.
static INPUT_PARAM_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"&\s*(?:'\w+\s+)?(?:mut\s+)?(?:\[u8\]|str\b)|\b(?:Vec<u8>|String|Bytes|BytesMut|Cow<'?\w*,?\s*(?:str|\[u8\])>)|\b(?:Read|BufRead|AsyncRead)\b",
    )
    .expect("valid input-parameter regex")
});

/// The parameter list of a function's signature (what precedes `->`).
fn params(span: &FnSpan) -> Option<&str> {
    span.signature
        .as_deref()
        .and_then(|sig| sig.split("->").next())
}

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
        let hits = EntryKind::ALL
            .iter()
            .map(|&kind| walk(project, &entries, &calls, kind))
            .collect();
        let mut methods: HashMap<(String, String), Vec<(String, String)>> = HashMap::new();
        for file in project.files() {
            for span in project.functions_in(file) {
                if let Some(owner) = owner(span).filter(|_| !span.is_test) {
                    methods
                        .entry((file.clone(), owner.to_string()))
                        .or_default()
                        .push((span.id.clone(), span.qualified_name.clone()));
                }
            }
        }
        Self {
            entries,
            hits,
            methods,
        }
    }

    /// For a method (or associated function) no entry reaches: the nearest
    /// reached method of the same type (same owner, same file — the index
    /// names types without their module), with that method's qualified
    /// name. A constructor such as `Client::new` runs at startup, but the
    /// object it builds serves the requests that reach its other methods.
    pub fn reached_via_type(
        &self,
        span: &FnSpan,
        kinds: &[EntryKind],
    ) -> Option<(Reached<'_>, &str)> {
        let owner = owner(span)?;
        self.methods
            .get(&(span.file.clone(), owner.to_string()))?
            .iter()
            .filter(|(id, _)| *id != span.id)
            .filter_map(|(id, name)| {
                self.reached_from(id, kinds)
                    .map(|reached| (reached, name.as_str()))
            })
            .min_by_key(|(reached, name)| (reached.depth, reached.entry.kind, *name))
    }

    /// How `fn_id` is reached from its nearest entry of any kind (the
    /// stronger kind on a tie), with the call path, or `None`.
    pub fn reached(&self, fn_id: &str) -> Option<Reached<'_>> {
        self.reached_from(fn_id, &EntryKind::ALL)
    }

    /// How `fn_id` is reached from the nearest entry of one of `kinds` (the
    /// stronger kind on a tie), with the call path, or `None`.
    pub fn reached_from(&self, fn_id: &str, kinds: &[EntryKind]) -> Option<Reached<'_>> {
        let (kind, hit) = kinds
            .iter()
            .filter_map(|&kind| self.hits[kind.index()].get(fn_id).map(|hit| (kind, hit)))
            .min_by_key(|(kind, hit)| (hit.depth, *kind))?;
        let hits = &self.hits[kind.index()];
        let mut path = Vec::new();
        let mut at = fn_id;
        while let Some(Hit {
            via: Some((caller, step)),
            ..
        }) = hits.get(at)
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

    /// The strongest kind of entry the project has (`None`: no entries).
    pub fn strongest_kind(&self) -> Option<EntryKind> {
        self.entries.iter().map(|entry| entry.kind).min()
    }

    /// Whether the project has request handlers (else entries are its
    /// public API).
    #[cfg(test)]
    pub fn has_handlers(&self) -> bool {
        self.entries.iter().any(|e| e.kind != EntryKind::PublicApi)
    }
}

/// Breadth-first from the entries of `kind`, in their order, so a
/// function's nearest entry is the first listed on a tie.
fn walk(
    project: &Project,
    entries: &[Entry],
    calls: &HashMap<&str, Vec<usize>>,
    kind: EntryKind,
) -> HashMap<String, Hit> {
    let mut hits: HashMap<String, Hit> = HashMap::new();
    let mut queue = VecDeque::new();
    for (index, entry) in entries.iter().enumerate() {
        if entry.kind == kind && !hits.contains_key(&entry.fn_id) {
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
    hits
}

/// The project's entry points: route handlers, request-taking functions,
/// listeners and message handlers; else public functions.
fn entries(project: &Project) -> Vec<Entry> {
    let spans: HashMap<&str, &FnSpan> = project
        .files()
        .iter()
        .flat_map(|file| project.functions_in(file))
        .map(|span| (span.id.as_str(), span))
        .collect();
    let mut entries = route_entries(project, &spans);

    // Request- and message-taking functions, by signature.
    let mut extractors: Vec<(&FnSpan, String)> = Vec::new();
    let mut messages: Vec<(&FnSpan, String)> = Vec::new();
    for span in spans.values().copied().filter(|span| !span.is_test) {
        let Some(params) = params(span) else {
            continue;
        };
        let request = if crate::extraction::detect_language(&span.file, None) == Language::Rust {
            &*EXTRACTOR_RE
        } else {
            &*OTHER_REQUEST_RE
        };
        if let Some(found) = request.find(params) {
            let found = found.as_str().trim_end_matches([',', ')', '<']).trim();
            extractors.push((span, format!("takes {found}")));
        } else if let Some(found) = MESSAGE_RE.find(params) {
            messages.push((span, format!("takes {}", found.as_str())));
        }
    }
    // Connection-accepting functions, by their library calls.
    let mut listeners: Vec<(&FnSpan, String)> = Vec::new();
    let mut listening = HashSet::new();
    for call in project.listener_calls() {
        let Some(span) = spans.get(call.caller_id.as_str()).copied() else {
            continue;
        };
        if !span.is_test && listening.insert(span.id.as_str()) {
            listeners.push((span, format!("calls `{}`", call.callee)));
        }
    }
    for (kind, mut found) in [
        (EntryKind::Extractor, extractors),
        (EntryKind::Listener, listeners),
        (EntryKind::Message, messages),
    ] {
        found.sort_by(|a, b| (&a.0.file, a.0.start_line).cmp(&(&b.0.file, b.0.start_line)));
        for (span, label) in found {
            entries.push(Entry {
                fn_id: span.id.clone(),
                kind,
                label,
                name: span.qualified_name.clone(),
                file: span.file.clone(),
                line: span.start_line,
            });
        }
    }
    if entries.is_empty() {
        entries = public_entries(project, &spans);
    }
    entries
}

/// Route handlers, by where their routes are registered.
fn route_entries(project: &Project, spans: &HashMap<&str, &FnSpan>) -> Vec<Entry> {
    // Free functions by name, for route handlers the index resolved to a
    // method: the route scan keeps only a handler path's last segment
    // (`calendar_api::get_events` → `get_events`), which can land on an
    // unrelated `CalendarManager::get_events(&self, …)`. A framework cannot
    // dispatch to a `self` method, so such a target stands for the free
    // function of that name (up to [`MAX_NAMESAKES`] of them).
    const MAX_NAMESAKES: usize = 3;
    let mut free_fns: HashMap<&str, Vec<&FnSpan>> = HashMap::new();
    for span in spans.values() {
        if span.kind == "function" && !span.is_test {
            free_fns.entry(span.name.as_str()).or_default().push(span);
        }
    }
    let mut entries = Vec::new();
    let mut seen = HashSet::new();
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
        let handlers: Vec<&FnSpan> = if takes_self {
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
    entries
}

/// A library's entry points: its public functions that take bytes, text
/// or a reader (parsers, decoders), else the rest of its public API.
fn public_entries(project: &Project, spans: &HashMap<&str, &FnSpan>) -> Vec<Entry> {
    let public: Vec<&FnSpan> = spans
        .values()
        .filter(|span| !span.is_test && project.is_public(&span.id))
        .copied()
        .collect();
    let takes_input =
        |span: &&FnSpan| params(span).is_some_and(|params| INPUT_PARAM_RE.is_match(params));
    let mut chosen: Vec<&FnSpan> = public.iter().copied().filter(takes_input).collect();
    let label = if chosen.is_empty() {
        chosen = public;
        "public API"
    } else {
        "public API taking bytes, text or a reader"
    };
    chosen.sort_by(|a, b| (&a.file, a.start_line).cmp(&(&b.file, b.start_line)));
    chosen
        .into_iter()
        .map(|span| Entry {
            fn_id: span.id.clone(),
            kind: EntryKind::PublicApi,
            label: label.to_string(),
            name: span.qualified_name.clone(),
            file: span.file.clone(),
            line: span.start_line,
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use std::path::Path;

    use super::*;
    use crate::analyze::bugs::project::{LibraryCall, RouteHandler};
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

    #[test]
    fn listeners_and_message_handlers_are_server_entries() {
        let functions = vec![
            span("serve", "src/net.rs", 1, 20, "(l: TcpListener) -> ()"),
            span("session", "src/net.rs", 22, 40, "(s: TcpStream) -> ()"),
            span(
                "on_order",
                "src/q.rs",
                1,
                9,
                "(d: lapin::message::Delivery) -> ()",
            ),
            span(
                "go_handler",
                "web/h.go",
                1,
                9,
                "(w http.ResponseWriter, r *http.Request)",
            ),
            span("cli", "src/main.rs", 1, 9, "() -> ()"),
        ];
        let calls = vec![call("serve", "session", "src/net.rs", 6)];
        let project = Project::from_parts(
            Path::new("/p"),
            vec![
                "src/net.rs".into(),
                "src/q.rs".into(),
                "web/h.go".into(),
                "src/main.rs".into(),
            ],
            functions,
            calls,
        )
        .with_listener_calls(vec![LibraryCall {
            caller_id: "serve".into(),
            callee: "accept".into(),
            file: "src/net.rs".into(),
            line: 4,
        }])
        .with_entries(vec![], &["cli"]);
        let reach = Reach::compute(&project);
        let kinds: Vec<(&str, EntryKind)> = reach
            .entries
            .iter()
            .map(|e| (e.fn_id.as_str(), e.kind))
            .collect();
        assert_eq!(
            kinds,
            [
                ("go_handler", EntryKind::Extractor),
                ("serve", EntryKind::Listener),
                ("on_order", EntryKind::Message),
            ],
            "a server's public fns are no entries"
        );
        let session = reach.reached_from("session", &EntryKind::SERVER).unwrap();
        assert_eq!(session.entry.kind, EntryKind::Listener);
        assert_eq!(session.entry_text(), "listener `m::serve` (calls `accept`)");
        assert_eq!(session.distance_text(), ", 1 call away");
        assert!(reach.reached_from("session", &[EntryKind::Route]).is_none());
        assert!(reach.reached_from("cli", &EntryKind::SERVER).is_none());
    }

    #[test]
    fn the_nearest_entry_of_the_asked_kinds_wins() {
        // route → a → b → sink, and a listener calling sink directly.
        let functions = vec![
            span("route", "src/a.rs", 1, 5, "() -> ()"),
            span("a", "src/a.rs", 7, 9, "() -> ()"),
            span("b", "src/a.rs", 11, 13, "() -> ()"),
            span("sink", "src/a.rs", 15, 17, "() -> ()"),
            span("listen", "src/b.rs", 1, 9, "() -> ()"),
        ];
        let calls = vec![
            call("route", "a", "src/a.rs", 2),
            call("a", "b", "src/a.rs", 8),
            call("b", "sink", "src/a.rs", 12),
            call("listen", "sink", "src/b.rs", 5),
        ];
        let project = Project::from_parts(
            Path::new("/p"),
            vec!["src/a.rs".into(), "src/b.rs".into()],
            functions,
            calls,
        )
        .with_entries(
            vec![RouteHandler {
                handler_id: "route".into(),
                route: "GET /".into(),
                file: "src/a.rs".into(),
                line: 1,
            }],
            &[],
        )
        .with_listener_calls(vec![LibraryCall {
            caller_id: "listen".into(),
            callee: "incoming".into(),
            file: "src/b.rs".into(),
            line: 3,
        }]);
        let reach = Reach::compute(&project);
        let any = reach.reached("sink").unwrap();
        assert_eq!((any.entry.kind, any.depth), (EntryKind::Listener, 1));
        let route = reach.reached_from("sink", &[EntryKind::Route]).unwrap();
        assert_eq!(route.depth, 3);
        assert_eq!(route.path.len(), 3);
        assert_eq!(route.path[0].caller, "m::route");
    }
}
