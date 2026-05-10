// Skeleton analyzer engine.
//
// Today: returns empty edge sets so the Rust side can exercise the wiring
// end-to-end against a real subprocess. Subsequent commits replace
// `_extractDataFlow` / `_extractControlFlow` / `_extractAsyncBoundary` with
// real `package:analyzer` AstVisitor + ElementResolver passes that walk
// every supplied file and emit the canonical edge intents documented in
// `bin/analyzer_sidecar.dart`.
//
// The visitor work is intentionally non-trivial and lives behind the same
// JSON-RPC contract, so this skeleton can ship + be exercised before the
// full extractor is implemented.

import 'dart:async';
import 'dart:io';

import 'package:analyzer/dart/analysis/analysis_context_collection.dart';
import 'package:analyzer/dart/analysis/results.dart';

import 'json_rpc.dart';

class AnalyzerEngine {
  AnalysisContextCollection? _collection;
  String? _workspace;

  Future<Map<String, Object?>> initialize(Map<String, Object?> params) async {
    final workspace = params['workspace'] as String?;
    if (workspace == null) {
      throw RpcError.invalidParams('workspace is required');
    }
    _workspace = workspace;
    _collection = AnalysisContextCollection(includedPaths: [workspace]);
    return {
      'analyzer_version': '7',
      'dart_version': Platform.version,
      'supports': const ['data_flow', 'control_flow', 'async_boundary'],
    };
  }

  Future<Map<String, Object?>> extractEdges(Map<String, Object?> params) async {
    final collection = _collection;
    final workspace = _workspace;
    if (collection == null || workspace == null) {
      throw RpcError.invalidParams('initialize must run before extractEdges');
    }

    final files = (params['files'] as List?)?.cast<String>() ?? const [];
    final kinds =
        ((params['kinds'] as List?)?.cast<String>() ?? const []).toSet();

    final edges = <Map<String, Object?>>[];
    final coverage = <String, int>{};

    for (final relative in files) {
      final absolute = _absolute(workspace, relative);
      final session = collection.contextFor(absolute).currentSession;
      final unit = await session.getResolvedUnit(absolute);
      if (unit is! ResolvedUnitResult) continue;

      if (kinds.contains('data_flow')) {
        final result = _extractDataFlow(unit, relative);
        edges.addAll(result);
        coverage['data_flow'] = (coverage['data_flow'] ?? 0) + result.length;
      }
      if (kinds.contains('control_flow')) {
        final result = _extractControlFlow(unit, relative);
        edges.addAll(result);
        coverage['control_flow'] =
            (coverage['control_flow'] ?? 0) + result.length;
      }
      if (kinds.contains('async_boundary')) {
        final result = _extractAsyncBoundary(unit, relative);
        edges.addAll(result);
        coverage['async_boundary'] =
            (coverage['async_boundary'] ?? 0) + result.length;
      }
    }

    return {'edges': edges, 'coverage': coverage};
  }

  // --------------------------------------------------------------------
  //  TODO(S8-B): replace the three stubs below with real analyzer passes.
  // --------------------------------------------------------------------

  List<Map<String, Object?>> _extractDataFlow(
    ResolvedUnitResult unit,
    String relative,
  ) =>
      const [];

  List<Map<String, Object?>> _extractControlFlow(
    ResolvedUnitResult unit,
    String relative,
  ) =>
      const [];

  List<Map<String, Object?>> _extractAsyncBoundary(
    ResolvedUnitResult unit,
    String relative,
  ) =>
      const [];

  String _absolute(String workspace, String relative) {
    if (relative.startsWith('/')) return relative;
    final separator = workspace.endsWith('/') ? '' : '/';
    return '$workspace$separator$relative';
  }
}
