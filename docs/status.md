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
| 简化 NLHE ES-MCCFR | ✅ InfoSetId v2 layout（hand_bucket / node_id / street_tag）跨街抽象动作历史单射；preflop 病态消失（BB 拿 AKo limp 后 100% AllIn 变 ~0）；1B update LBR plateau @ 940 mbb/g ± 20（bucket abstraction floor，stage 2 重做才能再降）；1K finite strategy + 1M × 3 fixed-seed BLAKE3 byte-equal smoke | `tests/cfr_simplified_nlhe.rs` + `tests/nlhe_infoset_history_collision.rs` + `src/training/nlhe_betting_tree.rs` |
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
- 简化 NLHE 复测：`cargo test --release --test cfr_simplified_nlhe -- --ignored --nocapture`
  通过 2 条 ignored 测试；1M × 3 fixed-seed snapshot BLAKE3 =
  `4d211620a09ed97ce9593055eb8e4ee42b592d6b44f9fa90e65faaa8d84d1ab4`。
- 简化 NLHE InfoSetId v2 + 1B 验证（2026-05-19）：
  - 抽象 betting tree 节点数 = 48,224（`tools/nlhe_betting_tree_sizing.rs` 实测，16-bit node_id 足够）。
  - `tests/nlhe_infoset_history_collision.rs`：SB-aggressor vs BB-aggressor 两条 preflop 线推进到 flop 同一决策点，InfoSetId 不同（按 node_id 区分）。
  - 1B update LBR proxy 曲线（`--lbr-probes 10000 --lbr-rollouts 16`，bucket_table v3）：
    `0M=2711, 100M=947, 200M=937, 300M=944, 400M=955, 500M=959, 600M=964, 700M=957, 800M=956, 900M=958, 1B=934`
    （SE ≈ 20）。100M 后所有点都在 940 ± 20 chips 噪声带内 → bucket abstraction floor。
  - preflop 策略合理：SB-root 72o 100% fold；BB-vs-limp AKo 96% Raise FULL_POT / 2% AllIn（旧版 500M 此 spot AKo 87% AllIn 病态已消除）。
  - bucket_table BLAKE3 = `67ee555439f2c918698650c05f40a7a5e9e812280ceb87fc3c6590add98650cd`。
  - 训练 seed = `0x48335f4e4c48455f`。
- 进一步降 LBR proxy 需要换 bucket abstraction（500/500/500 → 更细），属 stage 2 重做范围，不在 stage 3 范围。

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
