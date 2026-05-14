# 阶段 4：6-max NLHE Blueprint 训练的量化验证方式

## 阶段目标

阶段 4 的目标是把阶段 1 锁定的真实 NLHE 规则环境 + 阶段 2 锁定的抽象层 + 阶段 3 锁定的 CFR / MCCFR 训练核接到 **6-player alternating-traverser blueprint 训练循环**，**先在 `10⁹` (first usable) update 量级打通流水线 + 工程稳定性 + 监控告警 + LBR / Slumbot 评测桥接，再扩展到 `10¹¹` (production) update 量级对标 Pluribus 论文规模**。本阶段产出 6-max 100BB NLHE blueprint policy，可独立击败基础基线 Bot，并作为阶段 6 实时 nested subgame solving 的 leaf evaluation 输入。本阶段 **不引入实时 search**（那是阶段 6）/ 不引入紧凑存储与 pruning（那是阶段 5）/ 不引入策略服务化（那是阶段 8）。

阶段 4 需要支持：

- **6-player alternating traverser 协议**（path.md §阶段 4 字面）：每 iter 选一名 traverser，按 stage 3 D-321-rev2 thread-local accumulator + batch merge 模式（继承 ES-MCCFR 真并发）对该 traverser 决策点遍历所有 action、对 chance node + non-traverser 决策点按当前 strategy 采样一个 action。所有 6 名玩家各自维护独立 `RegretTable` + `StrategyAccumulator`，按 `(t mod 6)` 轮转 traverser。
- **Linear MCCFR + Regret Matching+**（D-400 / D-401 / D-402 锁定，stage 4 A0 [决策] 翻面 stage 3 D-302-rev1 / D-303-rev1 deferred）：External-Sampling 采样核（继承 stage 3）+ **Linear discounting**（regret 累积按 `t / (t + 1)` 折扣 / average strategy 按 `t` 加权 / Brown & Sandholm 2019 AAAI 字面）+ **Regret Matching+**（每 update 后 regret 负值 clamp 到 0 / Tammelin 2015 字面）。两项 stage 3 字面 deferred 在 stage 4 A0 翻面 lock。
- **Pluribus 字面 14-action abstraction**（D-420 锁定）：`fold / check / call / raise {0.5×pot, 0.75×pot, 1×pot, 1.5×pot, 2×pot, 3×pot, 5×pot, 10×pot, 25×pot, 50×pot} / all-in`，Pluribus 论文 §2.2 同型。Preflop 是否独立 action set（D-421 待 batch 2-4 锁定）。Stage 3 5-action `DefaultActionAbstraction` 在 stage 4 期间作为 ablation baseline 保留但不进 production。
- **24-hour continuous run validation**（path.md §阶段 4 字面）：单次训练连续 24 小时无 panic / 无明显内存泄漏（RSS 上限由 D-461 锁定 + 监控）/ checkpoint cadence 期间不中断训练循环。
- **"first usable" blueprint 门槛**（path.md §阶段 4 字面 + D-440 锁定）：至少 `1,000,000,000`（`10⁹`）次 sampled decision update。该 blueprint 仅用于打通流水线 + 初步消融，**不具备实战质量**（path.md 字面）。
- **"production" blueprint 门槛**（path.md §阶段 4 字面 + D-441 锁定）：至少 `100,000,000,000`（`10¹¹`）次 sampled decision update，Pluribus 论文规模。**只有 production blueprint 才能进入阶段 6 之后的实战评测**（path.md 字面）。
- **LBR (Local Best Response) exploitability**：持续下降，最终 `< 100 mbb/g`（path.md 字面，依抽象规模调整，D-450 锁定具体阈值；mbb/g = milli big blinds per game）。
- **Slumbot / 开源参考 bot head-to-head**：至少 `100,000` 手不输，`mbb/g` 在 95% 置信区间内不显著为负（path.md §阶段 4 字面 + D-460 锁定具体对手 + 评测协议）。
- **多人 CFR 收敛监控**（path.md §阶段 4 字面）：实时输出 average regret 增长曲线 / 策略 entropy / 动作概率震荡幅度；average regret 应呈 sublinear 增长；持续震荡 / 线性增长必须能告警并定位（D-470 锁定告警阈值）。
- **Baseline sanity check**（path.md §阶段 4 字面 + D-480 锁定）：blueprint-only 策略在 `1,000,000` 手牌评测中稳定击败 random / call-station / tight-aggressive 三类基线（**必要但非充分条件**，path.md 字面 — 不能替代 LBR + Slumbot 评测）。

阶段 4 与阶段 3 最大边界差异：**state space 从 2-player 简化 NLHE 跳到 6-player 真实 NLHE + 14-action abstraction**。InfoSet 数量级从 stage 3 的 ~10⁶-10⁷（v3 bucket 500/500/500 × 4 街 × 5-action history）跃升到 ~10⁹-10¹⁰（v3 bucket × 4 街 × 14-action history × 6-player position × multi-aggressor）。`RegretTable` HashMap-backed 单进程存储承压：单 InfoSet `f64 × 14 action × 2 (regret + strategy sum) = 224 byte` × `10⁹ InfoSet` = `224 GB`，**单 host RAM 上限**。stage 4 production blueprint 训练**必须** 走 64-128 GB+ ECC RAM bare-metal-equivalent 实例（AWS r7a / m7a / c7a 32-64 vCPU，或 vultr Bare Metal），**不可** 在 stage 3 的 vultr 4-core 1.9 GB 实例上跑（stage 4 A0 [决策] D-491 锁定具体 host 计划 + spot/on-demand 权衡）。

阶段 4 与阶段 3 最大工程风险差异：**LBR exploitability + Slumbot head-to-head 两条评测路径在 stage 3 完全没有先例**。Stage 3 Kuhn / Leduc 收敛靠 closed-form anchor + fixed-seed byte-equal；Stage 4 6-max NLHE blueprint **没有 closed-form 锚点**（state space 太大），**没有 OpenSpiel 同规模对照**（OpenSpiel CFR 实现规模限于 Kuhn / Leduc / 简化 Limit Hold'em）。stage 4 验收 **强依赖：① 多人 CFR sublinear regret growth 监控 / ② LBR 上界单调下降 / ③ Slumbot 100K 手对战置信区间 / ④ 1M 手 baseline sanity**，四条相互独立的弱锚点共同守住正确性。任一条单独不够说明 blueprint 实战质量。

