#!/usr/bin/env python3
"""M3 影子模式：在 live 数据上跑 range 预测（docs/opponent_range_ml_design.md §7/§9 M3）。

零 EV 风险——不改 live 决策路径（决策仍出自 Rust advisor）。以摊牌亮牌为标签，
在每个街末快照复算 v1 GRU 预测，输出：
  1. 影子命中报告（分街 top-5/NLL/rank/q），验证「复现离线指标」出闸条件；
  2. 每样本 `range_pred` 侧车记录（top-5 类 + 命中 + rank + q），既是在线监控口径，
     也随新 run 积累标签。

**手写前向（numpy）**：把 `ResidualGRU`（输入线性+tanh → 单层 GRU → 零残差头）逐义
移植成纯 numpy 前向，`--parity-torch` 对 torch 原版逐元素对拍（<1e-3）。这是 §6 golden
对拍的 Python 侧参考实现——待 multi_field §2 Phase 3（Rust `RangeVec`/`equity_weighted`
+ P0-1 动作线）落地后，Rust 手写前向对拍的就是它。

两个样本源（同一前向 + 同一指标口径）：
  --from-npz（默认 --verify 用）：读 seq_v1.npz 的 test/val 切分（== run29,30 / run28），
    与训练 eval 完全同输入 → 复现 m1_report 逐位（唯一差异 = numpy vs torch 前向，
    已被 --parity-torch 界定）。证「影子命中复现离线指标」。
  --from-runs（新 run 的真实影子路径）：用 range_dataset_build.parse_live_run 因果重放
    profiles 在内存重建样本（与 live_opp.jsonl 同代码路径），SampleFeaturizer 现场特征化
    → 对 run31+ 未见数据出真实部署分布命中率 + 发 range_pred。

用法：
  .venv-rangeml/bin/python tools/range_shadow.py --verify                 # 复现离线指标（test/val）
  .venv-rangeml/bin/python tools/range_shadow.py --parity-torch           # numpy↔torch 前向对拍
  .venv-rangeml/bin/python tools/range_shadow.py --from-runs run29,run30 \
        --emit artifacts/range_ml/shadow_run2930.jsonl                    # 现场特征化 + 发 range_pred
  .venv-rangeml/bin/python tools/range_shadow.py --from-runs auto ...     # 全量 raw run（含新 run）
"""

import argparse
import glob
import json
import math
import os
import re
import sys

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from range_ml_common import build_strength_table, class_name  # noqa: E402
from range_baselines import B2_PARAMS, B2RangeVec  # noqa: E402
from range_featurize import SampleFeaturizer  # noqa: E402

STREET_NAME = {0: "preflop", 1: "flop", 2: "turn", 3: "river"}
# m1_report test 切分 rscale=0.8 逐街 top-5（复现基准；改 ckpt/rscale 时同步更新）
M1_TEST_TOP5 = {0: 0.163, 1: 0.172, 2: 0.178, 3: 0.183}
M1_VAL_TOP5 = {0: 0.149, 1: 0.154, 2: 0.156, 3: 0.163}


# ---------------------------------------------------------------------------
# 手写前向（numpy）——逐义移植 range_train.ResidualGRU（use_shows=False）
# ---------------------------------------------------------------------------


def _sigmoid(x):
    return 1.0 / (1.0 + np.exp(-x))


class NpResidualGRU:
    """纯 numpy 单层 GRU 先验残差前向；权重从 range_v1_gru.pt 抽出（f64）。"""

    def __init__(self, ckpt_path):
        import torch  # 仅用于读取 state_dict，前向不依赖 torch

        ck = torch.load(ckpt_path, map_location="cpu", weights_only=False)
        assert not ck.get("use_shows"), "v1 手写前向不含亮牌记忆通道（use_shows）"
        sd = ck["model"]
        f = lambda k: sd[k].detach().numpy().astype(np.float64)  # noqa: E731
        self.fdim = int(ck["fdim"])
        self.d = int(ck["d"])
        self.inp_w, self.inp_b = f("inp.weight"), f("inp.bias")          # [d,fdim],[d]
        self.Wih, self.Whh = f("gru.weight_ih_l0"), f("gru.weight_hh_l0")  # [3d,d]
        self.bih, self.bhh = f("gru.bias_ih_l0"), f("gru.bias_hh_l0")    # [3d]
        self.head_w, self.head_b = f("head.weight"), f("head.bias")      # [169,d],[169]
        self.epoch = ck.get("epoch")
        self.val_nll = ck.get("val_nll")

    def residual(self, feat):
        """feat [T,fdim] → 残差 logits [T,169]（因果，与 pack_padded 单序列等价）。"""
        d = self.d
        x = np.tanh(feat.astype(np.float64) @ self.inp_w.T + self.inp_b)  # [T,d]
        gi = x @ self.Wih.T + self.bih                                    # [T,3d] 预算输入门
        h = np.zeros(d, dtype=np.float64)
        H = np.empty((feat.shape[0], d), dtype=np.float64)
        for t in range(feat.shape[0]):
            gh = h @ self.Whh.T + self.bhh                               # [3d]
            ir, iz, in_ = gi[t, :d], gi[t, d:2 * d], gi[t, 2 * d:]
            hr, hz, hn = gh[:d], gh[d:2 * d], gh[2 * d:]
            r = _sigmoid(ir + hr)
            z = _sigmoid(iz + hz)
            n = np.tanh(in_ + r * hn)
            h = (1.0 - z) * n + z * h
            H[t] = h
        return H @ self.head_w.T + self.head_b                           # [T,169]


