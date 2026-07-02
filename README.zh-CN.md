# poker — 一个用 Rust 写的无限注德州扑克求解器

> English version: [README.md](./README.md)

这是一个无限注德州扑克（No-Limit Texas Hold'em）求解器。它先离线跟自己打上亿手牌、学出一套策略，
再用这套策略——需要时叠加实时搜索——去和 Slumbot、OpenPoker 这类公开 bot 打真实对局。

项目最初是一个 heads-up（2 人，200BB）求解器，现在和 6-max（6 人，100BB）版本共用一套代码。规则引擎、
座位模型、抽象层、trainer 都按"任意人数"来写，6-max 版本跑在同一套内核上，再配上新的抽象和一棵大得多的
博弈树。

贯穿始终的一条取舍是正确性优先：一切都对外部真值做校验，并且逐字节可复现。技术栈：Rust 2021，
工具链锁定 `1.95.0`，禁用 `unsafe`。

---

## 结果速览

| 线 | 设置 | 结果 |
|---|---|---|
| Heads-up 200BB | 1B update 的 blueprint 对 Slumbot，10,000 手 AIVAT | raw −85.25 / AIVAT −108.31 mbb/g——置信区间跨 0，近 break-even |
| 6-max 100BB | blueprint 对 OpenPoker 实时牌池 | 评测进行中；没有够强的公开参照 bot 可比 |

heads-up 是已完成的基线，6-max 是当前在做的主线。完整数字与方法见
[`docs/heads_up_nlhe_solver_target.md`](./docs/heads_up_nlhe_solver_target.md) 与
[`docs/six_max_nlhe_target.md`](./docs/six_max_nlhe_target.md)。

---

## 难在哪，怎么解

做一个无限注德州扑克求解器，本质上是要越过几个没有标准答案的难题。下面是每个难题，以及我们的选择。

**1. 博弈树大到没法直接求解。** 完整的 heads-up 德州，局面数量远超任何表能存下的规模，所以第一件事是把它
压缩成能训练的东西，同时保住真正要紧的区别。翻前的压缩是白送的：1,326 个起手牌落进 169 类（对子、同花、
非同花），翻牌前花色可以互换，这样归类是精确的。翻后是有损的——每手牌变成一个短特征向量（equity 直方图 +
对手簇手牌强度），再用 k-means 聚成桶，heads-up 每条街 1,000 个、6-max 200 个，按从弱到强编号。微妙之处
在于：看着相似的牌，在有结构的牌面上打法差别很大，所以对手簇强度是在单个 combo 粒度上算的，这样单色、对面
等牌面才不会算歪。

**2. 下注量抽象是一条钢丝。** 档位太少，策略容易被剥削；档位太多，树会爆到训不动。这一点我们是量过、不是拍
脑袋——光加一档半池下注，就能把 6-max 的树撑大 20 倍以上。所以下注量按底池比例、刻意留得很稀疏，翻后动作
封顶到 3 人，翻前再做一次重塑：删掉那些只加分支、不加多少策略的 limp，换上一档干净的开池。这次重塑把翻前
支配对翻转从 ~13% 降到 <1%，树最多缩了 4.2 倍。（GTO Wizard 的核对确认，我们保留的小盲 limp——包括用 AA
limp——是正确的 GTO，而不是杂讯。）

**3. 光看输出没法判断一个采样求解器对不对。** 离线策略（blueprint）由 External-Sampling MCCFR 训练，可选
叠加 Linear CFR 折扣（Brown & Sandholm 2018），跑在 dense 后端上、训到约十亿次 update。而 MCCFR 是蒙特
卡洛的：一个隐蔽 bug 不会让程序崩，它只会收敛到一套略微跑偏、但从外面看一切正常的策略。防线是让每次运行都能
从种子逐字节复现，并把每个算法都钉死在外部真值上——Kuhn、Leduc 这些小博弈有闭式解（trainer 正好命中
−1/18），PokerKit 交叉验证规则引擎，byte-equal 锚点抓不同后端之间的漂移。

