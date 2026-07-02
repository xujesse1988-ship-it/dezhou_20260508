#!/usr/bin/env python3
"""M0 数据集构建：live raw 事件流 + Slumbot 手牌日志 → range 预测监督样本。

产出（--out 目录，默认 artifacts/range_ml/）：
  live_opp.jsonl   对手亮牌样本（有截断偏差：仅摊牌亮牌者有标签）——主训练/评估集
  live_hero.jsonl  hero 自标注样本（每手都知道自己的牌，含 fold 手，零截断）
  slumbot.jsonl    Slumbot HU 全知样本（每手都有 bot 底牌，含 fold 结束手，零截断）
  audit.md         数据审计报告
  meta.json        构建参数与计数

样本 schema（每行一个 JSON）：
  src/run/hid/ts/tbl        来源、run 标签（时间切分用）、hand id、时间戳、table id
  tgt/tseat/label/y         目标玩家名、座位、底牌两张、canonical 169 类索引
  hseat/hhole               hero 座位与底牌（hero 不在手 → null；card-removal 用）
  btn/sb/bb/sbamt/bbamt/ns  按钮/盲注座与额度、座位数
  inh/names/stacks          在手座位、座位→名字、首个 table_state 的座位栈（盲注已扣）
  board                     最终公共牌
  ev                        统一事件流：{st(0-3), s(seat), a(act), c(本街累计投入),
                            p(pot_after), tc(to_call_before), hb(动作前本街最高投入)}
  npr/pagg                  翻前加注次数 / 翻前最后加注者座位（无则 null）
  prof                      目标玩家开手前画像快照：life（累计，observe_hand 同口径）、
                            w50（最近 50 手窗口）、shows（最近 ≤20 次亮牌的 169 类）
所有金额为筹码绝对值；ev 的因果序 = 服务端 table_seq 序。
"""

import argparse
import glob
import json
import os
import re
import sys
from collections import Counter, defaultdict, deque

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from range_ml_common import canonical_index, card_id  # noqa: E402

STREET_IDX = {"preflop": 0, "flop": 1, "turn": 2, "river": 3}
AGGR_ACTS = ("bet", "raise", "allin", "all_in")
VPIP_ACTS = ("call", "bet", "raise", "allin", "all_in")

_TYPE_RE = re.compile(r'"type": "([a-z_]+)"')
_HID_RE = re.compile(r'"hand_id": "([0-9a-f-]+)"')
_STREAM_STATE = '"stream": "state"'


# ---------------------------------------------------------------------------
# 对手画像（因果重放：快照永远取自「该手开始之前」的观测）
# ---------------------------------------------------------------------------

LIFE_KEYS = ("hands", "vpip_opp", "vpip_cnt", "pfr_opp", "pfr_cnt", "aggr_actions", "call_actions")


class OppProfile:
    def __init__(self, window=50, shows_cap=20):
        self.life = dict.fromkeys(LIFE_KEYS, 0)
        self.window = deque(maxlen=window)  # 每手一个 delta dict
        self.wsum = dict.fromkeys(LIFE_KEYS, 0)
        self.shows = deque(maxlen=shows_cap)  # 亮牌 169 类索引
        self.n_shows = 0

    def snapshot(self):
        return {
            "life": dict(self.life),
            "w50": dict(self.wsum, hands=len(self.window)),
            "shows": list(self.shows),
            "n_shows": self.n_shows,
        }

    def add_hand(self, delta, shown_y=None):
        for k in LIFE_KEYS:
            self.life[k] += delta.get(k, 0)
        if len(self.window) == self.window.maxlen:
            old = self.window[0]
            for k in LIFE_KEYS:
                self.wsum[k] -= old.get(k, 0)
        self.window.append(delta)
        for k in LIFE_KEYS:
            self.wsum[k] += delta.get(k, 0)
        if shown_y is not None:
            self.shows.append(shown_y)
            self.n_shows += 1


