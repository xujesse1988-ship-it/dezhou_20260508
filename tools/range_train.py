#!/usr/bin/env python3
"""M1 v1 序列模型训练：GRU 先验残差（docs/opponent_range_ml_design.md §4）。

架构：token 特征线性投影 → 单层 GRU(d) → 零初始化线性头 → 残差 logits f_θ；
  logits = log m_prior（因果 B2b，featurize 预计算）+ f_θ + log alive（死类 -inf）
  ⇒ 训练第 0 步逐位 == B2b 基线；网络只学「手写似然错在哪」。

损失（§4.3）：逐前缀（[PROF] 起每个 token 位置）label-smoothing CE（平滑质量只摊给
alive 类），源权重 live_opp 1.0 / hero 0.3 / slumbot 0.1（npz 内置，--src-filter 可消融）。

评估：与 M0 harness 同口径的 4 街末快照（val=run28 早停 / test=run29,30 出闸）；
报告含 vs B2b 的逐样本配对 bootstrap（Δtop-5 95% CI + p 值）、单调断言、后街斜率
（river−preflop 差距，M0 结论要求显著拉大）。

用法：
  OMP_NUM_THREADS=2 .venv-rangeml/bin/python tools/range_train.py             # 训练+评估
  ... range_train.py --eval-only --ckpt artifacts/range_ml/range_v1_gru.pt    # 仅评估
"""

import argparse
import json
import math
import os
import sys
import time

import numpy as np
import torch
import torch.nn as nn
import torch.nn.functional as F

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

STREET_NAME = {0: "preflop", 1: "flop", 2: "turn", 3: "river"}


class SeqData:
    def __init__(self, path):
        d = np.load(path)
        self.feat = d["feat"]
        self.street = d["street"]
        self.pidx = d["pidx"]
        self.ptr = d["ptr"]
        self.prior = d["prior"]
        self.alive = d["alive"]
        self.snaps = d["snaps"]
        self.y = d["y"].astype(np.int64)
        self.w = d["w"]
        self.src = d["src"]
        self.split = d["split"]
        self.fdim = int(d["fdim"][0])
        self.lens = np.diff(self.ptr).astype(np.int64)
        self.n = len(self.y)

    def batch(self, idxs):
        """→ dict of torch tensors，pad 到批内最大长度。"""
        L = int(self.lens[idxs].max())
        B = len(idxs)
        feat = np.zeros((B, L, self.fdim), dtype=np.float32)
        logprior = np.zeros((B, L, 169), dtype=np.float32)
        alive = np.zeros((B, L, 169), dtype=np.uint8)
        posmask = np.zeros((B, L), dtype=bool)
        for j, i in enumerate(idxs):
            a, b = self.ptr[i], self.ptr[i + 1]
            l = b - a
            feat[j, :l] = self.feat[a:b].astype(np.float32)
            logprior[j, :l] = self.prior[self.pidx[a:b]].astype(np.float32)
            alive[j, :l] = self.alive[i][self.street[a:b]]
            posmask[j, 1:l] = True  # 位置 0 = CTX，不进损失
        return {
            "feat": torch.from_numpy(feat),
            "logprior": torch.from_numpy(logprior),
            "alive": torch.from_numpy(alive),
            "posmask": torch.from_numpy(posmask),
            "y": torch.from_numpy(self.y[idxs]),
            "w": torch.from_numpy(self.w[idxs].astype(np.float32)),
            "lens": torch.from_numpy(self.lens[idxs]),
        }


