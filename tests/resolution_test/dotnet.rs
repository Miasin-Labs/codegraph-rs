use crate::fixture::*;

#[test]
fn csharp_type_references_resolve_to_dto_classes_381() {
    // Extraction-side #381 produces `references` refs from method returns/
    // params, properties and fields; this pins the RESOLUTION of those refs
    // to the DTO classes.
    let fx = Fx::new();
    let q = fx.q();
    fx.write(
        "src/Dtos.cs",
        "namespace MyApp;\npublic class SessionInfoDto { public string Id { get; set; } = \"\"; }\npublic class UserDto { public string Name { get; set; } = \"\"; }\n",
    );
    fx.write(
        "src/Service.cs",
        "using System.Threading.Tasks;\nnamespace MyApp;\npublic class DataExporter\n{\n  public SessionInfoDto Build(UserDto user, SessionInfoDto session) { return session; }\n  public Task<SessionInfoDto> BuildAsync(UserDto user) { return Task.FromResult(new SessionInfoDto()); }\n  public SessionInfoDto Latest { get; set; } = new();\n  private UserDto _cached;\n}\n",
    );
    fx.track(&q, "src/Dtos.cs", Language::Csharp);
    fx.track(&q, "src/Service.cs", Language::Csharp);

    let session_dto = exported(node(
        "class:src/Dtos.cs:SessionInfoDto:2",
        NodeKind::Class,
        "SessionInfoDto",
        "MyApp::SessionInfoDto",
        "src/Dtos.cs",
        Language::Csharp,
        2,
        2,
    ));
    let user_dto = exported(node(
        "class:src/Dtos.cs:UserDto:3",
        NodeKind::Class,
        "UserDto",
        "MyApp::UserDto",
        "src/Dtos.cs",
        Language::Csharp,
        3,
        3,
    ));
    let exporter = exported(node(
        "class:src/Service.cs:DataExporter:3",
        NodeKind::Class,
        "DataExporter",
        "MyApp::DataExporter",
        "src/Service.cs",
        Language::Csharp,
        3,
        9,
    ));
    let build = node(
        "method:src/Service.cs:DataExporter.Build:5",
        NodeKind::Method,
        "Build",
        "MyApp::DataExporter::Build",
        "src/Service.cs",
        Language::Csharp,
        5,
        5,
    );
    let build_async = node(
        "method:src/Service.cs:DataExporter.BuildAsync:6",
        NodeKind::Method,
        "BuildAsync",
        "MyApp::DataExporter::BuildAsync",
        "src/Service.cs",
        Language::Csharp,
        6,
        6,
    );
    let latest = node(
        "prop:src/Service.cs:DataExporter.Latest:7",
        NodeKind::Property,
        "Latest",
        "MyApp::DataExporter::Latest",
        "src/Service.cs",
        Language::Csharp,
        7,
        7,
    );
    let cached = node(
        "field:src/Service.cs:DataExporter._cached:8",
        NodeKind::Field,
        "_cached",
        "MyApp::DataExporter::_cached",
        "src/Service.cs",
        Language::Csharp,
        8,
        8,
    );
    q.insert_nodes(&[
        session_dto.clone(),
        user_dto.clone(),
        exporter,
        build.clone(),
        build_async.clone(),
        latest.clone(),
        cached.clone(),
    ])
    .unwrap();
    // SessionInfoDto: Build return, Build param, BuildAsync return (inside
    // Task<>), Latest property. UserDto: Build param, BuildAsync param,
    // _cached field.
    q.insert_unresolved_refs_batch(&[
        uref(
            &build.id,
            "SessionInfoDto",
            EdgeKind::References,
            5,
            "src/Service.cs",
            Language::Csharp,
        ),
        uref(
            &build.id,
            "UserDto",
            EdgeKind::References,
            5,
            "src/Service.cs",
            Language::Csharp,
        ),
        uref(
            &build.id,
            "SessionInfoDto",
            EdgeKind::References,
            5,
            "src/Service.cs",
            Language::Csharp,
        ),
        uref(
            &build_async.id,
            "SessionInfoDto",
            EdgeKind::References,
            6,
            "src/Service.cs",
            Language::Csharp,
        ),
        uref(
            &build_async.id,
            "UserDto",
            EdgeKind::References,
            6,
            "src/Service.cs",
            Language::Csharp,
        ),
        uref(
            &latest.id,
            "SessionInfoDto",
            EdgeKind::References,
            7,
            "src/Service.cs",
            Language::Csharp,
        ),
        uref(
            &cached.id,
            "UserDto",
            EdgeKind::References,
            8,
            "src/Service.cs",
            Language::Csharp,
        ),
    ])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .unwrap();

    let session_incoming = incoming(&q, &session_dto.id, EdgeKind::References);
    let user_incoming = incoming(&q, &user_dto.id, EdgeKind::References);
    assert!(
        session_incoming.len() >= 4,
        "got {}",
        session_incoming.len()
    );
    assert!(user_incoming.len() >= 3, "got {}", user_incoming.len());
}
