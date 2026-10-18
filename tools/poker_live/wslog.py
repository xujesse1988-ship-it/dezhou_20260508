"""--ws-log：与服务端往来的原始 WS 消息逐条落 JSONL（openpoker_play / wopo_play 共用）。"""

import json
import threading
import time


class WsLog:
    """与服务端往来的原始 WS 消息逐条落 JSONL（append + 每行 flush）。

    行格式 `{"ts", "conn", "dir", "raw"}`：dir = in（收）/ out（发），raw = 线上原始字符串（收到的
    不重新编码，发出的就是 ws.send 的那串）；dir = event 时无 raw、带 event（open/close/error）
    + 附加字段。conn = 第几次连接（1 起）。看门狗线程也会发消息 → 写入加锁。
    纯旁路：不改任何发往 advisor / ws 的字节。"""

    def __init__(self, path):
        self._f = open(path, "a")
        self._lock = threading.Lock()
        self.conn = 0

    def _write(self, rec):
        line = json.dumps(rec, ensure_ascii=False) + "\n"
        with self._lock:
            self._f.write(line)
            self._f.flush()

    def msg(self, direction, raw):
        self._write({"ts": round(time.time(), 3), "conn": self.conn, "dir": direction, "raw": raw})

    def event(self, kind, **info):
        self._write({"ts": round(time.time(), 3), "conn": self.conn, "dir": "event",
                     "event": kind, **info})

    def close(self):
        with self._lock:
            self._f.close()