def profile_deltas(events, in_names, sb_seat, bb_seat):
    """一手 → 每名玩家的画像增量（heuristic9_advisor feed_observe / opponent.rs
    observe_hand 同口径：hands 每手 +1；翻前首个非盲位强制动作记 vpip/pfr 机会；
    翻后 bet/raise → aggr、call → call）。"""
    deltas = {nm: dict.fromkeys(LIFE_KEYS, 0) for nm in in_names.values()}
    for nm in deltas:
        deltas[nm]["hands"] = 1
    counted_pre = set()
    for e in events:
        nm = in_names.get(e["s"])
        if nm is None:
            continue
        d = deltas[nm]
        act = e["a"]
        if e["st"] == 0:
            forced = e["s"] in (sb_seat, bb_seat)
            if not forced and e["s"] not in counted_pre:
                d["vpip_opp"] += 1
                d["pfr_opp"] += 1
                if act in VPIP_ACTS:
                    d["vpip_cnt"] += 1
                    if act != "call":
                        d["pfr_cnt"] += 1
                counted_pre.add(e["s"])
        else:
            if act in AGGR_ACTS:
                d["aggr_actions"] += 1
            elif act == "call":
                d["call_actions"] += 1
    return deltas


# ---------------------------------------------------------------------------
# live raw 解析
# ---------------------------------------------------------------------------


class HandAcc:
    __slots__ = ("start", "state", "hole", "actions", "result", "first_ts", "board")

    def __init__(self):
        self.start = None
        self.state = None
        self.hole = None
        self.actions = []
        self.result = None
        self.first_ts = None
        self.board = []


def parse_live_run(path, run_tag, profiles, writers, audit):
    pending = {}
    order = []  # 到达序的 hand_id（result 到达即 finalize，天然按时间）
    with open(path) as f:
        for line in f:
            m = _TYPE_RE.search(line)
            if not m:
                continue
            typ = m.group(1)
            if typ == "table_state":
                if _STREAM_STATE not in line:
                    continue
                hm = _HID_RE.search(line)
                if not hm:
                    continue
                hid = hm.group(1)
                acc = pending.get(hid)
                if acc is None or acc.state is not None:
                    continue  # 只要每手第一个 state
                try:
                    acc.state = json.loads(line)
                except json.JSONDecodeError:
                    audit["bad_json"] += 1
                continue
            if typ not in ("hand_start", "player_action", "hand_result", "hole_cards", "community_cards"):
                continue
            try:
                rec = json.loads(line)
            except json.JSONDecodeError:
                audit["bad_json"] += 1
                continue
            hid = rec.get("hand_id")
            if not hid:
                continue
            acc = pending.get(hid)
            if acc is None:
                acc = pending[hid] = HandAcc()
                order.append(hid)
                acc.first_ts = rec.get("ts")
            if typ == "hand_start":
                acc.start = rec
            elif typ == "hole_cards":
                acc.hole = rec.get("cards")
            elif typ == "player_action":
                acc.actions.append(rec)
            elif typ == "community_cards":
                cards = rec.get("cards") or []
                if len(cards) > len(acc.board):
                    acc.board = cards  # 事件为全量累积（flop 3 / turn 4 / river 5）
            elif typ == "hand_result":
                acc.result = rec
                finalize_live_hand(hid, acc, run_tag, profiles, writers, audit)
                pending.pop(hid, None)
            # 防泄漏：断线残手太多时丢最老的
            if len(pending) > 300:
                oldest = order.pop(0)
                if oldest in pending:
                    pending.pop(oldest)
                    audit["dropped_incomplete"] += 1
    audit["dangling_hands"] += len(pending)


def derive_blind_seats(dealer, in_hand):
    """SB/BB：在手座位按钮后顺时针（跳过空位）；HU 按钮即 SB。"""
    if dealer is None or not in_hand:
        return None, None
    ring = sorted(in_hand)
    if dealer in ring:
        i = ring.index(dealer)
        rot = ring[i:] + ring[:i]
    else:  # 按钮位玩家不在手（罕见），从下一个在手位起
        rot = sorted(in_hand, key=lambda s: ((s - dealer) % 32, s))
        rot = [None] + rot
    if len(ring) == 2:
        return rot[0], rot[1]
    if len(rot) >= 3:
        return rot[1], rot[2]
    return None, None


