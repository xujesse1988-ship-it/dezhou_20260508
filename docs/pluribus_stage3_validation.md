# 阶段 3：MCCFR 小规模验证的量化验证方式

## 阶段目标

阶段 3 的目标是把阶段 1 锁定的真实 NLHE 规则环境 + 阶段 2 锁定的抽象层接到 CFR / MCCFR 训练循环，**先在小博弈上验证 CFR 实现数值正确，再迁移到 2-player 简化 NLHE 跑通 100M 量级 sampled decision update**。本阶段不产 6-max blueprint（那是阶段 4），只验证 regret minimization 的数学正确性 + 训练循环的工程稳定性 + checkpoint 持久化。

阶段 3 需要支持：

- **Vanilla CFR**（full-tree counterfactual regret minimization）跑 Kuhn / Leduc 两个标准 benchmark：
    - **Kuhn Poker**（3-card 1-street，2-player，~12 InfoSet）— exploitability 必须收敛到 `< 0.01` chips/game（path.md §阶段 3 字面）。Kuhn Nash 解析解已知（player 1 expected value `-1/18`），可作为 ground truth 收敛锚点。
    - **Leduc Poker**（6-card 2-street，2-player，~288 InfoSet）— 训练曲线必须稳定改善（exploitability 单调下降），固定 seed 下结果可复现（path.md §阶段 3 字面）。Leduc 无 closed-form Nash 解析解，但 5K iter 后 exploitability 应稳定低于 `0.1` chips/game（CFR 文献常见 reference 数）。
- **ES-MCCFR**（External-Sampling Monte Carlo CFR）跑 **2-player 简化 NLHE**：
    - 简化范围（A0 [决策] 锁定 D-313..D-315）：2-player NLHE + stage 2 抽象层（5-action / preflop 169 / postflop bucket）+ 100 BB starting stack + 完整 4 街。这是 stage 4 6-max blueprint 的子集，复用 stage 2 的 `ActionAbstraction` / `InfoAbstraction` / `BucketTable` 全部基础设施。
    - 训练规模：至少 `100,000,000`（`10⁸`）次 sampled decision update（path.md §阶段 3 字面）。
- **Regret matching 数值正确性**：每次 regret matching 输出的动作概率分布 sum 必须 `[1 - 1e-9, 1 + 1e-9]` 内（path.md §阶段 3 字面）。
- **Checkpoint 持久化**：训练过程中任意 update count 写出 checkpoint → 进程重启 → 加载 checkpoint → 继续训练 → 训练结果（average strategy / regret table）必须与不中断的对照训练 byte-equal。

阶段 3 与阶段 1 / 阶段 2 最大边界差异：**regret / strategy 累积允许浮点**（必然——regret 不是整数量），但 InfoSet id（继承阶段 2）/ chip amount（继承阶段 1）/ random sampling（继承阶段 1 `RngSource`）路径整数 / 显式 RNG 不变量**全部继承**。浮点不得渗入阶段 1 锁定的规则路径 + 阶段 2 锁定的运行时映射热路径。

阶段 3 与阶段 1 / 阶段 2 最大工程风险差异：**没有 PokerKit 这样 byte-level 的开源参考实现可对照 CFR 训练曲线**。Kuhn 有 closed-form Nash 解析解可验证收敛锚点，Leduc 和简化 NLHE 没有。所以阶段 3 验收 **强依赖：① Kuhn closed-form anchor / ② Leduc 训练曲线 fixed-seed byte-equal / ③ 简化 NLHE 训练 throughput + checkpoint round-trip**，clustering determinism（继承阶段 2 头号不变量）+ regret matching 数值容差（path.md `1e-9` 字面）共同守住数值正确性。

## 量化验证方式

### 1. CFR / MCCFR 算法变体（双轨）

