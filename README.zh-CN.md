# poker — 无限注德州扑克求解器（Rust）

> English version: [README.md](./README.md)

一个可复现、可训练、可评测的 **无限注德州扑克（No-Limit Texas Hold'em）** 求解器，Rust 实现。
共用一套内核，覆盖两条线：**heads-up（2 人）200BB** 与 **6-max（6 人）100BB blueprint**。
核心 API（规则引擎、座位模型、抽象层、训练 trait、收益向量）全部对 `n_seats` 保持通用，
在 2 人与 6 人之间切换时不需要重写内核。

- **语言 / 技术栈**：Rust 2021，工具链锁定 `1.95.0`（`rust-toolchain.toml`）。
- **算法**：External-Sampling MCCFR / LCFR（Brown & Sandholm 2018 Discounted MCCFR）、dense 表后端、
  流式 checkpoint、信息抽象（翻前 169 lossless + 翻后 equity/OCHS 桶）。
- **评测**：LBR / best-response（heads-up）、AIVAT 降方差对照、以及对 Slumbot（HUNL）与
  OpenPoker（6-max）的实测对战。

---

## 核心工作

从规则到实测对战，靠三块工作串起来：牌怎么分桶、离线 blueprint 怎么训、实时搜索怎么在牌桌上细化它。

### 信息抽象（分桶）

- **翻前 169 类 lossless**：1,326 个起手牌收敛为 13 对子 + 78 同花 + 78 非同花。翻前 equity 在类内
  suit-invariant，所以这一步不丢信息。
- **翻后**用每条街 16 维特征向量——flop/turn = 8 桶 equity 直方图 + 8 桶 OCHS，river = OCHS-16——
  用 k-means 聚类（HU 生产 1000/1000/1000，6-max 200/200/200，按 flop/turn/river）。OCHS（Opponent
  Cluster Hand Strength）在 combo 级展开，所以单色 / 对面 / 双色牌面能被正确打分。
- 簇按 EHS 中位数升序重排，**簇 0 = 最弱**；桶表 schema 为 v4（v3 表直接拒读，不会静默错读）。除手牌桶外，
  `InfoSetId` 还把位置、筹码深度、下注状态作为独立维度携带。
- 6-max **复用 heads-up 的单对手桶，最多到 3-way**——已验证而非假设（river OCHS Spearman 0.9995；
  flop/turn 簇一致性落在 k-means 种子噪声地板内）。

### Blueprint（离线训练）

- 用 **External-Sampling MCCFR** 训练，可选叠加 **Linear CFR** 折扣（Brown & Sandholm 2018 Discounted
  MCCFR），跑在 **dense 表后端**（对 HashMap 后端 byte-equal、吞吐 ~2.2×、RAM 平稳），训到约 1B 采样
  update，配流式 checkpoint。
- 动作抽象是 **pot 相对的下注档**。6-max 抽象（A3×A4）把翻后封顶 ≤3-way 并用精选档位，因为小注会让树爆炸。
  `--reshape` 可选删掉非 SB 的 open-limp 并加一档 2.25BB 开池，清理翻前支配对翻转。
- 码深：**HU 200BB**（对齐 Slumbot）、**6-max 100BB**（对齐 Pluribus）。实战时 advisor 通过每张网一个
  "抽象影子" 查 blueprint，把脱树下注档做 off-tree 映射；遇到结构性动作集缺口会显式报 desync，而不是静默塌缩。

### 实时搜索

- Pluribus / Modicum 风格的**表式深度受限子博弈搜索**，只替换 "向 blueprint 要一个分布" 这一步——通过一个
  只重写树 root 的 `SubgameNlheGame` 复用同一套 MCCFR trainer。
- 触发默认 **flop-first**（中性）；放宽到所有翻后节点会在当前基底上退化，所以那是研究性开关。MVP 把子树解到真
  终局；也提供深度受限的 blueprint 续局叶子值（含 biased 续局）。
- **叠加剥削（Tier 2，`--exploit on|vpip|off`，默认 off）** 在进程内画像对手（VPIP / PFR / 翻后 AF），
  过统计收敛门后，在脱锚搜索路径上把对手翻前 range 向观测宽度做凸混合。默认 off 与现网策略 byte-equal。

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
tools/          诊断 / 训练 / 实战 binary（见下）+ Python 辅助脚本
benches/        Criterion 基准
proto/          protobuf schema（hand history）
scripts/        安装 + 部署 + 跨验证脚本
docs/           设计文档、验收目标、决策/API 记录（中文）
```

### 主要 binary（`tools/`，在 `Cargo.toml` 声明）

- **训练**：`train_cfr`
- **6-max 评测 / 实验**：`six_max_eval`、`six_max_blueprint_h2h`、`six_max_search_probe`、
  `six_max_exploit_ab`、`six_max_cross_street_ab`、`six_max_unanchored_prefix_ab`
- **实战（advisor + driver）**：`slumbot_advisor` + `tools/slumbot_play.py`（HUNL）、
  `openpoker_advisor` + `tools/openpoker_play.py`（6-max WebSocket）
- **AIVAT**：`aivat_build_values`、`aivat_eval`、`openpoker_hh_aivat`
- **桶 / 抽象**：`bucket_kmeans_fit`、`bucket_quality_dump`、`bucket_table_reindex_v3_to_v4`、
  `bucket_features_dump`
- **复现 / 诊断**：`b3sum`、`nlhe_blake3_anchor`、`nlhe_checkpoint_vs_checkpoint`、
  `nlhe_betting_tree_sizing`、`leduc_es_mccfr_report`、`mccfr_trace`、`nlhe_trace`

---

## 快速上手

```bash
# 一次性：安装锁定的 Rust 工具链（rustup + 1.95.0 + rustfmt + clippy）。幂等，已装则跳过。
./scripts/setup-rust.sh
# 首次安装后把 cargo 加进当前 shell（之后新 shell 自动加载）。
. "$HOME/.cargo/env"