**4. 六个人会打破 heads-up 依赖的理论。** 两人德州是零和的，所以 CFR 可证明地逼近 Nash 均衡，
"exploitability" 是一个能往零压的真实数字。6-max 是多人一般和博弈，这些全都不成立——自对弈没有均衡保证，
exploitability 也不再有意义。所以强弱以对外部 bot 的实战为准，而不是自对弈分数。仍然能迁移的是抽象：我们
实测确认过，heads-up 的单对手桶到 3 人依然有效（河牌的手牌排序相关性达 Spearman 0.9995），所以 6-max 直接
建在同一套桶上，而不是另起一套。

**5. 方差会盖住你到底赢没赢。** 德州的噪声大到几千手牌都分不清是真实优势还是运气。heads-up 用 AIVAT——一种
无偏的降方差方法，在同样的实战日志上能把估计收紧约 1.2 倍。6-max 没有够强的公开参照 bot 可比，所以那边的
评测就是对 OpenPoker 牌池的实战。

**6. 实时搜索是强度所在，也最容易帮倒忙。** 最强的现代 bot 会在牌桌上现场搜索当前局面，而不只是查表。但支撑
这件事的安全性保证（DeepStack 式的 re-solving）在多人一般和里并不成立，而在弱基底上做搜索反而会亏。我们的
子博弈搜索是一种紧凑、深度受限的求解，思路接近 Pluribus 和 Modicum，在子树上复用同一套 trainer。老实的
结论是：瓶颈是 blueprint 和抽象的质量，而不是搜索本身——保守的触发（翻牌第一个决策点）保持中性，而放宽就
退化。所以搜索按保守配置发布，真正的杠杆是更好的 blueprint。另有一个可选模式，边打边给对手建画像、对见得
够多的对手轻微偏移范围；默认关闭，关闭时与原策略逐字节一致。

---

## 构建与运行

```bash
# 一次性：安装锁定的 Rust 工具链（rustup + 1.95.0 + rustfmt + clippy）。幂等，已装则跳过。
./scripts/setup-rust.sh
. "$HOME/.cargo/env"   # 首次安装后把 cargo 加进当前 shell

# 构建 / lint / 格式化门槛
cargo build --all-targets
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings

# 测试
cargo test                          # 默认套件
cargo test --release -- --ignored   # 长跑性能/正确性 SLO + BLAKE3 anchor
```

完整流水线是：导出手牌特征 → 拟合桶表 → 训练 blueprint → 评测 → 对打实时对手。每个工具都用朴素的
空格分隔参数（不支持 `--flag=value`），都接受 `--help`。

```bash
# 1. 导出每条街的手牌特征
cargo run --release --bin bucket_features_dump -- --street flop  --output artifacts/features_flop.bin
cargo run --release --bin bucket_features_dump -- --street turn  --output artifacts/features_turn.bin
cargo run --release --bin bucket_features_dump -- --street river --output artifacts/features_river.bin

# 2. 用 k-means 拟合桶表（heads-up 每条街 1000；6-max 用 200）
cargo run --release --bin bucket_kmeans_fit -- \
  --feature-flop artifacts/features_flop.bin --feature-turn artifacts/features_turn.bin \
  --feature-river artifacts/features_river.bin \
  --bucket-flop 1000 --bucket-turn 1000 --bucket-river 1000 \
  --training-seed 0xcafebabe --output artifacts/bucket_table.bin

# 3. 训练 blueprint（很重——在远端主机跑；参考 scripts/deploy-aws-training.sh）
#    6-max 100BB，A3×A4 + preopen 重塑：
cargo run --release --bin train_cfr -- --game nlhe --trainer es-mccfr --dense --lockfree \
  --profile six-max --postflop-cap 3 --reshape preopen \
  --bucket-table artifacts/bucket_table_200_200_200_seed_cafebabe_schemav4.bin \
  --updates 10000000000 --lcfr-period 1000000000 --checkpoint-dir artifacts/run_6max

# 4. 评测：6-max 基线门，或 heads-up AIVAT
cargo run --release --bin six_max_eval -- \
  --bucket-table artifacts/bucket_table_200_200_200_seed_cafebabe_schemav4.bin \
  --checkpoint artifacts/run_6max/nlhe_es_mccfr_final_010000000000.ckpt \
  --postflop-cap 3 --hands-per-seat 170000

cargo run --release --bin aivat_build_values -- --checkpoint <ckpt> --bucket-table <bt> --out artifacts/aivat_values.bin
cargo run --release --bin aivat_eval -- --checkpoint <ckpt> --bucket-table <bt> \
  --vf artifacts/aivat_values.bin --strategy-log slumbot_strategy.jsonl

# 5. 对打实时对手（Python driver 通过 stdio JSON 拉起 Rust advisor）
python3 tools/slumbot_play.py   --checkpoint <ckpt> --bucket-table <bt> --username <u> --password <p> --num-hands 1000
python3 tools/openpoker_play.py --checkpoint <ckpt> --bucket-table <bt> --reshape preopen --postflop-cap 3 --api-key <key> --num-hands 1000
```

