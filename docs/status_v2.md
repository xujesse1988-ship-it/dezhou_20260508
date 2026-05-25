# 项目当前状态

## 算法正确性

| 项目 | 状态 | 依据 |
|---|---|---|
| Kuhn Vanilla CFR | ✅ 收敛到 closed-form Nash `-1/18` | `tests/cfr_kuhn.rs` |
| Leduc Vanilla CFR | ✅ exploitability `< 0.1` @ 10K iter | `tests/cfr_leduc.rs` |
| Leduc ES-MCCFR | ✅ Rust 2M update `ev_p0=-0.087396516` / `exploitability=0.258471407` / hash byte-equal `leduc_mccfr.py` 1M iter anchor | `leduc_mccfr.py` + `tools/leduc_es_mccfr_report` |
| Leduc LCFR-MCCFR | ✅ 算法正确（所有变体 `ev_p0` 收敛到 -0.087）；不带 LCFR 的 ES-MCCFR 路径 BLAKE3 byte-equal 保留；早期 regime（2M update）显著降 exploit（p=10K -38.4%） | `artifacts/lcfr_leduc/` on vultr |
| 简化 NLHE ES-MCCFR（不带 LCFR） | ✅ 100M LBR proxy 1,863 chips；500M LBR 1,849 chips（floor，100→500M < 1 SE） | `run_v3_100m`, `run_v3_500m` on vultr |
| 简化 NLHE LCFR-MCCFR | ✅ **100M LBR proxy 1,503 chips，破 ES-MCCFR 500M floor 18.7%（差距 > 3 SE）** | `run_lcfr_100m` on aws 18.222.240.47 |

### 简化 NLHE profile

- 2-player heads-up 200BB
- 6-action 抽象 `{0.5p, 1p, 2p}` + Fold/Check/Call/AllIn（`DefaultActionAbstraction::default_6_action`）
- preflop lossless 169 hand class
- postflop K=500/500/500 bucket（v3 cafebabe artifact）
- abstract betting tree 240,096 决策节点（18-bit node_id；preflop 912 / flop 9,072 / turn 48,176 / river 181,936）

### Leduc LCFR-MCCFR 的注意点

LCFR-MCCFR 在 Leduc 10M+ update 时反退化（不带 LCFR 的 ES-MCCFR exploit 0.188 vs LCFR p=50K 0.204 / p=200K 0.317）。
**这不是实现 bug**：

- 所有 LCFR 变体 `ev_p0` 仍收敛 -0.087（Nash 量级）
- 根因：Leduc 仅 288 InfoSet，10M update / 288 ≈ 35K visits/infoset 远超 LCFR 需要的"early bad strategy 平摊"regime
- Brown & Sandholm 2018 §Discounted MCCFR 的验证场景是 HUNL subgame（millions of infoset / 单 infoset ≪ 100 visits），与本 NLHE 119M infoset / 100M update ≈ 0.8 visit 同 regime
- → NLHE 100M LCFR 实测降 LBR 19.3% 验证了 algorithm 在对症 regime 下的收益

## 训练吞吐基线

`tools/train_cfr` 默认 profile 下：

| 主机 | 模式 | 0-5M | 50-100M | 100M avg | 来源 |
|---|---|---:|---:|---:|---|
| AWS c7i.4xlarge (32 vCPU Intel) | ES-MCCFR（不带 LCFR） | 10,679/s | 7,312/s | n/a | run_v3_100m |
| AWS c6a.8xlarge (32 vCPU AMD EPYC 7R13) | LCFR-MCCFR period=1M | 9,877/s | 7,154/s | 6,885/s | run_lcfr_100m |

LCFR rescale 开销不可见（period boundary 每 1M update 全表 O(N) rescale，amortize 后 <1% wall）。

### batched-parallel 热路径优化（2026-05-24，已告一段落）

`step_parallel` 加 `batch_per_worker`：每 worker 连跑 B 条 trajectory 再合并，把
rayon dispatch / `sched_yield` 调度开销摊薄 B 倍（旧版每次只 dispatch `n_threads`
条 ~1ms 任务，调度成本与计算同量级）。配套热路径减分配：history fast path
（`with_rng_no_history` + `track_history` flag）、traverser fan-out consume-last、
info_set per-street bucket cache、`next` CSE、`legal_actions` move-out。
`tools/train_cfr --batch-per-worker` 默认 128。

净效果（c6a.8xlarge 32t，1M updates，含 checkpoint，同方法 3 次中位）：
steady last-200K `15,171 → 19,356/s`（+27.6%）、cumulative `10,048 → 12,208/s`
（+21.5%）、wall `113 → 94s`（-16.7%）、user CPU `550 → 427s`（-22.4%）。
剔除 11 GB checkpoint 写 tmpfs 噪声后，HEAD=`ac0df06` 当前 32t B=128 1M 短跑
last-200K **24,648/s**、cumulative **20,231/s**（wall 49.4s）。这跟上表长 run avg
`6,885/s` 差 ~3.5× 是预期的：长 run 受 11 GB RegretTable 内存饱和 + HashMap
collision 增长 + LCFR period rescale 全表 O(N) + checkpoint 写入摊薄，热路径
提速在长 run 上被吃掉大半。长 run 100M wall 估算（≈4h）是优化前全程均值，
优化后未复测。

