"""Betting replay: turns a RawHand into a validated Hand with one Decision per action.

This is a self-contained NLHE betting-round model (blinds, turn order, round closure,
all-ins). It does not compute side pots or payouts; chip validation uses the
source's final stacks instead. Any inconsistency raises ReplayError with a short
reason code, and the caller records the hand as rejected (never dropped silently).
"""

from __future__ import annotations

from .cards import RANKS, SUITS
from .model import STREETS, Decision, Hand, RawHand

BOARD_LEN = {"preflop": 0, "flop": 3, "turn": 4, "river": 5}


class ReplayError(Exception):
    def __init__(self, code: str, detail: str = ""):
        super().__init__(f"{code}: {detail}" if detail else code)
        self.code = code
        self.detail = detail


def check_cards(board: list, shown: dict, dealt: list[int]) -> None:
    """Board and shown hands must be well-formed, distinct cards (the showdown evaluator relies on it)."""
    if len(board) > 5:
        raise ReplayError("bad_cards", f"board has {len(board)} cards")
    cards = list(board)
    for s, c in shown.items():
        if s not in dealt or len(c) != 2:
            raise ReplayError("bad_cards", f"seat {s} shows {c}")
        cards += c
    for c in cards:
        if not (isinstance(c, str) and len(c) == 2 and c[0].upper() in RANKS and c[1].lower() in SUITS):
            raise ReplayError("bad_cards", repr(c))
    if len({c.lower() for c in cards}) != len(cards):
        raise ReplayError("bad_cards", f"duplicate card in {cards}")


def ring_from(seats: list[int], start: int) -> list[int]:
    """Seats in clockwise order (increasing seat number, wrapping) starting at `start`."""
    s = sorted(seats)
    i = s.index(start)
    return s[i:] + s[:i]


def assign_positions(dealt: list[int], button: int, heads_up_button_is_sb: bool) -> tuple[dict[int, str], int, int]:
    """Return (position label per seat, sb seat, bb seat)."""
    if button not in dealt:
        raise ReplayError("button_not_dealt", f"button {button} dealt {dealt}")
    order = ring_from(dealt, button)
    n = len(order)
    if n < 2:
        raise ReplayError("too_few_players")
    if n == 2:
        if heads_up_button_is_sb:
            return {order[0]: "BTN_SB", order[1]: "BB"}, order[0], order[1]
        return {order[1]: "SB", order[0]: "BTN_BB"}, order[1], order[0]
    pos = {order[0]: "BTN", order[1]: "SB", order[2]: "BB"}
    middle = order[3:]  # first after BB ... last before BTN
    k = len(middle)
    for i, seat in enumerate(middle):
        dist = k - i  # 1 = seat right of the button
        if dist == 1:
            pos[seat] = "CO"
        elif i == 0:
            pos[seat] = "UTG"
        elif dist == 2:
            pos[seat] = "HJ"
        elif dist == 3:
            pos[seat] = "LJ"
        else:
            pos[seat] = f"UTG{i}"
    return pos, order[1], order[2]


def position_class(label: str) -> str:
    if label in ("BTN", "CO", "BTN_SB"):
        return "LP"
    if label in ("SB", "BB", "BTN_BB"):
        return "BLIND"
    if label in ("HJ", "LJ"):
        return "MP"
    return "EP"


