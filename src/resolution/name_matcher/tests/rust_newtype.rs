//! Locals bound by a single-field tuple-struct pattern with a written type
//! (`State(state): State<AppState>` in an axum handler): typed as the one
//! field the written type holds, when that field is known, else not at all.

use super::rust_call::{FILE, at_call, id, project_with, rust_node, target, use_decl};
use crate::resolution::types::UnresolvedRef;
use crate::types::{Node, NodeKind, dropped_receiver_metadata};

const SOURCE: &str = "\
use axum::extract::State;
use axum::Json;
use actix_web::web;
use actix_web::web::Data;
fn put_root(State(state): State<AppState>, Json(req): Json<PutRootRequest>) -> u8 {
    state.devices.caller(token); // state
    req.holder.cache.clear(); // json
    let Json(body): Json<Holder> = parse();
    body.cache.clear(); // let
    run(|State(s): State<AppState>| s.devices.caller(t)); // closure
    state.cache.lock().clear(); // guard
    let guard = state.cache.lock();
    guard.clear(); // guard local
}
fn actix(web::Json(item): web::Json<Holder>) {
    item.cache.clear(); // actix
}
fn project_wrap(Wrap(w): Wrap<Holder>, Inner(i): Inner) {
    w.cache.clear(); // wrap
    i.clear(); // inner
}
fn unknown(Foo(x): Foo<Holder>, Data(d): Data<Holder>, Pair(a, b): Pair<Holder, Holder>) {
    x.cache.clear(); // foreign
    d.cache.clear(); // data
    b.cache.clear(); // pair
}
fn mismatched(State(s): Json<Holder>, Boxed(v): Boxed<Holder>, &Wrap(r): &Wrap<Holder>) {
    s.cache.clear(); // mismatch
    v.cache.clear(); // mentions param
    r.cache.clear(); // reference
}
";

const WRAP: &str = "\
pub struct Wrap<T>(pub T);
pub struct Inner(pub Cache);
pub struct Pair<A, B>(pub A, pub B);
pub struct Boxed<T>(pub Vec<T>);
";

fn method(qualified: &str, file: &str, line: u32, signature: &str) -> Node {
    let mut node = rust_node(NodeKind::Method, qualified, file, line);
    node.signature = Some(signature.into());
    node
}

fn fixture() -> super::Fixture {
    let extra = vec![
        use_decl(FILE, "use axum::extract::State;", 1),
        use_decl(FILE, "use axum::Json;", 2),
        use_decl(FILE, "use actix_web::web;", 3),
        use_decl(FILE, "use actix_web::web::Data;", 4),
        rust_node(NodeKind::Struct, "AppState", "src/state.rs", 1),
        rust_node(NodeKind::Field, "AppState::devices", "src/state.rs", 2),
        rust_node(NodeKind::Field, "AppState::cache", "src/state.rs", 3),
        rust_node(NodeKind::Struct, "PutRootRequest", "src/state.rs", 5),
        rust_node(NodeKind::Field, "PutRootRequest::holder", "src/state.rs", 6),
        rust_node(NodeKind::Struct, "Holder", "src/state.rs", 8),
        rust_node(NodeKind::Field, "Holder::cache", "src/state.rs", 9),
        rust_node(NodeKind::Struct, "DeviceManager", "src/device.rs", 1),
        method(
            "DeviceManager::caller",
            "src/device.rs",
            5,
            "(&self, auth: &str) -> Result<u8>",
        ),
        // A same-named decoy, so only the typed receiver decides.
        method("Session::caller", "src/session.rs", 5, "(&self) -> u8"),
        rust_node(NodeKind::Struct, "Cache", "src/cache.rs", 1),
        method("Cache::clear", "src/cache.rs", 5, "(&mut self)"),
        rust_node(NodeKind::Struct, "Wrap", "src/wrap.rs", 1),
        rust_node(NodeKind::Struct, "Inner", "src/wrap.rs", 2),
        rust_node(NodeKind::Struct, "Pair", "src/wrap.rs", 3),
        rust_node(NodeKind::Struct, "Boxed", "src/wrap.rs", 4),
    ];
    let mut fixture = project_with(SOURCE, extra);
    fixture.files.insert(
        "src/state.rs".into(),
        "pub struct AppState {\n    pub devices: DeviceManager,\n    pub cache: Arc<Mutex<Cache>>,\n}\npub struct PutRootRequest {\n    pub holder: Holder,\n}\npub struct Holder {\n    pub cache: Cache,\n}\n".into(),
    );
    fixture.files.insert("src/wrap.rs".into(), WRAP.into());
    fixture
}