`legal_actions` / `abstract_actions` 全面改 SmallVec（第四轮）实测负向
（last-200K -20%；`AbstractAction` 16B × inline-8 = 144B stack copy 压塌 L1），
已 `ac0df06` 回退；下一个候选是 `GameState` apply/undo 替 clone（clone+drop
仍 ~16%），未做。

行为中性的现有依据仅 50K updates × `--threads 1 --seed cafebabe` checkpoint
SHA-256 改前 / 改后 / 回退后三次 byte-equal（`92c12bd8…bed5c81`，只证确定性）。
NLHE 1M BLAKE3 anchor（`tests/cfr_simplified_nlhe.rs` Test 5）因期望 artifact
文件名 `_v3.bin` 与实际 `_schemav3.bin` 不符**当前在 skip**；并行 `step_parallel`
路径也无 Leduc/Kuhn 收敛对照（Leduc 288 infoset ≪ B×线程 window，无法走
batched）——本轮改动未被任一收敛 gate 覆盖。round 详记
`docs/temp/training_throughput_batched_parallel_2026_05_24.md`。

长 run wall 估算（同 32 vCPU 主机，c7i / c6a 量级相当）：

| 目标 | compute | + final ckpt | 总 wall | cost @ $1.224/h (c6a.8xlarge) |
|---|---|---|---|---|
| 100M | 14.2k s ≈ 3h 56min | +3 min | ~4h | ~$4.9 |
| 200M | ~7.7h | +6 min | ~7.8h | ~$9.5 |
| 500M | ~19h | +6 min | ~19h | ~$23 |
| 1B | ~38h | +6 min | ~38h | ~$47 |

throughput 上限由 `step_parallel` serial merge 卡死，加更多核 / 更大机器无效
（详 `src/training/trainer.rs::step_parallel` doc）。

## 当前 NLHE 训练产物

### ES-MCCFR baseline（不带 LCFR，v3，vultr 持久）

- `~/dezhou_20260508/artifacts/run_v3_100m/nlhe_es_mccfr_final_000100000000.ckpt`
  - 8.385 GiB / strategy_blake3 待补 / **LBR proxy 1,863.39 chips ± 132**（probes=1000, seed=0x42）
- `~/dezhou_20260508/artifacts/run_v3_500m/nlhe_es_mccfr_final_000500000000.ckpt`
  - 8.703 GiB / b3sum `66d9a724...` / **LBR proxy 1,849.19 chips ± 129**（floor）
- 100M → 500M LBR 在 [1841, 1887] 抖动，4× update 0 收益（详诊断见 docs/temp/v3_500m_training_state_2026_05_24.md）

### LCFR-MCCFR baseline（aws c6a.8xlarge 18.222.240.47）

- `~/dezhou_20260508/artifacts/run_lcfr_100m/nlhe_es_mccfr_final_000100000000.ckpt`
- 8.53 GiB / b3sum `15ddbddb70537a2215e2f242638863a8fe043cf93967dfdf4dc72dad311e701b`
- strategy_blake3 `63a9953fafefcfdff3736ab2f63057b7b659d6778cadf8b95ffc4db19579973e`
- seed `0x4e4c48455f48335f`（同 ES-MCCFR baseline）/ LCFR period `1_000_000`
- bucket BLAKE3 `1c22c1ee...`（v3 cafebabe）
- update_count 100,000,000 / wall 14,524s = 4h 2min / throughput avg 6,885/s
- **LBR proxy 1,503.17 chips ± 111**（probes=1000, seed=0x42）

### H3 baseline EV @ LCFR 100M（mbb/g，正值 = 训练侧赢）

| baseline | mbb/g | 95% CI |
|---|---:|---|
| random | +7,016 | [4,544, 9,487] |
| call-station | +2,972 | [2,107, 3,837] |
| overly-tight | +738 | [493, 982] |
| equity-ev | +6,246 | [3,734, 8,758] |

4 baseline 全 95% 正显著。

## bucket table 工件

3 个 production v3 artifact 在 vultr `~/dezhou_20260508/artifacts/`（不进 git，旁路 `.b3sum` anchor）：

| seed | filename | body BLAKE3 | EVR |
|---|---|---|---|
| **cafebabe (canonical)** | `bucket_table_default_500_500_500_seed_cafebabe_schemav3.bin` | `1c22c1ee32fdd557db2c2fefaeb8a5c287dfd165d648fa6c2488d76a226575a0` | **0.9712** |
| deadbeef | `bucket_table_default_500_500_500_seed_deadbeef_schemav3.bin` | `1a7f39882ddee8012a06420afc788bd6ad5ca03b593f187e5330b34004a630be` | 0.9711 |
| b16b00b5 | `bucket_table_default_500_500_500_seed_b16b00b5_schemav3.bin` | `9c47f4fdbe7ce4dd6388d7d980e6a115212a36a586ff1e528d9bd588f7cb9311` | 0.9709 |

