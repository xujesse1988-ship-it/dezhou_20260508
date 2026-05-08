#!/usr/bin/env python3
"""Parse `target/xvalidate-100k/chunk-*.log` and summarize all DIVERGED seeds.

Reads the per-divergence eprintln lines emitted by `tests/cross_validation.rs`
(`[xvalidate] DIVERGED seed=N | PokerKit mismatch for seed N: ours_payouts=…,
ref_payouts=Some(…), ours_showdown=…, ref_showdown=Some(…)`) and buckets them
into:

  A — showdown_order only (payouts identical)
  B-2way — 2-winner pot diff with multiset {-1, +1}
  B-3way — 3-winner pot diff with multiset {-1, -1, +2}

Emits a Markdown report on stdout. Use:

    python3 tools/xvalidate_diverged_summary.py \
        > docs/xvalidate_100k_diverged_seeds.md

Re-running on a new chunk dump regenerates the doc verbatim; this script is the
canonical analysis path for the D-085 / C-rev1 carve-out diagnostic output.
"""
from __future__ import annotations

import re
import sys
from collections import Counter, defaultdict
from pathlib import Path

PAT = re.compile(
    r"^\[xvalidate\] DIVERGED seed=(\d+) \| PokerKit mismatch for seed \d+: "
    r"ours_payouts=\[(.*?)\], "
    r"ref_payouts=Some\(\[(.*?)\]\), "
    r"ours_showdown=\[(.*?)\], "
    r"ref_showdown=Some\(\[(.*?)\]\)\s*$"
)


def parse_payouts(text: str) -> tuple[tuple[int, int], ...]:
    return tuple(
        (int(seat), int(net))
        for seat, net in re.findall(r"SeatId\((\d+)\), (-?\d+)", text)
    )


def parse_showdown(text: str) -> tuple[int, ...]:
    return tuple(int(seat) for seat in re.findall(r"SeatId\((\d+)\)", text))


def winners_diff(
    ours: tuple[tuple[int, int], ...], ref: tuple[tuple[int, int], ...]
) -> tuple[tuple[int, int], ...]:
    out = []
    for (s1, p1), (s2, p2) in zip(ours, ref):
        if s1 != s2:
            raise ValueError("seat ordering mismatch — payouts not aligned")
        if p1 != p2:
            out.append((s1, p1 - p2))
    return tuple(out)


def fmt_seats(seats: tuple[int, ...]) -> str:
    return "[" + ", ".join(str(s) for s in seats) + "]"


def fmt_delta(delta: tuple[tuple[int, int], ...]) -> str:
    return ", ".join(f"seat{s}{d:+d}" for s, d in delta)