## 量化验证方式

### 1. 主算法：Linear MCCFR + Regret Matching+ (D-400 / D-401 / D-402)

- **External-Sampling 采样核** — 继承 stage 3 D-301 ES-MCCFR 实现路径（`EsMccfrTrainer::step_parallel`，stage 3 E2-rev1 lock D-321-rev2 真并发 + SmallVec hot path）。**不**走 outcome sampling MCCFR（stage 3 §8.1 第 (II) 项 carry-forward 评估在 stage 4 A0 [决策] 后选择 external sampling 维持；具体在 `pluribus_stage4_decisions.md` §10 已知未决项中追加 D-400-revM 触发条件）。
- **Linear discounting weighting**（D-401 锁定）— Brown & Sandholm 2019 AAAI 字面 Linear CFR：每 update 后 regret 累积按 `R_t(I, a) = (R_{t-1}(I, a) × (t-1) + r_t(I, a)) / t × (t / (t + 1))` 折扣；average strategy 按 `S_t(I, a) = S_{t-1}(I, a) + t × σ_t(I, a)` 加权（**linear time weighting**，对早期低质量 strategy 降权）。
- **Regret Matching+**（D-402 锁定）— Tammelin 2015 字面 RM+：每 update 后 regret 负值 clamp 到 0：`R_t^+(I, a) = max(R_t(I, a), 0)`；strategy 计算用 clamp 后值 `σ_t(I, a) = R_{t-1}^+(I, a) / Σ R_{t-1}^+(I, b)`。Stage 3 D-303 lock 的标准 RM 在 stage 4 期间作为 ablation baseline 保留但不进 production。
- **stage 3 → stage 4 数值连续性**：stage 3 闭合的 `EsMccfrTrainer::step` BLAKE3 byte-equal 不变量（D-321-rev2 真并发实现，1M update × 3 重复 byte-equal）在 stage 4 引入 Linear weighting + RM+ 后**必然失效**（regret 公式 + clamp 改变数值轨迹）。stage 4 A0 [决策] 把 stage 3 BLAKE3 anchor 标记为 stage 3-only artifact，**不**作为 stage 4 数值正确性 anchor；stage 4 新建独立 anchor（D-403：固定 seed × 固定 100M update 的 6-player blueprint regret_table BLAKE3 byte-equal 跨 run 重复一致）。
- **Linear MCCFR + RM+ 消融对照**：stage 4 F3 [报告] 验收时必须附带 **非 Linear + 标准 RM** 路径（stage 3 实现，BLAKE3 anchor 跨进 stage 4 后**仅作为消融基线**不作为 stage 4 anchor）作为 ablation baseline，量化 Linear + RM+ 在 6-max NLHE 上的收敛加速倍数。预期 2-5× 加速（path.md §阶段 5 字面提升阈值的 stage 4 提前消融）。

### 2. 6-player Alternating Traverser 协议 (D-410 / D-411 / D-412)

- **规则**（D-410 锁定）— 6-player 100 BB starting stack / 0.5 BB SB / 1 BB BB / 完整 4 街 / 无 rake / 无 ante / 继承 stage 1 D-022 默认 + n_seats=6 路径（stage 1 D-022b-rev1 仅适用 n_seats==2 HU 分支，stage 4 6-player 走 stage 1 默认路径 byte-equal 不变）。
- **traverser 轮转**（D-411 锁定）— 每 iter 选一名 traverser `(t mod 6)`；6 个 traverser 各自维护独立 `RegretTable` + `StrategyAccumulator`，**不共享**。alternating 顺序固定 `[0, 1, 2, 3, 4, 5]`（继承 stage 3 D-307 alternating traverser 模式扩展到 n=6）。
- **position rotation**（D-412 锁定）— button 在 6 个 seat 间轮转（继承 stage 1 D-032 button rotation 协议）；blueprint 训练**不**按 position 独立维护策略，所有 position 共享同一份 `RegretTable[traverser]`（InfoSet 中已经编码 position via stage 1 `SeatId` + stage 2 `BettingState`）。
- **stage 1 + stage 2 不变量继承**：6-player 路径走 stage 1 默认 multi-seat 分支（n_seats=6 + 行动顺序 SB → BB → UTG → ... button → SB，继承 stage 1 D-029 左邻一致性 + D-039-rev1 odd-chip 余给按钮左侧最近获胜者 + D-037 last_aggressor + D-022b 盲注；6-player postflop 行动顺序 SB → BB → UTG → ... button 继承 stage 1 D-022b first_postflop_actor 多人通用规则）。任何 stage 1 / stage 2 测试套件在 stage 4 commit 上必须 byte-equal 维持（stage1-v1.0 / stage2-v1.0 / stage3-v1.0 三 tag 全套测试 0 failed 不退化锚点）。

### 3. 14-action Abstraction (D-420 / D-421 / D-422)

