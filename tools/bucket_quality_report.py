#!/usr/bin/env python3
"""
Bucket 质量报告生成器（C1 §输出）

`pluribus_stage2_workflow.md` §C1 §输出 line 318 字面：
    `tools/bucket_quality_report.py`：bucket 数量 / 内方差 / 间距 直方图，CI artifact 输出。

本脚本读取 bucket 质量数据（C1 阶段从 stub 路径手动喂入；C2 / D2 接入真实 mmap
bucket table 后由 `tools/bucket_table_reader.py` 在 release pipeline 自动喂入）
并输出 markdown 报告：

- bucket 数量（每条街 vs `BucketConfig` 默认 500/500/500）
- bucket 内 EHS 方差直方图 + 不变量门槛 `< 0.05` (path.md / validation §3) 命中率
- 相邻 bucket id `(k, k+1)` 间 1D EMD 直方图 + `T_emd = 0.02` (D-233) 命中率
- bucket id ↔ EHS 中位数单调性 violation 计数

输入 JSON 格式（自由约定，C2 实跑前先用 stub 数据演示报告骨架）::

    {
      "bucket_config": {"flop": 500, "turn": 500, "river": 500},
      "training_seed": "0xCAFEBABE",
      "blake3": "0000000000000000000000000000000000000000000000000000000000000000",
      "streets": {
        "flop":  {"std_dev": [...500...], "adjacent_emd": [...499...], "median": [...500...], "empty_buckets": []},
        "turn":  {"std_dev": [...500...], "adjacent_emd": [...499...], "median": [...500...], "empty_buckets": []},
        "river": {"std_dev": [...500...], "adjacent_emd": [...499...], "median": [...500...], "empty_buckets": []}
      }
    }

输出 markdown 到 stdout（CI artifact 重定向到 `pluribus_stage2_bucket_quality.md`）。

用法（C1 阶段 / 演示）::

    python3 tools/bucket_quality_report.py --stub  # 用占位数据生成报告骨架
    python3 tools/bucket_quality_report.py < input.json > report.md  # 真实数据

C2 / D2 阶段：`tools/train_bucket_table.rs` 写出 mmap artifact 后由
`tools/bucket_table_reader.py`（D-249）+ Python 端 EMD / std dev 重算 → 喂入本脚本。
本脚本在 C1 阶段不依赖 bucket_table_reader.py（后者 C2 落地）。

角色边界：本脚本属测试基础设施 [测试] agent 产物（C1）。任何对输出格式的修改
若影响 CI artifact 解析须由 [决策] / [报告] agent review。
"""
from __future__ import annotations

import argparse
import json
import math
import statistics
import sys
from typing import Any


# Validation §3 / D-233 阈值（与 Rust 端 `tests/bucket_quality.rs` 同源）。
EHS_STD_DEV_MAX = 0.05
T_EMD = 0.02


def stub_input() -> dict[str, Any]:
    """C1 阶段占位输入：500 bucket / 街，全部 std_dev = 0.20（fail），EMD = 0
    （fail），中位数全 0.5（单调性退化）。该数据正面演示 "C1 报告骨架 +
    C2 落地后切换到真实数据" 的端到端路径。
    """
    n = 500
    placeholder_std = [0.20] * n
    placeholder_emd = [0.0] * (n - 1)
    placeholder_median = [0.5] * n
    placeholder_empty = list(range(1, n))  # bucket 0 实际命中，1..499 全空（B2 stub 行为）
    streets = {
        "flop": {
            "std_dev": placeholder_std.copy(),
            "adjacent_emd": placeholder_emd.copy(),
            "median": placeholder_median.copy(),
            "empty_buckets": placeholder_empty.copy(),
        },
        "turn": {
            "std_dev": placeholder_std.copy(),
            "adjacent_emd": placeholder_emd.copy(),
            "median": placeholder_median.copy(),
            "empty_buckets": placeholder_empty.copy(),
        },
        "river": {
            "std_dev": placeholder_std.copy(),
            "adjacent_emd": placeholder_emd.copy(),
            "median": placeholder_median.copy(),
            "empty_buckets": placeholder_empty.copy(),
        },
    }
    return {
        "bucket_config": {"flop": 500, "turn": 500, "river": 500},
        "training_seed": "0x0 (C1 stub: 真实 seed 由 C2 train_bucket_table.rs 写入)",
        "blake3": "0" * 64,
        "streets": streets,
    }


