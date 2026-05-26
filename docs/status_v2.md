# 项目当前状态

## 算法正确性

| 项目 | 状态 | 依据 |
|---|---|---|
| Kuhn Vanilla CFR | ✅ 收敛到 closed-form Nash `-1/18` | `tests/cfr_kuhn.rs` |
| Leduc Vanilla CFR | ✅ exploitability `< 0.1` @ 10K iter | `tests/cfr_leduc.rs` |
| Leduc ES-MCCFR | ✅ Rust 2M update `ev_p0=-0.087396516` / `exploitability=0.258471407` / hash byte-equal `leduc_mccfr.py` 1M iter anchor | `leduc_mccfr.py` + `tools/leduc_es_mccfr_report` |
| Leduc LCFR-MCCFR | ✅ 算法正确（所有变体 `ev_p0` 收敛到 -0.087）；不带 LCFR 的 ES-MCCFR 路径 BLAKE3 byte-equal 保留；早期 regime（2M update）显著降 exploit（p=10K -38.4%） | `artifacts/lcfr_leduc/` on vultr |
| 简化 NLHE ES-MCCFR（不带 LCFR） | ✅ 100M LBR proxy 1,863 chips；500M LBR 1,849 chips（floor，100→500M < 1 SE） | `run_v3_100m`, `run_v3_500m` on vultr |
| 简化 NLHE LCFR-MCCFR | ✅ **旧路径 100M LBR 1,503；优化路径 100M 1,233 → 500M 1,126（破 ES-MCCFR floor 1,849 约 39%）；100M→500M < 1 SE = 饱和** | `run_lcfr_100m` / `run_lcfr_500m` |
| 简化 NLHE dense 后端（dense 表 + v4 bucket） | ✅ **byte-equal HashMap（5 对照）+ 端到端 100M LBR 1,143 ± 87 ≈ HashMap+v3 baseline 1,233 ± 96.9（差 91 < 合并 SE 130，不显著 = 同质量）** | `tests/dense_nlhe_trainer.rs` + `run_dense_lcfr_100m` on vultr |

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
| AWS c6a.8xlarge (32 vCPU AMD EPYC 7R13) | LCFR-MCCFR period=1M（旧路径） | 9,877/s | 7,154/s | 6,885/s | run_lcfr_100m |
| AWS c6a.8xlarge (32 vCPU AMD EPYC 7R13) | LCFR-MCCFR period=1M **优化 B=128** | 17,608/s¹ | 12,420/s | 12,965/s | run_lcfr_500m |
| AWS 32 vCPU (3.90.231.50, 已 terminate) | **dense 后端** LCFR period=1M B=128 | ~36,500/s² | ~26,800/s³ | 26,180/s | run_dense_lcfr_100m |

¹ 0–10M 窗口（该 run report-every=10M，无 5M 采样点）。优化 vs 旧路径同档主机：**稳态段 50–100M +70.5%、100M 累计 +88%**；全程 500M 累计 12,063/s（含 5 次 checkpoint 暂停）vs 旧 100M avg 6,885/s = **+75%**。

² 0–5M 窗口。³ dense 关键观察：稳态 ~26.8k/s **从 70M 后压平不再衰减**（无 HashMap collision growth / glibc arena bloat），对比 HashMap 同 profile 100M+ 衰到 ~12k/s → **dense ~2.2× 且长 run 不塌**。wall 63.7min（vs HashMap 100M 估算 2h18min ≈ 半程）。RSS 全程平 ~5.2 GiB（4.62 表 + 0.55 bucket），**checkpoint 写不暴涨**（dense ckpt 流式写 raw f64，无 bincode 全缓冲；对比 HashMap final 序列化峰值 46.8 GB）。dense ckpt 固定 4.7 GiB（含两表，比 HashMap ~8.5 GiB 小：无 InfoSetId key + 无 per-row Vec len，position=identity）。**跨主机非同档对照**：该 box CPU 型号未记录，2.2× 是跨机估算，未做同机 HashMap 对照。

LCFR rescale 开销不可见（period boundary 每 1M update 全表 O(N) rescale，amortize 后 <1% wall）。

### batched-parallel 热路径优化（2026-05-24，已告一段落）