def finalize_live_hand(hid, acc, run_tag, profiles, writers, audit):
    audit["hands"] += 1
    # raw 流的 hand_result 平铺；processed 包装在 "result" 下——两者兼容。
    res = acc.result.get("result") or acc.result
    board_strs = acc.result.get("board") or acc.board or []
    my_seat = acc.result.get("my_seat")
    if acc.state is None:
        audit["no_table_state"] += 1
        if res.get("shown_cards"):
            audit["skipped_shown_hands"] += 1
        return
    st_rec = acc.state
    seats_info = st_rec.get("seats") or []
    in_hand = [s["seat"] for s in seats_info if s.get("in_hand")]
    names = {s["seat"]: s.get("name") for s in seats_info if s.get("name")}
    stacks = {s["seat"]: s.get("stack") for s in seats_info}
    dealer = (acc.start or {}).get("dealer_seat", st_rec.get("dealer_seat"))
    blinds = (acc.start or {}).get("blinds") or {}
    sb_amt = blinds.get("small_blind", st_rec.get("small_blind", 0)) or 0
    bb_amt = blinds.get("big_blind", st_rec.get("big_blind", 0)) or 0
    sb_seat, bb_seat = derive_blind_seats(dealer, in_hand)
    if sb_seat is None or bb_amt == 0:
        audit["no_blind_seats"] += 1
        return
    hero_hole = acc.hole or ((st_rec.get("hero") or {}).get("hole_cards"))
    hero_seat = (st_rec.get("hero") or {}).get("seat", my_seat)

    # 统一事件流（按 table_seq 因果序），本街累计投入含盲注种子。
    # 街标签：live player_action 的 street 字段有竞态错标（异步发牌后送达的收尾跟注
    # 会被标成下一街）；hand_result.actions 合并列表的街标权威——按索引对齐后优先用它。
    acc.actions.sort(key=lambda r: r.get("table_seq", 0))
    consolidated = res.get("actions") or []
    use_consolidated = len(consolidated) == len(acc.actions) and all(
        ca.get("seat") == pa.get("seat") for ca, pa in zip(consolidated, acc.actions)
    )
    if consolidated and not use_consolidated:
        audit["action_len_mismatch"] += 1
    commits = defaultdict(int)  # (street, seat) → 本街累计
    commits[(0, sb_seat)] = sb_amt
    commits[(0, bb_seat)] = bb_amt
    street_high = {0: bb_amt}
    events = []
    last_st = 0
    pot_ok = True
    for i, pa in enumerate(acc.actions):
        if use_consolidated:
            st = STREET_IDX.get(consolidated[i].get("street"), 0)
        else:
            st = STREET_IDX.get(pa.get("street"), 0)
        if st < last_st:
            audit["street_order_anom"] += 1
        last_st = max(last_st, st)
        seat = pa.get("seat")
        act = (pa.get("action") or "").lower()
        delta = pa.get("contribution_delta") or 0
        hb = street_high.get(st, 0)
        c_after = commits[(st, seat)] + delta
        commits[(st, seat)] = c_after
        street_high[st] = max(hb, c_after)
        pot_after = pa.get("pot_after")
        if pot_after is None:
            pot_after = (events[-1]["p"] if events else sb_amt + bb_amt) + delta
        events.append({
            "st": st, "s": seat, "a": act, "c": c_after,
            "p": pot_after, "tc": pa.get("to_call_before", 0) or 0, "hb": hb,
        })
        nm = pa.get("name")
        if nm and seat not in names:
            names[seat] = nm
        if seat not in in_hand:
            in_hand.append(seat)
    # pot 一致性抽检：事件末 pot 必须 ≥ 终局 total_pot（未被跟的下注退回属正常，
    # 事件 pot 偏大可解释；偏小说明丢了投入 → 异常）。
    total_pot = res.get("total_pot") or acc.result.get("pot")
    if events and total_pot and events[-1]["p"] < total_pot - max(2, bb_amt):
        pot_ok = False
        audit["pot_mismatch"] += 1

    npr = sum(1 for e in events if e["st"] == 0 and e["a"] in AGGR_ACTS)
    pagg = next((e["s"] for e in reversed(events) if e["st"] == 0 and e["a"] in AGGR_ACTS), None)

    board_len = len(board_strs)
    audit["board_len"][board_len] += 1
    shown = res.get("shown_cards") or {}
    if board_len == 5 and not shown:
        audit["river_no_shown"] += 1

    base = {
        "run": run_tag, "hid": hid, "ts": acc.first_ts, "tbl": st_rec.get("table_id"),
        "hseat": hero_seat, "hhole": hero_hole,
        "btn": dealer, "sb": sb_seat, "bb": bb_seat, "sbamt": sb_amt, "bbamt": bb_amt,
        "ns": len(in_hand), "inh": sorted(in_hand),
        "names": {str(k): v for k, v in names.items()},
        "stacks": {str(k): v for k, v in stacks.items() if v is not None},
        "board": board_strs, "ev": events, "npr": npr, "pagg": pagg,
        "pot_ok": pot_ok,
    }

    # 对手亮牌样本（profile 快照在更新之前取 → 严格因果）。
    for seat_s, cards in shown.items():
        seat = int(seat_s)
        if hero_seat is not None and seat == hero_seat:
            continue
        nm = names.get(seat)
        if nm is None:
            audit["shown_no_name"] += 1
            continue
        if not cards or len(cards) != 2:
            audit["shown_bad_cards"] += 1
            continue
        y = canonical_index(card_id(cards[0]), card_id(cards[1]))
        sample = dict(base, src="live_opp", tgt=nm, tseat=seat, label=cards, y=y,
                      prof=profiles[nm].snapshot() if nm in profiles else OppProfile().snapshot())
        writers["live_opp"].write(json.dumps(sample) + "\n")
        audit["live_opp"] += 1
        audit["opp_counter"][nm] += 1
        audit["label_hist"][y] += 1
    # hero 自标注样本（含 fold 手，零截断）。
    if hero_seat is not None and hero_hole and len(hero_hole) == 2 and hero_seat in in_hand:
        nm = names.get(hero_seat, "hero")
        y = canonical_index(card_id(hero_hole[0]), card_id(hero_hole[1]))
        sample = dict(base, src="live_hero", tgt=nm, tseat=hero_seat, label=hero_hole, y=y,
                      prof=profiles[nm].snapshot() if nm in profiles else OppProfile().snapshot())
        writers["live_hero"].write(json.dumps(sample) + "\n")
        audit["live_hero"] += 1
    if shown:
        audit["shown_hands"] += 1

    # 画像更新（严格在快照之后）。
    in_names = {s: n for s, n in names.items() if s in in_hand and n}
    deltas = profile_deltas(events, in_names, sb_seat, bb_seat)
    shown_by_name = {}
    for seat_s, cards in shown.items():
        nm = names.get(int(seat_s))
        if nm and cards and len(cards) == 2:
            shown_by_name[nm] = canonical_index(card_id(cards[0]), card_id(cards[1]))
    for nm, d in deltas.items():
        profiles.setdefault(nm, OppProfile()).add_hand(d, shown_by_name.get(nm))