6-max advisor 还带实时搜索和对手剥削的参数（`--search*`、`--exploit on|vpip|off`，默认 off）——完整清单见
[`docs/six_max_nlhe_target.md`](./docs/six_max_nlhe_target.md)。两个 driver 都接受 `--selftest`，
无需账号即可做离线 IPC 自检。

可选 PokerKit 跨验证：

```bash
uv venv --python 3.11 .venv-pokerkit
uv pip install --python .venv-pokerkit/bin/python "pokerkit==0.4.14"
PATH=".venv-pokerkit/bin:$PATH" cargo test
```

> 本机只信任 `build` / `fmt` / `clippy`。真正的训练和长测试套件在远端主机跑——性能不足的本机跑出来的结果不可信。

---

## 仓库结构

```
src/
  core/         基础类型（Card / ChipAmount / SeatId / Street ...）+ 显式 RngSource
  rules/        桌面配置、动作、状态机、side pot、showdown
  abstraction/  动作抽象 + 信息抽象、翻前 169、equity/OCHS、桶表、InfoSetId
  training/     Game trait、CFR/MCCFR trainer、checkpoint、NLHE game 适配、
                blueprint advisor、子博弈搜索、对手画像、AIVAT/LBR 评测
  eval.rs history.rs error.rs lib.rs
tests/          集成测试 + 跨验证（在远端主机跑）
tools/          诊断 / 训练 / 实战 binary + Python driver
benches/        Criterion 基准
proto/          protobuf schema（hand history）
scripts/        安装 + 部署 + 跨验证脚本
docs/           设计文档、验收目标、决策/API 记录（中文）
```

主要 binary（在 `tools/`，于 `Cargo.toml` 声明）：

- 训练：`train_cfr`
- 桶 / 抽象：`bucket_features_dump`、`bucket_kmeans_fit`、`bucket_quality_dump`、
  `bucket_table_reindex_v3_to_v4`
- 6-max 评测 / 实验：`six_max_eval`、`six_max_blueprint_h2h`、`six_max_search_probe`、
  `six_max_exploit_ab`、`six_max_cross_street_ab`、`six_max_unanchored_prefix_ab`
- AIVAT：`aivat_build_values`、`aivat_eval`、`openpoker_hh_aivat`
- 实战：`slumbot_advisor` + `tools/slumbot_play.py`（HUNL）、
  `openpoker_advisor` + `tools/openpoker_play.py`（6-max WebSocket）
- 复现 / 诊断：`b3sum`、`nlhe_blake3_anchor`、`nlhe_checkpoint_vs_checkpoint`、
  `nlhe_betting_tree_sizing`、`leduc_es_mccfr_report`、`mccfr_trace`、`nlhe_trace`

---

## 测试、验证与硬约束

每个算法改动都要配一个外部对照——闭式解、PokerKit 对比、或者已知正确跑法的 byte-equal 锚点。完整证据清单
（哪个测试证明了什么）在 [`docs/status_v3.md`](./docs/status_v3.md)；PR 必须满足的正确性规则在
[`docs/invariants.md`](./docs/invariants.md)。由编译器、clippy 和 `Cargo.toml` 强制的硬约束：

