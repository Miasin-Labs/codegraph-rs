use crate::resolution::types::{
    ImportMapping,
    ResolutionContext,
    ResolvedBy,
    ResolvedRef,
    UnresolvedRef,
};
use crate::types::{EdgeKind, Language};

fn qualified_name_from_fqn(fqn: &str) -> Option<String> {
    let last_dot = fqn.rfind('.')?;
    if last_dot == 0 {
        return None;
    }
    Some(format!("{}::{}", &fqn[..last_dot], &fqn[last_dot + 1..]))
}

pub(super) fn resolve_jvm_import(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    if reference.reference_kind != EdgeKind::Imports {
        return None;
    }
    if reference.language != Language::Java && reference.language != Language::Kotlin {
        return None;
    }

    let fqn = &reference.reference_name;
    let last_dot = fqn.rfind('.')?;
    if last_dot == 0 {
        return None;
    }
    let pkg = &fqn[..last_dot];
    let sym = &fqn[last_dot + 1..];
    // Wildcard imports (`com.example.*`) deliberately punt to name-matcher.
    if sym == "*" {
        return None;
    }

    let candidates = context.get_nodes_by_qualified_name(&format!("{pkg}::{sym}"));
    let first = candidates.first()?;

    Some(ResolvedRef {
        original: reference.clone(),
        target_node_id: first.id.clone(),
        confidence: 0.95,
        resolved_by: ResolvedBy::Import,
    })
}

/// Resolve a Java/Kotlin reference whose receiver is the simple name of
/// an imported FQN: `Foo.bar(...)` where `import com.example.Foo;`. The
/// imported FQN converts to a file-path suffix (`com/example/Foo.java`
/// or `.kt`) which uniquely identifies the right symbol when multiple
/// classes share the same simple name.
///
/// Also handles bare references to the imported class itself
/// (`new Foo()` extraction emits `Foo` as a `references`/`instantiates`
/// ref) and `import static <Foo>.bar` style imports of a single member.
pub(super) fn resolve_java_imported_reference(
    reference: &UnresolvedRef,
    imports: &[ImportMapping],
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    if imports.is_empty() {
        return None;
    }

    let ext = if reference.language == Language::Kotlin {
        ".kt"
    } else {
        ".java"
    };

    for imp in imports {
        let matches_bare = imp.local_name == reference.reference_name;
        let matches_qualified = reference
            .reference_name
            .starts_with(&format!("{}.", imp.local_name));
        if !matches_bare && !matches_qualified {
            continue;
        }

        if matches_bare {
            if let Some(qualified_name) = qualified_name_from_fqn(&imp.source) {
                for node in context.get_nodes_by_qualified_name(&qualified_name) {
                    if node.language == reference.language {
                        return Some(ResolvedRef {
                            original: reference.clone(),
                            target_node_id: node.id.clone(),
                            confidence: 0.95,
                            resolved_by: ResolvedBy::Import,
                        });
                    }
                }
            }
        }

        // Convert FQN to a file-path suffix. `com.example.Foo` ->
        // `com/example/Foo.java` (or `.kt`). The actual file may live
        // under any source root (`src/main/java/`, `src/`, etc.), so match
        // by suffix rather than exact path.
        let fqn_path = format!("{}{ext}", imp.source.replace('.', "/"));

        // Which symbol name to look up: the class itself, or a member.
        let member_name = if matches_bare {
            imp.local_name.clone()
        } else {
            reference.reference_name[imp.local_name.len() + 1..].to_string()
        };

        let candidates = context.get_nodes_by_name(&member_name);
        for node in &candidates {
            if node.language != reference.language {
                continue;
            }
            let fp = node.file_path.replace('\\', "/");
            if fp.ends_with(&fqn_path) || fp.ends_with(&format!("/{fqn_path}")) {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: node.id.clone(),
                    confidence: 0.9,
                    resolved_by: ResolvedBy::Import,
                });
            }
        }

        // `import static com.example.Foo.bar;` — the FQN's tail is the
        // member name, the part before is the owner class. Look up the
        // member named `<imp.localName>` (e.g. `bar`) and prefer the
        // candidate whose file matches the parent FQN's path.
        if matches_bare {
            if let Some(dot) = imp.source.rfind('.') {
                if dot > 0 {
                    let owner_fqn = &imp.source[..dot];
                    let owner_path = format!("{}{ext}", owner_fqn.replace('.', "/"));
                    for node in &candidates {
                        if node.language != reference.language {
                            continue;
                        }
                        let fp = node.file_path.replace('\\', "/");
                        if fp.ends_with(&owner_path) || fp.ends_with(&format!("/{owner_path}")) {
                            return Some(ResolvedRef {
                                original: reference.clone(),
                                target_node_id: node.id.clone(),
                                confidence: 0.9,
                                resolved_by: ResolvedBy::Import,
                            });
                        }
                    }
                }
            }
        }
    }
    None
}
