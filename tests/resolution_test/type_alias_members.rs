use crate::fixture::*;

#[test]
fn type_alias_object_shape_members_resolve_method_calls_359() {
    // `recorder.stop()` (recorder: RecorderHandle) must attach to
    // `RecorderHandle::stop`, not the look-alike class method in a sibling
    // directory.
    let fx = Fx::new();
    let q = fx.q();
    fx.write(
        "voice/recorder.ts",
        "export type RecorderHandle = {\n  wavPath: string;\n  stop: () => Promise<{ ok: true }>;\n};\n",
    );
    fx.write(
        "voice/controller.ts",
        "import type { RecorderHandle } from \"./recorder\";\nexport async function finaliseRecording(recorder: RecorderHandle) {\n  return await recorder.stop();\n}\n",
    );
    fx.write(
        "codegraph/stdio-client.ts",
        "export class StdioMcpClient {\n  private stopped = false;\n  async stop(): Promise<void> { this.stopped = true; }\n}\n",
    );
    fx.track(&q, "voice/recorder.ts", Language::Typescript);
    fx.track(&q, "voice/controller.ts", Language::Typescript);
    fx.track(&q, "codegraph/stdio-client.ts", Language::Typescript);

    // type_alias produces member nodes (property/method) — #359. The TS test
    // asserts qualifiedName === 'RecorderHandle::stop' exactly.
    let alias = exported(node(
        "type:voice/recorder.ts:RecorderHandle:1",
        NodeKind::TypeAlias,
        "RecorderHandle",
        "voice/recorder.ts::RecorderHandle",
        "voice/recorder.ts",
        Language::Typescript,
        1,
        4,
    ));
    let wav_path = node(
        "prop:voice/recorder.ts:RecorderHandle.wavPath:2",
        NodeKind::Property,
        "wavPath",
        "RecorderHandle::wavPath",
        "voice/recorder.ts",
        Language::Typescript,
        2,
        2,
    );
    // Function-typed property surfaces as a `method` node, not `property`.
    let handle_stop = node(
        "method:voice/recorder.ts:RecorderHandle.stop:3",
        NodeKind::Method,
        "stop",
        "RecorderHandle::stop",
        "voice/recorder.ts",
        Language::Typescript,
        3,
        3,
    );
    let finalise = exported(node(
        "func:voice/controller.ts:finaliseRecording:2",
        NodeKind::Function,
        "finaliseRecording",
        "voice/controller.ts::finaliseRecording",
        "voice/controller.ts",
        Language::Typescript,
        2,
        4,
    ));
    let client = exported(node(
        "class:codegraph/stdio-client.ts:StdioMcpClient:1",
        NodeKind::Class,
        "StdioMcpClient",
        "codegraph/stdio-client.ts::StdioMcpClient",
        "codegraph/stdio-client.ts",
        Language::Typescript,
        1,
        4,
    ));
    let client_stop = node(
        "method:codegraph/stdio-client.ts:StdioMcpClient.stop:3",
        NodeKind::Method,
        "stop",
        "StdioMcpClient::stop",
        "codegraph/stdio-client.ts",
        Language::Typescript,
        3,
        3,
    );
    q.insert_nodes(&[
        alias,
        wav_path,
        handle_stop.clone(),
        finalise.clone(),
        client,
        client_stop.clone(),
    ])
    .unwrap();
    q.insert_unresolved_refs_batch(&[uref(
        &finalise.id,
        "recorder.stop",
        EdgeKind::Calls,
        3,
        "voice/controller.ts",
        Language::Typescript,
    )])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .unwrap();

    assert_eq!(handle_stop.kind, NodeKind::Method);
    let handle_callers = incoming(&q, &handle_stop.id, EdgeKind::Calls);
    let client_callers = incoming(&q, &client_stop.id, EdgeKind::Calls);
    assert!(
        !handle_callers.is_empty(),
        "RecorderHandle::stop should have a caller"
    );
    // The class method must have NO callers — voice/'s call must NOT
    // mis-attribute.
    assert!(client_callers.is_empty());
}
