use crate::fixture::*;

#[tokio::test(flavor = "current_thread")]
async fn php_property_receiver_resolves_declared_type_without_same_name_decoy() {
    // Given: a promoted property and two classes declaring greet().
    let fx = Fx::new();
    let q = fx.q();
    fx.write(
        "App.php",
        "<?php\nclass App {\n  public function __construct(private readonly Greeter $greeter) {}\n  public function run() { return $this->greeter->greet(); }\n}\n",
    );
    fx.track(&q, "App.php", Language::Php);

    let run = node(
        "method:app:run",
        NodeKind::Method,
        "run",
        "App::run",
        "App.php",
        Language::Php,
        4,
        4,
    );
    let greet = node(
        "method:greeter:greet",
        NodeKind::Method,
        "greet",
        "Greeter::greet",
        "Greeter.php",
        Language::Php,
        2,
        2,
    );
    let decoy = node(
        "method:other:greet",
        NodeKind::Method,
        "greet",
        "OtherGreeter::greet",
        "OtherGreeter.php",
        Language::Php,
        2,
        2,
    );
    q.insert_nodes(&[run.clone(), greet.clone(), decoy.clone()])
        .unwrap();
    q.insert_unresolved_refs_batch(&[uref(
        &run.id,
        "this->greeter.greet",
        EdgeKind::Calls,
        4,
        "App.php",
        Language::Php,
    )])
    .unwrap();

    // When: the real resolver processes the property call.
    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .await
        .unwrap();

    // Then: only the method on the declared property type is called.
    let calls = outgoing(&q, &run.id, EdgeKind::Calls);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].target, greet.id);
    assert_ne!(calls[0].target, decoy.id);
}
