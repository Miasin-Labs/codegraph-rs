//! Each source, sink, sanitizer and guard of the built-in Rust taint rules
//! (`rust-taint.yaml`), exercised one by one: a rule's examples replaced by
//! snippets aimed at one role each, checked like `analyze rules --check`.

use serde_yaml_ng::{Mapping, Value};

use super::check::check_rules;
use super::compile::RuleSet;

const RUST_TAINT: &str = include_str!("builtin/rust-taint.yaml");

/// A code example, optionally with a `resolves` map.
enum Example<'a> {
    Code(&'a str),
    Resolving(&'a str, &'a [(&'a str, &'a str)]),
}

fn example_value(example: &Example<'_>) -> Value {
    match example {
        Example::Code(code) => Value::String((*code).to_string()),
        Example::Resolving(code, resolves) => {
            let mut map = Mapping::new();
            map.insert("code".into(), Value::String((*code).to_string()));
            let mut resolved = Mapping::new();
            for (callee, target) in *resolves {
                resolved.insert((*callee).into(), (*target).into());
            }
            map.insert("resolves".into(), Value::Mapping(resolved));
            Value::Mapping(map)
        }
    }
}

/// The built-in rule `id` must match every `bad` snippet and none of the
/// `good` ones.
fn expect(id: &str, bad: &[Example<'_>], good: &[Example<'_>]) {
    let rules: Vec<Value> = serde_yaml_ng::from_str(RUST_TAINT).expect("rust-taint.yaml parses");
    let mut rule = rules
        .into_iter()
        .find(|rule| rule.get("id").and_then(Value::as_str) == Some(id))
        .unwrap_or_else(|| panic!("no rule {id}"));
    let mut examples = Mapping::new();
    examples.insert(
        "bad".into(),
        Value::Sequence(bad.iter().map(example_value).collect()),
    );
    examples.insert(
        "good".into(),
        Value::Sequence(good.iter().map(example_value).collect()),
    );
    rule.as_mapping_mut()
        .expect("a rule mapping")
        .insert("examples".into(), Value::Mapping(examples));
    let yaml = serde_yaml_ng::to_string(&vec![rule]).expect("serializes");
    let set = RuleSet::load(&[], &[("t.yaml".to_string(), yaml)], false);
    assert!(set.errors.is_empty(), "{:?}", set.errors);
    let report = check_rules(&set);
    let failures: Vec<String> = report.rules[0]
        .examples
        .iter()
        .filter(|e| !e.passed)
        .map(|e| e.explain())
        .collect();
    assert!(failures.is_empty(), "{id}: {failures:#?}");
}

use Example::{Code, Resolving};

#[test]
fn each_web_source_reaches_a_command() {
    expect(
        "rust-command-injection",
        &[
            Code("async fn h(Path(id): Path<String>) { Command::new(id).spawn(); }"),
            Code("async fn h(q: Query<P>) { Command::new(&q.cmd).spawn(); }"),
            Code("async fn h(Json(P { cmd, .. }): Json<P>) { Command::new(cmd).spawn(); }"),
            Code("async fn h(body: web::Json<P>) { Command::new(&body.cmd).spawn(); }"),
            Code(
                "async fn h(headers: HeaderMap) {\n    let v = headers.get(\"x\").unwrap().to_str().unwrap();\n    Command::new(v).spawn();\n}",
            ),
            Code(
                "async fn h(State(s): State<App>, body: String) { Command::new(\"sh\").arg(body).spawn(); }",
            ),
            Code(
                "fn h(mut stream: TcpStream) {\n    let mut b = vec![0u8; 8];\n    stream.read_exact(&mut b).unwrap();\n    Command::new(String::from_utf8(b).unwrap()).spawn();\n}",
            ),
            Code(
                "async fn h(Path(id): Path<String>) { tokio::process::Command::new(\"x\").args([\"-n\", &id]).spawn(); }",
            ),
        ],
        &[
            // A `String` parameter of a plain function is not a request body.
            Code("fn h(body: String) { Command::new(body).spawn(); }"),
            // Environment input is its own rule.
            Code("fn h() { Command::new(std::env::var(\"X\").unwrap()).spawn(); }"),
            // A file read is not a peer read.
            Code(
                "fn h(mut file: File) {\n    let mut b = Vec::new();\n    file.read_to_end(&mut b).unwrap();\n    Command::new(String::from_utf8(b).unwrap()).spawn();\n}",
            ),
            // A project's own `Command::new` is not the library's.
            Resolving(
                "async fn h(Path(id): Path<String>) { Command::new(id).spawn(); }",
                &[("Command::new", "src/cmd.rs::Command::new")],
            ),
        ],
    );
}

