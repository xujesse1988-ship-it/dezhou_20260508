#!/usr/bin/env python3
"""M1 特征化：数据集 jsonl → 序列模型张量（seq_v1.npz）+ B3 每街聚合特征（b3_v1.npz）。

序列侧（docs/opponent_range_ml_design.md §4.1）：
  每样本一条 token 序列 [CTX][PROF] e_1..e_T（动作 token + 发牌 token，board 花色按
  首现顺序同构重标号）。每个 token 位置附带：
    - prior_idx：**因果 m_prior**（手写 B2b tracker 在该前缀上的输出）在去重先验表中
      的下标。逐事件维护：翻前角色随前缀重算（前缀末 == range_baselines.preflop_role），
      目标玩家翻后动作逐个施 B2RangeVec.op；目标 fold 后冻结。
      ⇒ 残差网络第 0 步（残差=0）在街末快照上与 M0 的 B2b 逐样本一致（--parity 验证）。
    - street：token 所属街（alive 掩码查表用）。
  alive[sample, street, 169]：观察者视角死牌（board 前缀 + 观察者底牌）下的存活 combo
  数。观察者 = hero，但 live_hero 样本目标就是 hero → 死牌只含 board（不能把标签自己的
  牌当死牌）。

B3 侧（§3 方案 b / §7 基线 B3）：每（样本, 街末快照）一行聚合特征——角色/位置/
尺寸/质地/画像统计，不含序列结构——供 LightGBM 169 类多分类。

切分纪律：live_opp 按 run 时间切 train/val/test；live_hero 与 slumbot **只进 train**
（防同手事件流跨切分泄漏）。源权重 §4.3：live_opp 1.0 / hero 0.3 / slumbot 0.1。

用法：
  .venv-rangeml/bin/python tools/range_featurize.py            # 构建两个 npz
  .venv-rangeml/bin/python tools/range_featurize.py --parity   # 构建后做 B2b 对拍
"""

import argparse
import json
import math
import os
import sys
import time

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from range_ml_common import (  # noqa: E402
    CLASS_COMBOS, build_strength_table, card_id, class_axes, combo_weight,
)
from range_baselines import B2_PARAMS, B2RangeVec, HandStates, preflop_role  # noqa: E402

AGGR = ("bet", "raise", "allin", "all_in")
STREET_NEED = {0: 0, 1: 3, 2: 4, 3: 5}
STREET_NAME = {0: "preflop", 1: "flop", 2: "turn", 3: "river"}
MAXTOK = 96
SRC_ID = {"live_opp": 0, "live_hero": 1, "slumbot": 2}
SRC_W = {"live_opp": 1.0, "live_hero": 0.3, "slumbot": 0.1}
SPLIT_ID = {"train": 0, "val": 1, "test": 2}

# --- 169 类 alive 计数的向量化基表 -----------------------------------------
_CA = np.array([a for cs in CLASS_COMBOS for a, _ in cs], dtype=np.int8)
_CB = np.array([b for cs in CLASS_COMBOS for _, b in cs], dtype=np.int8)
_CIDX = np.array([i for i, cs in enumerate(CLASS_COMBOS) for _ in cs], dtype=np.int16)


def alive_np(dead_cards):
    dead = np.zeros(52, dtype=bool)
    for c in dead_cards:
        dead[c] = True
    mask = ~dead[_CA] & ~dead[_CB]
    return np.bincount(_CIDX[mask], minlength=169).astype(np.uint8)


# --- token 特征布局（flat 向量，无关组置零）---------------------------------
_IDX = {}
_off = 0
for _name, _n in (("ttype", 4), ("street", 4), ("ctx", 10), ("prof", 15),
                  ("act", 20), ("board", 19)):
    _IDX[_name] = _off
    _off += _n
FDIM = _off
TT_CTX, TT_PROF, TT_ACT, TT_BOARD = 0, 1, 2, 3
ACT_TYPES = {"fold": 0, "check": 1, "call": 2, "bet": 3, "raise": 4, "allin": 5, "all_in": 5}
POS_SB, POS_BB, POS_EARLY, POS_MID, POS_LATE = 0, 1, 2, 3, 4