- **Vanilla CFR**（Kuhn / Leduc 小博弈用）— full-tree backward induction：每 iter 遍历完整博弈树，对每个 InfoSet 计算 counterfactual value + counterfactual regret + 更新 average strategy。**不采样**，确定性 + 单线程为主（性能要求低，几秒到几分钟内收敛）。D-300 锁定为 Kuhn / Leduc 唯一算法变体。
- **ES-MCCFR**（External-Sampling MCCFR，简化 NLHE 用）— Pluribus 论文 §S2 字面方法 + Lanctot 2009 paper：每 iter 选定一个 traverser，对 traverser 决策点遍历所有 action，对 chance node + non-traverser 决策点采样一个 action（按当前 strategy）。D-301 锁定为简化 NLHE 算法变体。**Linear CFR** weighting（Brown & Sandholm 2019 / Pluribus 实战）排在阶段 4 起步，阶段 3 简化 NLHE 用 **非 Linear** ES-MCCFR（D-302 锁定，避免 stage 3 同时引入 sampling + weighting 双变量）。
- **Regret matching+ 与 regret matching 选型**：D-303 锁定阶段 3 全部走标准 **regret matching**（`σ(I, a) = max(R(I, a), 0) / Σ max(R(I, b), 0)`，分母为 0 时回退均匀分布）；**regret matching+** 由阶段 4 决定。
- **average strategy 累积方式**：D-304 锁定阶段 3 走标准 **average strategy = Σ_t π_t(I) × σ_t(I)** 累积（π 为到达概率，σ 为当前 strategy）。**vanilla CFR**：精确 π；**ES-MCCFR**：traverser sampled reach probability + opponent baseline reach probability。

### 2. Kuhn Poker：closed-form anchor 收敛

- **规则**（D-310 锁定）：3 张牌 deck `{J, Q, K}` / 2 player / 每人发 1 张 / 各 ante 1 chip / 1 round betting / 最多 1 bet（size = 1 chip）/ player 1 先行动 `check / bet`。
- **InfoSet 数**：12 个（每 player 6 个：3 张牌 × 2 历史 `["", "pb"]` for player 1；3 张牌 × 2 历史 `["c", "b"]` for player 2）。
- **量化验收门槛**（path.md §阶段 3 字面 + D-340 锁定 exploitability 算法）：
    - **Player 1 expected value 收敛**：Kuhn Nash 解析解 player 1 EV = `-1/18 ≈ -0.0555`。`10,000` iter Vanilla CFR 后 average strategy 计算的 player 1 EV 与 `-1/18` 差距 `< 1e-3`。这是 Kuhn 最强的 ground truth anchor。
    - **Exploitability `< 0.01`** chips/game（path.md §阶段 3 字面）。Exploitability = `(BR_1(σ_2) + BR_2(σ_1)) / 2`，其中 `BR_i(σ_{-i})` 是 player i 对 player `{-i}` strategy 的 best response value。Kuhn 12-InfoSet 上 best response 可精确算（full-tree backward induction），无浮点不确定性。
    - **确定性**：固定 seed + 固定 iter 数 → average strategy 各 InfoSet 各 action 概率 byte-equal 一致（重复 1000 次跑 BLAKE3 一致）。
- **训练时长预算**：`10,000` iter Vanilla CFR 单线程 release `< 1` second（D-360 锁定 SLO 上界）。

### 3. Leduc Poker：fixed-seed 收敛曲线

- **规则**（D-311 锁定）：6 张牌 deck `{J♠, J♥, Q♠, Q♥, K♠, K♥}` / 2 player / 每人发 1 张私有牌（preflop）+ 1 张公共牌（postflop）/ 各 ante 1 chip / 2 round betting / 每 round 最多 2 raise / preflop bet size = 2 chip、postflop bet size = 4 chip / showdown：先比 pair（私有 == 公共），再比 rank。
- **InfoSet 数**：~288 个（6 私有 × 6 公共 × 历史；具体数由 D-311 锁定算出）。
- **量化验收门槛**（path.md §阶段 3 字面 + D-341 锁定 exploitability 算法）：
    - **训练曲线稳定改善**：`1K / 2K / 5K / 10K` iter 4 个 checkpoint，exploitability 单调非升（允许相邻两次 ±5% 噪声但不允许持续上升）。
    - **fixed seed 可复现**：固定 seed `42` 跑 10K iter → average strategy BLAKE3 byte-equal 重复一致（重复 10 次跑同 host 同 toolchain）。
    - **Exploitability 终值上界**：10K iter 后 exploitability `< 0.1` chips/game（CFR 文献常见 reference；D-341 锁定该阈值作为 stage 3 出口）。