- **action set**（D-420 锁定）— Pluribus 论文 §2.2 字面 14-action：`fold / check / call / raise {0.5×pot, 0.75×pot, 1×pot, 1.5×pot, 2×pot, 3×pot, 5×pot, 10×pot, 25×pot, 50×pot} / all-in`。stage 2 `DefaultActionAbstraction` 5-action 扩展为 `PluribusActionAbstraction` 14-action（继承 stage 2 `ActionAbstraction` trait surface，**不修改** stage 2 trait 签名 — D-220 lock 仍生效；新增 14-action 实现作为 stage 2 trait 的第 2 个 impl）。
- **preflop 独立 action set**（D-421 待 batch 2-4 决策）— Pluribus 论文 §S2 字面 preflop 用更细的 action set（含 3× BB / 4× BB / pot raise 等）。D-421 在 `pluribus_stage4_decisions.md` §10 已知未决项中追加触发条件：A1 [实现] scaffold 起步前由 batch 2-4 决策 lock 或 deferred 到 C2 [实现] 起步前 lock（继承 stage 3 D-314 deferred 模式）。
- **stage 1 `GameState::apply` raise size legal 验证**（D-422 / 跨 stage 1 边界）— 14-action raise sizes `{0.5×, 0.75×, 1×, 1.5×, 2×, 3×, 5×, 10×, 25×, 50×} × pot` 在 stage 1 `GameState::apply` 路径下：(a) 半 raise 不 reopen raise option 继承 stage 1 D-033-rev1；(b) `Action::Raise { to }` 绝对量约定继承 stage 1 D-026；(c) all-in short raise 不 reopen 已经行动玩家继承 stage 1 D-033-rev1。14-action 落地必须在 stage 4 B1 [测试] 验证 stage 1 GameState::apply 在所有 raise sizes 路径下 byte-equal 不退化（stage 1 测试套件 0 failed 维持），**不修改** stage 1 `GameState::apply` 实现（D-374 stage 1 字面禁止改 — 与 stage 3 §8.1 第 (III) 项 carry-forward 一致）；任何 14-action 测试发现 stage 1 实现缺陷走 stage 1 `API-NNN-revM` 修订流程 + 用户授权（与 stage 3 D-022b-rev1 同型跨 stage 1 carve-out 模式）。
- **InfoSet bit 编码扩展**（D-423 / 跨 stage 3 边界）— stage 3 D-317-rev1 lock `bucket_id` field bits 12..18 编码 6-bit `legal_actions` availability mask（继承 stage 2 64-bit InfoSetId）。14-action 需要 14-bit mask，**bits 12..18 6-bit 不够**。stage 4 A0 [决策] 触发 D-317-rev2 InfoSetId schema 翻面（stage 2 D-218 InfoSetId 64-bit layout 扩展），具体在 `pluribus_stage4_decisions.md` §10 / §12 batch 2-4 lock；预备方向：(a) 复用 stage 2 IA-007 reserved 14 bits 作为 14-action mask 区域；(b) bucket_id field 收紧给 mask 让位；(c) InfoSetId 升 128-bit（破 stage 2 schema_version 1 → 2，最大破坏面）。
- **off-tree action 处理**：14-action 之外的 raise size（如对手下 0.6× pot 不在 14-action 集合中）处理协议由阶段 6c 锁定（path.md §阶段 6c 字面 — pseudo-harmonic mapping / nearest-action mapping / randomized rounding）；stage 4 blueprint 训练**只在 14-action 集合内训练**，off-tree 处理留 stage 6c。

### 4. 训练规模门槛：first usable 10⁹ + production 10¹¹

- **first usable 门槛**（D-440 锁定）— 至少 `1,000,000,000`（`10⁹`）次 sampled decision update。**目的**：打通 6-player + 14-action + 24h continuous + checkpoint + 监控 + LBR + Slumbot 评测全流水线，初步消融对照。**不具备实战质量**（path.md 字面）。预期时间预算 — stage 3 E2-rev1 vultr 4-core 实测 4-core throughput `~7.7K update/s`；stage 4 因 14-action（vs stage 3 5-action）+ 6-player（vs stage 3 2-player）每 update 路径长度增加约 2-3×，估计 4-core throughput `~2.5K-4K update/s`，10⁹ update / 3K = `~93 hours`；32-vCPU AWS c7a.8xlarge（D-491 候选 host）throughput 估计 `~20K update/s`（按 4-core 1.78× efficiency 外推 32-core 估计 efficiency 1.5-1.8×，约 `5K-7K update/s × 32 vCPU / 4 ≈ 16K-28K update/s`），10⁹ / 20K = `~14 hours`。**stage 4 A0 [决策] 锁定 first usable 训练预算 `≤ 24h` 单次连续运行**（D-440-rev0），可能需要 D-491 host 选型支持。
- **production 门槛**（D-441 锁定）— 至少 `100,000,000,000`（`10¹¹`）次 sampled decision update，Pluribus 论文规模。预期时间预算 — `10¹¹ / 20K update/s ≈ 1400 hours ≈ 58 days`。**stage 4 production blueprint 训练只在 D-441-rev0 host 选型 + 用户授权 + checkpoint cadence 充分验证后启动**（CLAUDE.md "高性能机器按需向用户申请" memory 字面 — 任何 `> 1h wall time` 任务必须用户授权）；**不**作为 stage 4 自动化训练 default path，由 stage 4 F3 [报告] 起步前用户手动触发 D-441-rev0 production 训练（继承 stage 3 F3 [报告] §G-batch1 §3.4-batch2 production 训练用户手动触发模式）。
- **first usable → production 演进**：stage 4 主线 A0..F3 验收**只要求 first usable `10⁹`**；production `10¹¹` 训练在 stage 4 F3 [报告] 闭合**之后** 由用户授权 D-441-rev0 启动，最终 production blueprint artifact 作为**阶段 5 → 6 切换的输入**（阶段 5 紧凑存储 + pruning 在 production blueprint 上做内存优化对照；阶段 6 实时 nested subgame solving 的 leaf evaluation 用 production blueprint）。Stage 4 F3 [报告] 验收清单**显式分离**：first usable 验收必须全绿（13 项门槛全 PASS） / production 验收作为 carve-out carry-forward 到 stage 5 起步并行清单（与 stage 3 D-362 100M anchor → 10M × 3 降标同型模式）。

### 5. LBR (Local Best Response) Exploitability (D-450)

