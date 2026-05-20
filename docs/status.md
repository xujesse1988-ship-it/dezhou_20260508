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
| 简化 NLHE ES-MCCFR | ⚠️ 算法 pipeline + InfoSetId v2 layout + 1K/1M smoke 跑通；默认 profile = 200BB + 6-action {0.5p, 1p, 2p} + preflop lossless 169（postflop 走 500/500/500 bucket）；betting tree 实测 240,096 节点（18-bit）；blueprint 未训练（H4 范围） | `tests/cfr_simplified_nlhe.rs` + `tests/nlhe_infoset_history_collision.rs` + `src/training/nlhe_betting_tree.rs` + `src/abstraction/preflop.rs` |
| H3 简化 NLHE 闭环工具 | ✅ 训练入口、baseline 评测、LBR proxy、Markdown/JSON report、preflop strategy dump、抽象 betting tree sizing 工具齐备（200BB 重新跑数据待 H4） | `tools/train_cfr.rs` + `tools/nlhe_h3_report.rs` + `tools/nlhe_preflop_strategy_dump.rs` + `tools/nlhe_betting_tree_sizing.rs` + `tests/nlhe_h3_eval.rs` |

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
- bucket_table v3 artifact body BLAKE3 = `67ee555439f2c918698650c05f40a7a5e9e812280ceb87fc3c6590add98650cd`
  （与 stack profile 无关，仍可用作 H4 200BB 训练的输入抽象）。
- `tests/nlhe_infoset_history_collision.rs`：SB-aggressor vs BB-aggressor 两条 preflop 线推进到 flop
  同一决策点，InfoSetId 不同（按 node_id 区分）。
- 200BB + 6-action 默认切换后简化 NLHE 训练数据待 H4 重跑：LBR proxy 曲线、preflop 策略 spot 检查、
  fixed-seed snapshot BLAKE3 都需要在 200BB / 6-action 上重新 baseline。
  betting tree 节点数已实测：`tools/nlhe_betting_tree_sizing` 输出 240,096 决策节点（18-bit node_id），
  与 `NodeId = u32` + `InfoSetId v2` 26-bit cap 一致。

## 下一步唯一允许的工作

训出**200BB / 6-action / preflop 169 lossless** 的 first usable blueprint：
- `tools/train_cfr.rs` 跑 ≥ 10⁹ sampled decision updates（H4 first-usable 门槛）。
- 训练前先短跑（10⁷–10⁸）确认 LBR proxy 曲线下降、preflop 策略 spot 没回归到 100% AllIn 病态。
- 训练完出 H4 baseline 数据填回上文"最近验证证据"：betting tree（已有 240,096）+ LBR 曲线 +
  fixed-seed snapshot BLAKE3 + preflop spot 检查。

跳过 / 暂不做：
- Slumbot HTTP H2H 接入（H4 验收门槛但"不阻塞 first usable"，留到 first usable 出曲线后再排）。
- 500/500/500 postflop bucket 质量提升（已知污染清单第 1 条；stage 2 重做范围，不在 H4 内）。

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

- `tests/bucket_quality.rs` 9 条 sqrt-scaled K=500 阈值断言 fail（v3 bucket_table，与 stack profile 无关）：
  - `adjacent_bucket_emd_above_threshold_{flop,turn,river}`：相邻 bucket EMD 实测 0.003 / 0.008 / 0.009，
    落到阈值 0.00894 以下（约 1.0–3.0× 偏差）。
  - `bucket_internal_ehs_std_dev_below_threshold_{flop,turn,river}`：bucket 内 EHS std dev 实测
    0.034 / 0.049 / 0.058，越上限 0.02236（约 1.5–2.6× 偏差）。
  - `bucket_id_ehs_median_monotonic_{flop,turn,river}`：bucket id 序与 EHS 中位数不单调，diff 实测
    0.010 / 0.017 / 0.011，超 MC-aware tol（约 1.2–1.7× 偏差）。
  - 在父 commit `ee4da88` 上同号同值复现，**非本次 200BB 切换引入**。
  - 根因：当前 500/500/500 bucket 抽象质量不达标，属 stage 2 重做范围
    （上文简化 NLHE 行 ⚠️ 状态的同一原因）。
- `tests/cross_host_blake3.rs::cross_host_baseline_byte_equal_for_current_arch` fail：
  32-seed Kuhn Vanilla CFR 5-iter checkpoint BLAKE3 实测值与
  `tests/data/checkpoint-hashes-linux-x86_64.txt` baseline 全条目漂移
  （actual `840fdf8c…` vs expected `191a4d72…` 等）。
  - 同测试文件的 `within_process_blake3_reproducible_twice` 和
    `cross_arch_baselines_byte_equal_when_both_present` 仍绿 → 本机内确定性 + 跨架构 baseline
    比对都 OK，只是 linux baseline 文件落后于当前 checkpoint 实测。
  - 在父 commit `ee4da88` 上同号同值复现，非本次 200BB 切换引入。
  - 处理：等下一次 Kuhn checkpoint serialization 真正稳定后，跑
    `scripts/capture-checkpoint-hashes.sh` 重新生成 baseline 文件。


## 文档维护规则

- 现在描述的状态错了 → **直接改本文件**，不追加"修订历史"。git 自带历史。
- 已经过时的事实 → 删除，不留"已废弃"标注。
- 新事实跟旧事实矛盾 → 旧的删掉，不并列。
