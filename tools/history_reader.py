#!/usr/bin/env python3
"""
跨语言反序列化（C1）：Python 端 minimal proto3 解码器，读取 Rust 端 prost 写出
的 `HandHistory` 二进制并产出 JSON 视图。

- 不依赖 `google.protobuf`（无需 protoc 编译 *_pb2.py）。
- 仅实现 proto3 wire format 的 4 种 case：
    wire 0 (varint), wire 2 (length-delimited)；其它 wire type 视为输入异常。
- schema 与 `proto/hand_history.proto` 严格对应；任何 prost 升级 / .proto 改动
  必须同步更新本脚本（与 `src/history.rs` 同步刷新由 [实现] / [决策] agent 负责）。

输入 JSON (stdin)::

    {
      "blobs_b64": ["...", "...", ...]   // 每项一个 base64 编码的 HandHistory proto
    }

输出 JSON (stdout)::

    {"ok": true, "decoded": [<json view per blob>], "ref_impl": "py-mini-proto"}

错误::

    {"ok": false, "error_kind": "BadInput", "message": "..."}
"""
from __future__ import annotations

import base64
import json
import sys
import traceback
from typing import Any


# ============================================================================
# Wire format decoder
# ============================================================================


def _decode_varint(buf: bytes, pos: int) -> tuple[int, int]:
    n = 0
    shift = 0
    while True:
        if pos >= len(buf):
            raise ValueError("truncated varint")
        b = buf[pos]
        pos += 1
        n |= (b & 0x7F) << shift
        if not (b & 0x80):
            return n, pos
        shift += 7
        if shift > 63:
            raise ValueError("varint too long")


def _decode_zigzag(n: int) -> int:
    return (n >> 1) ^ -(n & 1)


# Schema：{field_num: (name, wire_kind, extra)}.
# wire_kind ∈ {'varint', 'packed_varint_repeated', 'msg_single', 'msg_repeated'}.
# extra:
#   - varint: scalar tag — 'u32' / 'u64' / 'sint64' / 'bool' / 'enum'
#   - packed_varint_repeated: scalar tag for each element
#   - msg_*: nested schema dict
HOLE_CARDS_SCHEMA = {
    1: ("present", "varint", "bool"),
    2: ("c0", "varint", "u32"),
    3: ("c1", "varint", "u32"),
}

TABLE_CONFIG_SCHEMA = {
    1: ("n_seats", "varint", "u32"),
    2: ("starting_stacks", "packed_varint_repeated", "u64"),
    3: ("small_blind", "varint", "u64"),
    4: ("big_blind", "varint", "u64"),
    5: ("ante", "varint", "u64"),
    6: ("button_seat", "varint", "u32"),
}

RECORDED_ACTION_SCHEMA = {
    1: ("seq", "varint", "u32"),
    2: ("seat", "varint", "u32"),
    3: ("street", "varint", "enum"),
    4: ("kind", "varint", "enum"),
    5: ("to", "varint", "u64"),
    6: ("committed_after", "varint", "u64"),
}

PAYOUT_SCHEMA = {
    1: ("seat", "varint", "u32"),
    2: ("amount", "varint", "sint64"),
}

HAND_HISTORY_SCHEMA = {
    1: ("schema_version", "varint", "u32"),
    2: ("config", "msg_single", TABLE_CONFIG_SCHEMA),
    3: ("seed", "varint", "u64"),
    4: ("actions", "msg_repeated", RECORDED_ACTION_SCHEMA),
    5: ("board", "packed_varint_repeated", "u32"),
    6: ("hole_cards", "msg_repeated", HOLE_CARDS_SCHEMA),
    7: ("final_payouts", "msg_repeated", PAYOUT_SCHEMA),
    8: ("showdown_order", "packed_varint_repeated", "u32"),
}


def _coerce_scalar(value: int, tag: str) -> Any:
    if tag == "u32" or tag == "u64" or tag == "enum":
        return value
    if tag == "bool":
        return bool(value)
    if tag == "sint64":
        return _decode_zigzag(value)
    raise ValueError(f"unknown scalar tag: {tag}")