# 构建 / lint / 格式化门槛
cargo build --all-targets
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings

# 测试
cargo test                          # 默认套件
cargo test --release -- --ignored   # 长跑性能/正确性 SLO + BLAKE3 anchor
```

---

## 代码层硬约束（由编译器 / clippy / `Cargo.toml` 强制）

详见 [`docs/invariants.md`](./docs/invariants.md)。违反这些规则的 PR 不通过。

1. **规则 / 评估器 / 抽象层不用浮点**——筹码 `u64`、盈亏 `i64`、评估器返回整数 rank、bucket id 离散整数。
   唯一允许浮点的位置是 CFR 内部的 σ / regret 累加。
2. **不用全局 RNG**——所有随机性走显式 `RngSource`；byte-equal 复现是发现算法 bug 的最低门槛。
3. **不用 `unsafe`**——`Cargo.toml` 里 `unsafe_code = "forbid"`。
4. **`ChipAmount::Sub` 下溢 panic**（debug + release）——筹码负数永远是 bug；要 saturating 行为用 `checked_sub`。
5. **`Action::Raise { to }` 是绝对值**——`to` 是 raise 的目标金额（含已下注部分），与 NLHE / PokerKit 惯例一致。
6. **座位方向唯一约定**——`SeatId((k+1) mod n_seats)` 是 `SeatId(k)` 的左邻；所有"向左"语义
   （按钮轮转 / 大小盲 / odd-chip / 摊牌顺序 / 发牌起点）共用这一条。

---

## 基础设施

| 主机 | 角色 | 说明 |
|---|---|---|
| vultr（4 vCPU / 11.67 GiB） | 持久存储 + 短测试 | 存 1B dense ckpt + 桶表；**跑不动 NLHE 训练**（3M update 进 swap） |
| AWS（按需起停，IP 每次变） | 训练 | HU 用 `c6a.8xlarge`（32 vCPU）；6-max 大概率不够，待 S2 sizing 定更大机 |

持久 artifact（1B dense ckpt、桶表）在 vultr 的 `~/dezhou_20260508/artifacts/`。

---

## 工作语言

文档与 commit message 用中文；Rust 标识符与内联注释用英文（Rust 习惯）。

---

## 项目进度

### Heads-up NLHE（200BB，阶段 H1–H5）

1B update 的 dense blueprint 对 Slumbot **近 break-even**：10,000 手 AIVAT 下
raw −85.25 / AIVAT −108.31 mbb/g，置信区间跨 0（符合预期、未显著）。LCFR / batched-parallel /
dense 后端 + v4 bucket + AIVAT 评测链全部端到端验证。当前动作 = Slumbot 对战数据持续采集。
详见 [`docs/heads_up_nlhe_solver_target.md`](./docs/heads_up_nlhe_solver_target.md)。

### 6-max NLHE blueprint-only（路线 A，100BB）

2026-05-30 立项。路线 A = 先把离线自对弈 blueprint 端到端跑通（参数化 game → 多人抽象 →
复用 N-generic trainer → 实测对战评测），**不把实时 depth-limited search 作为硬门槛**。

| 阶段 | 重点 | 状态 |
|---|---|---|
| S1 | 规则 / 6-max profile | 已闭（余 100k PokerKit 重跑） |
| S2 | 树规模 + A3×A4 抽象接进生产 | 已闭 |
| S3 | 多人桶（单对手桶 ≤3-way 可复用） | 已闭 |
| S4 | 1B dense 训练 + preflop reshape（`--reshape none\|nolimp\|preopen\|preopen-small`） | 已跑完一轮并独立复核 |
| S5 | 脱锚跨抽象 advisor 引擎、跨抽象 h2h、OpenPoker live 客户端 | 端到端 smoke 已过 |
| S6 | 实时子博弈搜索 MVP | 核心已落地并验证（分支上，未并入） |

6-max 线的关键结论：

- **6-max 是多人一般和博弈**，CFR 自对弈不再保证收敛 Nash，LBR/exploitability 失去理论意义
  （只作诊断）。质量以**实测对战**为准，没有"训到 floor 就停"。
- **Preflop reshape**（删非 SB 开池 limp + 加 2.25BB 开池档）把翻前支配对翻转从 ~13% 降到 <1%，
  树最多缩 ~4.2×。GTO Wizard 真值修正确认 SB limp / AA-limp 是 GTO 而非缺陷。
- **实时搜索 MVP**：瓶颈是 **blueprint / 抽象质量**，不是搜索 root——干净、训透的节点（flop-first）
  中性不亏，而朴素放宽触发面到所有翻后节点会在弱基底上退化。
- **叠加剥削 Tier 2**（进程内对手画像 → 收敛门 → 脱锚搜索路径上翻前宽度 range 凸混合）在
  `--exploit on|vpip|off` 后面，默认 `off`（关时与现网策略 byte-equal）。

详见 [`docs/six_max_nlhe_target.md`](./docs/six_max_nlhe_target.md)（验收目标）与
[`docs/status_v3.md`](./docs/status_v3.md)（代码真实状态）。

---

## License

MIT OR Apache-2.0。