- **训练时长预算**：`10,000` iter Vanilla CFR 单线程 release `< 60` second（D-360 锁定 SLO 上界）。

### 4. 简化 NLHE：100M sampled decision update 规模

- **规则**（D-313 锁定简化范围）：
    - **2-player**（heads-up；非 6-max——6-max 留 stage 4 blueprint）。
    - **starting stack = 100 BB**（继承 stage 1 D-022 默认）。
    - **盲注 0.5 BB / 1.0 BB**（继承 stage 1 D-022 默认）。
    - **完整 4 街**（preflop / flop / turn / river）。
    - **action abstraction = stage 2 `DefaultActionAbstraction`**（5-action：fold / check / call / 0.5×pot / 1.0×pot / all-in，继承 D-200）。
    - **info abstraction = stage 2 `PreflopLossless169` + `PostflopBucketAbstraction`**（preflop 169 lossless / postflop 500/500/500 bucket，继承 D-213 + D-217）。
    - **bucket table artifact**：**D-314 deferred**——bucket table 依赖只在 B2/C2 [实现] 真正构造 `SimplifiedNlheGame` 时被消费，A0 [决策] / A1 [脚手架] / B1 [Kuhn+Leduc 测试] / B2 [Kuhn+Leduc 实现] 不依赖。D-314 暂列入 `pluribus_stage3_decisions.md` §10 已知未决项（同形态 stage 2 §10 D-NNN 待锁机制），在 B2/C2 [实现] 简化 NLHE 之前锁定。备选两条：(a) **stage 2 C2 hash-based 95 KB v1 artifact**（已有，FNV-hash canonical id，已知 collision 约束在 stage 2 §C-rev1 §2 carve-out 范围内）；(b) **§G-batch1 §3.4-batch2 production artifact**（528 MB，schema v2，D-218-rev2 真等价类，~120 min vultr 训练）。A0..B2 期间（按 stage 2 时间线 2-3 周）有充裕窗口让 §G-batch1 §3.4-batch2 在 vultr 上跑完；到 C2 起步时若 v2 已 ready 走 v2，否则 v1 fallback。
- **量化验收门槛**（path.md §阶段 3 字面 + D-342 锁定）：
    - **训练规模**：至少 `100,000,000`（`10⁸`）次 sampled decision update。
    - **训练吞吐**：单线程 release `≥ 10,000 update/s`（D-361 锁定 SLO 下界；100M update / 10K/s = 10,000 second ≈ 2.78 小时，单 host bare-metal 可行）。多线程 `≥ 50,000 update/s` on 4-core（D-361 锁定多线程 SLO，效率 ≥ 0.5）。
    - **训练稳定性**：完整 100M update 训练全程无 panic / NaN / inf / regret table 溢出（average strategy / regret table 全部 `f64`，无 overflow 风险但需 monitor）。
    - **exploitability 监控**：简化 NLHE **不计算精确 best response**（state space 太大），仅监控 **average regret growth rate** 应呈 sublinear（path.md §阶段 4 字面延伸到 stage 3 监控；D-343 锁定监控阈值：`avg_regret / sqrt(T) ≤ const`，constant 由 stage 3 实测落地决定）。
    - **fixed-seed 可复现**：固定 seed + 固定 update count → regret table BLAKE3 byte-equal 重复一致。

### 5. Regret matching 数值正确性

- **动作概率 sum 容差**（path.md §阶段 3 字面）：任意 `(InfoSet, iter)` 上 regret matching 输出的动作概率分布 sum 必须 `|Σ σ(I, a) - 1| < 1e-9`。D-330 锁定该约束。
- **退化局面**（所有 regret ≤ 0）：D-331 锁定回退**均匀分布** `σ(I, a) = 1 / |actions(I)|`（标准 CFR convention）。
- **零和约束**：vanilla CFR 训练后期 player 1 EV + player 2 EV 应满足 `|EV_1 + EV_2| < 1e-6`（零和博弈 sanity check，Kuhn / Leduc 严格零和）。简化 NLHE 含 rake = 0 默认（继承 stage 1）也严格零和。D-332 锁定该约束。
- **regret table 数值类型**：D-333 锁定 `f64`（不是 `f32`）—— `f64` 在 100M update 量级避免累积误差超过 `1e-9` 容差。
- **数值容差测试**：`tests/regret_matching_numeric.rs`（B1 [测试] 落地）跑 1M 次随机 regret vector 输入，断言 sum 容差全部命中。

