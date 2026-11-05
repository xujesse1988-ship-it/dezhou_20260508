"""The /preflop/ forwarding of the replay server, against a stand-in for the Rust viewer."""

import json
import os
import shutil
import sys
import tempfile
import time
import unittest

from opponent_analysis.preflop_proxy import PreflopBackend

# Same stderr / stdin protocol as tools/preflop_viewer.rs: load, bind, report the port, exit on stdin EOF.
FAKE_VIEWER = r"""
import os, sys, threading
from http.server import BaseHTTPRequestHandler, HTTPServer

go_file = sys.argv[1]
class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_GET(self):
        code = 404 if self.path == "/missing" else 200
        body = ("got " + self.path).encode()
        self.send_response(code)
        self.send_header("Content-Type", "text/x-fake")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

def watch_stdin():
    sys.stdin.read()
    os._exit(0)
threading.Thread(target=watch_stdin, daemon=True).start()
print("[open2] 建树", file=sys.stderr, flush=True)
while not os.path.exists(go_file):
    pass
httpd = HTTPServer(("127.0.0.1", 0), H)
print(f"打开 http://127.0.0.1:{httpd.server_address[1]}/", file=sys.stderr, flush=True)
httpd.serve_forever()
"""


def wait_until(predicate, what):
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.01)
    raise AssertionError(f"timed out waiting for {what}")


class Backend(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.dir)
        self.go = os.path.join(self.dir, "go")

    def start_fake(self):
        backend = PreflopBackend([sys.executable, "-c", FAKE_VIEWER, self.go])
        self.addCleanup(backend.close)
        return backend

    def test_loading_then_forwards_path_query_status_and_type(self):
        b = self.start_fake()
        code, body, ctype = b.handle("/preflop/", "")
        self.assertEqual(code, 503)
        self.assertIn("text/html", ctype)
        self.assertIn(b'http-equiv="refresh"', body)
        code, body, ctype = b.handle("/preflop/api/node", "bp=open2")
        self.assertEqual(code, 503)
        self.assertIn("正在加载", json.loads(body)["error"])

        open(self.go, "w").close()
        wait_until(lambda: b.port is not None, "ready line")
        self.assertEqual(b.handle("/preflop/", ""), (200, b"got /", "text/x-fake"))
        self.assertEqual(b.handle("/preflop/api/node", "bp=open2&line=F,F,R333"),
                         (200, b"got /api/node?bp=open2&line=F,F,R333", "text/x-fake"))
        self.assertEqual(b.handle("/preflop/missing", "")[0], 404)

    def test_only_paths_under_the_prefix(self):
        b = self.start_fake()
        for path in ("/", "/history", "/preflop", "/preflopx/", "/api/meta"):
            self.assertIsNone(b.handle(path, ""), path)

    def test_stdin_eof_stops_the_child(self):
        b = self.start_fake()
        b.proc.stdin.close()
        self.assertEqual(b.proc.wait(timeout=10), 0)
        wait_until(lambda: b.error is not None, "exit noticed")

    def test_failed_start_reports_the_reason_and_stays_failed(self):
        script = "import sys; print('错误: <crop> 校验失败', file=sys.stderr); sys.exit(1)"
        b = PreflopBackend([sys.executable, "-c", script])
        self.addCleanup(b.close)
        wait_until(lambda: b.error is not None, "failure noticed")
        code, body, ctype = b.handle("/preflop/", "")
        self.assertEqual(code, 503)
        self.assertIn("text/html", ctype)
        self.assertNotIn(b"refresh", body)  # retrying cannot help, so the page must not loop
        self.assertIn("&lt;crop&gt; 校验失败", body.decode())
        code, body, _ = b.handle("/preflop/api/meta", "")
        self.assertEqual(code, 503)
        self.assertIn("退出码 1", json.loads(body)["error"])

    def test_viewer_gone_after_ready_is_an_error_not_a_hang(self):
        b = self.start_fake()
        open(self.go, "w").close()
        wait_until(lambda: b.port is not None, "ready line")
        b.proc.kill()
        wait_until(lambda: b.error is not None, "exit noticed")
        code, _, _ = b.handle("/preflop/api/meta", "")
        self.assertEqual(code, 503)


if __name__ == "__main__":
    unittest.main()