`step_parallel` 加 `batch_per_worker`：每 worker 连跑 B 条 trajectory 再合并，把
rayon dispatch / `sched_yield` 调度开销摊薄 B 倍（旧版每次只 dispatch `n_threads`
条 ~1ms 任务，调度成本与计算同量级）。配套热路径减分配：history fast path
（`with_rng_no_history` + `track_history` flag）、traverser fan-out consume-last、
info_set per-street bucket cache、`next` CSE、`legal_actions` move-out。
`tools/train_cfr --batch-per-worker` CLI 默认 **16**；优化值 128 须显式传（`run_lcfr_500m`
及 `scripts/deploy-aws-training.sh` 均显式 `--batch-per-worker 128`）。

净效果（c6a.8xlarge 32t，1M updates，含 checkpoint，同方法 3 次中位）：
steady last-200K `15,171 → 19,356/s`（+27.6%）、cumulative `10,048 → 12,208/s`
（+21.5%）、wall `113 → 94s`（-16.7%）、user CPU `550 → 427s`（-22.4%）。
剔除 11 GB checkpoint 写 tmpfs 噪声后，HEAD=`ac0df06` 当前 32t B=128 1M 短跑
last-200K **24,648/s**、cumulative **20,231/s**（wall 49.4s）。

**长 run 已复测**（`run_lcfr_500m`，新 c6a.8xlarge / EPYC 7R13，32t B=128，500M
LCFR period=1M，seed 同 baseline，2026-05-25）：早期 0–10M 17,608/s，100M 后稳态压平
在 ~12,200/s（300–500M 段），50–100M 段 12,420/s，500M 累计 12,063/s，wall 11h31min。
长 run 确实受 11 GB×2 表内存 + HashMap collision + LCFR rescale + checkpoint 暂停拖累
（早期 24.6k → 稳态 12.2k，约掉一半，每 100M checkpoint 暂停 ~210s），但**稳态仍是旧
路径同档主机 7,154/s 的 1.7×**——热路径提速在长 run 上没被吃掉。RSS 稳态 ~30 GB
（infoset 100M 即饱和），final ckpt 序列化峰值 46.8 GB，61 GB 机全程无 OOM。

`legal_actions` / `abstract_actions` 全面改 SmallVec（第四轮）实测负向
（last-200K -20%；`AbstractAction` 16B × inline-8 = 144B stack copy 压塌 L1），
已 `ac0df06` 回退；下一个候选是 `GameState` apply/undo 替 clone（clone+drop
仍 ~16%），未做。

行为中性的现有依据：50K updates × `--threads 1 --seed cafebabe` checkpoint
SHA-256 改前 / 改后 / 回退后三次 byte-equal（`92c12bd8…bed5c81`，只证确定性）。
NLHE 1M BLAKE3 anchor（`tests/cfr_simplified_nlhe.rs` Test 5）因期望 artifact
文件名 `_v3.bin` 与实际 `_schemav3.bin` 不符**当前在 skip**；Leduc/Kuhn 仍无并行
`step_parallel` 收敛对照（Leduc 288 infoset ≪ B×线程 window，无法走 batched）。
**但 `run_lcfr_500m` 给了优化并行路径的 NLHE 收敛域证据**：其 100M blueprint 复现
旧路径（`run_lcfr_100m`）4 个 baseline 里 3 个的 EV，误差 ~2%（random 7,178 vs
7,016、call-station 3,072 vs 2,972、overly-tight 742 vs 738），4 baseline 全正显著，
LBR 1,233 优于旧路径 1,503（均 ≪ uniform 5,617）——即 batched-parallel 优化学习
正确，未破坏收敛。round 详记
`docs/temp/training_throughput_batched_parallel_2026_05_24.md`。

长 run wall 估算（c6a.8xlarge / EPYC 7R13，32t **优化 B=128**，按实测 500M 累计
12,063/s 含 checkpoint 暂停外推）：

| 目标 | 总 wall | cost @ $1.224/h (c6a.8xlarge) |
|---|---|---|
| 100M | ~2h18min | ~$2.8 |
| 200M | ~4h36min | ~$5.7 |
| 500M | **11h31min（实测 run_lcfr_500m）** | ~$14 |
| 1B | ~23h | ~$28 |

（旧路径估算 500M ~19h / $23，优化后 11.5h / $14，约省 40% wall。）
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

### LCFR-MCCFR 500M（优化 B=128，run_lcfr_500m，vultr 持久 + 新 aws box）

