"""No-limit hold'em hand with antes: the rules behind the local arena table.

Written independently of tools/poker_live/table.py on purpose. Every driver at the table re-derives
stacks, pots, the next actor and the legal raise range with that engine and voids the hand on the
first disagreement, so a clean match is a cross-check of both implementations.

Rules follow docs/wopo_vs_poker_manager.md section 5:
- Every dealt seat posts the ante first (in the pot, not part of the street bet), then the blinds.
- Minimum raise = current bet + max(last full raise size, big blind), capped at all-in.
- A short all-in raise does not reopen raising for seats that already acted since the last full raise.
- Chips above what anyone else matched come back as an uncalled refund before pots are built.

Seats are table seats 0..n-1 and every seat is dealt: arena stack rules keep all stacks deep enough
to post, so there is no sitting out and no blind or ante all-in.
"""

from __future__ import annotations

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))))

from opponent_analysis.cards import best_rank  # noqa: E402

STREETS = ("preflop", "flop", "turn", "river")
_BOARD_LEN = {"preflop": 0, "flop": 3, "turn": 4, "river": 5}


class IllegalAction(Exception):
    """The requested action is not in the seat's legal set."""


def settle_pots(committed, folded, rank_of, button):
    """Refund the uncalled excess, build main and side pots, pay them.

    committed: chips each seat put in during the hand, antes included.
    folded: set of folded seats.
    rank_of: seat -> comparable hand strength; only called when two or more seats reach showdown.
    Returns (refund per seat, payout per seat, total_pot), lists indexed by seat.
    """
    n = len(committed)
    order = sorted(range(n), key=lambda s: committed[s], reverse=True)
    refund = [0] * n
    top = order[0]
    refund[top] = committed[top] - committed[order[1]]
    eff = [committed[s] - refund[s] for s in range(n)]
    total_pot = sum(eff)
    payout = [0] * n
    live = [s for s in range(n) if s not in folded]
    if len(live) == 1:
        payout[live[0]] = total_pot
        return refund, payout, total_pot
    ranks = {s: rank_of(s) for s in live}
    prev = 0
    for level in sorted({eff[s] for s in live}):
        amount = sum(min(eff[s], level) - min(eff[s], prev) for s in range(n))
        eligible = [s for s in live if eff[s] >= level]
        best = max(ranks[s] for s in eligible)
        # Odd chips go one at a time starting from the first winner left of the button.
        winners = sorted((s for s in eligible if ranks[s] == best), key=lambda s: (s - button - 1) % n)
        share, odd = divmod(amount, len(winners))
        for i, s in enumerate(winners):
            payout[s] += share + (1 if i < odd else 0)
        prev = level
    if sum(payout) != total_pot:
        raise AssertionError(f"pots do not add up: paid {sum(payout)} of {total_pot}, committed {committed}")
    return refund, payout, total_pot


