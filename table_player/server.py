#!/usr/bin/env python3
"""Local, read-only OpenPoker table viewer. Requires Python 3.10+ only."""

import argparse
from collections import OrderedDict
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import threading
from urllib.parse import parse_qs, urlsplit

try:
    from .state import LogStore
except ImportError:
    from state import LogStore

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent


class Sources:
    def __init__(self, explicit=None, seats=9, hero_seat=None, root=ROOT):
        self.explicit = Path(explicit).resolve() if explicit else None
        self.seats, self.hero_seat, self.root = seats, hero_seat, root
        self.stores = OrderedDict()
        self.lock = threading.Lock()

    def list(self):
        paths = set(self.root.glob("*ws*.jsonl"))
        paths.update((self.root / "artifacts").glob("**/*ws*.jsonl"))
        if self.explicit:
            paths.add(self.explicit)
        result = []
        for p in paths:
            p = p.resolve()
            try:
                mtime = p.stat().st_mtime
            except FileNotFoundError:
                mtime = 0
            try:
                label = str(p.relative_to(self.root))
            except ValueError:
                label = str(p)
            result.append({"id": hashlib.sha256(str(p).encode()).hexdigest()[:16],
                           "label": label, "mtime": mtime, "demo": False, "path": p})
        result.sort(key=lambda s: s["mtime"], reverse=True)
        default = next((s["id"] for s in result if s["path"] == self.explicit), result[0]["id"] if result else None)
        result.append({"id": "demo", "label": "演示牌局 · 9 人桌", "mtime": 0, "demo": True,
                       "path": HERE / "demo.jsonl"})
        return result, default

    def state(self, source_id, cursor):
        with self.lock:
            sources, default = self.list()
            source_id = source_id or default
            source = next((s for s in sources if s["id"] == source_id), None)
            if source is None:
                raise KeyError("没有找到日志；请用 --ws-log 指定路径，或选择演示牌局")
            if source_id not in self.stores:
                self.stores[source_id] = LogStore(source["path"], self.seats, self.hero_seat)
            self.stores.move_to_end(source_id)
            while len(self.stores) > 4:
                self.stores.popitem(last=False)
            store = self.stores[source_id]
            store.refresh()
            view = store.view(cursor)
            view["meta"].update(source=source_id, label=source["label"], demo=source["demo"])
            return view


def handler_for(sources):
    class Handler(BaseHTTPRequestHandler):
        def log_message(self, fmt, *args):
            pass

        def send(self, status, body, mime="application/json; charset=utf-8"):
            if not isinstance(body, bytes):
                body = json.dumps(body, ensure_ascii=False, allow_nan=False).encode()
            self.send_response(status)
            self.send_header("Content-Type", mime)
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "no-store")
            self.send_header("X-Content-Type-Options", "nosniff")
            self.send_header("Content-Security-Policy", "default-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'")
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self):
            # A loopback binding plus Host checks keep private hole cards local.
            if self.headers.get("Host", "").split(":")[0] not in ("127.0.0.1", "localhost"):
                return self.send(403, {"error": "仅允许本机访问"})
            parsed = urlsplit(self.path)
            query = parse_qs(parsed.query)
            try:
                if parsed.path == "/api/sources":
                    items, default = sources.list()
                    return self.send(200, {"sources": [{k: v for k, v in s.items() if k != "path"} for s in items], "default": default})
                if parsed.path == "/api/state":
                    at = query.get("at", ["live"])[0]
                    cursor = None if at == "live" else int(at)
                    return self.send(200, sources.state(query.get("source", [None])[0], cursor))
                assets = {"/": ("index.html", "text/html"), "/index.html": ("index.html", "text/html"),
                          "/app.js": ("app.js", "text/javascript"), "/styles.css": ("styles.css", "text/css"),
                          "/favicon.svg": ("favicon.svg", "image/svg+xml")}
                if parsed.path in assets:
                    filename, mime = assets[parsed.path]
                    return self.send(200, (HERE / "static" / filename).read_bytes(), mime + "; charset=utf-8")
                self.send(404, {"error": "Not found"})
            except (ValueError, KeyError) as exc:
                self.send(400, {"error": str(exc)})
            except OSError:
                self.send(503, {"error": "无法读取日志文件"})

    return Handler


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ws-log", type=Path, help="OpenPoker --ws-log 文件；默认自动选择最近修改的日志")
    parser.add_argument("--port", type=int, default=8765)
    parser.add_argument("--seats", type=int, choices=range(2, 10), default=9, help="无快照时的座位数；快照自动覆盖")
    parser.add_argument("--hero-seat", type=int, choices=range(9), help="日志缺少 hero 信息时的初始座位")
    args = parser.parse_args()
    sources = Sources(args.ws_log, args.seats, args.hero_seat)
    server = ThreadingHTTPServer(("127.0.0.1", args.port), handler_for(sources))
    print(f"OpenPoker 牌桌播放器: http://127.0.0.1:{server.server_port}", flush=True)
    print("只读本地 WS 日志；Ctrl+C 停止。", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