# ---------------------------------------------------------------------------
# 街末快照指标（逐义复刻 range_train.snapshot_eval 单样本口径）
# ---------------------------------------------------------------------------

_ARANGE = np.arange(169)


def snapshot_metrics(logprior_sp, res_sp, alive_S, y, rscale):
    """单个街末快照 → 指标 dict（与 snapshot_eval 逐位一致）。
    logprior_sp/res_sp：该快照 token 位的 log 先验 / 残差 [169]；alive_S：街 S 存活数 [169]。"""
    av = alive_S.astype(np.float64)
    la = np.log(np.maximum(av, 1.0))
    dead = av == 0
    logits = logprior_sp + rscale * res_sp + la
    logits = np.where(dead, -1e30, logits)
    z = logits - logits.max()
    p = np.exp(z)
    p /= p.sum()
    order = np.lexsort((_ARANGE, -p))                 # (-p,k) 破平局，同 M0 harness
    rk = int(np.nonzero(order == y)[0][0])
    dens = np.divide(p, av, out=np.full(169, -1.0), where=av > 0)
    dy = dens[y]
    q = float(p[dens > dy].sum() + 0.5 * p[dens == dy].sum())
    return {
        "rank": rk, "hit5": rk < 5, "hit1": rk < 1, "hit10": rk < 10,
        "nll": -math.log(max(float(p[y]), 1e-12)), "q": q,
        "top5": [int(c) for c in order[:5]],
    }


def aggregate(rows):
    """rows: list of (S, metric_dict) → per_street 汇总（top-5/top-1/NLL/rank/q）。"""
    out = {}
    for S in range(4):
        ms = [m for s, m in rows if s == S]
        if not ms:
            continue
        out[S] = {
            "n": len(ms),
            "top5": float(np.mean([m["hit5"] for m in ms])),
            "top1": float(np.mean([m["hit1"] for m in ms])),
            "top10": float(np.mean([m["hit10"] for m in ms])),
            "nll": float(np.mean([m["nll"] for m in ms])),
            "rank_mean": float(np.mean([m["rank"] for m in ms]) + 1),
            "rank_med": int(np.median([m["rank"] for m in ms]) + 1),
            "q_mean": float(np.mean([m["q"] for m in ms])),
            "q_med": float(np.median([m["q"] for m in ms])),
        }
    return out


# ---------------------------------------------------------------------------
# 样本源 A：seq_v1.npz（复现离线指标）
# ---------------------------------------------------------------------------


def eval_from_npz(model, ds_dir, want_split, rscale):
    """读 seq_v1.npz，对给定切分（1=val/run28，2=test/run29,30）逐样本跑手写前向。
    输入与训练 eval 完全一致（f16 特征/先验）→ 复现 m1_report。返回 (per_street, rows)。"""
    d = np.load(os.path.join(ds_dir, "seq_v1.npz"))
    ptr, snaps, split, src = d["ptr"], d["snaps"], d["split"], d["src"]
    prior, pidx, alive, y = d["prior"].astype(np.float32), d["pidx"], d["alive"], d["y"]
    feat = d["feat"]
    sel = np.nonzero((split == want_split) & (src == 0))[0]
    rows = []
    for i in sel:
        a, b = ptr[i], ptr[i + 1]
        res = model.residual(feat[a:b].astype(np.float32))               # [L,169]
        lp = prior[pidx[a:b]]                                            # [L,169] per-token log prior
        for S in range(4):
            sp = int(snaps[i, S])
            if sp < 0:
                continue
            m = snapshot_metrics(lp[sp].astype(np.float64), res[sp],
                                 alive[i, S], int(y[i]), rscale)
            rows.append((S, m))
    return aggregate(rows), rows, len(sel)


