"""Serve a plugin over a socket: OpenTrack's external plugin protocol.

JSON lines. Each request is {"id", "method", "params"}; each reply
{"id", "result"} or {"id", "error"}. Every decoder, tracker or scorer
OpenTrack opens is one connection, which starts with `open`
({"kind", "options"}); the instance lives as long as the connection.

    describe                               -> manifest
    open      kind, options                -> null
    decode    frame (base64), received_at_ms -> records
    hints                                  -> hints or null
    push      plot, received_at_ms         -> null
    run       now_ms, force                -> tracks
    score     report, candidates           -> scores
"""

from __future__ import annotations

import base64
import json
import os
import socket
import socketserver
import sys
import traceback

from opentrack_plugin import Plugin, PluginError, log


def _handler(plugin: Plugin):
    class Session(socketserver.StreamRequestHandler):
        def handle(self):
            instance, kind = None, None
            for line in self.rfile:
                if not line.strip():
                    continue
                req_id = None
                try:
                    req = json.loads(line)
                    req_id = req.get("id")
                    method = req.get("method")
                    p = req.get("params") or {}
                    if method == "describe":
                        result = plugin.describe()
                    elif method == "open":
                        kind = p.get("kind")
                        instance = plugin.open(kind, p.get("options") or {})
                        result = None
                    elif instance is None:
                        raise PluginError(f"{method} before open")
                    elif method == "decode" and kind == "codec":
                        result = instance.decode(base64.b64decode(p["frame"]), int(p["received_at_ms"]))
                    elif method == "hints" and kind == "codec":
                        result = instance.hints()
                    elif method == "push" and kind == "tracker":
                        instance.push(p["plot"], int(p["received_at_ms"]))
                        result = None
                    elif method == "run" and kind == "tracker":
                        result = instance.run(int(p["now_ms"]), bool(p.get("force")))
                    elif method == "score" and kind == "scorer":
                        result = instance.score(p["report"], p["candidates"])
                    else:
                        raise PluginError(f"unknown method {method!r} for a {kind}")
                    reply = {"id": req_id, "result": result}
                except PluginError as e:
                    reply = {"id": req_id, "error": str(e)}
                except Exception as e:  # noqa: BLE001 - reported to OpenTrack, and logged here
                    traceback.print_exc(file=sys.stderr)
                    reply = {"id": req_id, "error": f"{type(e).__name__}: {e}"}
                self.wfile.write((json.dumps(reply, default=str) + "\n").encode())
                self.wfile.flush()

    return Session


class _Tcp(socketserver.ThreadingTCPServer):
    daemon_threads = True
    allow_reuse_address = True


class _Unix(socketserver.ThreadingUnixStreamServer):
    daemon_threads = True


def serve(plugin: Plugin, address: str) -> None:
    """Serve until interrupted: one thread per connection."""
    handler = _handler(plugin)
    if address.startswith("unix:"):
        path = address[len("unix:"):]
        if os.path.exists(path):
            os.unlink(path)
        server = _Unix(path, handler)
    else:
        host, _, port = address.removeprefix("tcp://").rpartition(":")
        server = _Tcp((host or "127.0.0.1", int(port)), handler)
        server.socket.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
    m = plugin.describe()
    log("info", f"{m['name']} {m['version']} ({', '.join(m['kinds'])}) serving on {address}")
    with server:
        try:
            server.serve_forever()
        except KeyboardInterrupt:
            pass
