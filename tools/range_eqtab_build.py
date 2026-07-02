#!/usr/bin/env python3
"""equity 残差评估表构建（docs/opponent_range_ml_design.md §7 equity 残差指标）。

live_opp val/test 样本 → 每个街末快照的：
  eqc[169]  per-class 对手 equity：类内存活 combo 的 eq(combo vs hero) 均值
  eqy       亮牌 equity：eq(实际亮牌 vs hero)
供 range_train 评估「预测 range 折算 equity − 亮牌 equity」的 bias / MAE——
这是对下游 equity_weighted 消费的那个线性泛函的直接检验：残差 >0 = range
高估对手强度（hero 会偏保守），<0 = 低估。

口径：
  - equity = 目标对手手牌 vs hero 底牌的胜率（win=1 / tie=0.5），街末 board
    前缀上补完 runout：river 精确（0 张）、turn 全枚举（1 张）、preflop/flop
    共享采样 M 个 runout；combo/亮牌撞 runout 时拒绝该 runout——等价于从
    「排除该 combo」的正确牌堆采样，无偏。
  - eqc 类内取 C 个存活 combo（river 全枚举）；模型与 B2b 共用同一张表 ⇒
    MC 噪声是共模的，配对 Δ 精确对消。
  - hero 底牌缺失（hero 不在手）的样本无 hero 视角 → 无行，评估端跳过。

样本行与 seq_v1.npz 的全局样本下标对齐（复刻 featurize 的切分跳过逻辑），
构建时逐行与 npz 的 y/tgt/split 对拍防错位。

用法：
  .venv-rangeml/bin/python tools/range_eqtab_build.py --selftest
  .venv-rangeml/bin/python tools/range_eqtab_build.py            # ~20min
"""

import argparse
import json
import os
import random
import sys
import time

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from range_ml_common import CLASS_COMBOS, card_id, class_name, eval_cards  # noqa: E402

STREET_NEED = {0: 0, 1: 3, 2: 4, 3: 5}


def street_eq(hero, board_s, label, rng, m_runouts, cpc):
    """→ (eqc[169] float list，dead 类 -1；eqy float)。equity 为对手视角。"""
    dead = set(hero) | set(board_s)
    deck = [c for c in range(52) if c not in dead]
    need = 5 - len(board_s)
    if need == 0:
        runouts = [()]
    elif need == 1:
        runouts = [(c,) for c in deck]
    else:
        runouts = [tuple(rng.sample(deck, need)) for _ in range(m_runouts)]
    bl = list(board_s)
    hero_vals = [eval_cards(list(hero) + bl + list(r)) for r in runouts]

    def combo_eq(a, b):
        num = den = 0.0
        for r, hv in zip(runouts, hero_vals):
            if a in r or b in r:
                continue
            ov = eval_cards([a, b] + bl + list(r))
            num += 1.0 if ov > hv else (0.5 if ov == hv else 0.0)
            den += 1.0
        return num / den if den else -1.0

    eqc = [-1.0] * 169
    for k in range(169):
        alive = [(a, b) for a, b in CLASS_COMBOS[k] if a not in dead and b not in dead]
        if not alive:
            continue
        pick = alive if (need == 0 or len(alive) <= cpc) else rng.sample(alive, cpc)
        vals = [v for v in (combo_eq(a, b) for a, b in pick) if v >= 0]
        if vals:
            eqc[k] = sum(vals) / len(vals)
    return eqc, combo_eq(label[0], label[1])