#[test]
fn environment_and_stdin_reach_a_command() {
    expect(
        "rust-command-from-environment",
        &[
            Code("fn h() { Command::new(env::var(\"PAGER\").unwrap()).spawn(); }"),
            Code(
                "fn h() { for line in std::io::stdin().lines() { Command::new(line.unwrap()).spawn(); } }",
            ),
            Resolving(
                "fn h() { Command::new(std::env::var(\"PAGER\").unwrap()).spawn(); }",
                &[("Command::new", "std::std/src/process.rs::Command::new")],
            ),
        ],
        &[Code(
            "fn h() {\n    let p = std::env::var(\"PAGER\").unwrap();\n    if p.chars().all(|c| c.is_ascii_alphanumeric()) {\n        Command::new(p).spawn();\n    }\n}",
        )],
    );
}

#[test]
fn file_sinks_and_path_guards() {
    let read = |guard: &str| {
        format!(
            "async fn h(Path(name): Path<String>) {{\n    let p = Path::new(\"/srv\").join(&name);\n{guard}\n}}"
        )
    };
    let bad = [
        "async fn h(Path(n): Path<String>) { std::fs::read_to_string(n).unwrap(); }".to_string(),
        "async fn h(Path(n): Path<String>) { File::open(format!(\"/srv/{n}\")).unwrap(); }"
            .to_string(),
        "async fn h(Query(q): Query<Q>) { tokio::fs::remove_dir_all(&q.dir).await.unwrap(); }"
            .to_string(),
        "async fn h(Path(n): Path<String>) { OpenOptions::new().write(true).open(n).unwrap(); }"
            .to_string(),
        "async fn h(Path(n): Path<String>) -> ServeFile { ServeFile::new(n) }".to_string(),
        // A prefix check without canonicalization does not stop `..`.
        read("    if p.starts_with(\"/srv\") { fs::read(&p).unwrap(); }"),
    ];
    let good = [
        read("    if p.to_str().unwrap().contains(\"..\") { return; }\n    fs::read(&p).unwrap();"),
        "async fn h(Path(name): Path<String>) {\n    if !name.contains(\"..\") { fs::read(Path::new(\"/srv\").join(&name)).unwrap(); }\n}"
            .to_string(),
        read(
            "    if p.components().any(|c| c == Component::ParentDir) { return; }\n    fs::read(&p).unwrap();",
        ),
        read(
            "    if p.components().all(|c| matches!(c, Component::Normal(_))) { fs::read(&p).unwrap(); }",
        ),
        read(
            "    let c = fs::canonicalize(&p).unwrap();\n    if c.starts_with(\"/srv\") { fs::read(&c).unwrap(); }",
        ),
        read("    if !is_safe_path(&p) { return; }\n    fs::read(&p).unwrap();"),
        "async fn h(Path(n): Path<String>) { fs::read(Path::new(&n).file_name().unwrap()).unwrap(); }"
            .to_string(),
        "async fn h(Path(n): Path<String>) { fs::read(sanitize_filename::sanitize(&n)).unwrap(); }"
            .to_string(),
        // A validator whose error returns early (`?`) guards what follows.
        "async fn h(Path(n): Path<String>) -> Result<Vec<u8>> {\n    Uuid::parse_str(&n).map_err(|_| E::NotFound)?;\n    Ok(fs::read(format!(\"/srv/{n}.png\"))?)\n}"
            .to_string(),
        "async fn h(Path(n): Path<String>) -> Result<Vec<u8>> {\n    validate_name(&n)?;\n    Ok(fs::read(format!(\"/srv/{n}\"))?)\n}"
            .to_string(),
    ];
    let bad: Vec<Example<'_>> = bad.iter().map(|c| Code(c)).collect();
    let good: Vec<Example<'_>> = good.iter().map(|c| Code(c)).collect();
    expect("rust-path-traversal", &bad, &good);
}

