#!/usr/bin/env python3
"""M2 censoring 对照实验（docs/opponent_range_ml_design.md §2.1 / §9 M2 出闸项）。

问题：live 训练只有摊牌亮牌样本，学到的是 P(y|前缀,摊牌亮牌) 而非 P(y|前缀)。
Slumbot 全知集（每手都有 bot 底牌，含 fold 结束手）可以直接量化这个伤害：

  同一架构、同超参、同种子，唯一差别 = train 是否包含 fold 结束手：
    A「full」   全部手训练（全知，L1 上限）
    B「sd-only」仅摊牌手训练（live 的 L0 处境）
  在全知 test（含 fold 手）上对比，子集分解：
    sd（摊牌结束）/ fold_bot（bot 弃牌——被删失的弱牌类）/ fold_hero（hero 弃牌）

早停纪律：sd-only 的 val 也只含摊牌手——线上没有 fold 标签可选型，模拟真实约束。
两组的源权重都强制 1.0（npz 内 slumbot 默认 0.1 是混源微调用的）。

用法：
  OMP_NUM_THREADS=2 .venv-rangeml/bin/python tools/range_censoring_ab.py   # ~10min
"""

import argparse
import json
import os
import sys
import time

import numpy as np
import torch
import torch.nn as nn

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from range_train import (  # noqa: E402
    STREET_NAME, ResidualGRU, SeqData, loss_fn, paired_bootstrap, snapshot_eval,
)


def train_model(data, train_idx, val_idx, args, tag):
    torch.manual_seed(args.seed)
    rng = np.random.default_rng(args.seed)
    model = ResidualGRU(data.fdim, d=args.d, dropout=args.dropout)
    opt = torch.optim.AdamW(model.parameters(), lr=args.lr, weight_decay=args.wd)
    sched = torch.optim.lr_scheduler.CosineAnnealingLR(opt, T_max=args.epochs)
    order = train_idx[np.argsort(data.lens[train_idx], kind="stable")]
    batches = [order[i:i + args.batch] for i in range(0, len(order), args.batch)]
    best_nll, best_ep, bad, best_state = float("inf"), -1, 0, None
    for ep in range(args.epochs):
        model.train()
        t0 = time.time()
        rng.shuffle(batches)
        for bidx in batches:
            batch = data.batch(bidx)
            loss = loss_fn(model(batch["feat"], batch["lens"]), batch, eps=args.eps)
            opt.zero_grad()
            loss.backward()
            nn.utils.clip_grad_norm_(model.parameters(), 1.0)
            opt.step()
        sched.step()
        met, _ = snapshot_eval(model, data, val_idx)
        val_nll = np.mean([met[S]["nll"] for S in met])
        if val_nll < best_nll - 1e-4:
            best_nll, best_ep, bad = val_nll, ep, 0
            best_state = {k: v.clone() for k, v in model.state_dict().items()}
        else:
            bad += 1
        if ep % 10 == 0 or bad == 0:
            print(f"[{tag}] ep{ep:03d} val_snapNLL={val_nll:.4f} ({time.time()-t0:.0f}s)"
                  f"{' *best' if bad == 0 else ''}", flush=True)
        if bad >= args.patience:
            break
    print(f"[{tag}] best ep{best_ep} val_snapNLL={best_nll:.4f}", flush=True)
    model.load_state_dict(best_state)
    model.eval()
    return model