def build(args):
    splits = {"train": set(args.train_runs.split(",")),
              "val": set(args.val_runs.split(",")),
              "test": set(args.test_runs.split(","))}
    want = set(args.splits.split(","))
    seq = np.load(os.path.join(args.ds, "seq_v1.npz"))
    seq_y, seq_tgt, seq_split = seq["y"], seq["tgt"], seq["split"]
    split_id = {"train": 0, "val": 1, "test": 2}

    rows_si, rows_eqc, rows_eqy, rows_valid, rows_y = [], [], [], [], []
    n_no_hero = n_scan = 0
    si = -1
    t0 = time.time()
    with open(os.path.join(args.ds, "live_opp.jsonl")) as f:
        for line in f:
            s = json.loads(line)
            split = next((k for k, v in splits.items() if s.get("run", "") in v), None)
            if split is None:
                continue  # featurize 同款跳过 → 不占 seq 下标
            si += 1
            if split not in want:
                continue
            # 与 seq npz 对拍防错位
            assert int(seq_y[si]) == s["y"] and str(seq_tgt[si]) == s["tgt"] \
                and int(seq_split[si]) == split_id[split], f"seq 对拍失败 @si={si}"
            if args.limit and len(rows_si) >= args.limit:
                break
            hhole = s.get("hhole") or []
            if len(hhole) != 2 or s.get("hseat") is None or s.get("hseat") == s["tseat"]:
                n_no_hero += 1
                continue
            hero = [card_id(c) for c in hhole]
            label = [card_id(c) for c in s["label"]]
            board = [card_id(c) for c in s.get("board") or []]
            rng = random.Random((args.seed << 20) ^ (si * 2654435761 % (1 << 31)))
            eqc4 = np.full((4, 169), -1.0, dtype=np.float16)
            eqy4 = np.full(4, np.nan, dtype=np.float32)
            valid4 = np.zeros(4, dtype=bool)
            for S in range(4):
                if len(board) < STREET_NEED[S]:
                    continue
                eqc, eqy = street_eq(hero, board[:STREET_NEED[S]], label, rng,
                                     args.runouts, args.cpc)
                eqc4[S] = np.asarray(eqc, dtype=np.float16)
                eqy4[S] = eqy
                valid4[S] = eqy >= 0
            rows_si.append(si)
            rows_eqc.append(eqc4)
            rows_eqy.append(eqy4)
            rows_valid.append(valid4)
            rows_y.append(s["y"])
            n_scan += 1
            if n_scan % 100 == 0:
                dt = time.time() - t0
                print(f"[eqtab] {n_scan} samples ({dt:.0f}s, {dt/n_scan:.2f}s/样本)", flush=True)

    out = os.path.join(args.ds, args.out)
    np.savez_compressed(
        out,
        sample=np.asarray(rows_si, dtype=np.int32), eqc=np.stack(rows_eqc),
        eqy=np.stack(rows_eqy), valid=np.stack(rows_valid),
        y=np.asarray(rows_y, dtype=np.int16),
        cfg=np.asarray([args.runouts, args.cpc, args.seed], dtype=np.int64),
    )
    print(f"[eqtab] done: {n_scan} 行（无 hero 底牌跳过 {n_no_hero}），"
          f"{time.time()-t0:.0f}s → {out}", flush=True)


def _selftest():
    cid = card_id
    kidx = {class_name(k): k for k in range(169)}
    # river 精确：hero AsKs 两对 on Ah Kh 2c 7d 9s
    hero = [cid("As"), cid("Ks")]
    board = [cid("Ah"), cid("Kh"), cid("2c"), cid("7d"), cid("9s")]
    rng = random.Random(1)
    eqc, eqy = street_eq(hero, board, [cid("Qh"), cid("Qd")], rng, 24, 2)
    assert eqc[kidx["AA"]] == 1.0    # AdAc 三条 A > hero 两对
    assert eqc[kidx["QQ"]] == 0.0    # QQ 一对 < 两对
    assert eqy == 0.0                # 亮牌 QQ 输
    assert eqc[kidx["22"]] > 0.9     # 22 成 2 的三条（葫芦 222AK... 三条+两对=葫芦）赢
    assert all(v == -1.0 or 0.0 <= v <= 1.0 for v in eqc)
    # turn 全枚举：数值域 + 强类>弱类
    board4 = board[:4]
    eqc4, eqy4 = street_eq(hero, board4, [cid("Qh"), cid("Qd")], rng, 24, 2)
    assert 0.0 <= eqy4 <= 0.2        # QQ 只有 2 张 Q 补牌
    assert eqc4[kidx["AA"]] > eqc4[kidx["QQ"]]
    # preflop MC：AA 对随机 hero 手强
    eqcp, eqyp = street_eq([cid("7c"), cid("2d")], [], [cid("Ad"), cid("Ac")], rng, 48, 2)
    assert eqyp > 0.75
    # 拒绝采样：runout 含 combo 牌时跳过 → eqc 仍在域内
    assert all(v == -1.0 or 0.0 <= v <= 1.0 for v in eqcp)
    print("range_eqtab_build selftest OK")


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--ds", default="artifacts/range_ml")
    ap.add_argument("--out", default="eqtab_v1.npz")
    ap.add_argument("--train-runs", default="run24,run25,run26,run27")
    ap.add_argument("--val-runs", default="run28")
    ap.add_argument("--test-runs", default="run29,run30")
    ap.add_argument("--splits", default="val,test")
    ap.add_argument("--runouts", type=int, default=24, help="preflop/flop 共享 runout 数")
    ap.add_argument("--cpc", type=int, default=2, help="每类采样 combo 数（river 全枚举）")
    ap.add_argument("--seed", type=int, default=20260702)
    ap.add_argument("--limit", type=int, default=0, help="调试：最多 N 行")
    ap.add_argument("--selftest", action="store_true")
    args = ap.parse_args()
    if args.selftest:
        _selftest()
        return
    build(args)


if __name__ == "__main__":
    main()
