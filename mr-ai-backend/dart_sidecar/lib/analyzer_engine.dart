// Real AstVisitor-driven extractors for the three semantic edge kinds
// Tree-sitter cannot supply: data flow, control flow, async boundaries.
//
// Each `_extract*` method walks the resolved AST of a single Dart file and
// emits edge intents whose `from_fqn` / `to_fqn` are stable across runs:
//
//   <file>::<owner_chain>::<symbol>          — top-level / class member fqn
//   <function_fqn>::var:<name>               — variable definition node
//   <function_fqn>::use:<name>@<offset>      — variable use site
//   <function_fqn>::branch:<kind>@<offset>   — branch / loop / try entry point
//   await:<callee>                           — async-boundary marker node
//
// The Rust side (`augment_with_sidecar`) folds these straight into the
// existing graph_persist pipeline without translation.

import 'dart:async';
import 'dart:io';

import 'package:analyzer/dart/analysis/analysis_context_collection.dart';
import 'package:analyzer/dart/analysis/results.dart';
import 'package:analyzer/dart/ast/ast.dart';
import 'package:analyzer/dart/ast/visitor.dart';

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
        final result = dataFlowEdgesForUnit(unit.unit, relative);
        edges.addAll(result);
        coverage['data_flow'] = (coverage['data_flow'] ?? 0) + result.length;
      }
      if (kinds.contains('control_flow')) {
        final result = controlFlowEdgesForUnit(unit.unit, relative);
        edges.addAll(result);
        coverage['control_flow'] =
            (coverage['control_flow'] ?? 0) + result.length;
      }
      if (kinds.contains('async_boundary')) {
        final result = asyncBoundaryEdgesForUnit(unit.unit, relative);
        edges.addAll(result);
        coverage['async_boundary'] =
            (coverage['async_boundary'] ?? 0) + result.length;
      }
    }

    return {'edges': edges, 'coverage': coverage};
  }

  // --------------------------------------------------------------------
  //  Public for tests + direct callers (e.g. `dart test`).
  // --------------------------------------------------------------------

  /// Walk every function/method in the unit, emit `var → use` edges for
  /// each local declaration. Returned shape matches the JSON-RPC contract.
  static List<Map<String, Object?>> dataFlowEdgesForUnit(
    CompilationUnit unit,
    String filePath,
  ) {
    final visitor = _DataFlowVisitor(filePath);
    unit.accept(visitor);
    return visitor.edges;
  }

  /// Emit one `function → branch:kind@offset` edge per branching /
  /// looping / try statement found inside each function body.
  static List<Map<String, Object?>> controlFlowEdgesForUnit(
    CompilationUnit unit,
    String filePath,
  ) {
    final visitor = _ControlFlowVisitor(filePath);
    unit.accept(visitor);
    return visitor.edges;
  }

  /// Emit one `function → await:callee` edge per `await` expression.
  static List<Map<String, Object?>> asyncBoundaryEdgesForUnit(
    CompilationUnit unit,
    String filePath,
  ) {
    final visitor = _AsyncBoundaryVisitor(filePath);
    unit.accept(visitor);
    return visitor.edges;
  }

  String _absolute(String workspace, String relative) {
    if (relative.startsWith('/')) return relative;
    final separator = workspace.endsWith('/') ? '' : '/';
    return '$workspace$separator$relative';
  }
}

// =====================================================================
//  Visitor implementations
// =====================================================================

abstract class _ScopedVisitor extends RecursiveAstVisitor<void> {
  _ScopedVisitor(this.filePath);

  final String filePath;
  final List<String> _ownerStack = <String>[];

  String get currentFqn {
    final base = filePath;
    if (_ownerStack.isEmpty) return base;
    return '$base::${_ownerStack.join('::')}';
  }

  void _enter(String name) => _ownerStack.add(name);
  void _exit() => _ownerStack.removeLast();

  @override
  void visitClassDeclaration(ClassDeclaration node) {
    _enter(node.name.lexeme);
    super.visitClassDeclaration(node);
    _exit();
  }

  @override
  void visitMixinDeclaration(MixinDeclaration node) {
    _enter(node.name.lexeme);
    super.visitMixinDeclaration(node);
    _exit();
  }