- **算法**（D-450 锁定）— LBR (Local Best Response) 是 Lisý & Bowling 2017 提出的 best response 上界算法，对大 state space 不可枚举的 imperfect-information game 适用（6-max NLHE 是典型场景）。算法核心：每决策点仅枚举本地一步 best response（call / fold / 几个 raise sizes），假设之后所有玩家走 blueprint。LBR 是真实 exploitability 的 **upper bound**（不是 lower bound），LBR 下降 = blueprint 抗剥削能力上升。
- **量化验收门槛**（D-451 锁定）— path.md §阶段 4 字面 `< 100 mbb/g`（依抽象规模调整）。**100 mbb/g = 0.1 BB/100 game**（1 mbb/g = 1/1000 big blind per game）。stage 4 first usable `10⁹` update 后 LBR 上界 `< 200 mbb/g`（先允许略松，作为 first usable 训练完成 sanity）；stage 4 production `10¹¹` update 后 LBR 上界 `< 100 mbb/g`（path.md 字面阈值，**production 门槛 deferred 到 stage 4 F3 [报告] 后的 D-441-rev0 production 训练完成时验收**）。
- **LBR 计算频率**（D-452 锁定）— stage 4 训练期间每 `10⁷` update 计算一次 LBR（first usable `10⁹` update 内共计算 100 次），曲线必须**单调非升**（允许相邻两次 ±10% 噪声但不允许持续上升）。LBR 上升超过 3 个连续采样点 = 训练异常告警（与 D-470 监控告警耦合）。
- **LBR 实现选型**（D-453 / 跨工具边界）— stage 4 stage F3 [报告] 起步前 lock：(a) OpenSpiel `algorithms/exploitability_descent.py` Python 实现 + Rust ↔ Python `pyo3` bridge（继承 stage 2 PokerKit 对照 `.venv` 模式）；(b) Rust 自实现（参考 Lisý & Bowling 2017 paper + OpenSpiel 实现）。批 2-4 决策 lock 时纳入复杂度评估。

### 6. Slumbot / 开源参考 Bot Head-to-Head (D-460 / D-461)

- **对手选型**（D-460 锁定）— **Slumbot** 是 Eric Jackson 2017 AAAI 公开的 HU NLHE bot（http://www.slumbot.com/）。Stage 4 6-max blueprint 无法直接与 HU bot 对战；候选方案：(a) 把 6-max blueprint 退化到 HU 1v1 对战 Slumbot（6-player → 2-player 子博弈，保留 Pluribus 14-action + Linear MCCFR）；(b) 用同等水平 6-max 开源 bot（候选：`PyPokerBot` / `OpenSpiel` 内置 6-max baseline / 同等社区开源实现）；(c) 自训练 5 份 stage 4 first usable blueprint 之间互打（5 副本 self-play 锦标赛模式）。D-460 在 batch 2-4 决策 lock 时选定具体 bot，预计选 (a) Slumbot HU 对战（path.md 字面 "Slumbot 或同等水平开源实现"）。
- **量化验收门槛**（D-461 锁定）— path.md §阶段 4 字面：至少 `100,000` 手不输，`mbb/g` 在 95% 置信区间内**不显著为负**。**置信区间**：100K 手 6-max NLHE 的 mbb/g standard error 约 `5-10 mbb/g`（依方差），95% CI = `mean ± 2 × SE`。stage 4 first usable 验收门槛：`mean ≥ -10 mbb/g` 且 95% CI 下界 `≥ -30 mbb/g`（先允许略弱于 Slumbot，作为 first usable sanity 不显著输）；stage 4 production 门槛：`mean ≥ 0 mbb/g`（不输）+ 95% CI 下界 `≥ -10 mbb/g`。
- **评测协议**（D-462 锁定）— 固定 seed 重复跑 5 次取均值 + standard error；duplicate dealing（同一手两个方向各打一次降方差，继承 PokerKit duplicate poker 模式）；evaluation profile = release + `--ignored` 显式触发。**首期 stage 4 验收只要求 first usable 门槛**；production 门槛 deferred 到 D-441-rev0 production 训练完成后。

### 7. 多人 CFR 收敛监控 (D-470 / D-471 / D-472)

path.md §阶段 4 字面 "average regret 增长曲线 / 策略 entropy / 动作概率震荡幅度" + "average regret 应呈 sublinear 增长 / 持续震荡 / 线性增长必须能告警并定位"。Stage 4 监控比 stage 3 D-343 单线 `avg_regret / sqrt(T) ≤ const` 严格 — 三条独立监控指标 + 告警机制。

- **average regret 增长率**（D-470 锁定）— 每 `10⁵` update 采样一次 `max_I R_t(I) / sqrt(T)`（max over all InfoSets），曲线必须 **非递增** 趋势（允许相邻 ±5% 噪声）。线性增长（`R_t ∝ T`）= P0 阻塞 bug，必须 5 个连续采样点 trip 时立即 panic / 写错误日志。
- **策略 entropy**（D-471 锁定）— 每 `10⁵` update 采样一次 `H(σ_t) = - Σ_I Σ_a σ_t(I, a) × log σ_t(I, a)`（average over reachable InfoSets）。Entropy 应在训练初期高（接近 `log(14) ≈ 2.64` for 14-action uniform）+ 训练后期单调下降（策略集中到 dominant actions）。Entropy 突然回升 = blueprint 出现 oscillation 候选信号，必须告警。
- **动作概率震荡幅度**（D-472 锁定）— 每 `10⁵` update 采样一次 `Σ_I Σ_a |σ_t(I, a) - σ_{t-10⁵}(I, a)|`（average over reachable InfoSets 的 strategy 变化量）。震荡幅度应单调下降；连续 5 个采样点震荡幅度增加 = 训练异常告警。
- **告警实现**（D-473 锁定）— stage 4 trainer 在 `EsMccfrTrainer::step` 主循环内每 `10⁵` update 计算三条指标 + 落到训练日志 + 触发阈值时通过 `Result<(), TrainerError>` 返回路径暴露给 CLI（CLI `tools/train_cfr.rs` 决定是否 abort）。Trainer **不**主动 abort（让 CLI / 用户决策）；stage 4 trainer 提供 `metrics() -> &TrainingMetrics` 公开 read-only 接口（API-440 lock）。

### 8. Baseline Sanity Check：1M 手 vs 3 类基线 (D-480)