- `~/dezhou_20260508/artifacts/run_lcfr_500m/nlhe_es_mccfr_final_000500000000.ckpt`
- 8.2 GiB / b3sum `07959a8630b9855eadfd83bec79ab60fe0b8157a92d4491dd2a80ee4bb920c92`
- strategy_blake3 `74addbc492f827f2d7247df4814592363119ce1751743015b9e2e257034516d4`
- seed `0x4e4c48455f48335f` / LCFR period `1_000_000` / bucket cafebabe v3 `1c22c1ee...`
- update_count 500,000,000 / wall 41,450s = 11h31min / throughput avg 12,063/s（c6a.8xlarge B=128）
- **LBR proxy 1,126.05 chips ± 88.1**（probes=1000, seed=0x42, fallback=hybrid）
- LBR 曲线（同 run 自身 checkpoint）：uniform 5,617.8 → 100M `1,233.53 ± 96.9` → 500M `1,126.05 ± 88.1`
- **100M → 500M 质量饱和**：LBR 仅降 −8.7%（差值 107 < 合并 SE 131，**不到 1 SE，不显著**），
  与 ES-MCCFR 同 profile 的 100M→500M 0 收益一致。→ **100M 是该 profile blueprint 甜点，
  500M 不值（多 ~7h / ~$8 只换噪声内微动）**。此前 next-step 选项 A 的疑问（LCFR 是否一路向下）
  答案 = 类似 floor。
- 此 run 100M blueprint 同时是 batched-parallel 优化路径的收敛域正确性证据（见 §训练吞吐基线）。

### dense 后端 100M（dense 表 + v4 bucket，run_dense_lcfr_100m，vultr 持久）

- `~/dezhou_20260508/artifacts/run_dense_lcfr_100m/nlhe_es_mccfr_final_000100000000.ckpt`
- 4.7 GiB（dense raw v3 格式）/ b3sum `e1a346717f31a7c5332603d6803cbc511195c1abbb76797c27b9e26e0d515502`
- strategy_blake3 `2fab8afe11fa03f18c9adbea98c8569889717c1d4a85eec588d3292fd63022f7`
- seed `0x4e4c48455f48335f` / LCFR period `1_000_000` / **bucket cafebabe v4** `ac501bcf...`（body BLAKE3）
- update_count 100,000,000 / wall 3,819.7s = 63.7min / throughput avg 26,180/s（AWS 32 vCPU dense，2026-05-26）
- **LBR proxy 1,142.86 chips ± 87.01**（probes=1000, fallback=hybrid, 964 probes used）
- vs HashMap+v3 100M baseline `1,233.53 ± 96.9`：差 91 < 合并 SE 130，**不显著 = 同质量**（dense 后端 + v4
  reindex bucket 未改变 blueprint 质量）。uniform-0 对照 5,680 ≈ 历史 5,617，estimator 自洽。
- **新增 CLI**（think `8a4023e` / `4b39efc`）：`train_cfr --dense`（DenseNlheEsMccfrTrainer 后端）、
  `nlhe_h3_report --dense`（评测 dense raw v3 ckpt）。中间 25/50/75M ckpt 未存（仅在已 terminate 的 AWS box）。
- H3 baseline EV（此 dense run，mbb/g，全 4 个 95% 正显著）：random +5,887 [3,480, 8,294] /
  call-station +3,628 [2,662, 4,593] / overly-tight +565 [367, 762] / equity-ev +2,658 [354, 4,961]。
  （绝对值与下表 LCFR 100M 不同档：eval seed 不同 + 2,000 hands 噪声大，random/equity-ev SE ±1,200；
  LBR 才是稳定质量度量，且对齐 baseline。）

### H3 baseline EV @ LCFR 100M（mbb/g，正值 = 训练侧赢）

| baseline | mbb/g | 95% CI |
|---|---:|---|
| random | +7,016 | [4,544, 9,487] |
| call-station | +2,972 | [2,107, 3,837] |
| overly-tight | +738 | [493, 982] |
| equity-ev | +6,246 | [3,734, 8,758] |

4 baseline 全 95% 正显著。

## bucket table 工件

3 个 production v4 artifact 在 vultr `~/dezhou_20260508/artifacts/`（不进 git，旁路 `.b3sum` anchor）：