def pos_bucket_idx(seat, s):
    """view.rs:639 my_pos_bucket 同义，环取在手座位（稀疏座位安全）。"""
    if seat == s["sb"]:
        return POS_SB
    if seat == s["bb"]:
        return POS_BB
    ring = sorted(s["inh"])
    n = len(ring)
    btn = s.get("btn")
    if btn in ring:
        idx = (ring.index(seat) - ring.index(btn)) % n if seat in ring else 2
    else:
        idx = 2 if seat not in ring else sorted(ring, key=lambda x: ((x - (btn or 0)) % 32, x)).index(seat)
    if idx == 0 or idx == n - 1:
        return POS_LATE
    if idx >= max(0, n - 3):
        return POS_MID
    return POS_EARLY


def prof_features(prof):
    """生命/最近 50 手双通道，分子分母都给（§5）。15 维。"""
    out = []
    for ch in ("life", "w50"):
        st = (prof or {}).get(ch) or {}
        hands = st.get("hands", 0)
        vo, vc = st.get("vpip_opp", 0), st.get("vpip_cnt", 0)
        po, pc = st.get("pfr_opp", 0), st.get("pfr_cnt", 0)
        ag, ca = st.get("aggr_actions", 0), st.get("call_actions", 0)
        out += [
            min(1.0, math.log1p(hands) / 7.0),
            vc / vo if vo else 0.25,
            min(1.0, math.log1p(vo) / 6.0),
            pc / po if po else 0.15,
            min(1.0, math.log1p(po) / 6.0),
            ag / (ag + ca) if (ag + ca) else 0.5,
            min(1.0, math.log1p(ag + ca) / 6.0),
        ]
    out.append(min(1.0, math.log1p((prof or {}).get("n_shows", 0)) / 4.0))
    return out


