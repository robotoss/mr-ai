// Unit tests for the AstVisitor passes.
//
// We bypass the full AnalysisContextCollection by using
// `analyzer/dart/analysis/utilities.dart`'s parseString — every visitor
// works on the syntactic tree alone, no element resolution required.

import 'package:analyzer/dart/analysis/utilities.dart';
import 'package:mr_ai_dart_analyzer_sidecar/analyzer_engine.dart';
import 'package:test/test.dart';

void main() {
  group('dataFlowEdgesForUnit', () {
    test('emits an edge for every use of a local variable', () {
      const source = '''
int compute(int n) {
  var total = 0;
  for (var i = 0; i < n; i++) {
    total += i;
  }
  return total;
}
''';
      final unit = parseString(content: source).unit;
      final edges = AnalyzerEngine.dataFlowEdgesForUnit(unit, 'lib/x.dart');

      // `total` declared once, used twice (`+=` line, `return`); `i` declared
      // once, used twice (`<` and `+=`). `n` is a parameter, not a local.
      final names = edges
          .map((e) => (e['meta'] as Map<String, Object?>)['name'])
          .toList();
      expect(names.where((n) => n == 'total').length, 2);
      expect(names.where((n) => n == 'i').length, 2);
      expect(edges.every((e) => e['edge_type'] == 'data_flow'), isTrue);
      expect(
        edges.first['from_fqn'],
        startsWith('lib/x.dart::compute::var:'),
      );
    });

    test('returns an empty list for a body without locals', () {
      const source = 'void noop() {}';
      final unit = parseString(content: source).unit;
      final edges = AnalyzerEngine.dataFlowEdgesForUnit(unit, 'lib/x.dart');
      expect(edges, isEmpty);
    });
  });

  group('dataFlowEdgesForUnit (cross-procedure)', () {
    test('emits param edges for positional argument passed by name', () {
      const source = '''
int outer() {
  var seed = 42;
  return inner(seed);
}
''';
      final unit = parseString(content: source).unit;
      final edges = AnalyzerEngine.dataFlowEdgesForUnit(unit, 'lib/x.dart');
      final crossEdges = edges
          .where((e) => (e['to_fqn'] as String).startsWith('inner::param'))
          .toList();
      expect(crossEdges, hasLength(1));
      final meta = crossEdges.first['meta'] as Map<String, Object?>;
      expect(meta['callee'], 'inner');
      expect(meta['arg'], 'seed');
      expect(meta['positional'], 0);
    });

    test('uses named-parameter label when arg is named', () {
      const source = '''
int outer() {
  var threshold = 7;
  return classify(score: threshold);
}
''';
      final unit = parseString(content: source).unit;
      final edges = AnalyzerEngine.dataFlowEdgesForUnit(unit, 'lib/x.dart');
      final crossEdges = edges
          .where((e) => (e['to_fqn'] as String).startsWith('classify::param:'))
          .toList();
      expect(crossEdges, hasLength(1));
      expect(crossEdges.first['to_fqn'], 'classify::param:score');
    });

    test('skips literal arguments and unknown identifiers', () {
      const source = '''
int outer() {
  var seed = 42;
  return both(seed, 99, undeclared);
}
''';
      final unit = parseString(content: source).unit;
      final edges = AnalyzerEngine.dataFlowEdgesForUnit(unit, 'lib/x.dart');
      final crossEdges = edges
          .where((e) => (e['to_fqn'] as String).startsWith('both::param'))
          .toList();
      expect(crossEdges, hasLength(1));
      final meta = crossEdges.first['meta'] as Map<String, Object?>;
      expect(meta['arg'], 'seed');
      expect(meta['positional'], 0);
    });

    test('parameters of the caller flow into nested calls', () {
      const source = '''
int outer(int n) {
  return inner(n);
}
''';
      final unit = parseString(content: source).unit;
      final edges = AnalyzerEngine.dataFlowEdgesForUnit(unit, 'lib/x.dart');
      expect(
        edges.any((e) =>
            e['to_fqn'] == 'inner::param@0' &&
            (e['meta'] as Map<String, Object?>)['arg'] == 'n'),
        isTrue,
        reason: 'parameter `n` should be a data-flow source for `inner(n)`',
      );
    });
  });

  group('controlFlowEdgesForUnit', () {
    test('emits one branch edge per if/for/while/switch/try inside a function',
        () {
      const source = '''
int classify(int n) {
  if (n < 0) return -1;
  for (var i = 0; i < n; i++) {}
  while (n > 100) { n--; }
  do { n--; } while (n > 50);
  switch (n) { case 0: return 0; }
  try { return n; } catch (_) { return -1; }
}
''';
      final unit = parseString(content: source).unit;
      final edges = AnalyzerEngine.controlFlowEdgesForUnit(unit, 'lib/x.dart');
      final kinds = edges
          .map((e) => (e['meta'] as Map<String, Object?>)['kind'])
          .toSet();
      expect(kinds, containsAll([
        'if',
        'for',
        'while',
        'do_while',
        'switch',
        'try',
      ]));
      expect(edges.every((e) => e['edge_type'] == 'control_flow'), isTrue);
      expect(
        edges.first['from_fqn'],
        equals('lib/x.dart::classify'),
      );
    });

    test('linear function with no branches emits nothing', () {
      const source = 'int double(int n) => n + n;';
      final unit = parseString(content: source).unit;
      final edges = AnalyzerEngine.controlFlowEdgesForUnit(unit, 'lib/x.dart');
      expect(edges, isEmpty);
    });
  });

  group('asyncBoundaryEdgesForUnit', () {
    test('emits one edge per await expression with the callee captured', () {
      const source = '''
Future<int> fetch() async {
  final raw = await client.get('/');
  final parsed = await decoder.decode(raw);
  return parsed.length;
}
''';
      final unit = parseString(content: source).unit;
      final edges =
          AnalyzerEngine.asyncBoundaryEdgesForUnit(unit, 'lib/x.dart');
      expect(edges.length, 2);
      final callees = edges
          .map((e) => (e['meta'] as Map<String, Object?>)['callee'])
          .toList();
      expect(callees, containsAll(['get', 'decode']));
      expect(edges.every((e) => e['edge_type'] == 'async_boundary'), isTrue);
      expect(edges.first['from_fqn'], equals('lib/x.dart::fetch'));
    });

    test('synchronous function emits nothing', () {
      const source = 'int sync() => 42;';
      final unit = parseString(content: source).unit;
      final edges =
          AnalyzerEngine.asyncBoundaryEdgesForUnit(unit, 'lib/x.dart');
      expect(edges, isEmpty);
    });
  });

  test('class methods inherit owner chain into fqn', () {
    const source = '''
class Foo {
  void bar() {
    if (true) {}
  }
}
''';
    final unit = parseString(content: source).unit;
    final edges = AnalyzerEngine.controlFlowEdgesForUnit(unit, 'lib/x.dart');
    expect(edges, hasLength(1));
    expect(edges.first['from_fqn'], equals('lib/x.dart::Foo::bar'));
  });
}
