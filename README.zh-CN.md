# poker — 无限注德州扑克求解器（Rust）

> English version: [README.md](./README.md)

一个可复现、可训练、可评测的 **无限注德州扑克（No-Limit Texas Hold'em）** 求解器，Rust 实现。
**heads-up（2 人）200BB** 求解器是已收尾的基线，**6-max（6 人）100BB blueprint** 是当前主线。
核心 API（规则引擎、座位模型、抽象层、训练 trait、收益向量）全部对 `n_seats` 保持通用，
在 2 人与 6 人之间切换时不需要重写内核。

- **语言 / 技术栈**：Rust 2021，工具链锁定 `1.95.0`（`rust-toolchain.toml`），禁用 `unsafe`。
- **算法**：External-Sampling MCCFR / LCFR（Brown & Sandholm 2018 Discounted MCCFR）、dense 表后端、
  流式 checkpoint、信息抽象（翻前 169 lossless + 翻后 equity/OCHS 桶）。
- **评测**：LBR / best-response（heads-up）、AIVAT 降方差对照、以及对 Slumbot（HUNL）与
  OpenPoker（6-max）的实测对战。

---

## 当前状态

### Heads-up NLHE（阶段 H1–H5）—— 已收尾 ✅

1B update 的 dense blueprint（200BB）对 Slumbot **近 break-even**：10,000 手 AIVAT 下
raw −85.25 / AIVAT −108.31 mbb/g，置信区间跨 0（符合预期、未显著）。LCFR / batched-parallel /
dense 后端 + v4 bucket + AIVAT 评测链全部端到端验证。剩余收尾动作 = Slumbot 对战数据持续采集。
详见 [`docs/heads_up_nlhe_solver_target.md`](./docs/heads_up_nlhe_solver_target.md)。

### 6-max NLHE blueprint-only（路线 A，100BB）—— 主线 🚧

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

6-max 主线的关键结论：

- **6-max 是多人一般和博弈**，CFR 自对弈不再保证收敛 Nash，LBR/exploitability 失去理论意义
  （只作诊断）。质量以**实测对战**为准，没有"训到 floor 就停"。
- **Preflop reshape**（删非 SB 开池 limp + 加 2.25BB 开池档）把翻前支配对翻转从 ~13% 降到 <1%，
  树最多缩 ~4.2×。GTO Wizard 真值修正确认 SB limp / AA-limp 是 GTO 而非缺陷。
- **实时搜索 MVP**：瓶颈是 **blueprint / 抽象质量**，不是搜索 root——干净、训透的节点（flop-first）
  中性不亏，而朴素放宽触发面到所有翻后节点会在弱基底上退化。
- **叠加剥削 Tier 2**（进程内对手画像 → 收敛门 → 脱锚搜索路径上翻前宽度 range 凸混合）在
  `--exploit on|vpip|off` 后面，默认 `off`（关时与现网策略 byte-equal）。

详见 [`docs/six_max_nlhe_target.md`](./docs/six_max_nlhe_target.md)（主线验收目标）与
[`docs/status_v3.md`](./docs/status_v3.md)（代码真实状态）。

---

## 算法正确性（已验证的基础）

这是 6-max 主线所建立在的可复用基础。每一行都有外部对照（不变量 #7：改算法必须有外部对照才能落地）。

| 项目 | 状态 | 依据 |
|---|---|---|
| Kuhn / Leduc Vanilla CFR | ✅ 收敛 closed-form `-1/18`，exploitability `<0.1` | `tests/cfr_kuhn.rs`、`tests/cfr_leduc.rs` |
| Leduc ES-MCCFR / LCFR-MCCFR | ✅ `ev_p0` 收敛 -0.087；ES 路径 BLAKE3 byte-equal anchor | `tools/leduc_es_mccfr_report` |
| 简化 NLHE ES-MCCFR / LCFR | ✅ LCFR 100M LBR 1,233 → 500M 1,126（100M 即饱和） | `run_lcfr_*`（vultr） |
| dense 后端 + v4 bucket | ✅ byte-equal（对 HashMap）；吞吐 ~2.2×、RAM 平 5.2 GiB、ckpt 不暴涨 | `tests/dense_nlhe_trainer.rs` |
| AIVAT 评测器 | ✅ 无偏（全证）；真日志降方差 1.21× | `tests/aivat_nlhe_*.rs`、`docs/aivat_eval.md` |
| CFR trainer / 规则引擎 6-max N-generic | ✅ 多人 side pot 返回 per-seat 收益向量；traverser 按 `% n_players` 轮换 | `src/training/trainer.rs`、`src/rules/state.rs` |

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

可选 PokerKit 跨验证（6-max 阶段 S1 会用）：

```bash
uv venv --python 3.11 .venv-pokerkit
uv pip install --python .venv-pokerkit/bin/python "pokerkit==0.4.14"
PATH=".venv-pokerkit/bin:$PATH" cargo test
```

> **测试在哪跑**：本机只信任 `build` / `fmt` / `clippy`。完整训练和长测试套件在远端主机跑
> （性能不足的本机跑出来的结果不可信）。

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

## 文档导航

按权威级别从高到低读：

1. [`docs/status_v3.md`](./docs/status_v3.md) —— 代码真实状态。**先看正确性表，再决定要不要动代码。**
2. [`docs/invariants.md`](./docs/invariants.md) —— 代码层硬约束。
3. [`docs/six_max_nlhe_target.md`](./docs/six_max_nlhe_target.md) —— 当前主线目标（6-max blueprint-only，S1–S6 门槛）。
4. [`docs/heads_up_nlhe_solver_target.md`](./docs/heads_up_nlhe_solver_target.md) —— heads-up 阶段（H1–H5），已收尾。
5. [`docs/aivat_eval.md`](./docs/aivat_eval.md) —— AIVAT 评测器细节。
6. [`CLAUDE.md`](./CLAUDE.md) / [`AGENTS.md`](./AGENTS.md) —— 仓库导航 + 给编码 agent 的工作规则。

---

## 工作语言

文档与 commit message 用中文；Rust 标识符与内联注释用英文（Rust 习惯）。

## License

MIT OR Apache-2.0。
