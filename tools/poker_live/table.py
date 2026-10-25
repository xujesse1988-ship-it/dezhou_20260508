"""牌桌逻辑校验层：按无限注德州规则自己跑一份牌局，逐条核对服务器消息。

HandEngine 只吃规范化后的数值（服务器适配层负责把各家 JSON 转过来），自己维护栈 / 本街下注 /
底池 / 该谁行动 / 最小加注幅度 / 谁还有加注权，服务器给的每个数字都和自己算的比；任何一处对不上
→ 抛 HandInvalid（调用方把这手作废、等下一手）。校验通过的状态直接给出 hero 决策要的全部输入
（合法区间、动作历史、开局真栈、发牌座），不再做推断。

座位环序 = 座位号升序（button → SB → BB → …）。至少 3 人发牌（两人局盲注 / 行动序约定不同，
advisor 只按 OpenPoker 的 HU 约定建模，这里直接不支持）。

桌子座位数 k 可以小于 solver 座位数 n_seats（同 poker_manager driver 的 table_fits_model）：服务端
座位号须正好是 0..k-1，物理座 i = solver 座 i，solver 座 k..n_seats-1 当空座（不发牌，advisor 按
dealt_seats 当幻影座）。空座跳过后的环序和物理桌一致，所以不用重排座位号。
"""

STREETS = ("preflop", "flop", "turn", "river")
_NEW_CARDS = {"flop": 3, "turn": 1, "river": 1}
_RANKS = set("23456789TJQKA")
_SUITS = set("cdhs")
MIN_DEALT = 3


class HandInvalid(Exception):
    """服务器消息与规则 / 自算状态不一致，这手无法继续推进。str(e) = 原因（短标签:细节）。"""


def _is_card(c):
    return isinstance(c, str) and len(c) == 2 and c[0] in _RANKS and c[1] in _SUITS


def _int(v, what):
    """服务器数值归一成 int（JSON 可能给 2000.0）；缺失 / 非整数 → HandInvalid。"""
    if isinstance(v, bool) or not isinstance(v, (int, float)):
        raise HandInvalid(f"missing_field:{what}")
    if isinstance(v, float):
        if not v.is_integer():
            raise HandInvalid(f"non_integer:{what}={v}")
        v = int(v)
    return v