/// The call of `name` on the line marked `marker`, its receiver `receiver`
/// dropped and recorded as text.
fn resolved(fixture: &super::Fixture, marker: &str, name: &str, receiver: &str) -> Option<String> {
    let reference = UnresolvedRef {
        metadata: Some(dropped_receiver_metadata(Some(receiver.to_string()))),
        ..at_call(fixture, SOURCE, marker, name, receiver)
    };
    target(fixture, &reference)
}

fn clear() -> Option<String> {
    Some(id(NodeKind::Method, "Cache::clear", "src/cache.rs"))
}

#[test]
fn axum_extractor_patterns_type_their_binding() {
    let fixture = fixture();
    let caller = Some(id(
        NodeKind::Method,
        "DeviceManager::caller",
        "src/device.rs",
    ));
    assert_eq!(
        resolved(&fixture, "// state", "caller", "state.devices"),
        caller
    );
    assert_eq!(
        resolved(&fixture, "// json", "clear", "req.holder.cache"),
        clear()
    );
    assert_eq!(resolved(&fixture, "// let", "clear", "body.cache"), clear());
    assert_eq!(
        resolved(&fixture, "// closure", "caller", "s.devices"),
        caller
    );
    assert_eq!(
        resolved(&fixture, "// actix", "clear", "item.cache"),
        clear()
    );
}

/// A call on a lock result (`m.lock().clear()`, chained or through a
/// local) runs on the value the lock guards: only a guard that hands the
/// value back (`parking_lot`, `tokio`) has such methods.
#[test]
fn calls_on_a_lock_guard_run_on_the_guarded_value() {
    let fixture = fixture();
    assert_eq!(
        resolved(&fixture, "// guard", "clear", "state.cache.lock()"),
        clear()
    );
    let local = UnresolvedRef {
        reference_name: "guard.clear".into(),
        ..at_call(
            &fixture,
            SOURCE,
            "// guard local",
            "guard.clear",
            "guard.clear",
        )
    };
    assert_eq!(target(&fixture, &local), clear());
}

/// A project tuple struct's one field is read from its declaration: a
/// generic parameter is the written type argument, a concrete type itself.
#[test]
fn project_tuple_structs_type_their_binding() {
    let fixture = fixture();
    assert_eq!(resolved(&fixture, "// wrap", "clear", "w.cache"), clear());
    let inner = UnresolvedRef {
        reference_name: "i.clear".into(),
        ..at_call(&fixture, SOURCE, "// inner", "i.clear", "i.clear")
    };
    assert_eq!(target(&fixture, &inner), clear());
}

/// An external wrapper outside the table (`Foo`, actix's `Data`, which
/// wraps an `Arc`), a two-field struct, a field that only mentions the
/// parameter, a constructor that is not the written type, and a reference
/// pattern all leave the binding unknown: `clear` is a std name, so the
/// call stays unresolved.
#[test]
fn unknown_fields_are_not_guessed() {
    let fixture = fixture();
    for (marker, receiver) in [
        ("// foreign", "x.cache"),
        ("// data", "d.cache"),
        ("// pair", "b.cache"),
        ("// mismatch", "s.cache"),
        ("// mentions param", "v.cache"),
        ("// reference", "r.cache"),
    ] {
        assert_eq!(
            resolved(&fixture, marker, "clear", receiver),
            None,
            "{marker}"
        );
    }
}