class ResidualGRU(nn.Module):
    def __init__(self, fdim, d=192, dropout=0.15):
        super().__init__()
        self.inp = nn.Linear(fdim, d)
        self.gru = nn.GRU(d, d, batch_first=True)
        self.drop = nn.Dropout(dropout)
        self.head = nn.Linear(d, 169)
        nn.init.zeros_(self.head.weight)  # 第 0 步 = 手写 tracker（§4.2）
        nn.init.zeros_(self.head.bias)

    def forward(self, feat, lens):
        x = torch.tanh(self.inp(feat))
        packed = nn.utils.rnn.pack_padded_sequence(x, lens, batch_first=True,
                                                   enforce_sorted=False)
        h, _ = self.gru(packed)
        h, _ = nn.utils.rnn.pad_packed_sequence(h, batch_first=True,
                                                total_length=feat.shape[1])
        return self.head(self.drop(h))


def masked_logits(res, batch, residual_scale=1.0):
    la = torch.log(batch["alive"].clamp(min=1).float())
    dead = batch["alive"] == 0
    return (batch["logprior"] + residual_scale * res + la).masked_fill(dead, -1e30)


def loss_fn(res, batch, eps=0.05):
    logits = masked_logits(res, batch)
    logp = F.log_softmax(logits, dim=-1)
    nll = -logp.gather(-1, batch["y"][:, None, None].expand(-1, logp.shape[1], -1)[..., :1]).squeeze(-1)
    alive_b = (batch["alive"] > 0).float()
    smooth = -(logp.clamp(min=-1e9) * alive_b).sum(-1) / alive_b.sum(-1).clamp(min=1)
    per_pos = (1 - eps) * nll + eps * smooth
    wmask = batch["posmask"].float() * batch["w"][:, None]
    return (per_pos * wmask).sum() / wmask.sum().clamp(min=1)


@torch.no_grad()
def snapshot_eval(model, data, idxs, batch_size=256, with_prior_ref=False, rscale=1.0):
    """街末快照指标。返回 per_street dict + 逐样本命中/NLL 矩阵（配对检验用）。
    rscale：残差温度（logits = log_prior + rscale·f_θ），<1 向 B2b 插值——q 失准的
    校准旋钮。"""
    model.eval()
    hit5 = np.full((len(idxs), 4), -1, dtype=np.int8)
    nll = np.full((len(idxs), 4), np.nan, dtype=np.float64)
    qpct = np.full((len(idxs), 4), np.nan, dtype=np.float64)
    ref_hit5 = np.full((len(idxs), 4), -1, dtype=np.int8)
    ref_nll = np.full((len(idxs), 4), np.nan, dtype=np.float64)
    ref_qpct = np.full((len(idxs), 4), np.nan, dtype=np.float64)
    ranks = np.full((len(idxs), 4), -1, dtype=np.int32)
    arange = np.arange(169)
    for lo in range(0, len(idxs), batch_size):
        sub = idxs[lo:lo + batch_size]
        batch = data.batch(sub)
        res = model(batch["feat"], batch["lens"])
        logits = masked_logits(res, batch, rscale).numpy()
        logits0 = masked_logits(torch.zeros_like(res), batch).numpy()
        for j, i in enumerate(sub):
            for S in range(4):
                sp = data.snaps[i, S]
                if sp < 0:
                    continue
                row = lo + j
                av = data.alive[i, S].astype(np.float64)
                for mat_h, mat_n, mat_q, lg in ((hit5, nll, qpct, logits),
                                                (ref_hit5, ref_nll, ref_qpct, logits0)):
                    if mat_h is ref_hit5 and not with_prior_ref:
                        continue
                    z = lg[j, sp]
                    z = z - z.max()
                    p = np.exp(z)
                    p /= p.sum()
                    order = np.lexsort((arange, -p))
                    rk = int(np.nonzero(order == data.y[i])[0][0])
                    mat_h[row, S] = rk < 5
                    mat_n[row, S] = -math.log(max(p[data.y[i]], 1e-12))
                    # range 质量分位：per-combo 密度降序累计预测质量到标签类，
                    # 类内/平局取中点（越小 = 亮牌越靠 range 头部；均匀预测期望 50%）
                    dens = np.divide(p, av, out=np.full(169, -1.0), where=av > 0)
                    dy = dens[data.y[i]]
                    mat_q[row, S] = p[dens > dy].sum() + 0.5 * p[dens == dy].sum()
                    if mat_h is hit5:
                        ranks[row, S] = rk
    out = {}
    for S in range(4):
        m = hit5[:, S] >= 0
        if not m.any():
            continue
        out[S] = {"n": int(m.sum()), "top5": float((hit5[m, S] == 1).mean()),
                  "top1": float((ranks[m, S] < 1).mean()),
                  "top10": float((ranks[m, S] < 10).mean()),
                  "nll": float(nll[m, S].mean()),
                  "rank_mean": float(ranks[m, S].mean() + 1),
                  "rank_med": int(np.median(ranks[m, S]) + 1),
                  "q_mean": float(qpct[m, S].mean()), "q_med": float(np.median(qpct[m, S]))}
        if with_prior_ref:
            out[S]["ref_top5"] = float((ref_hit5[m, S] == 1).mean())
            out[S]["ref_nll"] = float(ref_nll[m, S].mean())
            out[S]["ref_q_mean"] = float(ref_qpct[m, S].mean())
            out[S]["ref_q_med"] = float(np.median(ref_qpct[m, S]))
    return out, (hit5, nll, ref_hit5, ref_nll, qpct, ref_qpct)