path.md §阶段 4 字面 "blueprint-only 策略在 `1,000,000` 手牌评测中稳定击败 random / call-station / tight-aggressive 三类基线。该项不能替代 LBR 和 Slumbot 评测"。**必要但非充分条件**。

- **random opponent**（D-480 ①）— 每决策点均匀采样 legal action（call / fold / 14-action raise 集合 / all-in 均匀分布）。Blueprint 必须 `mean ≥ +500 mbb/g` 且 95% CI 下界 `> 0`。
- **call-station opponent**（D-480 ②）— 永远 call / check（fold = 1%随机性以避免 always-call 死局，但绝大多数 call）。Blueprint 必须 `mean ≥ +200 mbb/g`。
- **tight-aggressive opponent**（D-480 ③）— preflop top 20% range raise / 其余 fold；postflop continuation bet 70% / 否则 check-fold。Blueprint 必须 `mean ≥ +50 mbb/g`。
- **评测协议**（D-481 锁定）— 6-player 4-blueprint + 2-opponent（或 5-blueprint + 1-opponent，stage 4 batch 2-4 决策 lock 测试 seat 数量），1M 手固定 seed + duplicate dealing 重复 3 次取均值。release + `--ignored` 显式触发。

### 9. 24-Hour Continuous Run Validation (D-461)

path.md §阶段 4 字面 "单次训练可连续运行 `24` 小时无崩溃、无明显内存泄漏"。Stage 4 F1 [测试] / F2 [实现] 落地 24h 连续运行 fuzz 测试。

- **测试形态**（D-461 锁定）— `tests/training_24h_continuous.rs::*`（release + `--ignored` 显式触发，CI nightly 7-day rolling fuzz）。固定 seed + 24h wall time 启动 `EsMccfrTrainer::step` 循环 + 每 `10⁶` update 调用 `metrics()` 写日志 + 每 `10⁸` update 写 checkpoint + 退出前最后一次 checkpoint。
- **panic / NaN / inf 监控**（D-461 ①）— 24h 内任何 `panic!()` / `f64::NAN` / `f64::INFINITY` 出现在 regret_table / strategy_sum 中 = P0 fail。
- **内存监控**（D-461 ②）— 训练前 baseline RSS + 训练后 24h RSS 增量 `< 5 GB` (allow regret_table 自然增长 + 实现路径不应有 leak)。stage 4 batch 2-4 lock 具体阈值：先按 4-core AWS c7a.4xlarge 32 GB 实例预估 `regret_table peak ≤ 25 GB`，超过即告警。RSS 监控走 `/proc/self/status` VmRSS（Linux）+ stage 4 trainer `metrics()` 接口暴露 `peak_rss_bytes` 字段。
- **checkpoint cadence**（D-462 锁定）— 每 `10⁸` update 写一次完整 checkpoint（继承 stage 3 D-358 full snapshot 模式），24h 训练共写 `~24 × 20K × 3600 / 10⁸ = ~17` 次（按 c7a.8xlarge 20K update/s 估计）。每次 checkpoint 大小 `~30-50 GB`（regret_table + strategy_sum 6 traverser × InfoSet × 14-action × f64）— stage 4 batch 2-4 锁定 checkpoint 存储位置（local disk / S3 / GCS）。
- **resume 验证**（D-463 锁定）— 任意 checkpoint 重启 → 加载 → 继续训练 → 与不中断对照训练 BLAKE3 byte-equal（继承 stage 3 D-350 round-trip 不变量到 stage 4 6-player + Linear + RM+ 路径）。

### 10. 性能 SLO 汇总

| SLO | 阈值 | 路径 / 备注 |
|---|---|---|
| 6-player ES-MCCFR + Linear + RM+ 单线程 | `≥ 5,000 update/s` | D-490 锁定（继承 stage 3 D-361 单线程 SLO 退化 1/2，因 14-action + 6-player 路径长度增加 2-3×；stage 3 §8.1 第 (I)..(III) carry-forward 优化全做完后回升）|
| 6-player ES-MCCFR + Linear + RM+ 4-core | `≥ 15,000 update/s` on 4-core | D-490 锁定（效率 ≥ 0.75，继承 stage 3 E2-rev1 vultr 4-core 1.78× efficiency 估计）|
| 6-player ES-MCCFR + Linear + RM+ 32-core | `≥ 20,000 update/s` on c7a.8xlarge 32 vCPU | D-490 锁定（效率 ≥ 0.13；32-vCPU 受限于 HashMap contention + AWS Hyperthread sibling 竞争）|
| LBR exploitability | first usable `< 200 mbb/g` / production `< 100 mbb/g` | D-451 锁定（path.md 字面 `< 100 mbb/g`）|
| Slumbot 100K 手 | `mean ≥ -10 mbb/g` first usable / `mean ≥ 0` production | D-461 锁定（path.md 字面 95% CI 不显著为负）|
| 24h continuous run | 无 panic / NaN / inf / RSS 增量 `< 5 GB` | D-461 锁定（path.md §阶段 4 字面）|
| checkpoint round-trip | BLAKE3 byte-equal | D-463 锁定（继承 stage 3 D-350）|
| baseline sanity | random `≥ +500` / call-station `≥ +200` / TAG `≥ +50` mbb/g | D-480 锁定（path.md 字面必要非充分）|

性能 SLO 走 `tests/perf_slo.rs::stage4_*`，与 stage 1 / stage 2 / stage 3 同形态：release profile + `--ignored` 显式触发，CI nightly 跑 bench-full + 短 bench 在 push 时跑。Stage 4 SLO 主 host 走 D-491 锁定的 AWS / vultr cloud on-demand 实例（不在 dev box 或 stage 3 vultr 4-core 跑）。

### 11. 与阶段 1 / 阶段 2 / 阶段 3 的不变量边界

继承阶段 1 + 阶段 2 + 阶段 3 全部不变量（**规则路径无浮点 / 无 `unsafe` / 显式 `RngSource` / 整数筹码 / `SeatId` 左邻一致性 / Cargo.lock 锁版本 / clustering determinism / regret f64**），并在 stage 4 显式划分：

