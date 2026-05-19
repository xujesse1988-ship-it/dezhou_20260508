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
| Leduc ES-MCCFR | ✅ 1M 外部对照通过；Rust 2M per-player update EV 与 `leduc_mccfr.py` 1M iter 同量级 | `leduc_mccfr.py` + `tools/leduc_es_mccfr_report.rs` |
| 简化 NLHE ES-MCCFR | ⚠️ 默认 action profile 已扩为 `0.33/0.5/0.75/1.0/1.5/2.0 pot + all-in`；起始筹码 profile 支持 `100BB` / `200BB`（Slumbot 对齐）；checkpoint schema v4，旧 profile checkpoint 不兼容需重训；InfoSetId v2 layout（hand_bucket / node_id / street_tag）跨街抽象动作历史单射仍保留 | `src/abstraction/action.rs` + `src/training/nlhe.rs` + `src/training/nlhe_betting_tree.rs` + `tests/cfr_simplified_nlhe.rs` |
| H3 简化 NLHE 闭环工具 | ✅ 训练入口、三类 baseline 评测、H3 LBR proxy、Markdown/JSON report、preflop strategy dump、抽象 betting tree sizing 全套 smoke 通过 | `tools/train_cfr.rs` + `tools/nlhe_h3_report.rs` + `tools/nlhe_preflop_strategy_dump.rs` + `tools/nlhe_betting_tree_sizing.rs` + `tests/nlhe_h3_eval.rs` |

### 最近验证证据

- Leduc 外部参考：`python3 leduc_mccfr.py --iterations 1000000 --seed 7 --compact`
  输出 `Expected value for player 0: -0.08668`。
- Leduc Rust ES-MCCFR：`cargo run --release --bin leduc_es_mccfr_report -- --updates 2000000 --seed 0x4c454455435f4553 --report-every 2000000 --output artifacts/leduc_es_mccfr_2m_h2_status.txt`
  输出 `ev_p0=-0.087396516`、`exploitability_chips_per_game=0.258471407`、
  `average_strategy_blake3=b48a079c68fc595e722f3232e6c0219a52f91ebdabd1a9fadfe483ba9dce950a`。
  口径说明：`leduc_mccfr.py` 的 1 次 iteration 会分别更新两个 traverser；Rust `update_count`
  是 per-player update，因此用 Rust 2M update 对齐 Python 1M iter。
- Leduc 长跑趋势：已有 Rust 100M report
  `artifacts/leduc_es_mccfr_100m_full_history.txt` 输出 `ev_p0=-0.086036478`、
  `exploitability_chips_per_game=0.147556546`、
  `average_strategy_blake3=c0b8bcfa6db843b410f515b8526f08de19f573c88fb4eaf20afe431dba98385c`。
- 简化 NLHE action / stack profile 更新（2026-05-19）：
  - 默认 bet/raise ratio = `0.33 / 0.5 / 0.75 / 1.0 / 1.5 / 2.0 pot`，外加 all-in。
  - `NlheStackProfile` 支持 `100bb`（默认）与 `200bb`（Slumbot：20,000 chips,
    SB/BB=50/100）；`train_cfr` / H3 report / preflop dump / signal dump /
    betting-tree sizing 均支持 `--stack-bb 100|200`。
  - checkpoint `SCHEMA_VERSION = 4`；NLHE checkpoint 兼容 fingerprint 包含 bucket table
    hash + stack profile + table config。旧 v3/v2 checkpoint 当前代码会拒绝加载，
    需要按新 profile 重训。
  - 100BB 抽象 betting tree 节点数 = 5,201,712（node_id 23 bit）；200BB 节点数
    = 29,744,992（node_id 25 bit），均小于当前 InfoSetId v2 的 26-bit node_id 字段。
  - 旧 1B 结果（preflop 病态消失、LBR plateau @ 940 mbb/g ± 20）只作为历史参考，
    不再代表当前 action profile 的训练质量。

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
- `tools/train_cfr.rs` — H3 简化 NLHE ES-MCCFR 训练入口（checkpoint / resume / threads）
- `tools/nlhe_h3_report.rs` — H3 baseline 评测 + LBR proxy Markdown/JSON 报告

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

- 测试用 fixture（`--mode fixture --flop 10 --turn 10 --river 10`）每次现造。

## 已知污染清单


## 文档维护规则

- 现在描述的状态错了 → **直接改本文件**，不追加"修订历史"。git 自带历史。
- 已经过时的事实 → 删除，不留"已废弃"标注。
- 新事实跟旧事实矛盾 → 旧的删掉，不并列。