| seed | filename | body BLAKE3 | EVR |
|---|---|---|---|
| **cafebabe (canonical)** | `bucket_table_default_500_500_500_seed_cafebabe_schemav4.bin` | `ac501bcfb7aef43b816f78c81d315d92f2602d5d932afedfce5e0a314bbe19c9` | **0.9712** |
| deadbeef | `bucket_table_default_500_500_500_seed_deadbeef_schemav4.bin` | `56f836729ca5213affbb0ac9daad643de0432943593e92ced78972243f4ac57a` | 0.9711 |
| b16b00b5 | `bucket_table_default_500_500_500_seed_b16b00b5_schemav4.bin` | `18e5233fee7f2b17b04a0907ebba8337ee59728fe0d5af83519b3c9ccbd547a6` | 0.9709 |

Stage 3 CFR 输入钉死 `cafebabe`（EVR 最高）；`deadbeef` / `b16b00b5` 留 seed robustness 对照。
schema_version=4 / feature_set_id=2（16-dim hist+OCHS feature）；v1/v2/v3 artifact 不再可加载。

**v4 来历**（2026-05）：`canonical_enum` 把 canonical observation id 编号从「整表 u128
sort rank」改为 shape-major direct combinatorial rank（O(1) 公式，消除 ~2.2 GB lazy 表
+ ~3 min build）。bucket 分配与编号无关，故 v4 由 `tools/bucket_table_reindex_v3_to_v4`
对 v3 表 lookup 段做无损重排得到（**未重训**，bucket 逐一对应、EVR 不变）。旧 v3 文件
已删（重排可逆：排列是双射，必要时可从 v4 反向重排回 v3 编号，或重训）。校验：
`bucket_quality` 全部质量门槛在 v4 cafebabe 上 19/19 绿（= same hand → same bucket
端到端验证）。

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
| vultr 64.176.35.138 (4 vCPU AMD EPYC-Rome / 7.7 GiB) | 持久存储 + 短测试 | 长期持有；bucket artifact + v3 ckpt + Leduc LCFR + run_lcfr_500m ckpt 都在 |
| AWS c6a.8xlarge (32 vCPU AMD EPYC 7R13 / 61 GiB) | LCFR 训练机（IP 每次变，当前 18.221.200.43） | run_lcfr_500m 训练完成；按需起/停，一键部署见 `scripts/deploy-aws-training.sh` |

vultr **跑不动 NLHE 训练**：3M updates 时 RSS 超 7 GiB 进 swap，throughput 从 3.5K/s 衰减到 800/s。
NLHE 训练必须 ≥ 32 vCPU / ≥ 32 GiB。AWS c7i.4xlarge ($0.71/h) 或 c6a.8xlarge ($1.224/h) 都跑得动；
c6a 单核略弱但每 vCPU 便宜，wall 同档（受 serial merge 卡死）。

## 下一步（待决策）

之前的选项 A（500M LCFR）已执行 = `run_lcfr_500m`，结论入账：

- **update 数不是瓶颈**：ES-MCCFR 与 LCFR 在当前 profile 都 100M 即饱和（100M→500M LBR < 1 SE）。
  再加 update（1B 等）大概率同 floor，不值。LCFR 100M LBR 1,503 / 优化路径 100M 1,233 是当前最优区间。
- **优化 batched-parallel 路径已验证正确**（NLHE 收敛域：复现 baseline EV + LBR 在学习区间）。
- **dense 后端 + v4 bucket 已端到端验证**（`run_dense_lcfr_100m`：throughput ~2.2× HashMap 且长 run 不塌、
  RAM 平 5.2 GiB、checkpoint 不暴涨、100M LBR 同质量）——bet-size 扩张的前置 enabler 就位。

→ 若要更强 blueprint，杠杆不在迭代数，而在 **information abstraction（bucket 数 / 特征）或 action
abstraction 粒度**——这是架构级改动。**下一步明确**：上 flop bet-size 扩张 `{0.33,0.66,1,2}` / 359.6M infoset
（按街 abstraction 已落地 `3379db8`，dense 两表 13.48 GiB 需 32–64 GB 机），dense path 已扫清内存/正确性障碍。
剩余候选（D. strategy-only LCFR 对照）仍可选但优先级低。

## 文档维护规则

- 工作笔记 / 临时数据 → `docs/temp/*.md`