def replay(raw: RawHand, heads_up_button_is_sb: bool) -> Hand:
    dealt = sorted(raw.dealt_seats)
    positions, sb_seat, bb_seat = assign_positions(dealt, raw.button_seat, heads_up_button_is_sb)
    for s in dealt:
        if s not in raw.start_stacks:
            raise ReplayError("missing_start_stack", str(s))
        if s not in raw.final_stacks and s not in raw.missing_final:
            raise ReplayError("missing_final_stack", str(s))

    stack = {s: raw.start_stacks[s] for s in dealt}
    committed = {s: 0 for s in dealt}
    contrib = {s: 0 for s in dealt}
    folded: set[int] = set()
    all_in: set[int] = set()

    def post(seat: int, amount: int) -> None:
        amt = min(amount, stack[seat])
        stack[seat] -= amt
        contrib[seat] += amt
        committed[seat] += amt
        if stack[seat] == 0:
            all_in.add(seat)

    post(sb_seat, raw.sb)
    post(bb_seat, raw.bb)

    street_i = 0
    current_bet = max(contrib.values())
    raises = 0
    aggressor = None
    callers_since_aggr = 0
    acted_on_street: set[int] = set()
    order = ring_from(dealt, raw.button_seat)

    def can_act(s: int) -> bool:
        return s not in folded and s not in all_in

    need = {s for s in dealt if can_act(s)}
    last_actor = bb_seat  # preflop: first to act is the seat after BB
    preflop_aggr = None
    streets_seen = {s: "preflop" for s in dealt}
    street_start_pot = sum(committed.values())
    decisions: list[Decision] = []
    finished = False  # no more betting possible (one live seat, or nobody left to act)

    def next_actor(after: int) -> int | None:
        ring = ring_from(dealt, after)[1:] + [after]
        for s in ring:
            if s in need:
                return s
        return None

    def betting_closed() -> bool:
        live = [s for s in dealt if s not in folded]
        if len(live) <= 1:
            return True
        return not need

    def advance_street() -> None:
        nonlocal street_i, current_bet, raises, aggressor, callers_since_aggr, need, last_actor, street_start_pot, finished
        live = [s for s in dealt if s not in folded]
        if len(live) <= 1:
            finished = True
            return
        actors = [s for s in live if can_act(s)]
        if len(actors) <= 1 or street_i == 3:
            # No further decisions (everyone else all-in) or river closed.
            finished = True
            if street_i < 3 and len(live) >= 2:
                for s in live:
                    streets_seen[s] = "river"
            return
        street_i += 1
        for s in dealt:
            contrib[s] = 0
        current_bet = 0
        raises = 0
        aggressor = None
        callers_since_aggr = 0
        acted_on_street.clear()
        need = set(actors)
        last_actor = raw.button_seat
        street_start_pot = sum(committed.values())
        for s in live:
            streets_seen[s] = STREETS[street_i]

    if betting_closed():
        advance_street()

    for idx, a in enumerate(raw.actions):
        if finished:
            raise ReplayError("actions_after_end", f"action {idx} seat {a.seat}")
        street = STREETS[street_i]
        exp = next_actor(last_actor)
        if a.seat != exp:
            raise ReplayError("out_of_turn", f"{street} action {idx}: got seat {a.seat}, expected {exp}")
        if a.stack_before is not None and a.stack_before != stack[a.seat]:
            raise ReplayError("stack_mismatch", f"action {idx} seat {a.seat}: source {a.stack_before} replay {stack[a.seat]}")
        to_call = max(0, current_bet - contrib[a.seat])
        n_live = sum(1 for s in dealt if s not in folded)
        n_can = sum(1 for s in dealt if can_act(s))
        pot_before = sum(committed.values())
        kind = a.kind
        delta = 0
        if kind == "fold":
            pass
        elif kind == "check":
            if to_call != 0:
                raise ReplayError("check_facing_bet", f"action {idx} seat {a.seat} to_call {to_call}")
        elif kind == "call":
            if to_call == 0:
                kind = "check"
            else:
                delta = min(to_call, stack[a.seat])
        elif kind in ("bet", "raise"):
            if a.to_total is not None:
                delta = a.to_total - contrib[a.seat]
            elif a.delta is not None:
                delta = a.delta
            else:
                raise ReplayError("raise_without_amount", f"action {idx}")
            if delta > stack[a.seat]:
                raise ReplayError("raise_exceeds_stack", f"action {idx} delta {delta} stack {stack[a.seat]}")
            if contrib[a.seat] + delta <= current_bet:
                raise ReplayError("raise_not_above_bet", f"action {idx} to {contrib[a.seat] + delta} bet {current_bet}")
        elif kind == "all_in":
            delta = stack[a.seat]
        else:
            raise ReplayError("unknown_action", kind)
        if a.delta is not None and kind != "fold" and a.delta != delta:
            raise ReplayError("delta_mismatch", f"action {idx} seat {a.seat}: source {a.delta} replay {delta}")

        new_total = contrib[a.seat] + delta
        if kind in ("bet", "raise", "all_in"):
            if new_total > current_bet:
                kind = "bet" if current_bet == 0 else "raise"
            elif kind == "all_in":
                kind = "call" if delta > 0 else "check"
        went_all_in = delta > 0 and delta == stack[a.seat]

        decisions.append(Decision(
            idx=idx, seat=a.seat, street=street, kind=kind, all_in=went_all_in, delta=delta,
            to_total=new_total, to_call=to_call, pot_before=pot_before,
            street_start_pot=street_start_pot, current_bet=current_bet, raises_before=raises,
            n_live=n_live, n_can_act=n_can, first_on_street=a.seat not in acted_on_street,
            aggressor=aggressor, prior_callers=callers_since_aggr, forced=a.forced,
        ))

        acted_on_street.add(a.seat)
        need.discard(a.seat)
        last_actor = a.seat
        if kind == "fold":
            folded.add(a.seat)
        else:
            stack[a.seat] -= delta
            contrib[a.seat] = new_total
            committed[a.seat] += delta
            if stack[a.seat] == 0:
                all_in.add(a.seat)
            if new_total > current_bet:
                current_bet = new_total
                raises += 1
                aggressor = a.seat
                callers_since_aggr = 0
                if street == "preflop":
                    preflop_aggr = a.seat
                need = {s for s in dealt if can_act(s) and s != a.seat}
            elif kind == "call":
                callers_since_aggr += 1
        if betting_closed():
            advance_street()

    if not finished:
        raise ReplayError("incomplete", f"street {STREETS[street_i]} waiting for {sorted(need)}")

    live = [s for s in dealt if s not in folded]
    showdown = live if len(live) >= 2 else []
    last_street = max((streets_seen[s] for s in live), key=STREETS.index) if live else "preflop"
    want_board = 5 if showdown else BOARD_LEN[last_street]
    if len(raw.board) < want_board:
        raise ReplayError("board_incomplete", f"have {len(raw.board)} want {want_board}")
    check_cards(raw.board, raw.shown, dealt)

    final_stacks = dict(raw.final_stacks)
    notes = []
    for s in raw.missing_final:
        if s not in folded:
            raise ReplayError("missing_final_stack", f"seat {s} left without folding")
        final_stacks[s] = raw.start_stacks[s] - committed[s]
        notes.append(f"final_filled:{s}")

    # Chip checks: folded seats lose exactly what they put in; the table is zero-sum.
    for s in folded:
        if final_stacks[s] != raw.start_stacks[s] - committed[s]:
            raise ReplayError("folded_chip_mismatch", f"seat {s} final {final_stacks[s]} start {raw.start_stacks[s]} committed {committed[s]}")
    diff = sum(final_stacks[s] - raw.start_stacks[s] for s in dealt)
    if diff != 0:
        raise ReplayError("not_zero_sum", f"sum(final-start) = {diff}")
    if raw.source_net is not None:
        for s, net in raw.source_net.items():
            if s in final_stacks and final_stacks[s] - raw.start_stacks[s] != net:
                raise ReplayError("net_mismatch", f"seat {s} source net {net}")

    if any(d.forced is None for d in decisions):
        notes.append("forced_unknown")
    return Hand(
        platform=raw.platform, hand_id=raw.hand_id, table_id=raw.table_id, ts=raw.ts,
        sb=raw.sb, bb=raw.bb, button_seat=raw.button_seat, dealt_seats=dealt, positions=positions,
        start_stacks=dict(raw.start_stacks), final_stacks=final_stacks, names=dict(raw.names),
        hero_seat=raw.hero_seat, board=list(raw.board), shown={s: list(c) for s, c in raw.shown.items()},
        decisions=decisions, folded=sorted(folded), showdown_seats=sorted(showdown),
        streets_seen=streets_seen, preflop_aggressor=preflop_aggr, committed=dict(committed), source_file=raw.source_file, notes=notes,
    )
