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

#[tokio::test(flavor = "current_thread")]
async fn php_static_call_through_import_alias_resolves_to_aliased_class() {
    // Given: two classes define the same static method `getSettlesToExcel`,
    // and a controller calls it through an import alias
    // `use App\Services\SettleService as Settle;` (issue #1545).
    let fx = Fx::new();
    let q = fx.q();
    fx.write(
        "app/Services/SettleService.php",
        "<?php\nnamespace App\\Services;\nclass SettleService {\n  public static function getSettlesToExcel($stores) { return 1; }\n}\n",
    );
    fx.write(
        "app/Repositories/SettleRepository.php",
        "<?php\nnamespace App\\Repositories;\nclass SettleRepository {\n  public static function getSettlesToExcel($ids) { return 2; }\n}\n",
    );
    fx.write(
        "app/Http/Controllers/SettleController.php",
        "<?php\nnamespace App\\Http\\Controllers;\nuse App\\Services\\SettleService as Settle;\nclass SettleController {\n  public function excel($stores) {\n    return Settle::getSettlesToExcel($stores);\n  }\n}\n",
    );
    fx.track(&q, "app/Services/SettleService.php", Language::Php);
    fx.track(&q, "app/Repositories/SettleRepository.php", Language::Php);
    fx.track(
        &q,
        "app/Http/Controllers/SettleController.php",
        Language::Php,
    );

    let excel = node(
        "method:controller:excel",
        NodeKind::Method,
        "excel",
        "SettleController::excel",
        "app/Http/Controllers/SettleController.php",
        Language::Php,
        5,
        7,
    );
    let service_method = node(
        "method:service:getSettlesToExcel",
        NodeKind::Method,
        "getSettlesToExcel",
        "SettleService::getSettlesToExcel",
        "app/Services/SettleService.php",
        Language::Php,
        3,
        3,
    );
    let repo_method = node(
        "method:repo:getSettlesToExcel",
        NodeKind::Method,
        "getSettlesToExcel",
        "SettleRepository::getSettlesToExcel",
        "app/Repositories/SettleRepository.php",
        Language::Php,
        3,
        3,
    );
    // Insert the decoy Repository method FIRST so a method-name-only
    // fallback (which picks the first same-named candidate) would land on
    // the WRONG class — the failure mode this test guards against.
    q.insert_nodes(&[excel.clone(), repo_method.clone(), service_method.clone()])
        .unwrap();
    q.insert_unresolved_refs_batch(&[uref(
        &excel.id,
        "Settle.getSettlesToExcel",
        EdgeKind::Calls,
        6,
        "app/Http/Controllers/SettleController.php",
        Language::Php,
    )])
    .unwrap();

    // When: the real resolver processes the aliased static call.
    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .await
        .unwrap();

    // Then: the call resolves to the aliased SERVICE method, not the
    // same-named Repository method.
    let calls = outgoing(&q, &excel.id, EdgeKind::Calls);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].target, service_method.id);
    assert_ne!(calls[0].target, repo_method.id);
}