def histogram(values: list[float], n_bins: int, lo: float, hi: float) -> list[int]:
    """简单 fixed-bin 直方图。值 ∈ [lo, hi]；外溢值钳到边界 bin。"""
    if n_bins <= 0:
        return []
    bins = [0] * n_bins
    span = hi - lo
    if span <= 0:
        return bins
    for v in values:
        if v < lo:
            bins[0] += 1
            continue
        if v >= hi:
            bins[n_bins - 1] += 1
            continue
        idx = int((v - lo) / span * n_bins)
        if idx >= n_bins:
            idx = n_bins - 1
        bins[idx] += 1
    return bins


def render_histogram(bins: list[int], lo: float, hi: float, label: str) -> str:
    """渲染 ASCII 横向直方图。"""
    n = len(bins)
    if n == 0:
        return f"_{label} histogram empty_"
    max_count = max(bins) if bins else 0
    bar_max = 40
    lines = [f"### {label}", ""]
    span = (hi - lo) / n
    for i, c in enumerate(bins):
        bar = "█" * (int(c / max_count * bar_max) if max_count > 0 else 0)
        bin_lo = lo + i * span
        bin_hi = lo + (i + 1) * span
        lines.append(f"`[{bin_lo:.3f}, {bin_hi:.3f})` {bar} {c}")
    lines.append("")
    return "\n".join(lines)


def safe_stat(values: list[float], stat: str) -> str:
    if not values:
        return "N/A"
    if stat == "min":
        return f"{min(values):.4f}"
    if stat == "max":
        return f"{max(values):.4f}"
    if stat == "mean":
        return f"{statistics.fmean(values):.4f}"
    if stat == "median":
        return f"{statistics.median(values):.4f}"
    if stat == "std":
        if len(values) < 2:
            return "N/A"
        return f"{statistics.pstdev(values):.4f}"
    return "?"