### 6. Checkpoint 持久化

- **checkpoint 内容**（D-350 锁定 schema）：
    - `schema_version: u32 = 1`（stage 3 起 checkpoint schema 独立编号，与 stage 1 history schema / stage 2 bucket table schema 不冲突）。
    - `trainer_variant: enum { VanillaCFR, ESMCCFR }`。
    - `game_variant: enum { Kuhn, Leduc, SimplifiedNlhe }`。
    - `update_count: u64`（已完成 update 数）。
    - `rng_state: [u8; 32]`（继承 stage 1 ChaCha20 state，保证恢复后 sampling sequence 续接 byte-equal）。
    - `regret_table: HashMap<InfoSetId, Vec<f64>>`（每个 InfoSet 各 action 累积 regret）。
    - `strategy_sum: HashMap<InfoSetId, Vec<f64>>`（每个 InfoSet 各 action 累积加权 strategy）。
    - `bucket_table_blake3: [u8; 32]`（依赖的 bucket table artifact whole-file BLAKE3，恢复时校验匹配；仅简化 NLHE 用）。
    - `trailer_blake3: [u8; 32]`（checkpoint file body BLAKE3 自校验，继承 stage 2 D-243 模式）。
- **round-trip 不变量**（path.md §阶段 3 字面）：
    - 训练 N update → 保存 checkpoint → 进程退出 → 加载 checkpoint → 继续训练 M update → 最终 regret_table / strategy_sum BLAKE3 与不中断对照训练（直接跑 N+M update）byte-equal。该不变量是 stage 3 头号工程门槛（继承 stage 2 clustering determinism 强度）。
- **checkpoint 加载错误路径**（D-351 锁定 5 类，继承 stage 2 D-247 模式）：
    - `CheckpointError::FileNotFound { path }`
    - `CheckpointError::SchemaMismatch { expected, got }`
    - `CheckpointError::TrainerMismatch { expected, got }`（trainer_variant / game_variant 不匹配）
    - `CheckpointError::BucketTableMismatch { expected, got }`（bucket_table_blake3 不匹配）
    - `CheckpointError::Corrupted { offset, reason }`（trailer BLAKE3 不匹配 / 字段越界）

### 7. 性能 SLO 汇总

| SLO | 阈值 | 路径 / 备注 |
|---|---|---|
| Kuhn 10K iter Vanilla CFR | 单线程 release `< 1 s` | D-360 锁定 |
| Leduc 10K iter Vanilla CFR | 单线程 release `< 60 s` | D-360 锁定 |
| 简化 NLHE 单线程 ES-MCCFR | `≥ 10,000 update/s` | D-361 锁定（100M update / 10K = 2.78 h） |
| 简化 NLHE 多线程 ES-MCCFR | `≥ 50,000 update/s` on 4-core | D-361 锁定（效率 ≥ 0.5） |
| Kuhn exploitability | `< 0.01` chips/game | path.md §阶段 3 字面 |
| Leduc exploitability | `< 0.1` chips/game @ 10K iter | D-341 锁定 |
| regret matching 概率 sum | `\|Σ σ - 1\| < 1e-9` | path.md §阶段 3 字面 / D-330 锁定 |
| checkpoint round-trip | BLAKE3 byte-equal | path.md §阶段 3 字面 / D-350 锁定 |
| Kuhn / Leduc / 简化 NLHE 重复确定性 | 同 seed BLAKE3 一致 | D-362 锁定 |

性能 SLO 走 `tests/perf_slo.rs::stage3_*`，与阶段 1 / 阶段 2 同形态：release profile + `--ignored` 显式触发，CI nightly 跑 bench-full + 短 bench 在 push 时跑。

### 8. 与阶段 1 / 阶段 2 的不变量边界

继承阶段 1 + 阶段 2 全部不变量（**规则路径无浮点 / 无 `unsafe` / 显式 `RngSource` / 整数筹码 / `SeatId` 左邻一致性 / Cargo.lock 锁版本 / clustering determinism**），并在 stage 3 显式划分：

