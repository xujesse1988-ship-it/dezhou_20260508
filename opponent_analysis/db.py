"""Per-platform hand store: <db>/<platform>/{hands,rejected,ingest_log}.jsonl.

A platform directory only ever receives hands of that platform; ingest refuses a file
whose detected platform differs from the requested one.
"""

from __future__ import annotations

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


def ingest(db: str, hh_path: str, ws_path: Optional[str] = None, platform: Optional[str] = None) -> dict:
    first = next(iter_records(hh_path), None)
    if first is None:
        raise ValueError(f"{hh_path}: empty")
    detected = detect_platform(first[1])
    if platform is not None and platform != detected:
        raise ValueError(f"{hh_path}: file is {detected}, not {platform}; platforms are never mixed")
    plat = PLATFORMS[detected]
    pdir = platform_dir(db, detected)
    hands_path = os.path.join(pdir, "hands.jsonl")
    rej_path = os.path.join(pdir, "rejected.jsonl")
    have = _ids(hands_path)
    rejected_before = _ids(rej_path)
    ws = openpoker_ws_actions(ws_path) if (ws_path and detected == "openpoker") else None

    counts: Counter = Counter()
    with open(hands_path, "a") as hf, open(rej_path, "a") as rf:
        for line_no, rec in iter_records(hh_path):
            if detect_platform(rec) != detected:
                raise ValueError(f"{hh_path}:{line_no}: mixed platforms in one file")
            hid = rec.get("hand_id")
            if hid in have:
                counts["duplicate"] += 1
                continue
            if hid in rejected_before:
                counts["duplicate_rejected"] += 1
                continue
            try:
                raw = ADAPTERS[detected](rec, hh_path, ws)
                hand = replay(raw, plat.heads_up_button_is_sb)
            except ReplayError as e:
                counts["rejected:" + e.code] += 1
                rf.write(json.dumps({"hand_id": hid, "file": hh_path, "line": line_no, "code": e.code, "detail": e.detail}) + "\n")
                continue
            except (KeyError, TypeError, ValueError) as e:
                counts["rejected:malformed"] += 1
                rf.write(json.dumps({"hand_id": hid, "file": hh_path, "line": line_no, "code": "malformed", "detail": repr(e)}) + "\n")
                continue
            hf.write(json.dumps(hand.to_json(), separators=(",", ":")) + "\n")
            have.add(hid)
            counts["accepted"] += 1
            if "forced_unknown" in hand.notes:
                counts["forced_unknown"] += 1
    entry = {"time": time.time(), "platform": detected, "hh": os.path.abspath(hh_path), "hh_sha256": _sha256(hh_path),
             "ws": os.path.abspath(ws_path) if ws_path else None, "counts": dict(counts)}
    with open(os.path.join(pdir, "ingest_log.jsonl"), "a") as f:
        f.write(json.dumps(entry) + "\n")
    return entry


def load_hands(db: str, platform: str) -> list[Hand]:
    path = os.path.join(platform_dir(db, platform), "hands.jsonl")
    if not os.path.exists(path):
        return []
    with open(path) as f:
        hands = [Hand.from_json(json.loads(line)) for line in f if line.strip()]
    for h in hands:
        if h.platform != platform:
            raise ValueError(f"{path}: hand {h.hand_id} is {h.platform}")
    hands.sort(key=lambda h: (h.ts is None, h.ts or 0.0))
    return hands


def load_rejected(db: str, platform: str) -> list[dict]:
    path = os.path.join(platform_dir(db, platform), "rejected.jsonl")
    if not os.path.exists(path):
        return []
    with open(path) as f:
        return [json.loads(line) for line in f if line.strip()]