def gen_report(data: dict[str, Any]) -> str:
    cfg = data.get("bucket_config", {})
    flop_n = cfg.get("flop", 0)
    turn_n = cfg.get("turn", 0)
    river_n = cfg.get("river", 0)
    seed = data.get("training_seed", "?")
    blake3 = data.get("blake3", "?")
    streets = data.get("streets", {})

    out: list[str] = []
    out.append("# Stage 2 Bucket Quality Report")
    out.append("")
    out.append("生成于 `tools/bucket_quality_report.py`（C1 §输出 line 318）。")
    out.append("")
    out.append("## 元数据")
    out.append("")
    out.append(f"- BucketConfig: flop={flop_n} / turn={turn_n} / river={river_n}")
    out.append(f"- training_seed: `{seed}`")
    out.append(f"- BLAKE3 trailer: `{blake3}`")
    out.append("- 阈值（path.md / D-233）：")
    out.append(f"  - bucket 内 EHS std dev `< {EHS_STD_DEV_MAX}`")
    out.append(f"  - 相邻 bucket EMD `≥ {T_EMD}`")
    out.append("- 单调性：bucket id ↔ EHS 中位数 单调一致（D-236b）")
    out.append("")

    for street in ("flop", "turn", "river"):
        s = streets.get(street, {})
        std_dev = s.get("std_dev", [])
        emd = s.get("adjacent_emd", [])
        medians = s.get("median", [])
        empty = s.get("empty_buckets", [])
        n_buckets = cfg.get(street, 0)
        out.append(f"## {street.upper()}")
        out.append("")
        out.append("| 指标 | 值 | 阈值 | 通过 |")
        out.append("|---|---|---|---|")
        empty_count = len(empty)
        out.append(
            f"| 空 bucket 数 | {empty_count} / {n_buckets} | 0 | "
            f"{'✓' if empty_count == 0 else '✗'} |"
        )
        if std_dev:
            sd_max = max(std_dev)
            sd_pass_count = sum(1 for v in std_dev if v < EHS_STD_DEV_MAX)
            out.append(
                f"| EHS std dev max | {sd_max:.4f} | < {EHS_STD_DEV_MAX} | "
                f"{'✓' if sd_max < EHS_STD_DEV_MAX else '✗'} |"
            )
            out.append(
                f"| EHS std dev 通过率 | {sd_pass_count} / {len(std_dev)} "
                f"({100.0 * sd_pass_count / max(len(std_dev), 1):.1f}%) | 100% | "
                f"{'✓' if sd_pass_count == len(std_dev) else '✗'} |"
            )
        if emd:
            emd_min = min(emd)
            emd_pass_count = sum(1 for v in emd if v >= T_EMD)
            out.append(
                f"| 相邻 EMD min | {emd_min:.4f} | ≥ {T_EMD} | "
                f"{'✓' if emd_min >= T_EMD else '✗'} |"
            )
            out.append(
                f"| 相邻 EMD 通过率 | {emd_pass_count} / {len(emd)} "
                f"({100.0 * emd_pass_count / max(len(emd), 1):.1f}%) | 100% | "
                f"{'✓' if emd_pass_count == len(emd) else '✗'} |"
            )
        if medians:
            mono_violations = sum(
                1 for i in range(1, len(medians)) if medians[i] < medians[i - 1]
            )
            out.append(
                f"| 单调性 violation | {mono_violations} / {max(len(medians) - 1, 0)} | 0 | "
                f"{'✓' if mono_violations == 0 else '✗'} |"
            )
        out.append("")
        out.append("### 描述统计")
        out.append("")
        out.append("| 维度 | min | max | mean | median |")
        out.append("|---|---|---|---|---|")
        out.append(
            f"| EHS std dev | {safe_stat(std_dev, 'min')} | {safe_stat(std_dev, 'max')} "
            f"| {safe_stat(std_dev, 'mean')} | {safe_stat(std_dev, 'median')} |"
        )
        out.append(
            f"| 相邻 EMD | {safe_stat(emd, 'min')} | {safe_stat(emd, 'max')} "
            f"| {safe_stat(emd, 'mean')} | {safe_stat(emd, 'median')} |"
        )
        out.append(
            f"| EHS 中位数 | {safe_stat(medians, 'min')} | {safe_stat(medians, 'max')} "
            f"| {safe_stat(medians, 'mean')} | {safe_stat(medians, 'median')} |"
        )
        out.append("")
        if std_dev:
            out.append(
                render_histogram(
                    histogram(std_dev, 10, 0.0, 0.30),
                    0.0,
                    0.30,
                    "bucket 内 EHS std dev 分布",
                )
            )
        if emd:
            out.append(
                render_histogram(
                    histogram(emd, 10, 0.0, 0.20),
                    0.0,
                    0.20,
                    "相邻 bucket EMD 分布",
                )
            )

    out.append("---")
    out.append("")
    out.append("## C1 状态说明")
    out.append("")
    out.append(
        "- **C1**：B2 stub `BucketTable::lookup` 全部返回 `Some(0)`，本报告在 stub "
        "数据上各项指标几乎全 fail（设计如此）。预期失败已在 "
        "`tests/bucket_quality.rs` 用 `#[ignore]` 标注。"
    )
    out.append(
        "- **C2**：`tools/train_bucket_table.rs` 落地后写出真实 mmap artifact，本"
        "脚本配合 `tools/bucket_table_reader.py`（D-249）读出 bucket 质量数据生成"
        "真实指标。所有 ✗ 应转为 ✓；任一 ✗ 视为聚类质量门槛未达，回归 [实现]。"
    )
    out.append(
        "- **D1**：1M-volume bucket id determinism + cross-host BLAKE3 byte-equal "
        "纳入夜间 fuzz；本报告作为 fuzz 输入数据来源之一。"
    )
    out.append("")
    return "\n".join(out)


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Generate stage-2 bucket quality markdown report"
    )
    parser.add_argument(
        "--stub",
        action="store_true",
        help="使用 C1 占位数据（无 stdin 输入），生成报告骨架",
    )
    args = parser.parse_args()

    try:
        if args.stub:
            data = stub_input()
        else:
            raw = sys.stdin.read()
            if not raw.strip():
                # 容错：stdin 空时 fallback 到 stub。
                sys.stderr.write(
                    "[bucket_quality_report] stdin 为空，退化到 --stub 模式\n"
                )
                data = stub_input()
            else:
                data = json.loads(raw)
    except json.JSONDecodeError as e:
        sys.stderr.write(f"[bucket_quality_report] 输入 JSON 解析失败: {e}\n")
        return 2
    except Exception as e:  # noqa: BLE001
        sys.stderr.write(f"[bucket_quality_report] 未预期错误: {e}\n")
        return 3

    report = gen_report(data)
    print(report)
    return 0


if __name__ == "__main__":
    sys.exit(main())