# ---------------------------------------------------------------------------
# Slumbot 解析（ParseAction 语义：b<N> = 本街累计加注到 N；pos1=SB 翻前先动）
# ---------------------------------------------------------------------------

SLUM_SB, SLUM_BB, SLUM_STACK = 50, 100, 20000


def parse_slumbot_hand(action):
    """动作串 → 统一事件流。座位号：0=BB，1=SB(button)。返回 (events, ended_by_fold, folder)。"""
    st = 0
    pos = 1
    street_bet_to = SLUM_BB
    commits = {0: SLUM_BB, 1: SLUM_SB}
    totals = {0: SLUM_BB, 1: SLUM_SB}
    pot = SLUM_SB + SLUM_BB
    ends = False
    events = []
    i, sz = 0, len(action)
    ended_by_fold, folder = False, None

    def advance_street():
        nonlocal st, pos, street_bet_to, commits, ends
        st += 1
        pos = 0
        street_bet_to = 0
        commits = {0: 0, 1: 0}
        ends = False

    while i < sz:
        ch = action[i]
        i += 1
        if ch == "/":
            continue
        actor = pos
        hb = max(commits.values())
        tc = max(0, street_bet_to - commits[actor])
        if ch == "k":
            events.append({"st": st, "s": actor, "a": "check", "c": commits[actor], "p": pot, "tc": 0, "hb": hb})
            if ends:
                if st < 3:
                    advance_street()
                else:
                    pos = -1
            else:
                pos = 1 - pos
                ends = True
        elif ch == "c":
            afford = SLUM_STACK - (totals[actor] - commits[actor])
            new_c = min(street_bet_to, afford)
            add = new_c - commits[actor]
            pot += add
            totals[actor] += add
            commits[actor] = new_c
            events.append({"st": st, "s": actor, "a": "call", "c": new_c, "p": pot, "tc": tc, "hb": hb})
            if totals[actor] >= SLUM_STACK or totals[1 - actor] >= SLUM_STACK:
                break  # all-in call：后续只有 runout
            if ends:
                if st < 3:
                    advance_street()
                else:
                    pos = -1
            else:
                pos = 1 - pos
                ends = True
            street_bet_to = max(commits.values()) if st == 0 else street_bet_to
        elif ch == "f":
            events.append({"st": st, "s": actor, "a": "fold", "c": commits[actor], "p": pot, "tc": tc, "hb": hb})
            ended_by_fold, folder = True, actor
            break
        elif ch == "b":
            j = i
            while i < sz and action[i].isdigit():
                i += 1
            n = int(action[j:i])
            add = n - commits[actor]
            pot += add
            totals[actor] += add
            act = "bet" if hb == 0 else "raise"
            commits[actor] = n
            events.append({"st": st, "s": actor, "a": act, "c": n, "p": pot, "tc": tc, "hb": hb})
            street_bet_to = n
            pos = 1 - pos
            ends = True
        else:
            raise ValueError(f"bad action char {ch!r} in {action!r}")
    return events, ended_by_fold, folder