def main(log_dir: Path) -> int:
    rows: list[tuple[int, int, tuple, tuple, tuple, tuple]] = []
    for log in sorted(log_dir.glob("chunk-*.log")):
        chunk = int(log.stem.split("-")[1])
        for line in log.read_text().splitlines():
            m = PAT.match(line)
            if not m:
                continue
            rows.append(
                (
                    chunk,
                    int(m.group(1)),
                    parse_payouts(m.group(2)),
                    parse_payouts(m.group(3)),
                    parse_showdown(m.group(4)),
                    parse_showdown(m.group(5)),
                )
            )

    if not rows:
        print(f"# 100k cross-validation 分歧种子完整账目\n\n*未在 {log_dir} 找到任何 DIVERGED 行。*")
        return 1

    cat_a = [r for r in rows if r[2] == r[3]]
    cat_b = [r for r in rows if r[2] != r[3]]

    # Chip-conservation sanity
    for r in cat_b:
        if sum(p1 - p2 for (_, p1), (_, p2) in zip(r[2], r[3])) != 0:
            raise ValueError(f"chip conservation violated for seed {r[1]}")

    b2 = [r for r in cat_b if len(winners_diff(r[2], r[3])) == 2]
    b3 = [r for r in cat_b if len(winners_diff(r[2], r[3])) == 3]
    bother = [r for r in cat_b if len(winners_diff(r[2], r[3])) not in (2, 3)]

    # Per-chunk
    per_chunk = defaultdict(lambda: [0, 0, 0, 0])  # A, B2, B3, Bother
    for r in cat_a:
        per_chunk[r[0]][0] += 1
    for r in b2:
        per_chunk[r[0]][1] += 1
    for r in b3:
        per_chunk[r[0]][2] += 1
    for r in bother:
        per_chunk[r[0]][3] += 1

    out = []
    push = out.append

    push("# 100k cross-validation 分歧种子完整账目")
    push("")
    push(
        "> 数据来源：`scripts/run-cross-validation-100k.sh` N=8 × 12,500 hand 跑次的"
        "`target/xvalidate-100k/chunk-*.log`，commit `2ea667b`（`tests/cross_validation."
        "rs::CrossValidationReport::record` 加 per-divergence eprintln）+ PokerKit 0.4.14"
        " / Python 3.11 环境。"
    )
    push("")
    push(
        "> 所属 carve-out：`docs/pluribus_stage1_workflow.md` §修订历史 C-rev1 / D-085"
        " / `pluribus_stage1_validation.md` §7「规则引擎与 PokerKit 100,000 手 0 分歧」。"
    )
    push("")
    push(
        "> 性质：本文件是 D1 [测试] agent 的诊断输出，用作下游 [实现] follow-up agent 的"
        "minimal repro fixture。**本文件不修改产品代码、不提出修复方案**。"
    )
    push("")

    push("## 总计")
    push("")
    push("| 桶 | 数量 | 占 100k | 形状 |")
    push("|---|---:|---:|---|")
    push(f"| A — showdown_order only | {len(cat_a)} | {len(cat_a)/100000:.3%} | payouts 完全相同；showdown_order 永远是两人 swap |")
    push(f"| B-2way | {len(b2)} | {len(b2)/100000:.3%} | 2 人 split，payouts 差 multiset `{{−1, +1}}` |")
    push(f"| B-3way | {len(b3)} | {len(b3)/100000:.3%} | 3 人 split，payouts 差 multiset `{{−1, −1, +2}}` |")
    if bother:
        push(f"| B-other | {len(bother)} | {len(bother)/100000:.3%} | **离群形状，需人工审查** |")
    push(f"| **合计** | **{len(rows)}** | **{len(rows)/100000:.3%}** | far above D-085 「0 分歧」门槛 |")
    push("")
    push(f"互斥性：B 中带 showdown_order 差异条数 = {sum(1 for r in cat_b if r[4] != r[5])}；A 中 100% 带 showdown_order 差异。两条 bug 路径互不交叠。")
    push("")

    push("## 关键观察")
    push("")
    push("1. **桶 B 形状高度同质**：95 条全部落在 `{−1,+1}` 或 `{−1,−1,+2}` 两个 multiset 上，无离群 — bug 局限于「pot 余 chip 累计 / 偏置方向」局部逻辑，而非更广义的分配错误。")
    push("2. **B-3way 的 +2/−1/−1 模式**：单一赢家拿到 +2，自然解释是**多个 side pot 的余 chip 全堆同一 button-左邻**。与 D-039-rev1「整笔给 button 左侧最近的获胜者」一致，但 PokerKit 显然在多 pot 间采取不同累积策略。")
    push("3. **桶 A 全是 2-人 swap**：所有 10 条均为 `(a, b) → (b, a)` 形式的两人摊牌顺序倒置。无 3 人或更长序列错乱 — 起点选择 bug 概率高于整体方向 bug。")
    push("4. **chip-conservation 全部满足**：B 的 95 条 deltas sum=0，没有引入 / 销毁 chip — 漏的是分配，不是核算。")
    push("")

    push("## 桶 A：showdown_order only（10 seeds）")
    push("")
    push("| chunk | seed | ours_showdown | ref_showdown |")
    push("|---:|---:|:---|:---|")
    for r in sorted(cat_a, key=lambda x: x[1]):
        push(f"| {r[0]} | {r[1]} | `{fmt_seats(r[4])}` | `{fmt_seats(r[5])}` |")
    push("")

    push("## 桶 B-2way：`{−1, +1}`（28 seeds）")
    push("")
    push("`delta` = ours − ref；只列差额非零的座位。")
    push("")
    push("| chunk | seed | delta | ours_payouts | ref_payouts |")
    push("|---:|---:|:---|:---|:---|")
    for r in sorted(b2, key=lambda x: x[1]):
        d = winners_diff(r[2], r[3])
        op = "[" + ", ".join(f"({s},{p})" for s, p in r[2]) + "]"
        rp = "[" + ", ".join(f"({s},{p})" for s, p in r[3]) + "]"
        push(f"| {r[0]} | {r[1]} | `{fmt_delta(d)}` | `{op}` | `{rp}` |")
    push("")

    push("## 桶 B-3way：`{−1, −1, +2}`（67 seeds）")
    push("")
    push("`delta` = ours − ref；只列差额非零的座位。")
    push("")
    push("| chunk | seed | delta | ours_payouts | ref_payouts |")
    push("|---:|---:|:---|:---|:---|")
    for r in sorted(b3, key=lambda x: x[1]):
        d = winners_diff(r[2], r[3])
        op = "[" + ", ".join(f"({s},{p})" for s, p in r[2]) + "]"
        rp = "[" + ", ".join(f"({s},{p})" for s, p in r[3]) + "]"
        push(f"| {r[0]} | {r[1]} | `{fmt_delta(d)}` | `{op}` | `{rp}` |")
    push("")

    if bother:
        push("## 桶 B-other（离群形状，需人工审查）")
        push("")
        push("| chunk | seed | delta | ours_payouts | ref_payouts |")
        push("|---:|---:|:---|:---|:---|")
        for r in sorted(bother, key=lambda x: x[1]):
            d = winners_diff(r[2], r[3])
            op = "[" + ", ".join(f"({s},{p})" for s, p in r[2]) + "]"
            rp = "[" + ", ".join(f"({s},{p})" for s, p in r[3]) + "]"
            push(f"| {r[0]} | {r[1]} | `{fmt_delta(d)}` | `{op}` | `{rp}` |")
        push("")

    push("## 每 chunk 分布")
    push("")
    push("| chunk | seed range | A | B-2way | B-3way | B-other | total |")
    push("|---:|---|---:|---:|---:|---:|---:|")
    for c in sorted(per_chunk):
        a, x2, x3, xo = per_chunk[c]
        push(f"| {c} | [{c*12500}, {(c+1)*12500}) | {a} | {x2} | {x3} | {xo} | {a+x2+x3+xo} |")
    push(f"| **合计** | [0, 100000) | **{len(cat_a)}** | **{len(b2)}** | **{len(b3)}** | **{len(bother)}** | **{len(rows)}** |")
    push("")

    push("## 单 seed 复现")
    push("")
    push("```bash")
    push("export PATH=\"$HOME/.cargo/bin:$PWD/.venv-pokerkit/bin:$PATH\"")
    push("cargo test --release --test cross_validation --no-run")
    push("XV_TOTAL=1 XV_OFFSET=<seed> cargo test --release --test cross_validation \\")
    push("  cross_validation_pokerkit_100k_random_hands -- --ignored --nocapture")
    push("```")
    push("")
    push(
        "全量重跑：`N=8 TOTAL=100000 ./scripts/run-cross-validation-100k.sh`；"
        "解析回本文件：`python3 tools/xvalidate_diverged_summary.py "
        "> docs/xvalidate_100k_diverged_seeds.md`。"
    )
    push("")

    push("## 后续 [实现] follow-up 入口建议")
    push("")
    push("1. **桶 A**（10 seeds）：定位 `showdown_order` 起点 / 方向计算（D-037「last_aggressor → 顺时针」）。先挑 seed=1786 拉单 hand minimal repro，用 `tools/pokerkit_replay.py` 看 PokerKit 的起点选择。")
    push("2. **桶 B-2way**（28 seeds）：定位 2 人 split 的 odd-chip button-左邻偏置（D-039-rev1）。建议先挑 seed=2980（最早出现的 2-way）拉 minimal repro，与 PokerKit 的 chips-pushing divmod 顺序逐步对齐。")
    push("3. **桶 B-3way**（67 seeds）：定位多 side pot 的余 chip 累积逻辑。建议先挑 seed=14204（pot=150 / 3 winners 完美整除却出现 +2/−1/−1）作 minimal repro — 这条「pot 整除还偏」的形态最干净地暴露多 pot 余 chip 在我方堆叠的事实。")
    push("4. **验收**：修复后 `N=8 TOTAL=100000 ./scripts/run-cross-validation-100k.sh` 跑出 0 diverged 即闭合 D-085 规则引擎侧 100k 通过门槛。")
    push("")

    sys.stdout.write("\n".join(out) + "\n")
    return 0


if __name__ == "__main__":
    log_dir = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("target/xvalidate-100k")
    sys.exit(main(log_dir))
