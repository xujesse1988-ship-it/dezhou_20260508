"""Per-platform hand store: <db>/<platform>/{hands,rejected,ingest_log}.jsonl.

A platform directory only ever receives hands of that platform; ingest refuses a file
whose detected platform differs from the requested one. One process at a time may write a
platform directory (a Store holds an exclusive lock on it); readers need no lock.
"""

from __future__ import annotations

import fcntl
import glob
import hashlib
import json
import os
import time
from collections import Counter
from typing import Optional

from .adapters import ADAPTERS, iter_records, openpoker_ws_actions
from .model import Hand
from .platforms import PLATFORMS, detect_platform
from .replay import ReplayError, replay
from .spots import hand_observations


def platform_dir(db: str, platform: str) -> str:
    if platform not in PLATFORMS:
        raise ValueError(f"unknown platform {platform}")
    d = os.path.join(db, platform)
    os.makedirs(d, exist_ok=True)
    return d


def _ids(path: str) -> set[str]:
    if not os.path.exists(path):
        return set()
    with open(path) as f:
        return {json.loads(line)["hand_id"] for line in f if line.strip()}


def _sha256(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def find_run_files(run_dir: str) -> tuple[str, Optional[str]]:
    hh = sorted(glob.glob(os.path.join(run_dir, "*_hh.jsonl")))
    if len(hh) != 1:
        raise ValueError(f"{run_dir}: expected one *_hh.jsonl, found {len(hh)}")
    ws = sorted(glob.glob(os.path.join(run_dir, "*_ws.jsonl")))
    return hh[0], (ws[0] if len(ws) == 1 else None)


class DatabaseBusy(RuntimeError):
    pass


class Store:
    """Append-only hand store of one platform, with the known hand ids kept in memory.

    Used by file ingest and by the live poller alike, so both go through the same
    adapter + replay validation and the same dedupe. The known ids are read once, so a
    second writer would append duplicates: the store holds an exclusive lock on the
    platform directory until close() (or process exit).
    """

    def __init__(self, db: str, platform: str):
        self.platform = platform
        self.plat = PLATFORMS[platform]
        self.dir = platform_dir(db, platform)
        self._lock = open(os.path.join(self.dir, "write.lock"), "a+")
        try:
            fcntl.flock(self._lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            self._lock.seek(0)
            holder = self._lock.read().strip() or "?"
            self._lock.close()
            raise DatabaseBusy(f"{self.dir} is being written by another process (pid {holder}); "
                               "stop that ingest / live first") from None
        self._lock.seek(0)
        self._lock.truncate()
        self._lock.write(str(os.getpid()))
        self._lock.flush()
        self.hands_path = os.path.join(self.dir, "hands.jsonl")
        self.rej_path = os.path.join(self.dir, "rejected.jsonl")
        self.have = _ids(self.hands_path)
        self.rejected = _ids(self.rej_path)

    def close(self) -> None:
        if not self._lock.closed:
            self._lock.close()  # releases the flock

    def __enter__(self) -> "Store":
        return self

    def __exit__(self, *exc) -> None:
        self.close()

    def known(self, hid: str) -> bool:
        return hid in self.have or hid in self.rejected

    def add(self, rec: dict, source: str, ws: Optional[dict] = None, line_no: Optional[int] = None) -> tuple[str, Optional[Hand]]:
        """Validate and store one record. Returns (count key, Hand if newly accepted)."""
        hid = rec.get("hand_id") if isinstance(rec, dict) else None
        if hid in self.have:
            return "duplicate", None
        if hid in self.rejected:
            return "duplicate_rejected", None
        try:
            raw = ADAPTERS[self.platform](rec, source, ws)
            hand = replay(raw, self.plat.heads_up_button_is_sb)
        except ReplayError as e:
            self._reject(hid, source, line_no, e.code, e.detail)
            return "rejected:" + e.code, None
        except Exception as e:  # a record of any unexpected shape is rejected, never a crash
            self._reject(hid, source, line_no, "malformed", repr(e))
            return "rejected:malformed", None
        # A stored hand is re-read by every report and live start, so one that the analysis
        # cannot process would block all of them: run it once here and reject on failure.
        # (No percentile function: replay already checked the cards the evaluator reads.)
        try:
            hand_observations(hand)
        except Exception as e:
            self._reject(hid, source, line_no, "spots_error", repr(e))
            return "rejected:spots_error", None
        with open(self.hands_path, "a") as f:
            f.write(json.dumps(hand.to_json(), separators=(",", ":")) + "\n")
        self.have.add(hid)
        return "accepted", hand

    def _reject(self, hid, source, line_no, code, detail) -> None:
        with open(self.rej_path, "a") as f:
            f.write(json.dumps({"hand_id": hid, "file": source, "line": line_no, "code": code, "detail": detail}) + "\n")
        self.rejected.add(hid)

    def log(self, entry: dict) -> None:
        with open(os.path.join(self.dir, "ingest_log.jsonl"), "a") as f:
            f.write(json.dumps(dict(entry, time=time.time(), platform=self.platform)) + "\n")


def ingest(db: str, hh_path: str, ws_path: Optional[str] = None, platform: Optional[str] = None) -> dict:
    first = next(iter_records(hh_path), None)
    if first is None:
        raise ValueError(f"{hh_path}: empty")
    detected = detect_platform(first[1])
    if platform is not None and platform != detected:
        raise ValueError(f"{hh_path}: file is {detected}, not {platform}; platforms are never mixed")
    ws = openpoker_ws_actions(ws_path) if (ws_path and detected == "openpoker") else None

    counts: Counter = Counter()
    with Store(db, detected) as store:
        for line_no, rec in iter_records(hh_path):
            if detect_platform(rec) != detected:
                raise ValueError(f"{hh_path}:{line_no}: mixed platforms in one file")
            key, hand = store.add(rec, hh_path, ws, line_no)
            counts[key] += 1
            if hand is not None and "forced_unknown" in hand.notes:
                counts["forced_unknown"] += 1
        entry = {"hh": os.path.abspath(hh_path), "hh_sha256": _sha256(hh_path),
                 "ws": os.path.abspath(ws_path) if ws_path else None, "counts": dict(counts)}
        store.log(entry)
    return dict(entry, platform=detected)


def load_hands(db: str, platform: str) -> list[Hand]:
    path = os.path.join(platform_dir(db, platform), "hands.jsonl")
    if not os.path.exists(path):
        return []
    hands: list[Hand] = []
    seen: set[str] = set()
    with open(path) as f:
        for line in f:
            if not line.strip():
                continue
            h = Hand.from_json(json.loads(line))
            if h.hand_id in seen:  # left by two concurrent writers before the store lock existed
                continue
            seen.add(h.hand_id)
            hands.append(h)
    for h in hands:
        if h.platform != platform:
            raise ValueError(f"{path}: hand {h.hand_id} is {h.platform}")
    hands.sort(key=lambda h: (h.ts is None, h.ts or 0.0))
    return hands


def load_rejected(db: str, platform: str) -> list[dict]:
    path = os.path.join(platform_dir(db, platform), "rejected.jsonl")
    if not os.path.exists(path):
        return []
    out: list[dict] = []
    seen: set[str] = set()
    with open(path) as f:
        for line in f:
            if not line.strip():
                continue
            r = json.loads(line)
            hid = r.get("hand_id")
            if hid is not None:
                if hid in seen:
                    continue
                seen.add(hid)
            out.append(r)
    return out