# ---------------------------------------------------------------------------
# 样本源 B：raw run 因果重建 + 现场特征化（真实影子路径 / 新 run）
# ---------------------------------------------------------------------------


class _Collector:
    """假 writer：收集 finalize_live_hand 写出的样本行（保留 run 字段供筛选）。"""

    def __init__(self):
        self.rows = []

    def write(self, s):
        self.rows.append(json.loads(s))


class _Sink:
    def write(self, s):
        pass


def samples_from_runs(run_spec):
    """按 run 升序因果重放所有 raw run（profiles 累积），返回全部 live_opp 样本行。
    与 range_dataset_build 同代码路径 → 与 live_opp.jsonl 逐手一致；新 run 落 raw 即被捡起。"""
    from collections import Counter, defaultdict
    from range_dataset_build import parse_live_run

    if run_spec == "auto":
        run_files = sorted(glob.glob("h9_live_run*.raw.jsonl"),
                           key=lambda p: int(re.search(r"run(\d+)", p).group(1)))
    else:
        run_files = [f"h9_live_run{r.strip().replace('run', '')}.raw.jsonl"
                     for r in run_spec.split(",")]
        # 因果正确性：目标 run 之前的所有 run 也要重放以喂饱 profiles。
        max_run = max(int(re.search(r"run(\d+)", rf).group(1)) for rf in run_files)
        run_files = sorted(glob.glob("h9_live_run*.raw.jsonl"),
                           key=lambda p: int(re.search(r"run(\d+)", p).group(1)))
        run_files = [rf for rf in run_files
                     if int(re.search(r"run(\d+)", rf).group(1)) <= max_run]
    collector = _Collector()
    writers = {"live_opp": collector, "live_hero": _Sink()}
    audit = defaultdict(int)
    audit["board_len"] = Counter()
    audit["opp_counter"] = Counter()
    audit["label_hist"] = Counter()
    profiles = {}
    for rf in run_files:
        if not os.path.exists(rf):
            print(f"[shadow] 缺 raw 文件 {rf}，跳过", file=sys.stderr)
            continue
        run_tag = re.search(r"(run\d+)", rf).group(1)
        parse_live_run(rf, run_tag, profiles, writers, audit)
    return collector.rows


def load_canonical_keys(ds_dir):
    """已进 M1 数据集的 (hid,tseat)（live_opp.jsonl）→ 用于把影子样本拆
    「已评估（复现口径）」vs「新样本（真实部署分布）」。缺文件则全记为新。"""
    path = os.path.join(ds_dir, "live_opp.jsonl")
    keys = set()
    if os.path.exists(path):
        with open(path) as f:
            for line in f:
                s = json.loads(line)
                keys.add((s["hid"], s["tseat"]))
    return keys


def eval_from_samples(model, samples, want_runs, rscale, featurizer, canon_keys):
    """现场特征化目标 run 的样本 → 手写前向 → 街末指标 + range_pred 记录。
    canon_keys：已进 M1 数据集的 (hid,tseat)，据此给每条记录打 seen/new。"""
    per_sample = []
    rows_seen, rows_new = [], []
    for s in samples:
        if want_runs and s.get("run") not in want_runs:
            continue
        feat, streets, pidxs, priors, alive, snaps, _b3, _trunc = featurizer.run(s)
        res = model.residual(feat.astype(np.float32))
        y = int(s["y"])
        seen = (s.get("hid"), s.get("tseat")) in canon_keys
        bucket = rows_seen if seen else rows_new
        pred = {}
        for S in range(4):
            sp = int(snaps[S])
            if sp < 0:
                continue
            lp = np.asarray(priors[pidxs[sp]], dtype=np.float64)
            m = snapshot_metrics(lp, res[sp], alive[S], y, rscale)
            bucket.append((S, m))
            pred[STREET_NAME[S]] = {
                "top5": m["top5"], "top5_names": [class_name(c) for c in m["top5"]],
                "hit5": bool(m["hit5"]), "rank": m["rank"] + 1,
                "nll": round(m["nll"], 4), "q": round(m["q"], 4),
            }
        per_sample.append({
            "kind": "range_pred", "run": s.get("run"), "hid": s.get("hid"),
            "tgt": s.get("tgt"), "tseat": s.get("tseat"), "seen": seen,
            "y": y, "label": s.get("label"), "class_name": class_name(y),
            "rscale": rscale, "ckpt": "range_v1_gru.pt", "per_street": pred,
        })
    return rows_seen, rows_new, per_sample