class Hand:
    def __init__(self, stacks, button, small_blind, big_blind, ante, holes, board):
        n = len(stacks)
        if not (3 <= n <= 9) or len(holes) != n or len(board) != 5 or not 0 <= button < n:
            raise ValueError("bad hand setup")
        self.n = n
        self.button = button
        self.sb, self.bb, self.ante = small_blind, big_blind, ante
        self.holes = [list(h) for h in holes]
        self.full_board = list(board)
        self.start_stack = list(stacks)
        self.stack = list(stacks)
        self.bet = [0] * n
        self.committed = [0] * n
        self.folded = set()
        self.street = "preflop"
        self.pot = 0
        self.sb_seat = (button + 1) % n
        self.bb_seat = (button + 2) % n
        for s in range(n):
            self._pay(s, ante)
        self.bet[self.sb_seat] = self._pay(self.sb_seat, small_blind)
        self.bet[self.bb_seat] = self._pay(self.bb_seat, big_blind)
        if min(self.stack) <= 0:
            raise ValueError(f"a seat cannot cover ante and blind: start stacks {stacks}")
        self.max_bet = big_blind
        self.last_raise = big_blind
        # Seats that still owe an action this round, and seats that acted since the last full raise.
        self.owes = set(range(n))
        self.acted_since_full = set()
        self.to_act = self._next_after(self.bb_seat)
        self.result = None

    def _pay(self, seat, amount):
        if amount > self.stack[seat]:
            raise ValueError(f"seat {seat} cannot post {amount}: stack {self.stack[seat]}")
        self.stack[seat] -= amount
        self.committed[seat] += amount
        self.pot += amount
        return amount

    def _next_after(self, seat):
        for i in range(1, self.n + 1):
            s = (seat + i) % self.n
            if s in self.owes:
                return s
        return None

    def _can_still_bet(self):
        return [s for s in range(self.n) if s not in self.folded and self.stack[s] > 0]

    def in_pot(self):
        return [s for s in range(self.n) if s not in self.folded]

    @property
    def board(self):
        return self.full_board[:_BOARD_LEN[self.street]]

    def legal(self):
        """Legal actions of the seat to act; amounts are street totals (same shape as driver `valid`)."""
        s = self.to_act
        if s is None:
            raise IllegalAction("nobody to act")
        to_call = self.max_bet - self.bet[s]
        opponents_can_act = any(x != s for x in self._can_still_bet())
        can_raise = self.stack[s] > to_call and opponents_can_act and s not in self.acted_since_full
        min_to = max_to = None
        if can_raise:
            max_to = self.bet[s] + self.stack[s]
            min_to = min(self.max_bet + max(self.last_raise, self.bb), max_to)
        return {"can_check": to_call == 0, "can_call": to_call > 0, "can_raise": can_raise,
                "min_raise": min_to, "max_raise": max_to}

    def apply(self, kind, to=None):
        """Apply fold / check / call / raise (to = street total) / all_in for the seat to act.

        Returns the action record. `action` and `amount` use the server's wording: anything that empties
        the stack is `all_in` with an incremental amount, a raise is a street total, a call is incremental.
        `label` is the betting meaning: fold / check / call / raise / all_in, a short call being `call`.
        """
        seat = self.to_act
        legal = self.legal()
        to_call = self.max_bet - self.bet[seat]
        stack_before, pot_before = self.stack[seat], self.pot
        if kind == "fold":
            delta = 0
        elif kind == "check":
            if not legal["can_check"]:
                raise IllegalAction(f"check facing {to_call}")
            delta = 0
        elif kind == "call":
            if not legal["can_call"]:
                raise IllegalAction("call with nothing to call")
            delta = min(to_call, stack_before)
        elif kind in ("raise", "all_in"):
            if not legal["can_raise"]:
                raise IllegalAction(f"{kind}: raising is closed for seat {seat}")
            total = legal["max_raise"] if kind == "all_in" else to
            if isinstance(total, bool) or not isinstance(total, int):
                raise IllegalAction(f"raise amount {total!r}")
            if not legal["min_raise"] <= total <= legal["max_raise"]:
                raise IllegalAction(f"raise to {total} outside {legal['min_raise']}..{legal['max_raise']}")
            delta = total - self.bet[seat]
        else:
            raise IllegalAction(f"unknown action {kind!r}")

        self.stack[seat] -= delta
        self.bet[seat] += delta
        self.committed[seat] += delta
        self.pot += delta
        raised = self.bet[seat] > self.max_bet
        if kind == "fold":
            self.folded.add(seat)
            self.owes.discard(seat)
        elif raised:
            size = self.bet[seat] - self.max_bet
            if size >= max(self.last_raise, self.bb):
                self.last_raise = size
                self.acted_since_full = {seat}
            else:
                self.acted_since_full.add(seat)
            self.max_bet = self.bet[seat]
            self.owes = {s for s in self._can_still_bet() if s != seat}
        else:
            self.acted_since_full.add(seat)
            self.owes.discard(seat)
        if len(self.in_pot()) <= 1:
            self.owes = set()
        self.to_act = self._next_after(seat)

        all_in = delta > 0 and self.stack[seat] == 0
        if kind == "fold":
            label = "fold"
        elif kind == "check":
            label = "check"
        elif raised:
            label = "all_in" if all_in else "raise"
        else:
            label = "call"
        if all_in:
            wire, amount, mode = "all_in", delta, "incremental"
        elif label == "raise":
            wire, amount, mode = "raise", self.bet[seat], "to_total"
        elif label == "call":
            wire, amount, mode = "call", delta, "incremental"
        else:
            wire, amount, mode = label, None, "incremental"
        return {"seat": seat, "action": wire, "amount": amount, "amount_mode": mode, "label": label,
                "delta": delta, "to": self.bet[seat] if raised else None, "to_call_before": to_call,
                "stack_before": stack_before, "stack": self.stack[seat], "pot_before": pot_before,
                "pot": self.pot, "street": self.street}

    def over_by_fold(self):
        return len(self.in_pot()) == 1

    def round_closed(self):
        return self.to_act is None

    def needs_showdown(self):
        return self.round_closed() and not self.over_by_fold() and self.street == "river"

    def next_street(self):
        """Close the betting round and deal the next street. Returns the new cards."""
        if not self.round_closed() or self.over_by_fold() or self.street == "river":
            raise IllegalAction(f"cannot deal after {self.street}")
        before = len(self.board)
        self.street = STREETS[STREETS.index(self.street) + 1]
        self.bet = [0] * self.n
        self.max_bet = 0
        self.last_raise = self.bb
        self.acted_since_full = set()
        live = self._can_still_bet()
        self.owes = set(live) if len(live) >= 2 else set()
        self.to_act = self._next_after(self.button)
        return self.board[before:]

    def settle(self):
        """Pay the hand out once it is over. Sets and returns self.result."""
        if not (self.over_by_fold() or self.needs_showdown()):
            raise IllegalAction("hand is not over")
        refund, payout, total_pot = settle_pots(
            self.committed, self.folded, lambda s: best_rank(self.holes[s] + self.full_board), self.button)
        final = [self.stack[s] + refund[s] + payout[s] for s in range(self.n)]
        if sum(final) != sum(self.start_stack):
            raise AssertionError(f"chips not conserved: {final} from {self.start_stack}")
        self.result = {
            "refund": refund, "payout": payout, "total_pot": total_pot, "final": final,
            "net": [final[s] - self.start_stack[s] for s in range(self.n)],
            "winners": [s for s in range(self.n) if payout[s] > 0],
            "showdown": [] if self.over_by_fold() else self.in_pot(),
        }
        return self.result
