#[path = "common/jvm_resolution.rs"]
mod jvm_resolution;

use codegraph::types::{EdgeKind, NodeKind};
use jvm_resolution::{Fx, class_node, node, ref_from};

#[test]
fn java_imported_type_reference_wins_over_same_package_candidate() {
    let fx = Fx::new();
    let q = fx.q();
    let caller_file = "server/src/main/java/com/acme/web/Handler.java";
    let imported_file = "server/src/main/java/com/acme/service/Settings.java";
    let same_package_file = "server/src/main/java/com/acme/web/Settings.java";

    fx.write(
        caller_file,
        "package com.acme.web;\nimport com.acme.service.Settings;\nclass Handler { Settings settings; }\n",
    );
    fx.write(
        imported_file,
        "package com.acme.service;\nclass Settings {}\n",
    );
    fx.write(
        same_package_file,
        "package com.acme.web;\nclass Settings {}\n",
    );
    for file in [caller_file, imported_file, same_package_file] {
        fx.track(&q, file);
    }

    let caller = node(
        "method:server:web:Handler.use",
        NodeKind::Method,
        "use",
        "com.acme.web::Handler::use",
        caller_file,
        2,
        2,
    );
    let same_package = class_node(
        "class:server:web:Settings",
        "com.acme.web",
        same_package_file,
    );
    let imported = class_node(
        "class:server:service:Settings",
        "com.acme.service",
        imported_file,
    );
    q.insert_nodes(&[caller.clone(), same_package, imported.clone()])
        .expect("insert nodes");

    let resolver = fx.resolver();
    resolver.warm_caches();
    let resolved = resolver
        .resolve_one(&ref_from(
            &caller,
            "Settings",
            EdgeKind::References,
            caller_file,
        ))
        .expect("Settings resolves");

    assert_eq!(resolved.target_node_id, imported.id);
}

#[test]
fn java_same_package_type_reference_beats_path_proximity() {
    let fx = Fx::new();
    let q = fx.q();
    let caller_file = "server/src/test/java/com/acme/web/Handler.java";
    let correct_file = "server/src/main/java/com/acme/web/Settings.java";
    let closer_wrong_file = "server/src/test/java/com/acme/other/Settings.java";

    fx.write(
        caller_file,
        "package com.acme.web;\nclass Handler { Settings settings; }\n",
    );
    fx.write(correct_file, "package com.acme.web;\nclass Settings {}\n");
    fx.write(
        closer_wrong_file,
        "package com.acme.other;\nclass Settings {}\n",
    );
    for file in [caller_file, correct_file, closer_wrong_file] {
        fx.track(&q, file);
    }

    let caller = node(
        "method:server:test:web:Handler.use",
        NodeKind::Method,
        "use",
        "com.acme.web::Handler::use",
        caller_file,
        2,
        2,
    );
    let wrong = class_node(
        "class:server:test:other:Settings",
        "com.acme.other",
        closer_wrong_file,
    );
    let correct = class_node(
        "class:server:main:web:Settings",
        "com.acme.web",
        correct_file,
    );
    q.insert_nodes(&[caller.clone(), wrong, correct.clone()])
        .expect("insert nodes");

    let resolver = fx.resolver();
    resolver.warm_caches();
    let resolved = resolver
        .resolve_one(&ref_from(
            &caller,
            "Settings",
            EdgeKind::References,
            caller_file,
        ))
        .expect("Settings resolves");

    assert_eq!(resolved.target_node_id, correct.id);
}

#[test]
fn java_global_fallback_still_resolves_without_package_scope() {
    let fx = Fx::new();
    let q = fx.q();
    let caller_file = "src/Main.java";
    let target_file = "lib/Foo.java";

    fx.write(caller_file, "class Main { Foo foo; }\n");
    fx.write(target_file, "package lib;\nclass Foo {}\n");
    for file in [caller_file, target_file] {
        fx.track(&q, file);
    }

    let caller = node(
        "method:src:Main.run",
        NodeKind::Method,
        "run",
        "src/Main.java::Main::run",
        caller_file,
        1,
        1,
    );
    let target = node(
        "class:lib:Foo",
        NodeKind::Class,
        "Foo",
        "lib::Foo",
        target_file,
        1,
        1,
    );
    q.insert_nodes(&[caller.clone(), target.clone()])
        .expect("insert nodes");

    let resolver = fx.resolver();
    resolver.warm_caches();
    let resolved = resolver
        .resolve_one(&ref_from(&caller, "Foo", EdgeKind::References, caller_file))
        .expect("Foo resolves through global fallback");

    assert_eq!(resolved.target_node_id, target.id);
}
