#!/usr/bin/env python3
"""
PokerKit cross-validation 子进程入口（B1 骨架）。

用途：

- 接收 Rust 端 [`HandHistory`] 的 JSON 表达（见下方 schema）
- 用 PokerKit 重放同一手牌
- 输出 PokerKit 终局快照 JSON 给 Rust 端比对

Rust 端 harness 见 `tests/cross_validation.rs`；该 harness 通过 stdin / stdout
JSON 通信。

约束（D-086）：cross-validation 必须把参考实现配置为"全程 n_seats 全部在场、
无 sit-in/sit-out、按钮机械每手左移、SB/BB 由按钮机械推导"。本脚本默认遵循
该约束。

阶段 1 状态：本脚本是骨架占位。运行需要 `pip install pokerkit`。
若 PokerKit 未安装，脚本以 exit code 2 退出并返回结构化错误，便于 harness
跳过该手而不崩溃。

输入 JSON schema（kebab 风格用 snake_case）：

    {
      "schema_version": 1,
      "n_seats": 6,
      "starting_stacks": [10000, 10000, 10000, 10000, 10000, 10000],
      "small_blind": 50,
      "big_blind": 100,
      "ante": 0,
      "button_seat": 0,
      "hole_cards": [
        ["As","Ks"], ["2c","3c"], ...
      ],
      "board": ["Td","9d","8d","7d","6d"],
      "actions": [
        {"seat": 3, "street": "preflop", "kind": "fold",  "to": 0},
        {"seat": 0, "street": "preflop", "kind": "raise", "to": 300},
        ...
      ]
    }

输出 JSON schema：

    {
      "ok": true,
      "final_payouts": [{"seat": 0, "net": 150}, ...],
      "showdown_order": [0, 1, 2],
      "ref_impl": "pokerkit",
      "ref_version": "x.y.z"
    }

错误：

    {"ok": false, "error_kind": "MissingDependency"|"BadInput"|"ReferenceMismatch",
     "message": "..."}
"""
from __future__ import annotations

import json
import sys
import traceback
from typing import Any


def _emit(payload: dict[str, Any]) -> None:
    json.dump(payload, sys.stdout)
    sys.stdout.write("\n")
    sys.stdout.flush()


def _missing_pokerkit(reason: str) -> None:
    _emit({"ok": False, "error_kind": "MissingDependency", "message": reason})
    # exit code 2: harness 检测到该值时跳过该手并标记 "skipped"，不计为分歧
    sys.exit(2)


def _bad_input(message: str) -> None:
    _emit({"ok": False, "error_kind": "BadInput", "message": message})
    sys.exit(3)


def main() -> int:
    # B1 骨架：先解析输入，再尝试 import pokerkit。任何失败均结构化输出。
    raw = sys.stdin.read()
    if not raw.strip():
        _bad_input("empty stdin payload")

    try:
        request = json.loads(raw)
    except json.JSONDecodeError as exc:
        _bad_input(f"invalid JSON: {exc}")

    # 基本字段校验
    required_keys = {
        "schema_version",
        "n_seats",
        "starting_stacks",
        "small_blind",
        "big_blind",
        "ante",
        "button_seat",
        "hole_cards",
        "board",
        "actions",
    }
    missing = required_keys - set(request)
    if missing:
        _bad_input(f"missing keys: {sorted(missing)}")

    if request["schema_version"] != 1:
        _bad_input(f"unsupported schema_version: {request['schema_version']}")

    # 尝试导入 pokerkit
    try:
        import pokerkit  # type: ignore
    except ImportError:
        _missing_pokerkit(
            "pokerkit not installed. Install with `pip install pokerkit` and re-run."
        )

    # B2/C1 阶段：把 request 翻译成 pokerkit 的 NoLimitTexasHoldem 状态机调用，
    # 步进 actions 序列，读取 final_payouts / showdown_order，结构化输出。
    #
    # 当前留 NotImplemented 占位 —— B1 出口标准只要求"harness 能用 stub 跑通流程"，
    # 故脚本端先行返回 ok=False 但 error_kind 区分；Rust 端识别 "B1Stub"
    # 并把该手计为 "skipped"，不计为分歧。
    _emit(
        {
            "ok": False,
            "error_kind": "B1Stub",
            "message": (
                "tools/pokerkit_replay.py is a B1 skeleton. PokerKit translation"
                " lands in B2 alongside GameState. Got valid request but no"
                " replay logic yet."
            ),
            "ref_impl": "pokerkit",
            "ref_version": getattr(pokerkit, "__version__", "unknown"),
        }
    )
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except SystemExit:
        raise
    except Exception:  # pragma: no cover — defensive top-level
        sys.stderr.write(traceback.format_exc())
        _emit(
            {
                "ok": False,
                "error_kind": "InternalError",
                "message": "unhandled exception in pokerkit_replay.py (see stderr)",
            }
        )
        sys.exit(1)