- 阶段 1 锁定的 `GameState` / `HandEvaluator` / `HandHistory` / `RngSource` API surface **冻结**。阶段 4 仅扩展上层 `Game` / `Trainer` impl，**不修改阶段 1 类型签名**。任何 stage 1 API-NNN 不够用走 stage 1 `API-NNN-revM` 修订流程 + 用户授权（D-422 / D-374 字面继承禁止改）。
- 阶段 2 锁定的 `ActionAbstraction` / `InfoAbstraction` / `EquityCalculator` / `BucketTable` API surface **冻结**。阶段 4 新增 `PluribusActionAbstraction` 14-action 实现作为 stage 2 trait 的第 2 个 impl；**不修改 stage 2 trait 签名**。任何 stage 2 API-2NN 不够用走 stage 2 `API-2NN-revM` 修订流程 + 用户授权。
- 阶段 3 锁定的 `Game` / `Trainer` / `RegretTable` / `Checkpoint` API surface **额外允许扩展**（stage 3 → stage 4 是同一作者的连续 API 演进）：(a) 新增 `Trainer::step_linear_rm_plus` 方法或 `EsMccfrTrainer::with_linear_rm_plus()` builder（D-400 / D-401 / D-402 落地路径在 batch 2-4 / batch 5 决定）；(b) 新增 `Game::action_set_size() -> u8` 配置 method（14-action vs 5-action 路径分流）；(c) stage 3 D-317-rev1 6-bit mask 翻面到 14-bit（D-317-rev2 / 跨 stage 3 边界 + 用户授权）。所有 stage 3 D-NNN 翻面走 `D-NNN-revM` 流程 + stage 3 api.md / decisions.md §修订历史追加（与 stage 2 D-218-rev2 / stage 3 D-022b-rev1 同型跨 stage carve-out 模式）。
- 阶段 3 引入的浮点（regret / average strategy / Linear discount factor / RM+ clamp）**仅用于训练循环 + checkpoint 持久化 + 监控 metrics**；不允许浮点泄露到 stage 1 规则路径 + stage 2 `abstraction::map` 子模块 + stage 2 bucket lookup 热路径（D-252 `clippy::float_arithmetic` 死锁继续生效）。
- **No global RNG**（继承 stage 1 D-027 + D-050）：6-player CFR 训练循环任何 sampling / tie-break / shuffle 必须显式接 `RngSource`。隐式 `rand::thread_rng()` 是 stage 4 的 P0 阻塞 bug。
- **stage 1 `RuleError` / `HistoryError` + stage 2 `BucketTableError` + stage 3 `CheckpointError` / `TrainerError`** 错误枚举只允许追加变体，不允许移除（继承 stage 1 §F-rev1 / stage 2 §F-rev1 / stage 3 §F1 错误前移模式）。stage 4 候选追加 `TrainerError::LinearWeightOverflow` / `TrainerError::LbrComputeFailed` / `TrainerError::SlumbotConnectionFailed`（batch 2-4 锁定）。
- **跨架构（x86_64 ↔ aarch64）一致性**：stage 4 不引入新的跨架构强约束。6-player blueprint 训练在不同 arch 下 BLAKE3 byte-equal 是 aspirational（继承 stage 1 D-051 / D-052 / stage 3 cross_host_blake3 carve-out 模式），仅在 32-seed regression baseline 强制（与 stage 1 cross_arch_hash / stage 2 bucket-table-arch-hashes / stage 3 checkpoint-hashes-linux-x86_64 同形态扩展，stage 4 batch 2-4 追加 stage 4 baseline 文件）。

## 通过标准

阶段 4 通过标准如下（**first usable 门槛**，production 门槛 deferred 到 stage 5 起步并行清单）：

- **6-player Linear MCCFR + RM+ 训练**：完整 `10⁹` sampled decision update（first usable 门槛）无 panic / NaN / inf；单线程吞吐 `≥ 5,000 update/s`；4-core 吞吐 `≥ 15,000 update/s`（效率 ≥ 0.75）；32-vCPU AWS c7a.8xlarge 吞吐 `≥ 20,000 update/s`；固定 seed + `10⁸` update BLAKE3 byte-equal 重复一致。
- **24h continuous run**：单次连续运行 24 小时无 panic / NaN / inf / RSS 增量 `< 5 GB`；resume from checkpoint round-trip BLAKE3 byte-equal；每 `10⁸` update checkpoint 写入成功。
- **LBR exploitability**：first usable `10⁹` update 后 LBR 上界 `< 200 mbb/g`；LBR 曲线 100 个采样点单调非升（允许 ±10% 噪声）。
- **Slumbot / 开源参考 bot head-to-head**：100K 手 `mean ≥ -10 mbb/g` 且 95% CI 下界 `≥ -30 mbb/g`；duplicate dealing + 固定 seed 重复 5 次取均值。
- **多人 CFR 监控**：average regret growth sublinear / 策略 entropy 单调下降 / 动作概率震荡幅度单调下降；三条曲线任意连续 5 个采样点违反趋势 → trainer `metrics()` 返回告警（CLI 决定是否 abort）。
- **Baseline sanity check**：1M 手 vs random `≥ +500 mbb/g` / call-station `≥ +200 mbb/g` / TAG `≥ +50 mbb/g`；95% CI 下界 `> 0`（**必要非充分**，不替代 LBR + Slumbot）。
- **阶段 1 + 2 + 3 接口未受 stage 4 修改**：stage 1 `GameState` / `HandEvaluator` / `HandHistory` / `RngSource` 全套测试 + stage 2 `ActionAbstraction` / `InfoAbstraction` / `EquityCalculator` / `BucketTable` 全套测试 + stage 3 `Game` / `Trainer` / `RegretTable` / `Checkpoint` 全套测试 `0 failed`，stage1-v1.0 / stage2-v1.0 / stage3-v1.0 tag 在 stage 4 任何 commit 上仍可重跑通过（stage 3 D-317-rev2 + D-422 跨 stage 边界翻面合规由 `D-NNN-revM` 流程 + 用户授权 + 测试套件 byte-equal 维持托底）。
- **与外部 6-max 实现 sanity 对照**：D-450 LBR exploitability 单调下降趋势 + D-460 Slumbot 100K 手不显著输 + D-480 baseline sanity 1M 手击败 3 类 — 四条**独立弱锚点**共同守住正确性（继承 stage 3 D-363 "Kuhn closed-form 优先 + OpenSpiel 轻量对照" 模式扩展到 stage 4 多锚点验收）。任一锚点单独失败 = stage 4 出口 carve-out（与 stage 3 §8.1 第 1 条 D-361 NLHE 双 fail 同型已知偏离模式），四个锚点同时失败 = stage 4 P0 阻塞。