def paired_bootstrap(hit_a, hit_b, n_boot=10000, seed=7):
    """配对 Δ=mean(a)−mean(b) 的 bootstrap CI 与双侧 p（H0: Δ≤0 的符号翻转近似）。"""
    rng = np.random.default_rng(seed)
    diff = hit_a.astype(np.float64) - hit_b.astype(np.float64)
    n = len(diff)
    if n == 0:
        return 0.0, (0.0, 0.0), 1.0
    boots = np.empty(n_boot)
    for b in range(n_boot):
        boots[b] = diff[rng.integers(0, n, n)].mean()
    lo, hi = np.percentile(boots, [2.5, 97.5])
    p = 2 * min((boots <= 0).mean(), (boots >= 0).mean())
    return float(diff.mean()), (float(lo), float(hi)), float(max(p, 1.0 / n_boot))


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--ds", default="artifacts/range_ml")
    ap.add_argument("--ckpt", default="artifacts/range_ml/range_v1_gru.pt")
    ap.add_argument("--report", default="artifacts/range_ml/m1_report.md")
    ap.add_argument("--d", type=int, default=192)
    ap.add_argument("--dropout", type=float, default=0.15)
    ap.add_argument("--epochs", type=int, default=80)
    ap.add_argument("--batch", type=int, default=96)
    ap.add_argument("--lr", type=float, default=2e-3)
    ap.add_argument("--wd", type=float, default=1e-4)
    ap.add_argument("--eps", type=float, default=0.05, help="label smoothing")
    ap.add_argument("--patience", type=int, default=12)
    ap.add_argument("--seed", type=int, default=20260702)
    ap.add_argument("--src-filter", default="", help="消融：只用这些源训练，如 live_opp")
    ap.add_argument("--eval-only", action="store_true")
    ap.add_argument("--residual-scale", type=float, default=1.0,
                    help="评估期残差温度（<1 向 B2b 插值，校准 q 用）")
    args = ap.parse_args()

    torch.manual_seed(args.seed)
    torch.set_num_threads(2)
    rng = np.random.default_rng(args.seed)

    data = SeqData(os.path.join(args.ds, "seq_v1.npz"))
    src_names = {0: "live_opp", 1: "live_hero", 2: "slumbot"}
    train_mask = data.split == 0
    if args.src_filter:
        want = {k for k, v in src_names.items() if v in args.src_filter.split(",")}
        train_mask &= np.isin(data.src, list(want))
    train_idx = np.nonzero(train_mask)[0]
    val_idx = np.nonzero((data.split == 1) & (data.src == 0))[0]
    test_idx = np.nonzero((data.split == 2) & (data.src == 0))[0]
    print(f"[train] sequences: train={len(train_idx)} val={len(val_idx)} test={len(test_idx)} "
          f"fdim={data.fdim}", flush=True)

    model = ResidualGRU(data.fdim, d=args.d, dropout=args.dropout)
    n_params = sum(p.numel() for p in model.parameters())
    print(f"[train] params={n_params/1e6:.2f}M", flush=True)

    if not args.eval_only:
        opt = torch.optim.AdamW(model.parameters(), lr=args.lr, weight_decay=args.wd)
        sched = torch.optim.lr_scheduler.CosineAnnealingLR(opt, T_max=args.epochs)
        # 按长度分桶再洗牌，减 padding
        order = train_idx[np.argsort(data.lens[train_idx], kind="stable")]
        batches = [order[i:i + args.batch] for i in range(0, len(order), args.batch)]
        best_nll, best_ep, bad = float("inf"), -1, 0
        for ep in range(args.epochs):
            model.train()
            t0 = time.time()
            rng.shuffle(batches)
            tot = cnt = 0.0
            for bidx in batches:
                batch = data.batch(bidx)
                loss = loss_fn(model(batch["feat"], batch["lens"]), batch, eps=args.eps)
                opt.zero_grad()
                loss.backward()
                nn.utils.clip_grad_norm_(model.parameters(), 1.0)
                opt.step()
                tot += float(loss.detach()) * len(bidx)
                cnt += len(bidx)
            sched.step()
            met, _ = snapshot_eval(model, data, val_idx)
            val_nll = np.mean([met[S]["nll"] for S in met])
            val_top5 = {S: met[S]["top5"] for S in met}
            marker = ""
            if val_nll < best_nll - 1e-4:
                best_nll, best_ep, bad = val_nll, ep, 0
                torch.save({"model": model.state_dict(), "fdim": data.fdim,
                            "d": args.d, "args": vars(args), "epoch": ep,
                            "val_nll": val_nll}, args.ckpt)
                marker = " *best"
            else:
                bad += 1
            print(f"[train] ep{ep:03d} loss={tot/cnt:.4f} val_snapNLL={val_nll:.4f} "
                  f"val_top5={[round(val_top5.get(S, 0), 3) for S in range(4)]} "
                  f"({time.time()-t0:.0f}s){marker}", flush=True)
            if bad >= args.patience:
                print(f"[train] early stop at ep{ep} (best ep{best_ep} NLL {best_nll:.4f})", flush=True)
                break

    ck = torch.load(args.ckpt, map_location="cpu", weights_only=False)
    model = ResidualGRU(ck["fdim"], d=ck["d"], dropout=0.0)
    model.load_state_dict(ck["model"])
    print(f"[eval] loaded {args.ckpt} (ep{ck['epoch']}, val_snapNLL={ck['val_nll']:.4f})", flush=True)

    ta = ck.get("args", vars(args))  # 训练时超参（eval-only 也如实报告）
    report = ["# M1 v1 序列模型报告（GRU 先验残差）", "",
              f"- 模型：GRU d={ck['d']}，参数 {n_params/1e6:.2f}M；ckpt `{args.ckpt}`（best ep{ck['epoch']}）",
              f"- 训练：{len(train_idx)} 序列（源权重 live_opp 1.0/hero 0.3/slumbot 0.1"
              f"{'; src_filter=' + ta['src_filter'] if ta.get('src_filter') else ''}），"
              f"lr={ta['lr']} wd={ta['wd']} smooth={ta['eps']} dropout={ta['dropout']} "
              f"batch={ta['batch']} seed={ta['seed']}",
              "- 口径：与 M0 harness 同（4 街末快照、top-k 平局按 (-p,k) 破、alive=观察者死牌）",
              ""]
    if args.residual_scale != 1.0:
        report.insert(3, f"- **残差温度 rscale={args.residual_scale}**（评估期向 B2b 插值）")
    final = {}
    for name, idxs in (("val", val_idx), ("test", test_idx)):
        met, mats = snapshot_eval(model, data, idxs, with_prior_ref=True,
                                  rscale=args.residual_scale)
        final[name] = (met, mats)
        report += [f"### {name} 切分（live_opp，n={len(idxs)}）", "",
                   "| 街 | top-1 | **top-5** | top-10 | NLL | rank 均值 | rank 中位 | "
                   "**q 中位** | q 均值 | B2b top-5 | B2b NLL | B2b q 中位 | Δtop-5 |",
                   "|---|---|---|---|---|---|---|---|---|---|---|---|---|"]
        for S in range(4):
            m = met.get(S)
            if not m:
                continue
            report.append(f"| {STREET_NAME[S]} | {m['top1']:.3f} | **{m['top5']:.3f}** | "
                          f"{m['top10']:.3f} | {m['nll']:.3f} | {m['rank_mean']:.1f} | {m['rank_med']} | "
                          f"**{m['q_med']*100:.1f}%** | {m['q_mean']*100:.1f}% | "
                          f"{m['ref_top5']:.3f} | {m['ref_nll']:.3f} | {m['ref_q_med']*100:.1f}% | "
                          f"{m['top5']-m['ref_top5']:+.3f} |")
        report.append("")

    met, (hit5, nllm, ref_hit5, ref_nllm, qpctm, ref_qpctm) = final["test"]
    report += ["### 出闸检验（test）", ""]
    vals = [met[S]["top5"] for S in sorted(met)]
    mono = all(vals[i] <= vals[i + 1] + 1e-9 for i in range(len(vals) - 1))
    report.append(f"- 单调断言（top-5 逐街）：{' → '.join(f'{v:.3f}' for v in vals)} "
                  f"{'✅ 单调' if mono else '⚠ 非单调'}")
    for S in sorted(met):
        m = hit5[:, S] >= 0
        d, (lo, hi), p = paired_bootstrap(hit5[m, S], ref_hit5[m, S])
        dn, (nlo, nhi), pn = paired_bootstrap(-nllm[m, S], -ref_nllm[m, S])
        dq, (qlo, qhi), pq = paired_bootstrap(-qpctm[m, S], -ref_qpctm[m, S])
        report.append(f"- {STREET_NAME[S]}：Δtop-5 = {d:+.3f} [{lo:+.3f}, {hi:+.3f}] p={p:.4f}；"
                      f"ΔNLL = {-dn:+.3f} [{-nhi:+.3f}, {-nlo:+.3f}] p={pn:.4f}；"
                      f"Δq = {-dq*100:+.1f}pp [{-qhi*100:+.1f}, {-qlo*100:+.1f}] p={pq:.4f}")
    # 后街斜率：river−preflop 的 top-5 差距（仅打满 4 街的样本，配对）
    full = (hit5[:, 0] >= 0) & (hit5[:, 3] >= 0)
    slope_m = hit5[full, 3].astype(float) - hit5[full, 0].astype(float)
    slope_r = ref_hit5[full, 3].astype(float) - ref_hit5[full, 0].astype(float)
    d, (lo, hi), p = paired_bootstrap(slope_m, slope_r)
    report.append(f"- 后街斜率（river−preflop top-5，n={int(full.sum())}）：模型 "
                  f"{slope_m.mean():+.3f} vs B2b {slope_r.mean():+.3f}，"
                  f"Δ = {d:+.3f} [{lo:+.3f}, {hi:+.3f}] p={p:.4f}")
    report.append("")

    with open(args.report, "w") as f:
        f.write("\n".join(report))
    print(f"[eval] 报告 → {args.report}", flush=True)
    for line in report[-12:]:
        print(line)


if __name__ == "__main__":
    main()