def board_texture(board_ids):
    """(paired, suitmax/5, top_rank/12, two_tone, mono)——B3 与发牌 token 共用。"""
    if not board_ids:
        return (0.0, 0.0, 0.0, 0.0, 0.0)
    ranks = [c // 4 for c in board_ids]
    suits = [c % 4 for c in board_ids]
    sc = max(suits.count(x) for x in set(suits))
    paired = 1.0 if len(set(ranks)) < len(ranks) else 0.0
    return (paired, sc / 5.0, max(ranks) / 12.0,
            1.0 if sc >= max(2, len(board_ids) - 1) and sc < len(board_ids) else 0.0,
            1.0 if sc == len(board_ids) and len(board_ids) >= 3 else 0.0)


class SampleFeaturizer:
    """单样本 → (tok_feat, tok_street, tok_prior_idx, priors, alive, snaps, b3_rows)。"""

    def __init__(self, b2, axes_cache):
        self.b2 = b2
        self.axes_cache = axes_cache

    def axes(self, board_ids, need, dead):
        key = (tuple(board_ids[:need]), tuple(sorted(dead)))
        v = self.axes_cache.get(key)
        if v is None:
            v = class_axes(list(board_ids[:need]), _mask_of(dead))
            self.axes_cache[key] = v
        return v

    def run(self, s):
        bb = max(1, s.get("bbamt") or 2)
        tseat = s["tseat"]
        hseat = s.get("hseat")
        hhole = s.get("hhole") or []
        obs_hole = [card_id(c) for c in hhole] if (len(hhole) == 2 and hseat is not None
                                                   and hseat != tseat) else []
        board_ids = [card_id(c) for c in s.get("board") or []]
        max_bstreet = max((st for st in (1, 2, 3) if len(board_ids) >= STREET_NEED[st]), default=0)
        life = (s.get("prof") or {}).get("life") or {}
        prof_vec = prof_features(s.get("prof"))
        stacks = {int(k): v for k, v in (s.get("stacks") or {}).items()}

        # alive per street（观察者视角死牌）
        alive = np.zeros((4, 169), dtype=np.uint8)
        for st in range(4):
            if st <= max_bstreet:
                alive[st] = alive_np(obs_hole + board_ids[:STREET_NEED[st]])

        # 花色同构重标号（board 首现顺序）；token 只喂重标号后的花色 →
        # 花色置换增广自动成为恒等，无需显式增广。
        suit_map = {}
        def canon_suit(su):
            if su not in suit_map:
                suit_map[su] = len(suit_map)
            return suit_map[su]

        # --- 因果 prior 状态 ---
        commits = {}          # (st, seat) → 本街累计
        commits[(0, s["sb"])] = s["sbamt"]
        commits[(0, s["bb"])] = s["bbamt"]
        total_commit = {s["sb"]: s["sbamt"], s["bb"]: s["bbamt"]}
        seed_commit = dict(total_commit)
        npr_sofar, pagg_sofar = 0, None
        tgt_folded = False
        op_prod = np.ones(169, dtype=np.float64)
        cur_role = None
        base_prior = None
        priors = []           # 去重 prior 向量表（f32）
        cur_pidx = -1

        def role_now():
            if pagg_sofar == tseat:
                return "3bet" if npr_sofar >= 2 else "open"
            cbb = commits.get((0, tseat), 0) / bb
            return "coldcall" if cbb >= 2.5 else ("limp" if cbb >= 1.5 else "bbcheck")

        def push_prior():
            nonlocal cur_pidx
            vec = np.asarray(base_prior, dtype=np.float64) * op_prod
            priors.append(np.log(np.maximum(vec, 1e-30)).astype(np.float32))
            cur_pidx = len(priors) - 1

        def set_role(r):
            nonlocal cur_role, base_prior
            if r != cur_role:
                cur_role = r
                base_prior = self.b2.prior(r, life)
                push_prior()

        set_role(role_now())

        # --- token 流 ---
        feats, streets, pidxs = [], [], []

        def emit(ttype, st, fill):
            f = np.zeros(FDIM, dtype=np.float32)
            f[_IDX["ttype"] + ttype] = 1.0
            f[_IDX["street"] + min(st, 3)] = 1.0
            fill(f)
            feats.append(f)
            # alive 查表街：board 覆盖不到的街 clamp 到已知 board 的最深街
            streets.append(st if st <= max_bstreet else max_bstreet)
            pidxs.append(cur_pidx)

        # CTX
        tgt_stack_bb = (stacks.get(tseat, 100 * bb) + seed_commit.get(tseat, 0)) / bb
        def fill_ctx(f):
            o = _IDX["ctx"]
            f[o + 0] = s.get("ns", 2) / 6.0
            f[o + 1] = 1.0 if s.get("ns", 2) == 2 else 0.0
            f[o + 2] = max(-1.0, min(1.0, math.log2(max(1.0, tgt_stack_bb) / 100.0) / 3.0))
            f[o + 3 + pos_bucket_idx(tseat, s)] = 1.0
        emit(TT_CTX, 0, fill_ctx)

        # PROF
        def fill_prof(f):
            o = _IDX["prof"]
            f[o:o + 15] = prof_vec
        emit(TT_PROF, 0, fill_prof)

        def emit_board_street(st):
            need_lo = STREET_NEED[st - 1] if st > 0 else 0
            cards = board_ids[need_lo:STREET_NEED[st]]
            tex = board_texture(board_ids[:STREET_NEED[st]])
            for c in cards:
                def fill_b(f, c=c, tex=tex):
                    o = _IDX["board"]
                    f[o + (c // 4)] = 1.0
                    f[o + 13 + canon_suit(c % 4)] = 1.0
                    f[o + 17] = tex[0]
                    f[o + 18] = tex[1]
                emit(TT_BOARD, st, fill_b)

        # B3 聚合状态（目标玩家逐街动作计数）
        tgt_n = np.zeros((4, 3), dtype=np.float32)  # street × (aggr, call, check)
        last_aggr_frac, last_faced_tc = 0.0, 0.0
        street_aggr_seat = {}

        last_st = 0
        for e in s["ev"]:
            st = max(last_st, min(3, e["st"]))
            while last_st < st:
                last_st += 1
                if last_st <= max_bstreet:
                    emit_board_street(last_st)
            seat, act = e["s"], e["a"]
            cb = commits.get((st, seat), 0)
            delta = e["c"] - cb
            commits[(st, seat)] = e["c"]
            total_commit[seat] = total_commit.get(seat, 0) + delta
            pot_before = max(1, e["p"] - delta)
            if act in AGGR and e["c"] > e["hb"]:
                street_aggr_seat[st] = seat
            # npr/pagg 口径与 dataset 字段一致（a∈AGGR 即计，不看 c>hb）→ 前缀末
            # role_now() == range_baselines.preflop_role（B2b parity 的前提）。
            if st == 0 and act in AGGR:
                npr_sofar += 1
                pagg_sofar = seat

            # 因果 prior 更新（动作先于 token 发射生效；目标 fold 后冻结）
            if not tgt_folded:
                if seat == tseat and act == "fold":
                    tgt_folded = True
                elif st == 0:
                    # 任何翻前事件都可能改变目标角色（如他人 3bet 夺走 pagg）
                    set_role(role_now())
                elif seat == tseat and st <= max_bstreet:
                    v, d, _ = self.axes(board_ids, STREET_NEED[st], obs_hole + board_ids[:STREET_NEED[st]])
                    L = self.b2.op(e, v, d)
                    if L is not None:
                        op_prod *= np.asarray(L, dtype=np.float64)
                        push_prior()

            # B3 聚合
            if seat == tseat:
                if act in AGGR and e["c"] > e["hb"]:
                    tgt_n[st, 0] += 1
                    last_aggr_frac = min(4.0, e["c"] / pot_before)
                elif act == "call" or (act in AGGR and e["c"] <= e["hb"]):
                    tgt_n[st, 1] += 1
                elif act == "check":
                    tgt_n[st, 2] += 1
                if e.get("tc", 0):
                    last_faced_tc = min(4.0, e["tc"] / pot_before)

            rem = max(0, stacks.get(seat, 100 * bb) - (total_commit.get(seat, 0) - seed_commit.get(seat, 0)))
            def fill_a(f, e=e, st=st, seat=seat, act=act, delta=delta, pot_before=pot_before, rem=rem):
                o = _IDX["act"]
                f[o + pos_bucket_idx(seat, s)] = 1.0
                f[o + 5] = 1.0 if seat == tseat else 0.0
                f[o + 6 + ACT_TYPES.get(act, 1)] = 1.0
                f[o + 12] = 1.0 if (act in AGGR and e["c"] > e["hb"]) else 0.0
                f[o + 13] = min(4.0, e["c"] / pot_before) / 4.0
                f[o + 14] = math.log1p(min(8.0, e["c"] / pot_before)) / 2.2
                f[o + 15] = min(4.0, delta / pot_before) / 4.0
                f[o + 16] = min(4.0, (e.get("tc") or 0) / pot_before) / 4.0
                f[o + 17] = min(25.0, rem / pot_before) / 25.0
                f[o + 18] = min(1.0, math.log1p(e["p"] / bb) / 6.0)
                f[o + 19] = min(4.0, e["hb"] / pot_before) / 4.0
            emit(TT_ACT, st, fill_a)

        while last_st < max_bstreet:
            last_st += 1
            emit_board_street(last_st)

        truncated = len(feats) > MAXTOK
        if truncated:
            feats, streets, pidxs = feats[:MAXTOK], streets[:MAXTOK], pidxs[:MAXTOK]

        # 街末快照位置
        snaps = np.full(4, -1, dtype=np.int32)
        st_arr = np.asarray(streets)
        for S in range(4):
            if len(board_ids) >= STREET_NEED[S]:
                idxs = np.nonzero(st_arr <= S)[0]
                if len(idxs):
                    snaps[S] = idxs[-1]

        # B3 行（每个可用街末快照一行）
        b3_rows = []
        final_role = cur_role
        pot_end = {}
        p = s["sbamt"] + s["bbamt"]
        for e in s["ev"]:
            p = e["p"]
            pot_end[min(3, e["st"])] = p
        for S in range(4):
            if snaps[S] < 0:
                continue
            pot = next((pot_end[x] for x in range(S, -1, -1) if x in pot_end),
                       s["sbamt"] + s["bbamt"])
            committed = sum(commits.get((x, tseat), 0) for x in range(S + 1))
            tex = board_texture(board_ids[:STREET_NEED[S]])
            row = []
            role_oh = [0.0] * 5
            role_oh[["3bet", "open", "coldcall", "limp", "bbcheck"].index(final_role)] = 1.0
            pos_oh = [0.0] * 5
            pos_oh[pos_bucket_idx(tseat, s)] = 1.0
            row += [S / 3.0] + role_oh + pos_oh
            row += [s.get("ns", 2) / 6.0, min(4, s.get("npr", 0)) / 4.0,
                    1.0 if s.get("pagg") == tseat else 0.0,
                    1.0 if street_aggr_seat.get(S) == tseat else 0.0,
                    min(1.0, math.log1p(committed / bb) / 6.0),
                    min(1.0, math.log1p(pot / bb) / 6.0),
                    min(25.0, max(0, stacks.get(tseat, 100 * bb) - (committed - seed_commit.get(tseat, 0))) / pot) / 25.0,
                    last_aggr_frac / 4.0, last_faced_tc / 4.0]
            counts = tgt_n.copy()
            counts[S + 1:] = 0.0
            row += list((np.minimum(counts, 4) / 4.0).flatten())
            row += list(tex)
            row += prof_vec
            b3_rows.append((S, row))

        return (np.stack(feats), np.asarray(streets, dtype=np.int8),
                np.asarray(pidxs, dtype=np.int32), priors, alive, snaps, b3_rows, truncated)


def _mask_of(cards):
    m = 0
    for c in cards:
        m |= 1 << c
    return m


B3_NFEAT = 1 + 5 + 5 + 9 + 12 + 5 + 15


def build(args):
    strength = build_strength_table(cache_path=os.path.join(args.ds, "strength169.json"))
    b2 = B2RangeVec(strength, B2_PARAMS, use_ops=True)
    fz = SampleFeaturizer(b2, {})

    splits = {"train": set(args.train_runs.split(",")),
              "val": set(args.val_runs.split(",")),
              "test": set(args.test_runs.split(","))}

    seq = {k: [] for k in ("feat", "street", "pidx", "ptr", "prior", "prior_ptr",
                           "alive", "snaps", "y", "w", "src", "split")}
    seq["ptr"] = [0]
    seq["prior_ptr"] = [0]
    tgts = []
    b3 = {"x": [], "street": [], "y": [], "w": [], "src": [], "split": [], "alive": [], "sample": []}
    audit = {"n": 0, "truncated": 0, "skipped_split": 0, "by_src": {}}
    t0 = time.time()

    for src in ("live_opp", "live_hero", "slumbot"):
        path = os.path.join(args.ds, f"{src}.jsonl")
        n_src = 0
        with open(path) as f:
            for line in f:
                s = json.loads(line)
                run = s.get("run", "")
                if src == "live_opp":
                    split = next((k for k, v in splits.items() if run in v), None)
                else:
                    split = "train" if (src == "slumbot" or run in splits["train"]) else None
                if split is None:
                    audit["skipped_split"] += 1
                    continue
                if args.limit and n_src >= args.limit:
                    break
                feat, street, pidx, priors, alive, snaps, b3_rows, truncated = fz.run(s)
                if truncated:
                    audit["truncated"] += 1
                si = len(seq["ptr"]) - 1
                seq["feat"].append(feat.astype(np.float16))
                seq["street"].append(street)
                seq["pidx"].append(pidx + seq["prior_ptr"][-1])
                seq["ptr"].append(seq["ptr"][-1] + len(feat))
                seq["prior"].append(np.stack(priors).astype(np.float16))
                seq["prior_ptr"].append(seq["prior_ptr"][-1] + len(priors))
                seq["alive"].append(alive)
                seq["snaps"].append(snaps)
                seq["y"].append(s["y"])
                seq["w"].append(SRC_W[src])
                seq["src"].append(SRC_ID[src])
                seq["split"].append(SPLIT_ID[split])
                tgts.append(s.get("tgt", ""))
                for S, row in b3_rows:
                    b3["x"].append(np.asarray(row, dtype=np.float32))
                    b3["street"].append(S)
                    b3["y"].append(s["y"])
                    b3["w"].append(SRC_W[src])
                    b3["src"].append(SRC_ID[src])
                    b3["split"].append(SPLIT_ID[split])
                    b3["alive"].append(alive[S])
                    b3["sample"].append(si)
                n_src += 1
                audit["n"] += 1
                if audit["n"] % 2000 == 0:
                    print(f"[featurize] {audit['n']} samples ({time.time()-t0:.0f}s, "
                          f"axes cache {len(fz.axes_cache)})", flush=True)
        audit["by_src"][src] = n_src

    out_seq = os.path.join(args.out, "seq_v1.npz")
    np.savez_compressed(
        out_seq,
        feat=np.concatenate(seq["feat"]), street=np.concatenate(seq["street"]),
        pidx=np.concatenate(seq["pidx"]), ptr=np.asarray(seq["ptr"], dtype=np.int64),
        prior=np.concatenate(seq["prior"]),
        alive=np.stack(seq["alive"]), snaps=np.stack(seq["snaps"]),
        y=np.asarray(seq["y"], dtype=np.int16), w=np.asarray(seq["w"], dtype=np.float32),
        src=np.asarray(seq["src"], dtype=np.int8), split=np.asarray(seq["split"], dtype=np.int8),
        tgt=np.asarray(tgts), fdim=np.asarray([FDIM]),
    )
    out_b3 = os.path.join(args.out, "b3_v1.npz")
    np.savez_compressed(
        out_b3,
        x=np.stack(b3["x"]), street=np.asarray(b3["street"], dtype=np.int8),
        y=np.asarray(b3["y"], dtype=np.int16), w=np.asarray(b3["w"], dtype=np.float32),
        src=np.asarray(b3["src"], dtype=np.int8), split=np.asarray(b3["split"], dtype=np.int8),
        alive=np.stack(b3["alive"]), sample=np.asarray(b3["sample"], dtype=np.int32),
    )
    print(f"[featurize] done in {time.time()-t0:.0f}s: {audit} → {out_seq}, {out_b3}", flush=True)
    return out_seq


def parity_check(seq_path, args):
    """残差=0 的快照预测（log prior + log alive）在 test live_opp 上复算 B2b 指标，
    应与 M0 报告逐位接近（街序 clamp 只影响 ~0.1% 异常手）。"""
    d = np.load(seq_path)
    ptr, snaps, split, src = d["ptr"], d["snaps"], d["split"], d["src"]
    prior, pidx, alive, y = d["prior"].astype(np.float32), d["pidx"], d["alive"], d["y"]
    for want_split, name in ((2, "test"), (1, "val")):
        sel = np.nonzero((split == want_split) & (src == 0))[0]
        print(f"[parity] {name}: {len(sel)} samples")
        for S in range(4):
            hits = n = 0
            nll = 0.0
            for i in sel:
                sp = snaps[i, S]
                if sp < 0:
                    continue
                lp = prior[pidx[ptr[i] + sp]] + np.log(np.maximum(alive[i, S], 1e-9))
                lp[alive[i, S] == 0] = -1e30
                p = np.exp(lp - lp.max())
                p /= p.sum()
                order = np.lexsort((np.arange(169), -p))
                hits += int(np.nonzero(order == y[i])[0][0] < 5)
                nll += -math.log(max(p[y[i]], 1e-12))
                n += 1
            print(f"  {STREET_NAME[S]:8s}: top5 {hits/max(1,n):.3f}  NLL {nll/max(1,n):.3f}  (n={n})")


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--ds", default="artifacts/range_ml")
    ap.add_argument("--out", default="artifacts/range_ml")
    ap.add_argument("--train-runs", default="run24,run25,run26,run27")
    ap.add_argument("--val-runs", default="run28")
    ap.add_argument("--test-runs", default="run29,run30")
    ap.add_argument("--limit", type=int, default=0, help="每源最多 N 样本（调试）")
    ap.add_argument("--parity", action="store_true", help="构建后做 B2b 快照对拍")
    ap.add_argument("--parity-only", action="store_true", help="只对已有 npz 做对拍")
    args = ap.parse_args()

    seq_path = os.path.join(args.out, "seq_v1.npz")
    if not args.parity_only:
        seq_path = build(args)
    if args.parity or args.parity_only:
        parity_check(seq_path, args)


if __name__ == "__main__":
    main()