def parse_slumbot_file(path, profiles, writers, audit):
    with open(path) as f:
        for line in f:
            try:
                rec = json.loads(line)
            except json.JSONDecodeError:
                audit["bad_json"] += 1
                continue
            if rec.get("type") != "hand":
                continue
            bot_cards = rec.get("bot_hole_cards")
            if not bot_cards or len(bot_cards) != 2:
                audit["slum_no_bot_cards"] += 1
                continue
            try:
                events, ended_by_fold, folder = parse_slumbot_hand(rec.get("action", ""))
            except ValueError:
                audit["slum_parse_fail"] += 1
                continue
            client_pos = rec.get("client_pos")
            if client_pos not in (0, 1):
                audit["slum_bad_pos"] += 1
                continue
            bot_seat = 1 - client_pos
            board = rec.get("board") or []
            hero_hole = rec.get("hole_cards")
            npr = sum(1 for e in events if e["st"] == 0 and e["a"] in AGGR_ACTS)
            pagg = next((e["s"] for e in reversed(events) if e["st"] == 0 and e["a"] in AGGR_ACTS), None)
            y = canonical_index(card_id(bot_cards[0]), card_id(bot_cards[1]))
            nm = "Slumbot"
            sample = {
                "src": "slumbot", "run": "slumbot", "hid": f"slum-{rec.get('hand')}", "ts": None,
                "tbl": "slumbot", "tgt": nm, "tseat": bot_seat, "label": bot_cards, "y": y,
                "hseat": client_pos, "hhole": hero_hole,
                "btn": 1, "sb": 1, "bb": 0, "sbamt": SLUM_SB, "bbamt": SLUM_BB,
                "ns": 2, "inh": [0, 1], "names": {"0": nm if bot_seat == 0 else "hero",
                                                  "1": nm if bot_seat == 1 else "hero"},
                "stacks": {"0": SLUM_STACK, "1": SLUM_STACK},
                "board": board, "ev": events, "npr": npr, "pagg": pagg, "pot_ok": True,
                "fold_end": ended_by_fold, "folder": folder,
                "prof": profiles[nm].snapshot() if nm in profiles else OppProfile().snapshot(),
            }
            writers["slumbot"].write(json.dumps(sample) + "\n")
            audit["slumbot"] += 1
            if ended_by_fold:
                audit["slum_fold_end"] += 1
            in_names = {bot_seat: nm, client_pos: "hero"}
            deltas = profile_deltas(events, in_names, 1, 0)
            shown_y = None if ended_by_fold else y
            for pnm, d in deltas.items():
                profiles.setdefault(pnm, OppProfile()).add_hand(d, shown_y if pnm == nm else None)


