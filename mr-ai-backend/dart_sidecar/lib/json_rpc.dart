// Minimal Content-Length-framed JSON-RPC transport over stdio.
//
// Mirrors the framing the Rust LSP client (`code-indexer::lsp::dart::client`)
// already uses, so the same parsing logic on the Rust side handles both the
// Dart Analysis Server and this sidecar.

import 'dart:async';
import 'dart:convert';
import 'dart:io';

class RpcError implements Exception {
  RpcError(this.code, this.message);
  factory RpcError.methodNotFound(String method) =>
      RpcError(-32601, 'method not found: $method');
  factory RpcError.invalidParams(String detail) =>
      RpcError(-32602, 'invalid params: $detail');
  final int code;
  final String message;
  @override
  String toString() => 'RpcError($code): $message';
}

class RpcRequest {
  RpcRequest({required this.id, required this.method, required this.params});
  final Object? id;
  final String method;
  final Map<String, Object?> params;
}

class JsonRpcTransport {
  JsonRpcTransport(this._stdin, this._stdout);

  final Stdin _stdin;
  final Stdout _stdout;

  /// Yields parsed requests as long as stdin stays open.
  Stream<RpcRequest> requests() async* {
    final buffer = <int>[];
    await for (final chunk in _stdin) {
      buffer.addAll(chunk);
      while (true) {
        final headerEnd = _findHeaderEnd(buffer);
        if (headerEnd < 0) break;
        final headerBytes = buffer.sublist(0, headerEnd);
        final header = utf8.decode(headerBytes);
        final length = _parseContentLength(header);
        if (length == null) {
          // Unknown header — drop it and resync to avoid runaway buffers.
          buffer.removeRange(0, headerEnd + 4);
          continue;
        }
        if (buffer.length < headerEnd + 4 + length) break;
        final body = buffer.sublist(headerEnd + 4, headerEnd + 4 + length);
        buffer.removeRange(0, headerEnd + 4 + length);
        try {
          final decoded = jsonDecode(utf8.decode(body)) as Map<String, Object?>;
          final method = decoded['method'] as String?;
          if (method == null) continue;
          final params = (decoded['params'] as Map<String, Object?>?) ?? const {};
          yield RpcRequest(id: decoded['id'], method: method, params: params);
        } catch (_) {
          // Malformed body — keep the stream alive.
          continue;
        }
      }
    }
  }

  void respond(Object? id, Object? result) {
    _send({'jsonrpc': '2.0', 'id': id, 'result': result});
  }

  void respondError(Object? id, int code, String message) {
    _send({
      'jsonrpc': '2.0',
      'id': id,
      'error': {'code': code, 'message': message},
    });
  }

  Future<void> flush() async => _stdout.flush();

  void _send(Object payload) {
    final body = utf8.encode(jsonEncode(payload));
    _stdout.add(utf8.encode('Content-Length: ${body.length}\r\n\r\n'));
    _stdout.add(body);
  }

  static int _findHeaderEnd(List<int> buffer) {
    for (var i = 0; i + 3 < buffer.length; i++) {
      if (buffer[i] == 13 &&
          buffer[i + 1] == 10 &&
          buffer[i + 2] == 13 &&
          buffer[i + 3] == 10) {
        return i;
      }
    }
    return -1;
  }

  static int? _parseContentLength(String header) {
    for (final line in const LineSplitter().convert(header)) {
      if (line.toLowerCase().startsWith('content-length:')) {
        final raw = line.split(':')[1].trim();
        return int.tryParse(raw);
      }
    }
    return null;
  }
}
