#!/usr/bin/env python3
"""M2 亮牌记忆侧车：dataset 的 prof.shows（该对手开手前最近 ≤20 次亮牌的 169 类，
严格因果快照）→ shows_v1.npz，与 seq_v1.npz 全样本按序对齐（featurize 同款过滤，
y 逐行对拍）。K=20 定长，pad=-1，序 = 旧→新。

v2 备注（§5）：完整设计是 (线摘要嵌入, 169 类) 对——dataset 目前只存类索引，
线摘要待 dataset schema 扩展；本侧车先变现「它亮过什么牌」的类信号。

用法：.venv-rangeml/bin/python tools/range_shows_sidecar.py   # ~30s
"""

import argparse
import json
import os

import numpy as np

K = 20


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--ds", default="artifacts/range_ml")
    ap.add_argument("--out", default="shows_v1.npz")
    ap.add_argument("--train-runs", default="run24,run25,run26,run27")
    ap.add_argument("--val-runs", default="run28")
    ap.add_argument("--test-runs", default="run29,run30")
    args = ap.parse_args()

    splits = {"train": set(args.train_runs.split(",")),
              "val": set(args.val_runs.split(",")),
              "test": set(args.test_runs.split(","))}
    seq = np.load(os.path.join(args.ds, "seq_v1.npz"))
    seq_y = seq["y"]

    shows = []
    ys = []
    for src, fname in ((0, "live_opp.jsonl"), (1, "live_hero.jsonl"), (2, "slumbot.jsonl")):
        with open(os.path.join(args.ds, fname)) as f:
            for line in f:
                s = json.loads(line)
                run = s.get("run", "")
                if src == 0:
                    if not any(run in v for v in splits.values()):
                        continue
                elif src == 1 and run not in splits["train"]:
                    continue
                row = np.full(K, -1, dtype=np.int16)
                sh = ((s.get("prof") or {}).get("shows") or [])[-K:]
                if sh:
                    row[:len(sh)] = sh
                shows.append(row)
                ys.append(s["y"])
    shows = np.stack(shows)
    ys = np.asarray(ys, dtype=np.int16)
    assert len(ys) == len(seq_y) and (ys == seq_y).all(), "与 seq_v1.npz 对拍失败"
    out = os.path.join(args.ds, args.out)
    np.savez_compressed(out, shows=shows, y=ys)
    nz = (shows[:, 0] >= 0).sum()
    print(f"[shows] {len(ys)} 行（有亮牌记忆 {nz}，均值条数 "
          f"{(shows >= 0).sum() / max(1, nz):.1f}）→ {out}")


if __name__ == "__main__":
    main()