## 阶段 4 完成产物

- `PluribusActionAbstraction` 14-action 实现（impl stage 2 `ActionAbstraction` trait 第 2 个 impl，stage 2 trait surface 不变）。
- `NlheGame6` 6-player NLHE Game 实现（impl stage 3 `Game` trait，stage 3 `SimplifiedNlheGame` 2-player 路径 stage 4 期间作为 ablation baseline 保留）。
- `EsMccfrTrainer::with_linear_rm_plus()` builder + Linear discounting + RM+ clamp 实现路径（stage 3 `EsMccfrTrainer::step_parallel` D-321-rev2 真并发路径扩展，serial baseline 保留）。
- `TrainingMetrics` struct + `EsMccfrTrainer::metrics()` 公开接口（average regret growth / 策略 entropy / 动作概率震荡 / peak RSS）。
- `LbrEvaluator` LBR exploitability 计算 + `tools/lbr_compute.rs` CLI（stage 4 F1 [测试] / F2 [实现] 落地，路径由 D-453 锁定 OpenSpiel bridge 或 Rust 自实现）。
- `SlumbotBridge` / `OpenSourceBot6maxBridge` head-to-head 评测 + `tools/eval_blueprint.rs` CLI（stage 4 F2 [实现] 落地，对手选型 D-460 batch 2-4 lock）。
- `tools/train_cfr.rs` CLI 扩展：`--game nlhe-6 --trainer es-mccfr-linear-rm-plus --abstraction pluribus-14 --iter N --seed S --checkpoint-dir DIR --metrics-interval 100000 --checkpoint-interval 100000000 --aws-instance c7a.8xlarge` 支持 resume from checkpoint + 多人 alternating traverser + Linear + RM+。
- 一套 stage 4 测试（6-player 数值正确性 / 14-action raise sizes legal / 24h continuous fuzz / checkpoint round-trip 6-traverser / LBR exploitability monotone / Slumbot 100K 手 / baseline sanity 1M 手 / 性能 SLO / TrainingMetrics 阈值告警）。
- 一份阶段 4 验收报告 `pluribus_stage4_report.md`：first usable `10⁹` 训练曲线 / LBR 100 采样点 / Slumbot 5×100K 重复 / baseline sanity 3 类 / 性能 SLO 实测值 / D-491 host 选型 + 时间预算实测 / 关键 seed 列表 / 版本哈希 / 已知偏离 + stage 5 起步并行清单（含 production `10¹¹` 训练 deferred + carry-forward 项）。
- git tag `stage4-v1.0` + first usable `10⁹` blueprint checkpoint artifact（按 stage 3 F3 模式由用户手动触发 GitHub Release 上传；checkpoint 大小估计 `30-50 GB`，走 git LFS 或 S3）。

## 进入阶段 5 的门槛

只有当阶段 4 所有通过标准全部满足（first usable `10⁹` 13 项门槛全 PASS），才能进入紧凑存储 + pruning（`pluribus_path.md` §阶段 5）。**production `10¹¹` 训练不阻塞 stage 5 起步**（与 stage 3 D-362 100M anchor → 10M × 3 降标 deferred 到 stage 4 同型模式）；production 训练作为 stage 5 起步并行清单 carry-forward 项，在 stage 5 起步前后由用户授权 D-441-rev0 启动。**阶段 4 不允许带已知 LBR 上界 ≥ 200 mbb/g / Slumbot 95% CI 显著为负 / baseline sanity 任一基线输 / 24h continuous run panic 进入阶段 5**。

阶段 1 + 阶段 2 + 阶段 3 + 阶段 4 共有的 carve-out（与代码合并解耦，不阻塞下一阶段起步）：

