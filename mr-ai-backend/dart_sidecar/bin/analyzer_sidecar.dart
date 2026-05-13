// Dart Analyzer sidecar entry point.
//
// Speaks Content-Length-framed JSON-RPC on stdio. The Rust client lives in
// `code-indexer::lsp::dart::sidecar`. Methods:
//
//   initialize:    { workspace: <abs path>, dart_sdk?: <abs path> }
//                  → { analyzer_version, dart_version,
//                      supports: ["data_flow","control_flow","async_boundary"] }
//
//   extractEdges:  { files: [<repo-relative path>], kinds: [<edge>...] }
//                  → { edges: [...], coverage: { ... } }
//
//   shutdown:      {} → {}
//
// Edge entries match the Rust intent shape so the worker can fold them into
// the existing graph_persist pipeline without translation:
//
//   { from_fqn: <str>, to_fqn: <str>, edge_type: <str>,
//     weight: <float>, meta: { branch?: <str>, line?: <int>, ... } }
//
// This file is the public API surface; the actual analyzer integration lives
// in lib/analyzer_engine.dart so the entry point stays tiny and testable.

import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:mr_ai_dart_analyzer_sidecar/analyzer_engine.dart';
import 'package:mr_ai_dart_analyzer_sidecar/json_rpc.dart';

Future<void> main(List<String> args) async {
  final engine = AnalyzerEngine();
  final transport = JsonRpcTransport(stdin, stdout);

  await for (final request in transport.requests()) {
    try {
      final result = switch (request.method) {
        'initialize' => await engine.initialize(request.params),
        'extractEdges' => await engine.extractEdges(request.params),
        'shutdown' => <String, Object?>{},
        _ => throw RpcError.methodNotFound(request.method),
      };
      transport.respond(request.id, result);
      if (request.method == 'shutdown') {
        await transport.flush();
        exit(0);
      }
    } on RpcError catch (err) {
      transport.respondError(request.id, err.code, err.message);
    } catch (err, stack) {
      transport.respondError(
        request.id,
        -32000,
        'unhandled: $err\n$stack',
      );
    }
  }
}