1. 规则 / 评估器 / 抽象层不用浮点——筹码 `u64`、盈亏 `i64`、评估器返回整数 rank、bucket id 离散整数。
   唯一允许浮点的位置是 CFR 内部的 σ / regret 累加。
2. 不用全局 RNG——所有随机性走显式 `RngSource`；byte-equal 复现是发现算法 bug 的最低门槛。
3. 不用 `unsafe`——`Cargo.toml` 里 `unsafe_code = "forbid"`。
4. `ChipAmount::Sub` 下溢 panic（debug + release）——筹码负数永远是 bug；要 saturating 行为用 `checked_sub`。
5. `Action::Raise { to }` 是绝对值——`to` 是 raise 的目标金额（含已下注部分）。
6. 座位方向唯一约定——`SeatId((k+1) mod n_seats)` 是 `SeatId(k)` 的左邻；所有"向左"语义
   （按钮轮转 / 大小盲 / odd-chip / 摊牌顺序 / 发牌起点）共用这一条。

---

## 项目进度

Heads-up（200BB，阶段 H1–H5）是已完成的基线：整条训练与评测链已端到端验证，blueprint 对 Slumbot 近
break-even（见上方结果）。当前那边的动作是 Slumbot 对战数据持续采集。

6-max 仅 blueprint（路线 A，100BB）是当前主线，2026-05-30 立项——先把离线自对弈 blueprint 端到端跑通
（参数化 game → 多人抽象 → 任意人数通用的 trainer → 实测对战评测），实时搜索作为后续，而不是硬门槛。

| 阶段 | 重点 | 状态 |
|---|---|---|
| S1 | 规则 / 6-max profile | 已闭（余 100k PokerKit 重跑） |
| S2 | 树规模 + A3×A4 抽象接进生产 | 已闭 |
| S3 | 多人桶（单对手桶 ≤3-way 可复用） | 已闭 |
| S4 | 1B dense 训练 + preflop reshape | 已跑完一轮并独立复核 |
| S5 | 跨抽象 advisor 引擎、跨抽象 h2h、OpenPoker live 客户端 | 端到端 smoke 已过 |
| S6 | 实时子博弈搜索 MVP | 核心已落地并验证（分支上，未并入） |

代码真实状态：[`docs/status_v3.md`](./docs/status_v3.md)。

---

## References

设计沿用的是一条成熟的不完美信息博弈研究脉络。

- Zinkevich, Johanson, Bowling, Piccione (2007). *Regret Minimization in Games with Incomplete Information.* — CFR。
- Lanctot, Waugh, Zinkevich, Bowling (2009). *Monte Carlo Sampling for Regret Minimization in Extensive Games.* — MCCFR。
- Johanson, Burch, Valenzano, Bowling (2013). *Evaluating State-Space Abstractions in Extensive-Form Games.* — OCHS。
- Moravčík 等 (2017). *DeepStack: Expert-Level AI in Heads-Up No-Limit Poker.* Science。
- Brown, Sandholm, Amos (2018). *Depth-Limited Solving for Imperfect-Information Games.* NeurIPS。— Modicum。
- Burch, Schmid, Moravčík, Morrill, Bowling (2018). *AIVAT: A New Variance Reduction Technique for Agent Evaluation.* AAAI。
- Brown, Sandholm (2019). *Solving Imperfect-Information Games via Discounted Regret Minimization.* AAAI。— Discounted/Linear CFR。
- Brown, Sandholm (2019). *Superhuman AI for Multiplayer Poker.* Science。— Pluribus。
- Kim 等 (2023). *PokerKit: A Comprehensive Python Library for Fine-Grained Multi-Variant Poker Game Simulations.* IEEE ToG。— 跨验证参照库。

---

## 工作语言

文档与 commit message 用中文；Rust 标识符与内联注释用英文（Rust 习惯）。

## License

MIT OR Apache-2.0。