- 阶段 1 锁定的 `GameState` / `HandEvaluator` / `HandHistory` / `RngSource` API surface **冻结**。阶段 3 仅新增上层 `Trainer` / `Game` / `RegretTable` / `BestResponse` / `Checkpoint` 接口，**不修改阶段 1 类型签名**。任何 stage 1 API-NNN 不够用走 stage 1 `API-NNN-revM` 修订流程。
- 阶段 2 锁定的 `ActionAbstraction` / `InfoAbstraction` / `EquityCalculator` / `BucketTable` API surface **冻结**。阶段 3 仅作为 consumer 调用，**不修改 stage 2 类型签名**。任何 stage 2 API-2NN 不够用走 stage 2 `API-2NN-revM` 修订流程。
- 阶段 3 引入的浮点（regret / average strategy）**仅用于训练循环 + checkpoint 持久化**；不允许浮点泄露到 stage 1 规则路径 + stage 2 `abstraction::map` 子模块（D-252 `clippy::float_arithmetic` 死锁继续生效）。
- **No global RNG**（继承 stage 1 D-027 + D-050）：CFR / MCCFR 训练循环任何 sampling、tie-break、shuffle 必须显式接 `RngSource`。隐式 `rand::thread_rng()` 是 stage 3 的 P0 阻塞 bug。
- **stage 1 `RuleError` / `HistoryError` + stage 2 `BucketTableError` + stage 3 `CheckpointError` / `TrainerError`** 错误枚举只允许追加变体，不允许移除（继承 stage 1 §F-rev1 / stage 2 §F-rev1 错误前移模式）。
- **跨架构（x86_64 ↔ aarch64）一致性**：stage 3 不引入新的跨架构强约束。Kuhn / Leduc 训练在不同 arch 下 BLAKE3 byte-equal 是 aspirational（继承 stage 1 D-051 / D-052 carve-out 模式），仅在 32-seed regression baseline 强制（与 stage 1 cross_arch_hash / stage 2 bucket-table-arch-hashes 同形态）。

## 通过标准

阶段 3 通过标准如下：

- **Kuhn Vanilla CFR**：10K iter 后 player 1 EV 与 `-1/18` 差距 `< 1e-3`；exploitability `< 0.01` chips/game；固定 seed 重复 1000 次 BLAKE3 byte-equal。
- **Leduc Vanilla CFR**：10K iter 后 exploitability `< 0.1` chips/game；1K / 2K / 5K / 10K checkpoint exploitability 单调非升（允许 ±5% 噪声）；固定 seed 重复 10 次 BLAKE3 byte-equal。
- **简化 NLHE ES-MCCFR**：完整 100M sampled decision update 无 panic / NaN / inf；单线程吞吐 `≥ 10,000 update/s`；4-core 多线程吞吐 `≥ 50,000 update/s`（效率 ≥ 0.5）；固定 seed + 100M update BLAKE3 byte-equal。
- **Regret matching 数值**：1M 次随机 regret vector 输入动作概率 sum 容差 `|Σ σ - 1| < 1e-9` 全部命中；退化局面回退均匀分布；零和约束 `|EV_1 + EV_2| < 1e-6`（Kuhn / Leduc）。
- **Checkpoint round-trip**：训练 N update → 保存 → 重启加载 → 继续训练 M update → 最终 regret_table / strategy_sum BLAKE3 与不中断对照 byte-equal；3 个 game variant（Kuhn / Leduc / 简化 NLHE）各自覆盖；5 类 `CheckpointError` 错误路径全部命中。
- **阶段 1 + 阶段 2 接口未受 stage 3 修改**：stage 1 `GameState` / `HandEvaluator` / `HandHistory` / `RngSource` 全套测试 + stage 2 `ActionAbstraction` / `InfoAbstraction` / `EquityCalculator` / `BucketTable` 全套测试 `0 failed`，stage1-v1.0 / stage2-v1.0 tag 在 stage 3 任何 commit 上仍可重跑通过。
- **与外部 CFR 实现 sanity 对照**：D-363 锁定 "**Kuhn closed-form 优先 + OpenSpiel CFR 轻量对照**"——主验收依赖 Kuhn closed-form anchor + Leduc 训练曲线 fixed-seed byte-equal + 简化 NLHE checkpoint round-trip；F3 验收报告附带 OpenSpiel `algorithms/cfr_py.py` Kuhn / Leduc 收敛曲线对照（D-364 锁定口径：**收敛轨迹趋势** 一致，**不**要求各 iter exploitability 数值 byte-equal——OpenSpiel 实现可能用 regret matching+ 或不同 sampling）。OpenSpiel exploitability 在 stage 3 任一 game 上**收敛失败**视为 stage 3 P0 bug（D-365）；具体 iter 数值差异不阻塞，仅在 F3 报告标注。外部对照 sanity 脚本 `tools/external_cfr_compare.py` 在 F3 [报告] 起草时一次性接入，stage 3 主线工作（A1..F2）不依赖 OpenSpiel（D-366）。

