#[path = "common/jvm_resolution.rs"]
mod jvm_resolution;

use codegraph::types::{EdgeKind, NodeKind};
use jvm_resolution::{Fx, class_node, node, ref_from};

#[test]
fn java_same_package_class_receiver_beats_global_class_order() {
    let fx = Fx::new();
    let q = fx.q();
    let caller_file = "server/src/test/java/com/acme/web/Handler.java";
    let correct_file = "server/src/main/java/com/acme/web/Settings.java";
    let closer_wrong_file = "server/src/test/java/com/acme/other/Settings.java";

    fx.write(
        caller_file,
        "package com.acme.web;\nclass Handler { Object use() { return Settings.builder(); } }\n",
    );
    fx.write(correct_file, "package com.acme.web;\nclass Settings { static Settings builder() { return new Settings(); } }\n");
    fx.write(closer_wrong_file, "package com.acme.other;\nclass Settings { static Settings builder() { return new Settings(); } }\n");
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
    let wrong_class = class_node(
        "class:server:test:other:Settings",
        "com.acme.other",
        closer_wrong_file,
    );
    let wrong_method = node(
        "method:server:test:other:Settings.builder",
        NodeKind::Method,
        "builder",
        "com.acme.other::Settings::builder",
        closer_wrong_file,
        1,
        2,
    );
    let correct_class = class_node(
        "class:server:main:web:Settings",
        "com.acme.web",
        correct_file,
    );
    let correct_method = node(
        "method:server:main:web:Settings.builder",
        NodeKind::Method,
        "builder",
        "com.acme.web::Settings::builder",
        correct_file,
        1,
        2,
    );
    q.insert_nodes(&[
        caller.clone(),
        wrong_class,
        wrong_method,
        correct_class,
        correct_method.clone(),
    ])
    .expect("insert nodes");

    let resolver = fx.resolver();
    resolver.warm_caches();
    let resolved = resolver
        .resolve_one(&ref_from(
            &caller,
            "Settings.builder",
            EdgeKind::Calls,
            caller_file,
        ))
        .expect("Settings.builder resolves");

    assert_eq!(resolved.target_node_id, correct_method.id);
}
