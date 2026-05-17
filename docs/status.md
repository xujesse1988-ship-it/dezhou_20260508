# 项目当前状态

本文档只记录"现在是什么"，不记录"我们做过什么"（后者查 `git log`）。
有变化就直接改这份文档。

## 算法正确性（最重要的一栏）

下表是核心算法在已知 ground truth 上的对照结果。
任何 stage 工作开始之前先看这栏。

| 项目 | 状态 | 依据 |
|---|---|---|
| Kuhn Vanilla CFR | ✅ 收敛到 closed-form Nash `-1/18` | `tests/cfr_kuhn.rs` |
| Leduc Vanilla CFR | ✅ exploitability `< 0.1` @ 10K iter | `tests/cfr_leduc.rs` |
| Leduc ES-MCCFR | ⚠️ 算法 2026-05-17 修正，**未跑 1M iter 对照 `leduc_mccfr.py`** | commit 240bb1a |
| 简化 NLHE ES-MCCFR | ⚠️ 同上路径，**未复测** | — |
| 6-max NLHE blueprint (stage 4) | ❌ 训练用的 ES-MCCFR 有 bug，工件不可信 | LBR 56,231 mbb/g vs 阈值 200 |
| stage 5 性能优化 | ❌ 建在 stage 4 之上，同污染 | — |

外部参照点（任何 ES-MCCFR 改动必须跟其中至少一条对照）：

- Brian Berns Vanilla CFR 500M iter on Leduc: EV P0 = **-0.08553**
- Python `leduc_mccfr.py` 1M iter: EV P0 = **-0.08668**
- zig-leduc-cfr 100K iter: EV P0 = **-0.08597**
- 三独立实现一致 → Leduc P0 Nash value ≈ **-0.087** 是 ground truth

## 下一步唯一允许的工作

跑 Leduc ES-MCCFR 1M iter 对照 Python ref，验证：

1. exploitability `< 0.1`
2. EV P0 ∈ `[-0.10, -0.07]`
3. P1 持 J 面对 bet（preflop）的策略 `fold ≥ 0.5`

三条都过之前不允许动 stage 4 / stage 5 任何代码。
三条任一失败 → 继续修 `recurse_es` 路径，不开新功能。

跑法：

```bash
cargo run --release --bin leduc_es_mccfr_report
# 对照工件：leduc_mccfr.py（python3 leduc_mccfr.py --iterations 1000000 --compact）
```

## 代码结构

```
src/
  rules/         规则引擎（座位 / 下注轮 / side pot / showdown）
  hand_eval/     7 张牌评估器
  abstraction/   action / information abstraction + bucket table
  training/      Game trait + Vanilla CFR + ES-MCCFR + checkpoint
tests/           cargo test 跑这里
tools/           一次性诊断 / 分析 binary
```

入口：

- `src/training/trainer.rs::recurse_es` — ES-MCCFR DFS（算法核心）
- `src/training/trainer.rs::recurse_vanilla` — Vanilla CFR DFS
- `src/training/leduc.rs` — Leduc 规则 + InfoSet 编码
- `tools/leduc_es_mccfr_report.rs` — Leduc ES-MCCFR 收敛报告

## 构建 / 测试

```bash
./scripts/setup-rust.sh                          # 一次性，装 rustup + 锁工具链
cargo build --all-targets
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test                                       # 默认套件，全绿
cargo test --release -- --ignored                # 长跑套件（含 BLAKE3 复现 / SLO）
```

可选 PokerKit 跨验证：

```bash
uv venv --python 3.11 .venv-pokerkit
uv pip install --python .venv-pokerkit/bin/python "pokerkit==0.4.14"
PATH=".venv-pokerkit/bin:$PATH" cargo test
```

## bucket table 工件

- `artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin`
  528 MiB / body BLAKE3 `67ee5554…98650cd` / 不进 git。
  生产用 abstraction，stage 4 NLHE 训练依赖。

- 测试用 fixture（`--mode fixture --flop 10 --turn 10 --river 10`）每次现造。

## 已知污染清单

- stage 3 docs 历史里 NLHE BLAKE3 anchor `9e8258d1…`、`8fa6a8fd…` 等数字：
  算法修复后不再可复现，不要拿这些值作 regression 比对。
- stage 4 训练结果（任何形式的 blueprint checkpoint）：用旧 ES-MCCFR 训练，不可信。
- stage 5 性能优化数字（吞吐 / 内存）：基于 stage 4 工件，参考价值有限。

## 文档维护规则

- 现在描述的状态错了 → **直接改本文件**，不追加"修订历史"。git 自带历史。
- 已经过时的事实 → 删除，不留"已废弃"标注。
- 新事实跟旧事实矛盾 → 旧的删掉，不并列。
- 算法 / 不变量 / 流程的本质性变化 → 改 `pluribus_path.md` 或 `invariants.md`，不在本文里堆叠。