def subset_table(mats_a, mats_b, subsets, name_a="full", name_b="sd-only"):
    """mats = (hit5, nll, ref_hit5, ref_nll, ...)；返回 markdown 行。"""
    lines = [f"| 子集 | 街 | n | {name_a} top-5 | {name_b} top-5 | B2b top-5 | "
             f"{name_a} NLL | {name_b} NLL | B2b NLL | Δtop-5 (p) | ΔNLL (p) |",
             "|---|---|---|---|---|---|---|---|---|---|---|"]
    hit_a, nll_a, ref_hit, ref_nll = mats_a[0], mats_a[1], mats_a[2], mats_a[3]
    hit_b, nll_b = mats_b[0], mats_b[1]
    for sub_name, sub_mask in subsets.items():
        for S in range(4):
            m = (hit_a[:, S] >= 0) & sub_mask
            if m.sum() < 20:
                continue
            d5, _, p5 = paired_bootstrap(hit_a[m, S], hit_b[m, S])
            dn, _, pn = paired_bootstrap(-nll_a[m, S], -nll_b[m, S])
            lines.append(
                f"| {sub_name} | {STREET_NAME[S]} | {int(m.sum())} | "
                f"{hit_a[m, S].mean():.3f} | {hit_b[m, S].mean():.3f} | "
                f"{(ref_hit[m, S] == 1).mean():.3f} | "
                f"{nll_a[m, S].mean():.3f} | {nll_b[m, S].mean():.3f} | "
                f"{ref_nll[m, S].mean():.3f} | {d5:+.3f} ({p5:.4f}) | {-dn:+.3f} ({pn:.4f}) |")
        lines.append("|  |  |  |  |  |  |  |  |  |  |  |")
    return lines


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--ds", default="artifacts/range_ml")
    ap.add_argument("--report", default="artifacts/range_ml/m2_censoring_report.md")
    ap.add_argument("--train-frac", type=float, default=0.8)
    ap.add_argument("--val-count", type=int, default=800)
    ap.add_argument("--d", type=int, default=192)
    ap.add_argument("--dropout", type=float, default=0.15)
    ap.add_argument("--epochs", type=int, default=120)
    ap.add_argument("--batch", type=int, default=96)
    ap.add_argument("--lr", type=float, default=5e-4)
    ap.add_argument("--wd", type=float, default=1e-4)
    ap.add_argument("--eps", type=float, default=0.05)
    ap.add_argument("--patience", type=int, default=15)
    ap.add_argument("--seed", type=int, default=7)
    args = ap.parse_args()

    torch.set_num_threads(2)
    data = SeqData(os.path.join(args.ds, "seq_v1.npz"))
    slum = np.nonzero(data.src == 2)[0]

    # slumbot.jsonl 与 npz 内 slumbot 段按序对齐（featurize 保序），y 对拍防错位
    fold_end = np.zeros(len(slum), dtype=bool)
    folder_bot = np.zeros(len(slum), dtype=bool)
    with open(os.path.join(args.ds, "slumbot.jsonl")) as f:
        for j, line in enumerate(f):
            s = json.loads(line)
            assert data.y[slum[j]] == s["y"], f"slumbot 对拍失败 @{j}"
            fold_end[j] = s["fold_end"]
            folder_bot[j] = s.get("folder") == s["tseat"]

    n_train = int(len(slum) * args.train_frac)
    pool, test_pos = np.arange(n_train), np.arange(n_train, len(slum))
    val_pos, train_pos = pool[-args.val_count:], pool[:-args.val_count]
    data.w[slum] = 1.0  # 单源训练，覆盖混源微调权重

    n_sd = int((~fold_end[train_pos]).sum())
    match_pos = np.random.default_rng(args.seed).choice(train_pos, size=n_sd, replace=False)
    sets = {
        "full": (slum[train_pos], slum[val_pos]),
        "sd-only": (slum[train_pos[~fold_end[train_pos]]],
                    slum[val_pos[~fold_end[val_pos]]]),
        # 同量级对照：全知分布下采样到 sd-only 的样本数——隔离「fold 信息」与「样本量」
        "full-matched": (slum[np.sort(match_pos)], slum[val_pos]),
    }
    print(f"[ab] slumbot {len(slum)}: train full={len(sets['full'][0])} "
          f"sd={len(sets['sd-only'][0])}, val full={len(sets['full'][1])} "
          f"sd={len(sets['sd-only'][1])}, test={len(test_pos)} "
          f"(fold_bot={folder_bot[test_pos].sum()} "
          f"fold_hero={(fold_end[test_pos] & ~folder_bot[test_pos]).sum()})", flush=True)

    test_idx = slum[test_pos]
    mats = {}
    for tag, (tr, va) in sets.items():
        model = train_model(data, tr, va, args, tag)
        _, m = snapshot_eval(model, data, test_idx, with_prior_ref=True)
        mats[tag] = m

    subsets = {
        "全集": np.ones(len(test_pos), dtype=bool),
        "sd（摊牌结束）": ~fold_end[test_pos],
        "fold_bot（bot 弃牌）": folder_bot[test_pos],
        "fold_hero（hero 弃牌）": fold_end[test_pos] & ~folder_bot[test_pos],
    }
    report = ["# M2 censoring 对照实验（Slumbot 全知集）", "",
              f"- 切分：前 {len(train_pos)}+{len(val_pos)}(val) 训 / 后 {len(test_pos)} 测"
              "（时间序）；各组同超参同种子，唯一差别 = train 组成",
              f"- 训练量：full {len(sets['full'][0])} / sd-only {len(sets['sd-only'][0])} / "
              f"full-matched {len(sets['full-matched'][0])}（全知分布下采样至 sd 量级，"
              "隔离样本量混淆）；sd-only 的 val 亦仅摊牌手（线上无 fold 标签可选型）",
              "- Δ = 左列 − sd-only（配对 bootstrap；top-5 正 / NLL 负 = 全知更好"
              " = censoring 有伤害）", "",
              "### full（全量全知） vs sd-only", ""]
    report += subset_table(mats["full"], mats["sd-only"], subsets)
    report += ["", "### full-matched（同量级全知） vs sd-only —— 纯分布效应", ""]
    report += subset_table(mats["full-matched"], mats["sd-only"], subsets,
                           name_a="matched")

    os.makedirs(os.path.dirname(args.report), exist_ok=True)
    with open(args.report, "w") as f:
        f.write("\n".join(report))
    print(f"[ab] 报告 → {args.report}", flush=True)
    for line in report[6:]:
        print(line)


if __name__ == "__main__":
    main()
