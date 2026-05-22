#!/usr/bin/env python3
"""
比较多个 bucket_table v3 artifact（不同 training seed）的聚类稳定性。

输出 markdown 报告：
- 每个 artifact 的 header 摘要（schema、content_hash、bucket_count）
- 每条街每个 artifact 的 bucket size 分布（min/max/mean/std/0-size count）
- 两两 artifact 间的 Adjusted Rand Index (ARI) per street，用于评估 partition 稳定性
  - ARI = 1: 两个 partition 完全一致（cluster id 可重命名）
  - ARI = 0: 与随机分配等价
  - ARI < 0: 比随机更差（罕见）

用法：
    python3 tools/bucket_table_seed_compare.py \
        artifacts/bucket_table_..._seed_cafebabe_schemav3.bin \
        artifacts/bucket_table_..._seed_deadbeef_schemav3.bin \
        artifacts/bucket_table_..._seed_12345678_schemav3.bin

依赖：标准库 + numpy。numpy 不可用时降级到纯 Python（慢，但 river 123M
samples × 4 artifacts 的 contingency 比较会显著变慢，不推荐）。
"""
from __future__ import annotations

import sys
from pathlib import Path

# 复用 bucket_table_reader 的 parse + decoder
THIS_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(THIS_DIR))
from bucket_table_reader import lookup_array, parse  # noqa: E402


def fmt_hex(h: bytes, k: int = 12) -> str:
    return h.hex()[:k] + "..." if isinstance(h, bytes) else str(h)[:k]


def bucket_size_stats(lookup_np, k: int) -> dict:
    """per-street bucket size 分布。lookup 是 np.ndarray uint32（每 canonical_id 一个 bucket id）。"""
    import numpy as np

    sizes = np.bincount(lookup_np.astype(np.int64), minlength=k)
    return {
        "min": int(sizes.min()),
        "max": int(sizes.max()),
        "mean": float(sizes.mean()),
        "std": float(sizes.std()),
        "n_empty": int((sizes == 0).sum()),
        "n_samples": int(sizes.sum()),
    }


def adjusted_rand_index(a_np, b_np, k_a: int, k_b: int) -> float:
    """Adjusted Rand Index between two partitions of the same N samples.

    Hubert & Arabie (1985) 公式：
        ARI = (sum_ij C(n_ij,2) - E) / (0.5*(sum_i C(a_i,2) + sum_j C(b_j,2)) - E)
        E = sum_i C(a_i,2) * sum_j C(b_j,2) / C(N,2)
    """
    import numpy as np

    n = len(a_np)
    assert len(b_np) == n, f"len mismatch a={len(a_np)} b={len(b_np)}"
    # joint cluster id = k_b * aa + ba ; bincount with minlength=k_a*k_b
    joint = (k_b * a_np.astype(np.int64)) + b_np.astype(np.int64)
    contingency_flat = np.bincount(joint, minlength=k_a * k_b)
    contingency = contingency_flat.reshape(k_a, k_b).astype(np.int64)

    def comb2(x):
        return x * (x - 1) // 2

    sum_comb_ij = int(comb2(contingency).sum())
    a_sums = contingency.sum(axis=1)
    b_sums = contingency.sum(axis=0)
    sum_comb_a = int(comb2(a_sums).sum())
    sum_comb_b = int(comb2(b_sums).sum())
    total_pairs = comb2(n)
    if total_pairs == 0:
        return 1.0
    expected = sum_comb_a * sum_comb_b / total_pairs
    max_index = (sum_comb_a + sum_comb_b) / 2
    if max_index == expected:
        return 1.0 if sum_comb_ij == expected else 0.0
    return float((sum_comb_ij - expected) / (max_index - expected))


def main():
    if len(sys.argv) < 3:
        print(__doc__)
        sys.exit(2)
    paths = [Path(p) for p in sys.argv[1:]]
    artifacts = []
    print(f"# bucket_table seed 对比报告\n")
    print(f"## 概览\n")
    print("| # | path | schema | feature_set_id | training_seed | content_hash (12 chars) |")
    print("|---|------|--------|----------------|---------------|-------------------------|")
    for i, p in enumerate(paths):
        buf = p.read_bytes()
        parsed = parse(buf)
        artifacts.append((p, parsed))
        h = parsed["header"]
        print(
            f"| {i} | `{p.name}` | v{h['schema_version']} "
            f"| {h['feature_set_id']} "
            f"| {h['training_seed_hex']} "
            f"| `{parsed['trailer']['blake3_hex'][:12]}...` |"
        )

    streets = ["flop", "turn", "river"]
    for street in streets:
        k = artifacts[0][1]["header"]["bucket_count"][street]
        n = artifacts[0][1]["header"]["n_canonical_observation"][street]
        print(f"\n## {street.upper()} (K={k} / N={n})\n")

        # 1) per-artifact size 分布
        print("### bucket size 分布\n")
        print("| # | min | max | mean | std | n_empty |")
        print("|---|----:|----:|-----:|----:|--------:|")
        lookups = []
        for i, (p, parsed) in enumerate(artifacts):
            lookup_np = lookup_array(parsed, street)
            lookups.append(lookup_np)
            stats = bucket_size_stats(lookup_np, k)
            print(
                f"| {i} | {stats['min']} | {stats['max']} | {stats['mean']:.1f} "
                f"| {stats['std']:.1f} | {stats['n_empty']} |"
            )

        # 2) pairwise ARI
        if len(artifacts) >= 2:
            print(f"\n### partition stability (Adjusted Rand Index)\n")
            print("| | " + " | ".join(f"#{j}" for j in range(len(artifacts))) + " |")
            print("|" + "---|" * (len(artifacts) + 1))
            for i in range(len(artifacts)):
                row = [f"#{i}"]
                for j in range(len(artifacts)):
                    if j < i:
                        row.append("-")
                    elif j == i:
                        row.append("1.000")
                    else:
                        ari = adjusted_rand_index(lookups[i], lookups[j], k, k)
                        row.append(f"{ari:.4f}")
                print("| " + " | ".join(row) + " |")

    print()


if __name__ == "__main__":
    main()