# ---------------------------------------------------------------------------
# 主流程 + 审计
# ---------------------------------------------------------------------------


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--out", default="artifacts/range_ml")
    ap.add_argument("--runs", default="auto", help="逗号分隔 run 号（如 24,25），auto=按 h9_live_run*.raw.jsonl 升序全量")
    ap.add_argument("--slumbot", default="auto", help="slumbot jsonl 路径，auto=glob，none=跳过")
    args = ap.parse_args()

    os.makedirs(args.out, exist_ok=True)
    if args.runs == "auto":
        run_files = sorted(glob.glob("h9_live_run*.raw.jsonl"),
                           key=lambda p: int(re.search(r"run(\d+)", p).group(1)))
    else:
        run_files = [f"h9_live_run{r.strip()}.raw.jsonl" for r in args.runs.split(",")]
    slum_files = []
    if args.slumbot == "auto":
        slum_files = sorted(glob.glob("slumbot_hands_*.jsonl"))
    elif args.slumbot != "none":
        slum_files = [args.slumbot]

    audit = defaultdict(int)
    audit["board_len"] = Counter()
    audit["opp_counter"] = Counter()
    audit["label_hist"] = Counter()
    audit["per_run"] = {}

    writers = {k: open(os.path.join(args.out, f"{k}.jsonl"), "w")
               for k in ("live_opp", "live_hero", "slumbot")}
    profiles = {}
    for rf in run_files:
        run_tag = re.search(r"(run\d+)", rf).group(1)
        before = (audit["hands"], audit["live_opp"], audit["live_hero"])
        parse_live_run(rf, run_tag, profiles, writers, audit)
        audit["per_run"][run_tag] = {
            "hands": audit["hands"] - before[0],
            "opp_samples": audit["live_opp"] - before[1],
            "hero_samples": audit["live_hero"] - before[2],
        }
        print(f"[build] {run_tag}: {audit['per_run'][run_tag]}")
    slum_profiles = {}
    for sf in slum_files:
        parse_slumbot_file(sf, slum_profiles, writers, audit)
        print(f"[build] slumbot: {audit['slumbot']} samples ({audit['slum_fold_end']} fold-ended)")
    for w in writers.values():
        w.close()

    write_audit(args, run_files, slum_files, audit)
    meta = {k: v for k, v in audit.items() if isinstance(v, int)}
    meta["per_run"] = audit["per_run"]
    with open(os.path.join(args.out, "meta.json"), "w") as f:
        json.dump(meta, f, indent=1, ensure_ascii=False)
    print(f"[build] done → {args.out}")


def write_audit(args, run_files, slum_files, audit):
    from range_ml_common import class_name

    lines = ["# range ML M0 数据审计", ""]
    lines.append(f"- 输入：{len(run_files)} 个 live run + {len(slum_files)} 个 slumbot 文件")
    lines.append(f"- live 手数 {audit['hands']}，亮牌手数 {audit['shown_hands']}，"
                 f"对手亮牌样本 **{audit['live_opp']}**，hero 样本 {audit['live_hero']}，"
                 f"slumbot 样本 {audit['slumbot']}（其中 fold 结束 {audit['slum_fold_end']}）")
    bl = audit["board_len"]
    lines.append(f"- board 长度分布：{dict(sorted(bl.items()))}；河牌未亮牌 {audit['river_no_shown']} 手（muck/弃牌到底）")
    anoms = {k: audit[k] for k in ("bad_json", "no_table_state", "skipped_shown_hands", "no_blind_seats",
                                   "pot_mismatch", "street_order_anom", "shown_no_name", "shown_bad_cards",
                                   "dropped_incomplete", "dangling_hands", "slum_parse_fail",
                                   "slum_no_bot_cards", "slum_bad_pos") if audit[k]}
    lines.append(f"- 异常计数：{anoms or '无'}")
    lines.append("")
    lines.append("## 每 run")
    lines.append("")
    lines.append("| run | hands | 对手样本 | hero 样本 |")
    lines.append("|---|---|---|---|")
    for r, d in audit["per_run"].items():
        lines.append(f"| {r} | {d['hands']} | {d['opp_samples']} | {d['hero_samples']} |")
    lines.append("")
    lines.append("## 对手样本 Top-20（按亮牌样本数）")
    lines.append("")
    lines.append("| 对手 | 样本 |")
    lines.append("|---|---|")
    for nm, n in audit["opp_counter"].most_common(20):
        lines.append(f"| {nm} | {n} |")
    lines.append("")
    lines.append(f"（共 {len(audit['opp_counter'])} 个不同对手）")
    lines.append("")
    lines.append("## 亮牌 169 类 Top-20")
    lines.append("")
    lines.append("| 类 | 次数 |")
    lines.append("|---|---|")
    for y, n in audit["label_hist"].most_common(20):
        lines.append(f"| {class_name(y)} | {n} |")
    lines.append("")
    with open(os.path.join(args.out, "audit.md"), "w") as f:
        f.write("\n".join(lines))


if __name__ == "__main__":
    main()