Stage 3 CFR 输入钉死 `cafebabe`（EVR 最高）；`deadbeef` / `b16b00b5` 留 seed robustness 对照。
schema_version=3 / feature_set_id=2（16-dim hist+OCHS feature）；v1/v2 artifact 不再可加载。

## 算法 + 关键代码入口

- LCFR-MCCFR 调用：`EsMccfrTrainer::new(game, seed).with_lcfr_period(period_size)`
  - 实现：`src/training/trainer.rs::maybe_lcfr_rescale`（period n 末 regret + strategy_sum 全表 × n/(n+1)）
  - 标准变体：双 rescale（Brown 2018 默认）
  - 对照变体：`with_lcfr_period_strategy_only(P)` 只 rescale strategy_sum（regret 不动）
  - 论文：Brown & Sandholm 2018 *Solving Imperfect-Information Games via Discounted Regret Minimization* §Discounted Monte Carlo CFR（arxiv 1809.04040）
- LCFR 与 checkpoint 不兼容 resume：`load_checkpoint` 强制回退到不带 LCFR 的 ES-MCCFR
  （period state 不存 schema；production 路径走 cold start）
- ES-MCCFR DFS 核心：`src/training/trainer.rs::recurse_es` / `recurse_es_parallel`
- NLHE state + tree：`src/training/nlhe.rs` + `src/training/nlhe_betting_tree.rs`
- preflop 169 lossless：`src/abstraction/preflop.rs::PreflopLossless169`
- LBR proxy：`src/training/lbr.rs::estimate_lbr_filtered`
- baseline 评测：`src/training/nlhe_eval.rs::evaluate_blueprint_vs_baseline`

## 代码结构

```
src/
  rules/         规则引擎（座位 / 下注轮 / side pot / showdown）
  hand_eval/     7 张牌评估器
  abstraction/   action / information abstraction + bucket table
  training/      Game trait + Vanilla CFR + ES-MCCFR(+LCFR) + checkpoint
tests/           cargo test 跑这里
tools/           诊断 / 训练 binary
```

## 构建 / 测试

```bash
./scripts/setup-rust.sh                          # 一次性，装 rustup + 锁工具链 1.95.0
cargo build --all-targets
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test                                       # 默认套件
cargo test --release -- --ignored                # 长跑套件（含 BLAKE3 anchor / SLO）
```

可选 PokerKit 跨验证：

```bash
uv venv --python 3.11 .venv-pokerkit
uv pip install --python .venv-pokerkit/bin/python "pokerkit==0.4.14"
PATH=".venv-pokerkit/bin:$PATH" cargo test
```

## 主机

| host | 角色 | 状态 |
|---|---|---|
| vultr 64.176.35.138 (4 vCPU AMD EPYC-Rome / 7.7 GiB) | 持久存储 + 短测试 | 长期持有；bucket artifact + v3 ckpt + Leduc LCFR 数据都在 |
| AWS c6a.8xlarge 18.222.240.47 (32 vCPU AMD EPYC 7R13 / 123 GiB) | LCFR 100M 训练机 | 训练完成；未 terminate，等下一步决定 |

vultr **跑不动 NLHE 训练**：3M updates 时 RSS 超 7 GiB 进 swap，throughput 从 3.5K/s 衰减到 800/s。
NLHE 训练必须 ≥ 32 vCPU / ≥ 32 GiB。AWS c7i.4xlarge ($0.71/h) 或 c6a.8xlarge ($1.224/h) 都跑得动；
c6a 单核略弱但每 vCPU 便宜，wall 同档（受 serial merge 卡死）。

## 下一步（待决策）

LCFR-MCCFR 100M 已经过 H3 baseline + LBR 双重 gate（4 baseline 全正显著 + LBR 破 ES-MCCFR 500M floor 18.7%）。
四条候选路径：

| 选项 | 内容 | 增量 wall | 增量 cost | 价值 |
|---|---|---|---|---|
| A. 500M LCFR | 同机器 cold start 跑到 500M，看是否继续降 LBR | +16h | ~$20 | 验证 LCFR 是否一路向下 vs 类似 floor；为 H4 1B 起点拿数据 |
| B. 200M LCFR | 半量试探 100→200M 改善曲线 | +4h | ~$5 | 决策 500M 是否值得 |
| C. 归档停下 | 写 commit + LCFR ckpt 长期保存到 vultr | 0 | 0 | 锁定结果不冒进 |
| D. strategy-only LCFR 100M | 验证只 rescale strategy_sum 在 NLHE 是否同 Leduc 一样退化 | +4h | ~$5 | 对照消除"双 rescale 是否真比单 rescale 强"歧义 |

## 文档维护规则

- 工作笔记 / 临时数据 → `docs/temp/*.md`
