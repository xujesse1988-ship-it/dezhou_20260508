"""Serve the Rust preflop strategy viewer (tools/preflop_viewer.rs) under /preflop/ of the replay server.

One address and one start command for both pages; the viewer stays a separate process because it
reuses the Rust tree building and crop checks. It binds a free loopback port it reports on stderr,
and exits when its stdin closes, so it goes away with this process even after a SIGKILL.

Loading takes about 30 s and briefly about 1.6 GiB, hence opt-in (`history --preflop`) and never on
the `live` server. The replay pages keep working while it loads and if it fails.
"""

from __future__ import annotations

import html
import http.client
import json
import os
import re
import subprocess
import threading
from typing import Optional

PREFIX = "/preflop"
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BINARY = os.path.join(ROOT, "target", "release", "preflop_viewer")
ARTIFACTS = os.path.join(ROOT, "artifacts")
READY = re.compile(r"打开 http://[^\s/]+:(\d+)/")
FORWARD_TIMEOUT_S = 30
HTML_TYPE = "text/html; charset=utf-8"
JSON_TYPE = "application/json; charset=utf-8"


def viewer_command() -> list[str]:
    if not os.path.exists(BINARY):
        raise SystemExit(f"没有 {BINARY}，先运行 cargo build --release --bin preflop_viewer")
    return [BINARY, "--port", "0", "--artifacts", ARTIFACTS, "--exit-on-stdin-eof"]


class PreflopBackend:
    def __init__(self, command: list[str]):
        self.port: Optional[int] = None
        self.error: Optional[str] = None
        self.proc = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.DEVNULL,
                                     stderr=subprocess.PIPE, text=True, encoding="utf-8", errors="replace")
        threading.Thread(target=self._watch, daemon=True).start()

    def _watch(self) -> None:
        last = ""
        for line in self.proc.stderr:
            line = line.rstrip()
            print(f"[preflop] {line}", flush=True)
            ready = READY.search(line)
            if ready:
                self.port = int(ready.group(1))
            else:
                last = line
        code = self.proc.wait()
        self.error = f"翻前查看器已退出（退出码 {code}）：{last}"
        self.port = None

    def close(self) -> None:
        self.proc.stdin.close()
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()

    def _unavailable(self, path: str, status: int, message: str, retry: bool):
        if path.startswith(PREFIX + "/api/"):
            return status, json.dumps({"error": message}, ensure_ascii=False).encode(), JSON_TYPE
        refresh = '<meta http-equiv="refresh" content="3">' if retry else ""
        page = (f'<!doctype html><meta charset="utf-8"><title>翻前策略</title>{refresh}'
                f'<meta name="color-scheme" content="light dark">'
                f'<body style="font:15px/1.6 system-ui,sans-serif;margin:40px">'
                f'<p>{html.escape(message)}</p><p><a href="/">← 牌局回放</a></p>')
        return status, page.encode(), HTML_TYPE

    def handle(self, path: str, query: str):
        """(status, body, content type) for a path under /preflop/, else None."""
        if not path.startswith(PREFIX + "/"):
            return None
        if self.error:
            return self._unavailable(path, 503, self.error, retry=False)
        port = self.port
        if port is None:
            return self._unavailable(path, 503, "翻前策略正在加载，大约 30 秒，好了会自动打开。", retry=True)
        target = path[len(PREFIX):] + (f"?{query}" if query else "")
        conn = http.client.HTTPConnection("127.0.0.1", port, timeout=FORWARD_TIMEOUT_S)
        try:
            conn.request("GET", target)
            res = conn.getresponse()
            return res.status, res.read(), res.getheader("Content-Type", "application/octet-stream")
        except (OSError, http.client.HTTPException) as e:
            return self._unavailable(path, 502, f"连不上翻前查看器：{e}", retry=False)
        finally:
            conn.close()
