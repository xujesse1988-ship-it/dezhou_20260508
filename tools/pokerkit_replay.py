#!/usr/bin/env python3
"""
PokerKit cross-validation 子进程入口（B2 replay）。

用途：

- 接收 Rust 端 [`HandHistory`] 的 JSON 表达（见下方 schema）
- 用 PokerKit 重放同一手牌
- 输出 PokerKit 终局快照 JSON 给 Rust 端比对

Rust 端 harness 见 `tests/cross_validation.rs`；该 harness 通过 stdin / stdout
JSON 通信。

约束（D-086）：cross-validation 必须把参考实现配置为"全程 n_seats 全部在场、
无 sit-in/sit-out、按钮机械每手左移、SB/BB 由按钮机械推导"。本脚本默认遵循
该约束。

阶段 1 状态：本脚本已执行 PokerKit replay。运行需要 `pip install pokerkit`；
在 Python 3.10 下还需要 `StrEnum` / `tomli` backport。若 PokerKit 未安装，
脚本以 exit code 2 退出并返回结构化错误，便于 harness 跳过该手而不崩溃。

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


def _reference_mismatch(message: str) -> dict[str, Any]:
    return {"ok": False, "error_kind": "ReferenceMismatch", "message": message}


def _bootstrap_pokerkit() -> Any:
    """Import PokerKit on Python 3.10 with small stdlib backports if installed."""
    try:
        import enum

        if not hasattr(enum, "StrEnum"):
            try:
                from strenum import StrEnum  # type: ignore
            except ImportError as exc:
                _missing_pokerkit(
                    "PokerKit on Python 3.10 needs `pip install StrEnum`: "
                    f"{exc}"
                )
            enum.StrEnum = StrEnum  # type: ignore[attr-defined]

        try:
            import tomllib  # noqa: F401
        except ModuleNotFoundError:
            try:
                import tomli  # type: ignore
            except ImportError as exc:
                _missing_pokerkit(
                    "PokerKit on Python 3.10 needs `pip install tomli`: "
                    f"{exc}"
                )
            sys.modules["tomllib"] = tomli

        import pokerkit  # type: ignore

        return pokerkit
    except ImportError as exc:
        _missing_pokerkit(
            "pokerkit not installed. Install with `pip install pokerkit` and re-run. "
            f"Details: {exc}"
        )


def _seat_to_ref(seat: int, button_seat: int, n_seats: int) -> int:
    sb = (button_seat + 1) % n_seats
    return (seat - sb) % n_seats


def _ref_to_seat(index: int, button_seat: int, n_seats: int) -> int:
    sb = (button_seat + 1) % n_seats
    return (sb + index) % n_seats


def _street_board_count(street: str) -> int:
    return {
        "preflop": 0,
        "flop": 3,
        "turn": 4,
        "river": 5,
        "showdown": 5,
    }[street]


def _deal_to_board_count(state: Any, board: list[str], count: int) -> None:
    current = len(tuple(state.board_cards))
    while current < count:
        if state.can_burn_card("??"):
            state.burn_card("??")
        if current == 0 and count >= 3:
            if not state.can_deal_board("".join(board[0:3])):
                break
            state.deal_board("".join(board[0:3]))
            current = 3
        elif current == 3 and count >= 4:
            if not state.can_deal_board(board[3]):
                break
            state.deal_board(board[3])
            current = 4
        elif current == 4 and count >= 5:
            if not state.can_deal_board(board[4]):
                break
            state.deal_board(board[4])
            current = 5
        else:
            break


def _drain_showdown(
        state: Any,
        request: dict[str, Any],
        showdown_order: list[int],
) -> bool:
    progressed = False
    n_seats = request["n_seats"]
    button_seat = request["button_seat"]

    while state.showdown_index is not None:
        ref_index = state.showdown_index
        seat = _ref_to_seat(ref_index, button_seat, n_seats)
        if seat not in showdown_order:
            showdown_order.append(seat)
        state.show_or_muck_hole_cards("".join(request["hole_cards"][seat]))
        progressed = True

    return progressed


def _settle_to_terminal(
        state: Any,
        request: dict[str, Any],
        showdown_order: list[int],
) -> None:
    board = request["board"]

    for _ in range(16):
        progressed = _drain_showdown(state, request, showdown_order)
        before_board_count = len(tuple(state.board_cards))
        _deal_to_board_count(state, board, len(board))
        progressed = progressed or len(tuple(state.board_cards)) != before_board_count

        if not state.status:
            break
        if not progressed:
            break


def _validate_request(request: dict[str, Any]) -> None:
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

    n_seats = request["n_seats"]
    if not isinstance(n_seats, int) or not 2 <= n_seats <= 9:
        _bad_input(f"invalid n_seats: {n_seats!r}")
    if len(request["starting_stacks"]) != n_seats:
        _bad_input("starting_stacks length != n_seats")
    if len(request["hole_cards"]) != n_seats:
        _bad_input("hole_cards length != n_seats")


def _replay_with_pokerkit(request: dict[str, Any], pokerkit: Any) -> dict[str, Any]:
    n_seats = request["n_seats"]
    button_seat = request["button_seat"]
    ref_stacks = [
        request["starting_stacks"][_ref_to_seat(i, button_seat, n_seats)]
        for i in range(n_seats)
    ]

    state = pokerkit.NoLimitTexasHoldem.create_state(
        (
            pokerkit.Automation.ANTE_POSTING,
            pokerkit.Automation.BET_COLLECTION,
            pokerkit.Automation.BLIND_OR_STRADDLE_POSTING,
            pokerkit.Automation.HAND_KILLING,
            pokerkit.Automation.CHIPS_PUSHING,
            pokerkit.Automation.CHIPS_PULLING,
        ),
        False,
        request["ante"],
        (request["small_blind"], request["big_blind"]),
        request["big_blind"],
        tuple(ref_stacks),
        n_seats,
    )

    for ref_index in range(n_seats):
        seat = _ref_to_seat(ref_index, button_seat, n_seats)
        hole = request["hole_cards"][seat]
        if not hole:
            _bad_input(f"missing hole cards for seat {seat}")
        state.deal_hole("".join(hole))

    board = request["board"]
    for action in request["actions"]:
        _deal_to_board_count(state, board, _street_board_count(action["street"]))
        expected_actor = _seat_to_ref(action["seat"], button_seat, n_seats)
        if state.actor_index != expected_actor:
            return _reference_mismatch(
                f"actor mismatch at seq={action.get('seq')}: "
                f"PokerKit actor={state.actor_index}, expected={expected_actor}, "
                f"action={action}"
            )

        kind = action["kind"]
        if kind == "fold":
            state.fold()
        elif kind in {"check", "call"}:
            state.check_or_call()
        elif kind in {"bet", "raise"}:
            state.complete_bet_or_raise_to(action["to"])
        else:
            _bad_input(f"unknown action kind: {kind!r}")

    showdown_order = []
    _settle_to_terminal(state, request, showdown_order)

    final_payouts = []
    for seat in range(n_seats):
        ref_index = _seat_to_ref(seat, button_seat, n_seats)
        final_payouts.append(
            {
                "seat": seat,
                "net": state.stacks[ref_index] - request["starting_stacks"][seat],
            }
        )

    return {
        "ok": True,
        "final_payouts": final_payouts,
        "showdown_order": showdown_order,
        "ref_impl": "pokerkit",
        "ref_version": getattr(pokerkit, "__version__", "unknown"),
    }


def main() -> int:
    # B1 骨架：先解析输入，再尝试 import pokerkit。任何失败均结构化输出。
    raw = sys.stdin.read()
    if not raw.strip():
        _bad_input("empty stdin payload")

    try:
        request = json.loads(raw)
    except json.JSONDecodeError as exc:
        _bad_input(f"invalid JSON: {exc}")

    _validate_request(request)
    pokerkit = _bootstrap_pokerkit()
    _emit(_replay_with_pokerkit(request, pokerkit))
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