def _decode_message(buf: bytes, schema: dict[int, tuple[str, str, Any]]) -> dict[str, Any]:
    out: dict[str, Any] = {}
    # 初始化所有 repeated 字段为空 list（proto3 默认空）。
    for _num, (name, kind, _extra) in schema.items():
        if kind in ("msg_repeated", "packed_varint_repeated"):
            out.setdefault(name, [])
    pos = 0
    while pos < len(buf):
        tag, pos = _decode_varint(buf, pos)
        field_num = tag >> 3
        wire = tag & 7
        info = schema.get(field_num)
        if info is None:
            # 未知字段：按 wire type 跳过（proto3 forward-compat）。
            if wire == 0:
                _, pos = _decode_varint(buf, pos)
            elif wire == 2:
                length, pos = _decode_varint(buf, pos)
                pos += length
            else:
                raise ValueError(f"unsupported wire type {wire}")
            continue

        name, kind, extra = info
        if kind == "varint":
            if wire != 0:
                raise ValueError(f"field {name}: expected varint, got wire {wire}")
            v, pos = _decode_varint(buf, pos)
            out[name] = _coerce_scalar(v, extra)
        elif kind == "packed_varint_repeated":
            if wire == 2:
                length, pos = _decode_varint(buf, pos)
                end = pos + length
                lst = out.setdefault(name, [])
                while pos < end:
                    v, pos = _decode_varint(buf, pos)
                    lst.append(_coerce_scalar(v, extra))
            elif wire == 0:
                # 也接受非 packed 编码：每个 element 单独 wire 0
                v, pos = _decode_varint(buf, pos)
                out.setdefault(name, []).append(_coerce_scalar(v, extra))
            else:
                raise ValueError(f"field {name}: bad wire for packed repeated: {wire}")
        elif kind == "msg_single":
            if wire != 2:
                raise ValueError(f"field {name}: expected length-delim, got {wire}")
            length, pos = _decode_varint(buf, pos)
            sub = buf[pos : pos + length]
            pos += length
            out[name] = _decode_message(sub, extra)
        elif kind == "msg_repeated":
            if wire != 2:
                raise ValueError(f"field {name}: expected length-delim, got {wire}")
            length, pos = _decode_varint(buf, pos)
            sub = buf[pos : pos + length]
            pos += length
            out.setdefault(name, []).append(_decode_message(sub, extra))
        else:
            raise ValueError(f"unknown kind: {kind}")
    return out


# ============================================================================
# 后处理：把 enum 数值转为字符串，使 JSON 与 Rust 端 encode_request_json 对齐
# ============================================================================

STREET_NAME = {
    0: "unspecified",
    1: "preflop",
    2: "flop",
    3: "turn",
    4: "river",
    5: "showdown",
}

ACTION_KIND_NAME = {
    0: "unspecified",
    1: "fold",
    2: "check",
    3: "call",
    4: "bet",
    5: "raise",
}

RANK_LABELS = ["2", "3", "4", "5", "6", "7", "8", "9", "T", "J", "Q", "K", "A"]
SUIT_LABELS = ["c", "d", "h", "s"]


def _card_to_string(v: int) -> str:
    return f"{RANK_LABELS[v // 4]}{SUIT_LABELS[v % 4]}"


def _normalize_history(decoded: dict[str, Any]) -> dict[str, Any]:
    """Normalize a decoded message, supplying proto3 defaults for missing scalar
    fields (proto3 omits zero-valued scalars on the wire — `.get(name, default)`
    is mandatory)."""
    cfg = decoded.get("config", {})
    actions = decoded.get("actions", [])
    normalized_actions = []
    for a in actions:
        normalized_actions.append({
            "seq": a.get("seq", 0),
            "seat": a.get("seat", 0),
            "street": STREET_NAME.get(a.get("street", 0), str(a.get("street", 0))),
            "kind": ACTION_KIND_NAME.get(a.get("kind", 0), str(a.get("kind", 0))),
            "to": a.get("to", 0),
            "committed_after": a.get("committed_after", 0),
        })
    return {
        "schema_version": decoded.get("schema_version", 0),
        "config": {
            "n_seats": cfg.get("n_seats", 0),
            "starting_stacks": cfg.get("starting_stacks", []),
            "small_blind": cfg.get("small_blind", 0),
            "big_blind": cfg.get("big_blind", 0),
            "ante": cfg.get("ante", 0),
            "button_seat": cfg.get("button_seat", 0),
        },
        "seed": decoded.get("seed", 0),
        "actions": normalized_actions,
        "board": [_card_to_string(v) for v in decoded.get("board", [])],
        "hole_cards": [
            (
                [_card_to_string(h.get("c0", 0)), _card_to_string(h.get("c1", 0))]
                if h.get("present", False)
                else None
            )
            for h in decoded.get("hole_cards", [])
        ],
        "final_payouts": [
            {"seat": p.get("seat", 0), "net": p.get("amount", 0)}
            for p in decoded.get("final_payouts", [])
        ],
        "showdown_order": list(decoded.get("showdown_order", [])),
    }


# ============================================================================
# 入口
# ============================================================================


def main() -> int:
    raw = sys.stdin.read()
    if not raw.strip():
        json.dump({"ok": False, "error_kind": "BadInput", "message": "empty stdin"}, sys.stdout)
        return 3

    request = json.loads(raw)
    blobs_b64 = request.get("blobs_b64", [])
    if not isinstance(blobs_b64, list):
        json.dump({"ok": False, "error_kind": "BadInput", "message": "blobs_b64 not list"}, sys.stdout)
        return 3

    decoded_all: list[dict[str, Any]] = []
    for i, b64 in enumerate(blobs_b64):
        try:
            blob = base64.b64decode(b64)
            raw_msg = _decode_message(blob, HAND_HISTORY_SCHEMA)
            decoded_all.append(_normalize_history(raw_msg))
        except Exception as exc:
            json.dump(
                {
                    "ok": False,
                    "error_kind": "BadInput",
                    "message": f"decode error at index {i}: {exc}",
                },
                sys.stdout,
            )
            return 3

    json.dump(
        {"ok": True, "decoded": decoded_all, "ref_impl": "py-mini-proto"},
        sys.stdout,
    )
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except SystemExit:
        raise
    except Exception:
        sys.stderr.write(traceback.format_exc())
        json.dump({"ok": False, "error_kind": "InternalError", "message": "unhandled"}, sys.stdout)
        sys.exit(1)