class HandEngine:
    def __init__(self, hand_id, button, small_blind, big_blind, hero_seat, n_seats):
        self.hand_id = hand_id
        self.button = _int(button, "dealer_seat")
        self.sb = _int(small_blind, "small_blind")
        self.bb = _int(big_blind, "big_blind")
        self.hero = hero_seat
        self.n_seats = n_seats
        if not (0 <= self.button < n_seats):
            raise HandInvalid(f"bad_button:{self.button}")
        if not (0 < self.sb <= self.bb):
            raise HandInvalid(f"bad_blinds:{self.sb}/{self.bb}")
        self.table_seats = None  # 本手桌子座位数（首个快照定，之后不许变）
        self.started = False    # 已用首个快照建好开局状态
        self.finished = False   # 已收到 hand_result
        self.street = "preflop"
        self.board = []
        self.hole = None
        self.dealt = []         # 发牌座（升序 = 环序）
        self.sb_seat = self.bb_seat = None
        self.start_stack = {}   # 开局栈（含盲注）
        self.stack = {}         # 当前剩余栈
        self.bet = {}           # 本街已投入
        self.folded = set()
        self.pot = 0
        self.max_bet = 0
        self.min_raise_size = self.bb  # 本街最近一次完整加注的幅度（最小再加注幅度）
        self.need_act = set()   # 本轮还欠一次行动的座
        self.acted_since_full = set()  # 最近一次完整加注后已行动过的座（只面对不完整全下加注时无加注权）
        self.to_act = None
        self.actions = []       # advisor 格式 [{seat, action, to?}]
        self.action_streets = []
        # 与 actions 一一对应：投入 / 动作前的跟注额（不截到栈）/ 动作前的栈（observe 金额、hh 日志用）
        self.action_amounts = []
        self.names = {}

    # ---- 查询 ----
    def _ring_after(self, seat):
        """从 seat 的下一个发牌座起按环序列出全部发牌座。"""
        i = self.dealt.index(seat) if seat in self.dealt else -1
        k = len(self.dealt)
        return [self.dealt[(i + 1 + j) % k] for j in range(k)]

    def _next_actor(self, after):
        for s in self._ring_after(after):
            if s in self.need_act:
                return s
        return None

    def _live(self):
        """未弃牌且还有筹码（能行动）的座。"""
        return [s for s in self.dealt if s not in self.folded and self.stack[s] > 0]

    def _in_pot(self):
        return [s for s in self.dealt if s not in self.folded]

    def turn_key(self):
        """一个行动点的标识：同一手、同样多的已发生动作 = 同一个决策点。"""
        return (self.hand_id, len(self.actions))

    def hero_turn(self):
        """轮到 hero 且信息齐 → 合法区间 dict（advisor ValidActions，金额都是本街 to 额）；否则 None。"""
        h = self.hero
        if not self.started or self.finished or self.to_act != h or not self.hole:
            return None
        to_call = self.max_bet - self.bet[h]
        stack = self.stack[h]
        opp_live = any(s != h for s in self._live())
        can_raise = stack > to_call and opp_live and h not in self.acted_since_full
        min_to = max_to = None
        if can_raise:
            max_to = self.bet[h] + stack
            min_to = min(self.max_bet + max(self.min_raise_size, self.bb), max_to)
        return {"can_check": to_call == 0, "can_call": to_call > 0, "can_raise": can_raise,
                "min_raise": min_to, "max_raise": max_to}

    def to_call(self, seat):
        return self.max_bet - self.bet.get(seat, 0)

    def hand_start_stacks(self, placeholder):
        """下标 = 座位号的开局真栈；非发牌座填 placeholder（advisor 按 dealt_seats 忽略）。"""
        return [self.start_stack.get(s, placeholder) for s in range(self.n_seats)]

    # ---- 事件 ----
    def apply_hole(self, cards):
        if (not isinstance(cards, list) or len(cards) != 2 or not all(map(_is_card, cards))
                or cards[0] == cards[1]):
            raise HandInvalid(f"bad_hole:{cards}")
        if self.hole is not None and self.hole != cards:
            raise HandInvalid(f"hole_changed:{self.hole}->{cards}")
        if set(cards) & set(self.board):
            raise HandInvalid("hole_on_board")
        self.hole = list(cards)

    def apply_snapshot(self, snap):
        """snap = {street, board, pot, actor, seats: {seat: {stack, bet, in_hand, folded, name}}, n_seats}。
        首个快照建开局状态（发牌座 / 盲注 / 开局栈），之后每个快照全量比对。"""
        if self.finished:
            return
        k = snap.get("n_seats")
        seats = snap["seats"]
        if self.table_seats is None:
            if not (isinstance(k, int) and MIN_DEALT <= k <= self.n_seats):
                raise HandInvalid(f"seat_count:{k}_not_in_{MIN_DEALT}..{self.n_seats}")
            if set(seats) != set(range(k)):
                raise HandInvalid(f"seat_numbers:{sorted(seats)}")
            self.table_seats = k
        elif k != self.table_seats or set(seats) != set(range(k)):
            raise HandInvalid(f"seat_count_changed:{self.table_seats}->{k}")
        for s, info in seats.items():
            if info.get("name") is not None:
                self.names[s] = info["name"]
        if not self.started:
            self._init_from_snapshot(snap)
        if snap.get("street") != self.street:
            raise HandInvalid(f"snapshot_street:{snap.get('street')}!={self.street}")
        if list(snap.get("board") or []) != self.board:
            raise HandInvalid(f"snapshot_board:{snap.get('board')}!={self.board}")
        if _int(snap.get("pot"), "pot") != self.pot:
            raise HandInvalid(f"snapshot_pot:{snap.get('pot')}!={self.pot}")
        for s, info in seats.items():
            if s not in self.dealt:
                if info.get("in_hand"):
                    raise HandInvalid(f"snapshot_extra_seat:{s}")
                continue
            if not info.get("in_hand"):
                raise HandInvalid(f"snapshot_seat_left:{s}")
            if _int(info.get("stack"), f"stack[{s}]") != self.stack[s]:
                raise HandInvalid(f"snapshot_stack[{s}]:{info.get('stack')}!={self.stack[s]}")
            if _int(info.get("bet"), f"bet[{s}]") != self.bet[s]:
                raise HandInvalid(f"snapshot_bet[{s}]:{info.get('bet')}!={self.bet[s]}")
            if bool(info.get("folded")) != (s in self.folded):
                raise HandInvalid(f"snapshot_folded[{s}]:{info.get('folded')}")
        if any(s not in seats for s in self.dealt):
            raise HandInvalid("snapshot_missing_seat")
        if snap.get("actor") != self.to_act:
            raise HandInvalid(f"snapshot_actor:{snap.get('actor')}!={self.to_act}")

    def _init_from_snapshot(self, snap):
        if snap.get("street") != "preflop" or snap.get("board"):
            raise HandInvalid("first_snapshot_not_preflop")
        seats = snap["seats"]
        dealt = sorted(s for s, info in seats.items() if info.get("in_hand"))
        if len(dealt) < MIN_DEALT:
            raise HandInvalid(f"short_handed:{len(dealt)}")
        if self.button not in dealt:
            raise HandInvalid(f"button_not_dealt:{self.button}")
        if self.hero not in dealt:
            raise HandInvalid("hero_not_dealt")
        self.dealt = dealt
        ring = self._ring_after(self.button)  # [SB, BB, …, button]
        self.sb_seat, self.bb_seat = ring[0], ring[1]
        for s in dealt:
            info = seats[s]
            stack = _int(info.get("stack"), f"stack[{s}]")
            bet = _int(info.get("bet"), f"bet[{s}]")
            want = self.sb if s == self.sb_seat else self.bb if s == self.bb_seat else 0
            if bet != want:
                raise HandInvalid(f"blinds[{s}]:bet={bet},want={want}")
            if info.get("folded"):
                raise HandInvalid(f"folded_at_start:{s}")
            if want and stack == 0:
                # 盲注就全下：min-call / 加注口径各家规则不一，不建模。
                raise HandInvalid(f"blind_all_in:{s}")
            self.stack[s] = stack
            self.bet[s] = bet
            self.start_stack[s] = stack + bet
        self.pot = self.sb + self.bb
        self.max_bet = self.bb
        self.need_act = {s for s in dealt if self.stack[s] > 0}
        self.to_act = self._next_actor(self.bb_seat)
        self.started = True

    def apply_action(self, a):
        """a = {seat, action, street, delta, to, stack_before, stack_after, pot_before, pot_after, name}。
        to = raise/bet 的本街总到额（None = 服务器没给）。动作归类以投入增量为准，action 字段只核对。"""
        if not self.started:
            raise HandInvalid("action_before_snapshot")
        if self.finished:
            raise HandInvalid("action_after_result")
        seat = a.get("seat")
        if seat != self.to_act:
            raise HandInvalid(f"out_of_turn:seat={seat},expected={self.to_act}")
        if a.get("street") != self.street:
            raise HandInvalid(f"action_street:{a.get('street')}!={self.street}")
        stack = self.stack[seat]
        if _int(a.get("stack_before"), "stack_before") != stack:
            raise HandInvalid(f"stack_before[{seat}]:{a.get('stack_before')}!={stack}")
        if _int(a.get("pot_before"), "pot_before") != self.pot:
            raise HandInvalid(f"pot_before:{a.get('pot_before')}!={self.pot}")
        delta = _int(a.get("delta"), "contribution_delta")
        if not (0 <= delta <= stack):
            raise HandInvalid(f"bad_delta:{delta},stack={stack}")
        if _int(a.get("stack_after"), "stack") != stack - delta:
            raise HandInvalid(f"stack_after[{seat}]:{a.get('stack_after')}!={stack - delta}")
        if _int(a.get("pot_after"), "pot") != self.pot + delta:
            raise HandInvalid(f"pot_after:{a.get('pot_after')}!={self.pot + delta}")

        kind = (a.get("action") or "").lower()
        to_call = self.max_bet - self.bet[seat]
        new = self.bet[seat] + delta
        all_in = delta == stack and delta > 0
        if kind == "fold":
            if delta != 0:
                raise HandInvalid(f"fold_with_chips:{delta}")
            label = "fold"
        elif kind == "check":
            if delta != 0 or to_call != 0:
                raise HandInvalid(f"bad_check:delta={delta},to_call={to_call}")
            label = "check"
        elif kind == "call" or (kind in ("all_in", "allin") and new <= self.max_bet):
            if to_call <= 0 or delta != min(to_call, stack):
                raise HandInvalid(f"bad_call:delta={delta},to_call={to_call},stack={stack}")
            label = "call"
        elif kind in ("raise", "bet", "all_in", "allin"):
            if kind in ("all_in", "allin") and not all_in:
                raise HandInvalid(f"all_in_not_all:{delta},stack={stack}")
            to = a.get("to")
            if to is not None and _int(to, "amount") != new:
                raise HandInvalid(f"raise_to:{to}!={new}")
            if new <= self.max_bet:
                raise HandInvalid(f"raise_not_above:{new}<={self.max_bet}")
            if seat in self.acted_since_full:
                raise HandInvalid(f"raise_not_reopened:{seat}")
            size = new - self.max_bet
            if size < max(self.min_raise_size, self.bb) and not all_in:
                raise HandInvalid(f"raise_too_small:{size}<{max(self.min_raise_size, self.bb)}")
            label = "all_in" if all_in else "raise"
        else:
            raise HandInvalid(f"unknown_action:{kind}")

        self.stack[seat] = stack - delta
        self.bet[seat] = new
        self.pot += delta
        if label == "fold":
            self.folded.add(seat)
        if new > self.max_bet:
            size = new - self.max_bet
            if size >= max(self.min_raise_size, self.bb):
                self.min_raise_size = size
                self.acted_since_full = {seat}
            else:
                self.acted_since_full.add(seat)  # 不完整全下加注：不给已行动者重开加注权
            self.max_bet = new
            self.need_act = {s for s in self._live() if s != seat}
        else:
            self.acted_since_full.add(seat)
            self.need_act.discard(seat)
        if len(self._in_pot()) <= 1:
            self.need_act = set()
        if a.get("name") is not None:
            self.names[seat] = a["name"]
        self.actions.append({"seat": seat, "action": label,
                             **({"to": new} if label in ("raise", "all_in") else {})})
        self.action_streets.append(self.street)
        self.action_amounts.append({"contribution_delta": delta, "to_call_before": to_call,
                                    "stack_before": stack})
        self.to_act = self._next_actor(seat)

    def apply_board(self, street, cards):
        """cards = 本街新发的牌，或到本街为止的完整公共牌（两种写法都认）。"""
        if not self.started:
            raise HandInvalid("board_before_snapshot")
        if self.finished:
            raise HandInvalid("board_after_result")
        if self.to_act is not None or self.need_act:
            raise HandInvalid(f"board_before_round_closed:to_act={self.to_act}")
        if len(self._in_pot()) < 2:
            raise HandInvalid("board_after_hand_over")
        i = STREETS.index(self.street)
        if i + 1 >= len(STREETS) or street != STREETS[i + 1]:
            raise HandInvalid(f"bad_street:{self.street}->{street}")
        if not isinstance(cards, list):
            raise HandInvalid("bad_board")
        n_new = _NEW_CARDS[street]
        if len(cards) == len(self.board) + n_new and cards[:len(self.board)] == self.board:
            new = cards[len(self.board):]
        elif len(cards) == n_new:
            new = cards
        else:
            raise HandInvalid(f"board_count:{street}:{cards}")
        seen = set(self.board) | set(self.hole or [])
        if not all(map(_is_card, new)) or len(set(new)) != len(new) or seen & set(new):
            raise HandInvalid(f"bad_board_cards:{cards}")
        self.street = street
        self.board = self.board + list(new)
        self.bet = {s: 0 for s in self.dealt}
        self.max_bet = 0
        self.min_raise_size = self.bb
        self.acted_since_full = set()
        live = self._live()
        self.need_act = set(live) if len(live) >= 2 else set()
        self.to_act = self._next_actor(self.button)

    def apply_result(self, final_stacks):
        """hand_result：标记结束；返回不一致说明列表（结算不影响决策，只记日志）。"""
        self.finished = True
        notes = []
        if not self.started:
            return ["result_before_snapshot"]
        if self.need_act or self.to_act is not None:
            notes.append(f"result_before_round_closed:to_act={self.to_act}")
        elif len(self._in_pot()) >= 2 and len(self.board) != 5:
            notes.append(f"result_board_incomplete:{len(self.board)}")
        try:
            final = {int(k): _int(v, f"final[{k}]") for k, v in (final_stacks or {}).items()}
        except (ValueError, HandInvalid) as e:
            return notes + [f"result_bad_final_stacks:{e}"]
        if sorted(final) != self.dealt:
            notes.append(f"result_seats:{sorted(final)}!={self.dealt}")
        elif sum(final.values()) != sum(self.start_stack.values()):
            notes.append(f"result_not_conserved:{sum(final.values())}!={sum(self.start_stack.values())}")
        return notes

    def observe_record(self, amounts=False):
        """叠加剥削 observe（同 openpoker HandState.observe_record 格式）。amounts=True（PM-17
        --exploit-4bet）时每个动作另带 contribution_delta / to_call_before，advisor 据此区分真加注和
        只够跟注的全下（同 poker_manager driver）。"""
        acts = []
        for a, st, amt in zip(self.actions, self.action_streets, self.action_amounts):
            rec = {"seat": a["seat"], "street": st, "action": a["action"]}
            if amounts:
                rec["contribution_delta"] = amt["contribution_delta"]
                rec["to_call_before"] = amt["to_call_before"]
            acts.append(rec)
        return {"observe": 1, "names": self.names, "actions": acts}

    def hh_record(self):
        """校验通过的整手牌局（hh 日志一行，opponent_analysis 的 wopo 适配器读）。只在本手打完且
        没作废时调用：开局栈 / 动作 / 投入都是本引擎逐条核对过的。"""
        return {"hand_id": self.hand_id, "button_seat": self.button, "small_blind": self.sb,
                "big_blind": self.bb, "my_seat": self.hero, "dealt_seats": list(self.dealt),
                "initial_stacks": {str(s): v for s, v in self.start_stack.items()},
                "names": {str(s): n for s, n in self.names.items()},
                "hole": list(self.hole or []), "board": list(self.board),
                "actions": [{**a, "street": st, **amt} for a, st, amt
                            in zip(self.actions, self.action_streets, self.action_amounts)]}