## 阶段 3 完成产物

- `Trainer` trait + `VanillaCfrTrainer` + `EsMccfrTrainer`（实现 train / save_checkpoint / load_checkpoint / current_average_strategy）。
- `Game` trait + `KuhnGame` + `LeducGame` + `SimplifiedNlheGame`（实现 chance / players / infoset / payoff / legal_actions）。
- `RegretTable` + `StrategyAccumulator`（HashMap-backed，f64，无 unsafe）。
- `BestResponse` trait + `KuhnBestResponse` + `LeducBestResponse`（full-tree backward induction，精确计算 exploitability）。
- `Checkpoint` trait + 二进制 schema（含 `schema_version` + `bucket_table_blake3` + trailer BLAKE3 自校验）+ Rust 写入器 / 读取器 + Python 跨语言读取参考（用于阶段 7 评测脚本）。
- `tools/train_cfr.rs` CLI：`--game {kuhn,leduc,nlhe} --trainer {vanilla,es-mccfr} --iter N --seed S --checkpoint-dir DIR`，支持 resume from checkpoint。
- 一套 CFR 测试（Kuhn closed-form / Leduc 收敛曲线 / 简化 NLHE checkpoint round-trip / regret matching 数值 / 性能 SLO / 5 类 `CheckpointError`）。
- 一份阶段 3 验收报告 `pluribus_stage3_report.md`：Kuhn / Leduc / 简化 NLHE 训练曲线 / 性能 SLO 实测值 / 关键 seed 列表 / 版本哈希（git commit + bucket table BLAKE3 + 关键 checkpoint BLAKE3）/ 已知偏离。
- git tag `stage3-v1.0` + Kuhn / Leduc / 简化 NLHE 训练 checkpoint artifact（小博弈 checkpoint 进 git history；简化 NLHE 100M checkpoint 走 release artifact / git LFS，由 F3 决定分发渠道）。

## 进入阶段 4 的门槛

只有当阶段 3 所有通过标准全部满足，才能进入 6-max blueprint 训练（`pluribus_path.md` §阶段 4）。CFR 数值正确性 / 训练循环稳定性 / checkpoint round-trip 任何缺陷都会以 regret divergence / strategy 不可复现的形式被阶段 4–6 放大，事后几乎不可定位（继承 stage 1 + stage 2 出口报告 §1 同型表述）。**阶段 3 不允许带已知 CFR 数值偏离 / checkpoint 不一致 / 训练 panic 进入阶段 4**。

阶段 1 + 阶段 2 + 阶段 3 共有的 carve-out（与代码合并解耦，不阻塞下一阶段起步）：

- 跨架构 1M 手 / 100M update 一致性（仅 32-seed baseline 强制；x86 ↔ aarch64 byte-equal 是 aspirational）。
- 24 小时夜间 fuzz 在 self-hosted runner 连续 7 天无 panic（继承 stage 1 + stage 2 carve-out）。
- 阶段 3 新增 carve-out 候选（A0 [决策] 决定是否纳入 stage 3 出口或 stage 4 起步并行）：
    - **§G-batch1 §3.4-batch2..§4 production artifact 重训 + 12 条 bucket quality 转 active**：本破例 carry-forward（用户授权 stage 3 [决策] 优先于 §G-batch1 closure）。bucket table 依赖（D-314）已 deferred 到 B2/C2 [实现] 简化 NLHE 时再锁；A0..B2 期间有充裕窗口让 §G-batch1 §3.4-batch2 在 vultr 上跑完，到 C2 起步时若 v2 已 ready 走 v2 否则 v1 fallback。§G-batch1 §3.4-batch2..§4 报告闭合不阻塞 stage 3 F3。
    - Linear CFR weighting（Brown & Sandholm 2019）消融对照（默认非 Linear 强验收；Linear 留 stage 4 起步）。
    - regret matching+ 消融对照（默认 regret matching 强验收；regret matching+ 留 stage 4 起步）。