- 跨架构 1M 手 / `10⁹` update 一致性（仅 32-seed baseline 强制；x86 ↔ aarch64 byte-equal 是 aspirational）。
- 24 小时夜间 fuzz 在 self-hosted runner 连续 7 天无 panic（继承 stage 1 + stage 2 + stage 3 carve-out + 扩展到 stage 4 6-player blueprint 训练 nightly fuzz）。
- 阶段 4 新增 carve-out 候选（A0 [决策] 决定是否纳入 stage 4 出口或 stage 5 起步并行）：
    - **production `10¹¹` blueprint 训练**：deferred 到 stage 4 F3 [报告] 闭合后用户授权 D-441-rev0 启动；training wall time `~58 days` AWS c7a.8xlarge，路径预算与 stage 3 §G-batch1 §3.4-batch2 长 wall-time training 模式同型，由用户手动触发。
    - **Stage 3 §8.1 carry-forward (I)..(VII) 7 项**：（I）perf flamegraph hot path 实测 / （II）outcome vs external sampling 评估 / （III）stage 1 `GameState::apply` micro-opt 跨 stage 1 / （IV）stage 3 D-361-revM 阈值翻面（stage 4 D-490 单线程 / 4-core SLO 替代 stage 3 D-361，**不**直接翻面 stage 3 D-361 字面） / （V）stage 3 D-362 100M anchor 恢复 / （VI）stage 2 bucket quality 12 条 #[ignore] 转 active / （VII）stage 2 `pluribus_stage2_report.md` §8 carve-out 翻面 — 7 项全部 carry-forward 到 stage 4 主线 13 步 + 并行清单分流处理，在 `pluribus_stage4_workflow.md` 详细分配。
    - **AIVAT / DIVAT 方差缩减接口** — path.md §阶段 7 字面 stage 7 评测体系的必备能力。stage 4 F3 [报告] Slumbot 100K 手评测**先用未方差缩减口径**输出 mbb/g + standard error；stage 7 起步前 D-491 锁定 AIVAT 实现路径（候选：OpenSpiel `evaluator/aivat.py` Python bridge / Rust 自实现）。stage 4 不阻塞 stage 7 起步该项 carve-out。
    - **D-301-revM outcome sampling 翻面**（继承 stage 3 §8.1 第 (II) 项 carry-forward）：stage 4 batch 2-4 决策评估 outcome sampling 在 6-player 路径下是否提供足够加速翻面 D-301 字面 lock。预期：outcome sampling 在 6-player 14-action 状态空间下**比 external sampling 慢**（每 trajectory 路径长 = 6 player × 4 街 × ~3 action depth = ~72 hops vs external 全 traverser 决策点遍历），评估结果大概率 D-301 维持 external sampling lock。
    - **stage 3 12 条 `tests/bucket_quality.rs` #[ignore] 转 active**（继承 stage 3 §8.1 第 (VI) 项）：14-action abstraction 落地后 InfoSet 数量级跃升，bucket quality EMD / monotonic 指标在 14-action 路径下重新校准。stage 4 F1 [测试] 决定是否 9 条 fail 翻面到 PASS（D-233-rev1 sqrt-scale K=500 偏紧 / D-236b MC reorder noise）。

## 参考资料

- Pluribus 主论文：https://noambrown.github.io/papers/19-Science-Superhuman.pdf §2 Algorithm / §S2 Training procedure / §S4 Real-time search
- Pluribus 补充材料：https://noambrown.github.io/papers/19-Science-Superhuman_Supp.pdf §S2 MCCFR algorithm / §S3 Action abstraction / §S5 Blueprint training cost
- Brown, Sandholm, "Solving Imperfect-Information Games via Discounted Regret Minimization"（AAAI 2019，Linear CFR / Linear discounting / Regret Matching+ 原始论文）
- Tammelin, Burch, Johanson, Bowling, "Solving Heads-up Limit Texas Hold'em"（IJCAI 2015，CFR+ Cepheus / Regret Matching+ 公认实现）
- Lisý, Bowling, "Equilibrium Approximation Quality of Current No-Limit Poker Bots"（AAAI 2017 Workshop，LBR Local Best Response 原始论文）
- Burch, Schmid, Moravčík, Morrill, Bowling, "AIVAT: A New Variance Reduction Technique for Agent Evaluation in Imperfect Information Games"（AAAI 2018，AIVAT 方差缩减）
- Lanctot, Waugh, Zinkevich, Bowling, "Monte Carlo Sampling for Regret Minimization in Extensive Games"（NeurIPS 2009，MCCFR / External Sampling 原始论文，继承 stage 3）
- Slumbot：http://www.slumbot.com/  Eric Jackson 2017 AAAI Computer Poker Competition HU NLHE bot
- OpenSpiel CFR / LBR / AIVAT Python 实现：https://github.com/google-deepmind/open_spiel/tree/master/open_spiel/python/algorithms

---

## 修订历史

本文档遵循与 `pluribus_stage1_validation.md` / `pluribus_stage2_validation.md` / `pluribus_stage3_validation.md` 相同的"追加不删"约定。决策性修订仍以 `D-NNN-revM` 为主导（在 `pluribus_stage4_decisions.md` §11 修订历史落地，编号从 D-400 起以避免与 stage-1 D-NNN（D-001..D-103）+ stage-2 D-NNN（D-200..D-283）+ stage-3 D-NNN（D-300..D-379）冲突），本节只记录 validation.md 自身的措辞同步。

阶段 4 实施期间的角色边界 carve-out 追认（B-rev / C-rev / D-rev / E-rev / F-rev 命名风格继承阶段 1 + 阶段 2 + 阶段 3）落到 `pluribus_stage4_workflow.md` §修订历史，本节不重复记录。

- **2026-05-14（A0 [决策] 起步 batch 1 落地）**：stage 4 A0 [决策] 起步 batch 1 落地 `docs/pluribus_stage4_validation.md`（本文档）骨架。本文档 §1–§10 + §通过标准 + §SLO 汇总全部 `D-NNN` 引用为 A0 [决策] 后续 batch 2-4 待锁占位；具体值在 `pluribus_stage4_decisions.md` §1-§8（D-400..D-49X）batch 2-4 落地时 in-place 替换。本节首条由 stage 4 A0 [决策] batch 1 commit 落地，与 `pluribus_stage4_decisions.md` §11 修订历史首条 + `pluribus_stage4_workflow.md` §修订历史首条 + `CLAUDE.md` "stage 4 A0 起步 batch 1 closed" 状态翻面同步。**核心 lock**：stage 4 主算法 = Linear MCCFR + RM+（D-400 / D-401 / D-402，翻面 stage 3 D-302-rev1 / D-303-rev1 deferred）/ action abstraction = Pluribus 字面 14-action（D-420，stage 2 `ActionAbstraction` 第 2 个 impl）/ host = AWS / vultr cloud on-demand（D-491，不走 Hetzner bare-metal）/ first usable `10⁹` + production `10¹¹` 双阈值分离（D-440 / D-441，production deferred 到 stage 5 起步并行清单）。**Carve-out carry-forward**：stage 3 §8.1 (I)..(VII) 7 项 carry-forward 项全部 carry 到 stage 4，分流到主线 13 步 + 并行清单处理；production `10¹¹` blueprint 训练 deferred 到 stage 4 F3 [报告] 后用户授权触发；AIVAT / DIVAT 方差缩减接口 deferred 到 stage 7 起步前；D-301-revM outcome sampling 翻面评估在 batch 2-4 决策（大概率维持 external sampling lock）。
