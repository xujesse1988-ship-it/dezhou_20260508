#!/usr/bin/env python3
"""
PokerKit evaluator subprocess（C1 cross-validation）。

输入 JSON（stdin，单行）::

    {
        "hands": [
            ["As", "Ks", "Tc", "9d", "2h", "3c", "4s"],   // 7 cards
            ...
        ]
    }

输出 JSON（stdout，单行）::

    {
        "ok": true,
        "categories": [9, 2, ...],              // 0..9 = HandCategory order
        "ref_impl": "pokerkit",
        "ref_version": "x.y.z"
    }

错误：

    {"ok": false, "error_kind": "MissingDependency"|"BadInput", "message": "..."}

退出码：

    0  成功
    2  PokerKit 缺失（harness 跳过该手）
    3  BadInput
    1  其它

PokerKit `StandardHighHand` 的 `entry_count` 用作 0-based 名次；其 `category`
则通过类别名 (即 entry 的 cards 集合特征) 判定 — 但 PokerKit 没有直接暴露
"category" 整数，所以我们用 entry index 反推：StandardHighHand.from_game
返回的是按"由强到弱"排序的 entry 类（0=最强 = StraightFlush / 9=最弱 = HighCard）。

为减少 PokerKit API 依赖深度，本脚本通过 `pokerkit.StandardHighHand` 的 5-card
评估接口枚举 7 选 5 找到最强子集，然后用类名字段（`label` / 类型）映射到我方
HandCategory 的 0..9 enum 编号。
"""
from __future__ import annotations

import json
import sys
from itertools import combinations
from typing import Any


def _emit(payload: dict[str, Any]) -> None:
    json.dump(payload, sys.stdout)
    sys.stdout.write("\n")
    sys.stdout.flush()


def _missing(reason: str) -> None:
    _emit({"ok": False, "error_kind": "MissingDependency", "message": reason})
    sys.exit(2)


def _bad(message: str) -> None:
    _emit({"ok": False, "error_kind": "BadInput", "message": message})
    sys.exit(3)


def _bootstrap_pokerkit() -> Any:
    """Import PokerKit on Python 3.10 with small stdlib backports if installed."""
    try:
        import enum

        if not hasattr(enum, "StrEnum"):
            try:
                from strenum import StrEnum  # type: ignore
            except ImportError as exc:
                _missing(f"PokerKit on Python 3.10 needs `pip install StrEnum`: {exc}")
            enum.StrEnum = StrEnum  # type: ignore[attr-defined]

        try:
            import tomllib  # noqa: F401
        except ModuleNotFoundError:
            try:
                import tomli  # type: ignore
            except ImportError as exc:
                _missing(f"PokerKit on Python 3.10 needs `pip install tomli`: {exc}")
            sys.modules["tomllib"] = tomli

        import pokerkit  # type: ignore

        return pokerkit
    except ImportError as exc:
        _missing(f"pokerkit not installed. `pip install pokerkit`: {exc}")


# 我方 HandCategory 编号（与 src/eval.rs 一致）：
#   0 HighCard, 1 OnePair, 2 TwoPair, 3 Trips, 4 Straight, 5 Flush,
#   6 FullHouse, 7 Quads, 8 StraightFlush, 9 RoyalFlush.
def _classify_hand(hand: Any, pokerkit: Any) -> int:
    """Map a PokerKit `StandardHighHand` to our 0..9 HandCategory ordinal.

    Strategy: inspect the entry's 5-card composition deterministically rather
    than depend on PokerKit's internal class names (which can drift across
    versions).
    """
    # 5 张牌组成的 PokerKit hand 对象
    cards = list(hand.cards)
    ranks = sorted([c.rank for c in cards], reverse=True)
    suits = [c.suit for c in cards]
    rank_counts: dict[Any, int] = {}
    for r in ranks:
        rank_counts[r] = rank_counts.get(r, 0) + 1
    counts = sorted(rank_counts.values(), reverse=True)

    is_flush = len(set(suits)) == 1
    # PokerKit Rank 是字符串，需要映射到整数 (2..14)。
    rank_order = "23456789TJQKA"
    rank_int = sorted([rank_order.index(str(r)) for r in ranks], reverse=True)
    is_straight = len(set(rank_int)) == 5 and (rank_int[0] - rank_int[4] == 4)
    # Wheel A-2-3-4-5: ranks include 12 (A) + 0..3 (2..5)
    is_wheel = sorted(rank_int) == [0, 1, 2, 3, 12]
    is_straight = is_straight or is_wheel
    is_royal = is_flush and sorted(rank_int) == [8, 9, 10, 11, 12]  # T J Q K A

    if is_royal:
        return 9
    if is_straight and is_flush:
        return 8
    if counts == [4, 1]:
        return 7
    if counts == [3, 2]:
        return 6
    if is_flush:
        return 5
    if is_straight:
        return 4
    if counts == [3, 1, 1]:
        return 3
    if counts == [2, 2, 1]:
        return 2
    if counts == [2, 1, 1, 1]:
        return 1
    return 0


def _evaluate_hand(seven_cards: list[str], pokerkit: Any) -> int:
    """Find the best 5-of-7 by enumeration; return its HandCategory ordinal."""
    best_cat = -1
    for subset in combinations(seven_cards, 5):
        try:
            hand = pokerkit.StandardHighHand("".join(subset))
        except Exception as exc:  # PokerKit may throw for invalid card strings
            _bad(f"invalid hand {subset!r}: {exc}")
        cat = _classify_hand(hand, pokerkit)
        if cat > best_cat:
            best_cat = cat
    return best_cat


def main() -> int:
    raw = sys.stdin.read()
    if not raw.strip():
        _bad("empty stdin payload")
    try:
        request = json.loads(raw)
    except json.JSONDecodeError as exc:
        _bad(f"invalid JSON: {exc}")

    if "hands" not in request or not isinstance(request["hands"], list):
        _bad("missing/invalid `hands` array")

    pokerkit = _bootstrap_pokerkit()

    out: list[int] = []
    for i, hand in enumerate(request["hands"]):
        if not (isinstance(hand, list) and len(hand) == 7):
            _bad(f"hand[{i}] must be a 7-card list")
        out.append(_evaluate_hand(hand, pokerkit))

    _emit(
        {
            "ok": True,
            "categories": out,
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
    except Exception:
        import traceback

        sys.stderr.write(traceback.format_exc())
        _emit({"ok": False, "error_kind": "InternalError", "message": "unhandled"})
        sys.exit(1)