## 参考资料

- Pluribus 主论文：https://noambrown.github.io/papers/19-Science-Superhuman.pdf  §"Self-play" + §S2
- Pluribus 补充材料：https://noambrown.github.io/papers/19-Science-Superhuman_Supp.pdf  §S2 MCCFR algorithm
- Zinkevich, Bowling, Johanson, Piccione, "Regret Minimization in Games with Incomplete Information"（NeurIPS 2007，CFR 原始论文）
- Lanctot, Waugh, Zinkevich, Bowling, "Monte Carlo Sampling for Regret Minimization in Extensive Games"（NeurIPS 2009，MCCFR / External Sampling 原始论文）
- Brown, Sandholm, "Solving Imperfect-Information Games via Discounted Regret Minimization"（AAAI 2019，Linear CFR / regret matching+）
- Tammelin, Burch, Johanson, Bowling, "Solving Heads-up Limit Texas Hold'em"（IJCAI 2015，CFR+ Cepheus）
- OpenSpiel CFR / MCCFR Python 实现：https://github.com/google-deepmind/open_spiel/tree/master/open_spiel/python/algorithms

---

## 修订历史

本文档遵循与 `pluribus_stage1_validation.md` / `pluribus_stage2_validation.md` / `pluribus_stage1_decisions.md` §10 / `pluribus_stage2_decisions.md` §11 / `pluribus_stage1_api.md` §11 / `pluribus_stage2_api.md` §9 相同的 "追加不删" 约定。决策性修订仍以 `D-NNN-revM` 为主导（在 `pluribus_stage3_decisions.md` §11 修订历史落地，编号从 D-300 起以避免与 stage-1 D-NNN（D-001..D-103）+ stage-2 D-NNN（D-200..D-283）冲突），本节只记录 validation.md 自身的措辞同步。

阶段 3 实施期间的角色边界 carve-out 追认（B-rev / C-rev / D-rev / E-rev / F-rev 命名风格继承阶段 1 + 阶段 2）落到 `pluribus_stage3_workflow.md` §修订历史，本节不重复记录。

- **2026-05-12（A0 [决策] 起步 batch 1 落地同步占位）**：stage 3 A0 [决策] 起步 batch 1 落地 `docs/pluribus_stage3_validation.md`（本文档）骨架。本文档 §1–§7 + §通过标准 + §SLO 汇总全部 `D-NNN` 引用为 A0 [决策] 后续 batch 2-4 待锁占位；具体值在 `pluribus_stage3_decisions.md` §1-§8（D-300..D-379）batch 2-4 落地时 in-place 替换。本节首条由 stage 3 A0 [决策] batch 1 commit 落地，与 `pluribus_stage3_decisions.md` §11 修订历史首条 + `pluribus_stage3_workflow.md` §修订历史首条 + `CLAUDE.md` "stage 3 A0 起步" 状态翻面同步。**Carve-out carry-forward**：本 batch 起草前用户授权 stage 3 [决策] 优先于 §G-batch1 §3.4-batch2..§4 closure 启动；§G-batch1 §3.4-batch2..§4 production artifact 重训 + bucket quality 12 条转 active + stage 2 report §8 carve-out 翻面延迟到 stage 3 F3 [报告] 后回头补。bucket table 依赖（D-314）**deferred 到 B2/C2 [实现] 简化 NLHE 时锁定**——A0..B2 runway（按 stage 2 时间线 2-3 周）期间 §G-batch1 §3.4-batch2 可在 vultr 并行跑完，到 C2 起步时若 v2 528 MB artifact 已 ready 走 v2，否则 v1 95 KB fallback。