# ---------------------------------------------------------------------------
# parity：numpy 前向 ↔ torch 前向逐元素对拍
# ---------------------------------------------------------------------------


def parity_torch(model, ds_dir, ckpt_path, n=64):
    import torch
    from range_train import ResidualGRU, SeqData

    ck = torch.load(ckpt_path, map_location="cpu", weights_only=False)
    tm = ResidualGRU(ck["fdim"], d=ck["d"], dropout=0.0, use_shows=False)
    tm.load_state_dict(ck["model"])
    tm.eval()
    data = SeqData(os.path.join(ds_dir, "seq_v1.npz"))
    idxs = np.nonzero((data.split == 2) & (data.src == 0))[0][:n]
    max_abs = 0.0
    with torch.no_grad():
        for i in idxs:
            a, b = data.ptr[i], data.ptr[i + 1]
            fi = data.feat[a:b].astype(np.float32)
            np_res = model.residual(fi)
            t_res = tm(torch.from_numpy(fi[None]),
                       torch.tensor([b - a]))[0].numpy()
            max_abs = max(max_abs, float(np.abs(np_res - t_res).max()))
    return max_abs, len(idxs)


# ---------------------------------------------------------------------------
# 报告
# ---------------------------------------------------------------------------


def render_report(title, sections, extra=None):
    lines = [f"# {title}", ""]
    if extra:
        lines += extra + [""]
    for name, per_street, n_samp, ref in sections:
        lines += [f"### {name}（live_opp，n={n_samp}）", "",
                  "| 街 | top-1 | **top-5** | top-10 | NLL | rank 均值 | rank 中位 | q 中位 | q 均值 |"
                  + (" 离线基准 top-5 | Δ |" if ref else ""),
                  "|---|---|---|---|---|---|---|---|---|" + ("---|---|" if ref else "")]
        for S in range(4):
            m = per_street.get(S)
            if not m:
                continue
            row = (f"| {STREET_NAME[S]} | {m['top1']:.3f} | **{m['top5']:.3f}** | "
                   f"{m['top10']:.3f} | {m['nll']:.3f} | {m['rank_mean']:.1f} | "
                   f"{m['rank_med']} | {m['q_med']*100:.1f}% | {m['q_mean']*100:.1f}% |")
            if ref:
                row += f" {ref[S]:.3f} | {m['top5']-ref[S]:+.3f} |"
            lines.append(row)
        lines.append("")
    return "\n".join(lines)


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--ds", default="artifacts/range_ml")
    ap.add_argument("--ckpt", default="artifacts/range_ml/range_v1_gru.pt")
    ap.add_argument("--rscale", type=float, default=0.8, help="残差温度（生产配置 0.8）")
    ap.add_argument("--verify", action="store_true",
                    help="从 seq_v1.npz 复现 val/test 离线指标并断言接近 m1_report")
    ap.add_argument("--parity-torch", action="store_true",
                    help="numpy 手写前向 ↔ torch 原版逐元素对拍")
    ap.add_argument("--from-runs", default="",
                    help="raw run 现场影子：逗号分隔 run（run29,run30）或 auto（全量）")
    ap.add_argument("--emit", default="",
                    help="range_pred 侧车输出路径（--from-runs 时）")
    ap.add_argument("--report", default="artifacts/range_ml/m3_shadow_report.md")
    ap.add_argument("--tol", type=float, default=0.006,
                    help="--verify 逐街 top-5 与离线基准的容差")
    args = ap.parse_args()

    model = NpResidualGRU(args.ckpt)
    print(f"[shadow] ckpt={args.ckpt} (ep{model.epoch}, val_nll={model.val_nll:.4f}) "
          f"fdim={model.fdim} d={model.d} rscale={args.rscale}", flush=True)

    if args.parity_torch:
        max_abs, n = parity_torch(model, args.ds, args.ckpt)
        ok = max_abs < 1e-3
        print(f"[parity] numpy vs torch 残差 logits，{n} 样本：max|Δ|={max_abs:.2e} "
              f"{'✅ <1e-3' if ok else '❌ ≥1e-3'}", flush=True)
        if not ok:
            sys.exit(1)
        if not (args.verify or args.from_runs):
            return

    if args.from_runs:
        strength = build_strength_table(cache_path=os.path.join(args.ds, "strength169.json"))
        b2 = B2RangeVec(strength, B2_PARAMS, use_ops=True)
        fz = SampleFeaturizer(b2, {})
        samples = samples_from_runs(args.from_runs)
        want = None if args.from_runs == "auto" else {
            f"run{r.strip().replace('run', '')}" for r in args.from_runs.split(",")}
        canon = load_canonical_keys(args.ds)
        rows_seen, rows_new, per_sample = eval_from_samples(
            model, samples, want, args.rscale, fz, canon)
        n = len(per_sample)
        n_new = sum(1 for p in per_sample if not p["seen"])
        runs_shown = sorted({p["run"] for p in per_sample})
        seen_ps, new_ps = aggregate(rows_seen), aggregate(rows_new)
        print(f"[shadow] from-runs {runs_shown}: {n} 亮牌样本"
              f"（seen {n - n_new} / 新 {n_new}）", flush=True)
        # seen 样本 = 已在 M1 数据集。仅当 seen 恰为完整 test 集（run29+run30）时对离线
        # 基准给 Δ（现场 f32 vs npz f16 几手漂移）；run30-only 等子集与聚合基准不同分母，
        # 不给误导性 Δ——权威复现口径见 --verify。
        seen_ref = M1_TEST_TOP5 if want == {"run29", "run30"} else None
        sections = []
        if rows_seen:
            sections.append(("seen（已在 M1 数据集）", seen_ps,
                             sum(1 for p in per_sample if p["seen"]), seen_ref))
        if rows_new:
            sections.append(("new（新样本，真实部署分布影子）", new_ps, n_new, None))
        report = render_report(
            "M3 影子模式报告（现场特征化 raw run）", sections,
            extra=[f"- 源：raw run 因果重建（range_dataset_build.parse_live_run）+ "
                   f"现场 SampleFeaturizer；手写 numpy 前向；rscale={args.rscale}",
                   f"- ckpt `{args.ckpt}`（ep{model.epoch}）；runs={runs_shown}",
                   "- seen = 已进 M1 seq_v1.npz 的 (hid,tseat)，与离线 test 同口径（复现校验）；"
                   "new = raw 增长/新 run 的未见亮牌手（真实部署分布命中率）"])
        with open(args.report, "w") as f:
            f.write(report)
        print(f"[shadow] 报告 → {args.report}", flush=True)
        for label, ps, ref in (("seen", seen_ps, seen_ref), ("new", new_ps, None)):
            if not ps:
                continue
            for S in range(4):
                m = ps.get(S)
                if not m:
                    continue
                extra = f"（基准 {ref[S]:.3f}, Δ{m['top5']-ref[S]:+.3f}）" if ref else ""
                print(f"  [{label:4s}] {STREET_NAME[S]:8s} top5={m['top5']:.3f} "
                      f"nll={m['nll']:.3f} q_med={m['q_med']*100:.1f}% n={m['n']}{extra}")
        if args.emit:
            with open(args.emit, "w") as f:
                for p in per_sample:
                    f.write(json.dumps(p, ensure_ascii=False) + "\n")
            print(f"[shadow] {n} 条 range_pred → {args.emit}", flush=True)
        return

    if args.verify:
        sections = []
        failed = False
        for split, name, ref in ((1, "val (run28)", M1_VAL_TOP5),
                                 (2, "test (run29,30)", M1_TEST_TOP5)):
            per_street, _rows, n = eval_from_npz(model, args.ds, split, args.rscale)
            sections.append((name, per_street, n, ref))
            for S in range(4):
                m = per_street.get(S)
                if not m:
                    continue
                d = abs(m["top5"] - ref[S])
                tag = "✅" if d <= args.tol else "❌"
                if d > args.tol:
                    failed = True
                print(f"  {name:16s} {STREET_NAME[S]:8s} top5={m['top5']:.3f} "
                      f"(基准 {ref[S]:.3f}, Δ{m['top5']-ref[S]:+.3f}) {tag}")
        report = render_report(
            "M3 影子复现验证（seq_v1.npz，手写 numpy 前向）",
            sections,
            extra=[f"- 手写 numpy 前向复现 range_train eval 口径；rscale={args.rscale}；"
                   f"基准 = m1_report 逐街 top-5",
                   f"- ckpt `{args.ckpt}`（ep{model.epoch}, val_nll={model.val_nll:.4f}）"])
        with open(args.report, "w") as f:
            f.write(report)
        print(f"[verify] 报告 → {args.report}", flush=True)
        if failed:
            print("[verify] ❌ 某街 top-5 超出容差 → 复现失败", flush=True)
            sys.exit(1)
        print(f"[verify] ✅ val/test 逐街 top-5 均在 ±{args.tol} 内复现离线指标", flush=True)
        return

    ap.error("需指定 --verify / --parity-torch / --from-runs 之一")


if __name__ == "__main__":
    main()