  @override
  void visitExtensionDeclaration(ExtensionDeclaration node) {
    final name = node.name?.lexeme ?? 'extension';
    _enter(name);
    super.visitExtensionDeclaration(node);
    _exit();
  }
}

class _DataFlowVisitor extends _ScopedVisitor {
  _DataFlowVisitor(super.filePath);

  final List<Map<String, Object?>> edges = [];

  void _emitVarUseEdges(FunctionBody body, String functionFqn) {
    final defs = <String, int>{};
    // Parameters declared by the surrounding function are first-class
    // data-flow sources alongside locals.
    body.visitChildren(_VarCollector(defs));
    final uses = _IdentifierUseCollector(defs);
    body.visitChildren(uses);
    for (final use in uses.uses) {
      edges.add({
        'from_fqn': '$functionFqn::var:${use.name}',
        'to_fqn': '$functionFqn::use:${use.name}@${use.offset}',
        'edge_type': 'data_flow',
        'weight': 1.0,
        'meta': {'name': use.name, 'offset': use.offset},
      });
    }

    // Cross-procedure: for every MethodInvocation in the body, emit a
    // syntactic data-flow edge from each local-variable argument to the
    // callee's positional parameter slot. No element resolution required —
    // matches by lexical name. The to_fqn is intentionally callee-name-
    // scoped (no FQN guesswork) so edges remain stable when the callee
    // lives in another file.
    body.visitChildren(_CallSiteVisitor(defs, functionFqn, edges));
  }

  @override
  void visitFunctionDeclaration(FunctionDeclaration node) {
    _enter(node.name.lexeme);
    _emitVarUseEdges(node.functionExpression.body, currentFqn);
    super.visitFunctionDeclaration(node);
    _exit();
  }

  @override
  void visitMethodDeclaration(MethodDeclaration node) {
    _enter(node.name.lexeme);
    _emitVarUseEdges(node.body, currentFqn);
    super.visitMethodDeclaration(node);
    _exit();
  }
}

class _VarCollector extends RecursiveAstVisitor<void> {
  _VarCollector(this.defs);
  final Map<String, int> defs;

  @override
  void visitVariableDeclaration(VariableDeclaration node) {
    defs.putIfAbsent(node.name.lexeme, () => node.offset);
    super.visitVariableDeclaration(node);
  }

  @override
  void visitSimpleFormalParameter(SimpleFormalParameter node) {
    final name = node.name?.lexeme;
    if (name != null) {
      defs.putIfAbsent(name, () => node.offset);
    }
    super.visitSimpleFormalParameter(node);
  }

  @override
  void visitDefaultFormalParameter(DefaultFormalParameter node) {
    final name = node.name?.lexeme;
    if (name != null) {
      defs.putIfAbsent(name, () => node.offset);
    }
    super.visitDefaultFormalParameter(node);
  }
}

class _CallSiteVisitor extends RecursiveAstVisitor<void> {
  _CallSiteVisitor(this.defs, this.callerFqn, this.edges);
  final Map<String, int> defs;
  final String callerFqn;
  final List<Map<String, Object?>> edges;

  @override
  void visitMethodInvocation(MethodInvocation node) {
    final callee = node.methodName.name;
    final args = node.argumentList.arguments;
    for (var index = 0; index < args.length; index++) {
      final arg = args[index];
      String? localName;
      int slot = index;
      if (arg is SimpleIdentifier) {
        localName = arg.name;
      } else if (arg is NamedExpression) {
        final inner = arg.expression;
        if (inner is SimpleIdentifier) {
          localName = inner.name;
          // Named arg keeps its label as the slot identifier.
          slot = -1;
        }
        if (localName != null && defs.containsKey(localName)) {
          edges.add({
            'from_fqn': '$callerFqn::var:$localName',
            'to_fqn': '$callee::param:${arg.name.label.name}',
            'edge_type': 'data_flow',
            'weight': 0.6,
            'meta': {
              'callee': callee,
              'arg': localName,
              'named': arg.name.label.name,
              'offset': arg.offset,
            },
          });
        }
        super.visitMethodInvocation(node);
        return;
      }
      if (localName != null && defs.containsKey(localName)) {
        edges.add({
          'from_fqn': '$callerFqn::var:$localName',
          'to_fqn': '$callee::param@$slot',
          'edge_type': 'data_flow',
          'weight': 0.6,
          'meta': {
            'callee': callee,
            'arg': localName,
            'positional': slot,
            'offset': arg.offset,
          },
        });
      }
    }
    super.visitMethodInvocation(node);
  }
}

