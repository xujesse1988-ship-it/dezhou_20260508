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
| 简化 NLHE ES-MCCFR | ✅ release ignored smoke 通过：1K finite strategy + 1M × 3 fixed-seed BLAKE3 byte-equal | `tests/cfr_simplified_nlhe.rs` |
| H3 简化 NLHE 闭环工具 | ✅ 训练入口、三类 baseline 评测、H3 LBR proxy、Markdown/JSON report smoke 通过；完整 100M H3 gate 待跑 | `tools/train_cfr.rs` + `tools/nlhe_h3_report.rs` + `tests/nlhe_h3_eval.rs` |

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
- H3 闭环工具 smoke：
  - `cargo run --release --bin train_cfr -- --game nlhe --trainer es-mccfr --bucket-table artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin --updates 4 --seed 0x48335f4e4c48455f --threads 2 --checkpoint-dir artifacts/h3_smoke --checkpoint-every 2 --quiet`
    成功写出 4 update checkpoint。
  - `cargo run --release --bin nlhe_h3_report -- --checkpoint artifacts/h3_smoke/nlhe_es_mccfr_final_000000000004.ckpt --artifact artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin --eval-hands-per-seat 2 --lbr-probes 2 --lbr-rollouts 1 --output artifacts/h3_smoke/h3_report.md`
    成功写出 `artifacts/h3_smoke/h3_report.md` / `.json`，包含 random / call-station / overly-tight 与 LBR proxy 曲线。
  - 完整 H3 gate 仍需按 100M update + 1M hands 评测命令单独跑，当前未把 smoke 结果冒充为正式通过。

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