#[test]
fn sql_sinks_bind_parameters_and_builders() {
    expect(
        "rust-sql-injection",
        &[
            Code(
                "async fn h(Path(id): Path<String>, c: &Client) { c.batch_execute(&format!(\"DROP TABLE {id}\")).await.unwrap(); }",
            ),
            Code(
                "async fn h(Path(id): Path<String>, db: &Connection) { db.query_row(&(\"SELECT \".to_string() + &id), [], |r| r.get(0)).unwrap(); }",
            ),
            Code(
                "async fn h(Path(id): Path<String>, db: &Pool) { sqlx::query_as::<_, Row>(&format!(\"SELECT {id}\")).fetch_all(db).await.unwrap(); }",
            ),
        ],
        &[
            // Bound parameters are not the statement.
            Code(
                "async fn h(Path(id): Path<String>, c: &Client) { c.query(\"SELECT * FROM t WHERE id = $1\", &[&id]).await.unwrap(); }",
            ),
            // A prepared statement's methods bind parameters.
            Code(
                "async fn h(Path(id): Path<String>, db: &Connection) { let mut stmt = db.prepare(\"SELECT 1 WHERE a = ?1\").unwrap(); stmt.query_map(params![id], |r| r.get(0)).unwrap(); }",
            ),
            // An HTTP builder's `.query(...)` is a URL query.
            Code(
                "async fn h(Path(id): Path<String>, http: &reqwest::Client) { http.get(\"https://x\").query(&[(\"id\", id)]).send().await.unwrap(); }",
            ),
        ],
    );
}

#[test]
fn ssrf_sinks_and_host_guards() {
    expect(
        "rust-ssrf",
        &[
            Code(
                "async fn h(Query(q): Query<Q>, client: &Client) { client.post(&q.url).send().await.unwrap(); }",
            ),
            Code("async fn h(Path(u): Path<String>) { reqwest::blocking::get(u).unwrap(); }"),
            Code(
                "async fn h(Path(u): Path<String>) { let r = Request::builder().uri(u).body(Body::empty()).unwrap(); }",
            ),
            Resolving(
                "async fn h(Path(u): Path<String>, api: &Api) { api.get(&u).send().await.unwrap(); }",
                &[("api.get", "reqwest::src/async_impl/client.rs::Client::get")],
            ),
        ],
        &[
            Code(
                "async fn h(Query(q): Query<Q>, client: &Client) {\n    let u = Url::parse(&q.url).unwrap();\n    if ALLOW.contains(u.host_str().unwrap()) {\n        client.get(u).send().await.unwrap();\n    }\n}",
            ),
            Code(
                "async fn h(Query(q): Query<Q>, client: &Client) {\n    if !is_allowed_url(&q.url) { return; }\n    client.get(&q.url).send().await.unwrap();\n}",
            ),
            // A map lookup named `get` is not a request.
            Code(
                "async fn h(Query(q): Query<Q>, seen: &HashMap<String, u8>) { seen.get(&q.url); }",
            ),
        ],
    );
}

#[test]
fn html_sinks_and_escapes() {
    expect(
        "rust-xss",
        &[Code(
            "async fn h(Path(n): Path<String>) -> Html<String> { Html(n) }",
        )],
        &[
            Code(
                "async fn h(Path(n): Path<String>) -> Html<String> { let e = escape_html(&n); Html(format!(\"<b>{e}</b>\")) }",
            ),
            Code(
                "async fn h(Path(n): Path<String>) -> HttpResponse { HttpResponse::Ok().content_type(\"application/json\").body(n) }",
            ),
            // A check inside `&&` guards the branch it opens.
            Code(
                "async fn h(Path(n): Path<String>) -> Html<String> {\n    if n.len() <= 16 && n.chars().all(|c| c.is_ascii_digit()) {\n        return Html(n);\n    }\n    Html(String::new())\n}",
            ),
        ],
    );
}

#[test]
fn allocation_sinks_and_bounds() {
    expect(
        "rust-allocation-from-input",
        &[
            Code("fn d(b: &mut Bytes) -> Vec<u8> { let n = b.get_u32() as usize; vec![0u8; n] }"),
            Code(
                "fn d(r: &mut R) { let n = r.read_u16::<LE>().unwrap(); let mut v: Vec<u8> = Vec::new(); v.reserve(n as usize); }",
            ),
        ],
        &[
            Code(
                "fn d(b: &mut Bytes) -> Vec<u8> { let n = b.get_u32() as usize; if n <= 4096 { vec![0u8; n] } else { Vec::new() } }",
            ),
            Code(
                "fn d(b: &mut Bytes) -> Vec<u8> { let n = b.get_u32() as usize; Vec::with_capacity(n.min(1024)) }",
            ),
        ],
    );
}