class _IdentifierUse {
  _IdentifierUse(this.name, this.offset);
  final String name;
  final int offset;
}

class _IdentifierUseCollector extends RecursiveAstVisitor<void> {
  _IdentifierUseCollector(this.defs);
  final Map<String, int> defs;
  final List<_IdentifierUse> uses = [];

  @override
  void visitSimpleIdentifier(SimpleIdentifier node) {
    final name = node.name;
    if (!defs.containsKey(name)) return;
    final defOffset = defs[name];
    if (defOffset != null && node.offset == defOffset) {
      // Skip the declaring identifier itself.
      return;
    }
    uses.add(_IdentifierUse(name, node.offset));
  }
}

class _ControlFlowVisitor extends _ScopedVisitor {
  _ControlFlowVisitor(super.filePath);

  final List<Map<String, Object?>> edges = [];

  void _emitBranch(String kind, int offset) {
    final fqn = currentFqn;
    edges.add({
      'from_fqn': fqn,
      'to_fqn': '$fqn::branch:$kind@$offset',
      'edge_type': 'control_flow',
      'weight': 1.0,
      'meta': {'kind': kind, 'offset': offset},
    });
  }

  @override
  void visitFunctionDeclaration(FunctionDeclaration node) {
    _enter(node.name.lexeme);
    super.visitFunctionDeclaration(node);
    _exit();
  }

  @override
  void visitMethodDeclaration(MethodDeclaration node) {
    _enter(node.name.lexeme);
    super.visitMethodDeclaration(node);
    _exit();
  }

  @override
  void visitIfStatement(IfStatement node) {
    _emitBranch('if', node.offset);
    super.visitIfStatement(node);
  }

  @override
  void visitForStatement(ForStatement node) {
    _emitBranch('for', node.offset);
    super.visitForStatement(node);
  }

  @override
  void visitWhileStatement(WhileStatement node) {
    _emitBranch('while', node.offset);
    super.visitWhileStatement(node);
  }

  @override
  void visitDoStatement(DoStatement node) {
    _emitBranch('do_while', node.offset);
    super.visitDoStatement(node);
  }

  @override
  void visitSwitchStatement(SwitchStatement node) {
    _emitBranch('switch', node.offset);
    super.visitSwitchStatement(node);
  }

  @override
  void visitTryStatement(TryStatement node) {
    _emitBranch('try', node.offset);
    super.visitTryStatement(node);
  }
}

class _AsyncBoundaryVisitor extends _ScopedVisitor {
  _AsyncBoundaryVisitor(super.filePath);

  final List<Map<String, Object?>> edges = [];

  @override
  void visitFunctionDeclaration(FunctionDeclaration node) {
    _enter(node.name.lexeme);
    super.visitFunctionDeclaration(node);
    _exit();
  }

  @override
  void visitMethodDeclaration(MethodDeclaration node) {
    _enter(node.name.lexeme);
    super.visitMethodDeclaration(node);
    _exit();
  }

  @override
  void visitAwaitExpression(AwaitExpression node) {
    final callee = _calleeText(node.expression);
    edges.add({
      'from_fqn': currentFqn,
      'to_fqn': 'await:$callee',
      'edge_type': 'async_boundary',
      'weight': 1.0,
      'meta': {'callee': callee, 'offset': node.offset},
    });
    super.visitAwaitExpression(node);
  }

  String _calleeText(Expression expr) {
    if (expr is MethodInvocation) {
      return expr.methodName.name;
    }
    if (expr is SimpleIdentifier) {
      return expr.name;
    }
    if (expr is PropertyAccess) {
      return expr.propertyName.name;
    }
    return expr.toString();
  }
}
