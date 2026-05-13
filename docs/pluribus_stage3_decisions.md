# 阶段 3 决策记录

## 文档地位

本文档记录阶段 3（MCCFR 小规模验证）的全部技术与规则决策。一旦 commit，后续步骤（A1 / B1 / B2 / ... / F3）的所有 agent 必须严格按此 spec 执行。

任何决策修改必须：
1. 在本文档以 `D-NNN-revM` 形式追加新条目（不删除原条目）
2. 必要时 bump `Checkpoint.schema_version`（D-350）或 `HandHistory.schema_version`（继承阶段 1 D-101，仅当 stage 3 修改影响序列化时触发）或 `BucketTable.schema_version`（继承阶段 2 D-240，仅当 stage 3 倒逼 bucket table format 修改时触发）
3. 通知所有正在工作的 agent（在工作流 issue / PR 中显式标注）

未在本文档列出的细节，agent 应在 PR 中显式标注 "超出 A0 决策范围"，由决策者补充决策后再实施。

阶段 3 决策编号从 **D-300** 起，与阶段 1 D-NNN（D-001..D-103）+ 阶段 2 D-NNN（D-200..D-283）不冲突。阶段 1 + 阶段 2 D-NNN 全集 + D-NNN-revM 修订作为只读 spec 继承到阶段 3，未在本文档显式覆盖的部分以 `pluribus_stage1_decisions.md` + `pluribus_stage2_decisions.md` 为准。

---

## 1. 算法变体（D-300..D-309）

| 编号 | 决策项 | 选定值 |
|---|---|---|
| D-300 | Kuhn / Leduc 算法变体 | **Vanilla CFR**（full-tree counterfactual regret minimization，Zinkevich et al. 2007 NeurIPS 原始论文）。每 iter 遍历完整博弈树，对每个 InfoSet 计算 counterfactual value + counterfactual regret + 更新 average strategy；不采样、确定性、单线程为主。详见下方 D-300 详解。 |
| D-301 | 简化 NLHE 算法变体 | **External-Sampling MCCFR**（Lanctot et al. 2009 NeurIPS 原始论文，Pluribus 论文 §S2 同型）。每 iter 选定一个 traverser（轮换），traverser 决策点遍历所有 action（外采样 = exhaustive on traverser side），chance node + non-traverser 决策点采样一个 action（按当前 strategy）。详见下方 D-301 详解。 |
| D-302 | Linear CFR weighting | **非 Linear**（阶段 3）。Brown & Sandholm 2019 AAAI 提出的 Linear CFR weighting `w_t = t` 在 stage 4 6-max blueprint 起步时引入。阶段 3 用**常数权重 1**保留为 baseline，避免 sampling + weighting 双变量同时引入；Linear 在 stage 4 起步首笔 commit 走 D-302-revM 翻面。 |
| D-303 | regret matching 选型 | **标准 regret matching**（`σ(I, a) = max(R(I, a), 0) / Σ max(R(I, b), 0)`）。**regret matching+**（Tammelin et al. 2015 IJCAI / CFR+）在 stage 4 起步首笔 commit 走 D-303-revM 翻面。阶段 3 用标准 RM 与 stage 4 RM+ 对照避免 sampling + weighting + matching variant 三变量同时引入。 |
| D-304 | average strategy 累积方式 | **标准累积** `S_T(I, a) = Σ_{t=1..T} π_t^σ(I) × σ_t(I, a)`，其中 `π_t^σ(I)` 为时刻 t 下 player(I) 到达 InfoSet I 的 reach probability。**Vanilla CFR**：精确 π（full-tree backward induction）；**ES-MCCFR**：traverser 用 sampled reach probability + opponent 用 baseline reach probability（D-301 详解 §3）。average strategy `\bar{σ}(I, a) = S_T(I, a) / Σ_b S_T(I, b)`，分母为 0 时回退均匀分布（与 D-306 退化局面一致）。 |
| D-305 | regret update 公式 | **标准 CFR regret update**：在 iter t 上对每个 traverser InfoSet I 和 action a，累积 regret `R_{t}(I, a) = R_{t-1}(I, a) + cfv_t(I, a) - Σ_b σ_t(I, b) × cfv_t(I, b)`，其中 `cfv_t(I, a)` 为 counterfactual value（traverser 视角，不含 traverser 自身到达 I 的概率）。`R_0(I, a) = 0`。**Vanilla CFR**：cfv 精确计算；**ES-MCCFR**：cfv 通过 outcome-level Monte Carlo 估计（External Sampling 公式，详见 Lanctot 2009 §4）。 |
| D-306 | regret matching 算法定义（标准公式 + 退化局面 + 数值容差） | **标准公式**：`σ(I, a) = max(R(I, a), 0) / Σ max(R(I, b), 0)`；**退化局面**（所有 regret ≤ 0）锁在 D-331（§4 sampling/traversal）回退均匀分布 `σ(I, a) = 1 / |actions(I)|`；**数值容差**锁在 D-330（§4）`|Σ_a σ(I, a) - 1| < 1e-9`；**数值类型**锁在 D-333（§4）f64。本条仅锁定算法定义边界，具体数值 lock 跨 §1 / §4 拆分。 |
| D-307 | traverser 选择策略（ES-MCCFR） | **轮换**（alternating）：iter t 上 traverser = `(t mod n_players)`。**不**采用 uniform random traverser selection（Pluribus 论文 §S2 / Lanctot 2009 都用 alternating）。简化 NLHE n_players = 2，所以 odd iter player 0 / even iter player 1。 |
| D-308 | chance node 采样（ES-MCCFR） | **采样 1 outcome**（按 chance distribution）。chance node 含 deal hole cards + deal board cards（继承 stage 1 `RngSource` 显式注入）。**不**做 Public Chance Sampling（PCS）—— PCS 是 stage 5 实时搜索优化范围，阶段 3 保持 outcome sampling 标准形态。 |
| D-309 | non-traverser action 采样（ES-MCCFR） | **按当前 strategy `σ_t`** 采样 1 action（External Sampling 标准）。**不**采用 mixed sampling / opponent strategy importance sampling（那是 Outcome Sampling MCCFR 的范畴，stage 3 不引入）。该约定让 importance weighting 仅作用于 traverser 自身的 reach probability，避免 opponent reach 引入额外方差。 |

### D-300 详解（Vanilla CFR for Kuhn / Leduc）

**伪代码**（单 iter，所有 player）：

```
function Vanilla_CFR_iter(strategy_t, regret_t, strategy_sum_t):
    for each player i in [0, n_players):
        # 遍历完整博弈树，计算 i 的 reach probability + opponent reach probability
        cfv_i, σ_t = recurse(root, player_i, π_i = 1.0, π_minus_i = 1.0)
    # 在 recurse 末端 update regret + strategy_sum (in-place)
    return regret_{t+1}, strategy_sum_{t+1}
```

**recurse(state, traverser, π_traverser, π_opp)**：
- terminal state：返回 (utility(state, traverser), σ)
- chance node：对每个 chance outcome o，累积 `Σ_o p(o) × recurse(state.next(o), ...)`
- decision node：
    - 若 actor != traverser：σ = current_strategy(I)，返回 `Σ_a σ(a) × recurse(state.next(a), traverser, π_traverser, π_opp × σ(a))`
    - 若 actor == traverser：
        - 对每个 action a，递归 `cfv_a = recurse(state.next(a), traverser, π_traverser × σ(I, a), π_opp)`
        - σ_node = Σ_a σ(I, a) × cfv_a
        - 对每个 action a，累积 regret `R(I, a) += π_opp × (cfv_a - σ_node)`
        - strategy_sum `S(I, a) += π_traverser × σ(I, a)`
        - 返回 σ_node

**关键不变量**：
- `π_traverser × π_opp` 是当前节点 reach probability（不含 chance），用于 cfv 的反事实权重。
- regret update 的乘子是 `π_opp`（不含 traverser），即 "若 traverser 总是选这条路径会到达 I 的概率"——这正是 counterfactual reach。
- strategy_sum 的乘子是 `π_traverser`（包含 traverser 自身），即 "实际到达 I 的概率"——average strategy 应按实际到达加权。

**Kuhn / Leduc 收敛性**（Zinkevich 2007 定理 1）：average strategy `\bar{σ}_T` 是 `2 × √(|I_max| × |A_max|) / √T` 近似 Nash，其中 `|I_max|` 为最大 InfoSet 数、`|A_max|` 为最大 action 数。Kuhn `|I| = 12, |A| = 2`，`T = 10K` 下理论收敛上界 `2 × √(12 × 2) / √10000 ≈ 0.098`，实测应远低于（CFR 上界宽松，实测通常 100× tighter）。

### D-301 详解（External-Sampling MCCFR for 简化 NLHE）

**伪代码**（单 iter，单 traverser，alternating per D-307）：

```
function ESMccfr_iter(strategy_t, regret_t, strategy_sum_t, rng, traverser):
    # 单 iter 只更新 traverser 的 regret + strategy_sum
    # 注：External Sampling 中 strategy_sum 在 non-traverser 决策点也累积（Lanctot 2009 §4.1）
    recurse_es(root, traverser, π_traverser = 1.0, π_opp = 1.0, rng)
    return regret_{t+1}, strategy_sum_{t+1}
```

**recurse_es(state, traverser, π_traverser, π_opp, rng)**：
- terminal state：返回 `utility(state, traverser) / π_traverser`（importance weighting：traverser sampled reach 倒数）
- chance node：按 chance distribution 采样 1 outcome o（D-308）；递归 `recurse_es(state.next(o), traverser, π_traverser, π_opp, rng)`
- decision node：
    - 若 actor != traverser：
        - σ = current_strategy(I_actor)
        - 按 σ 采样 1 action a'（D-309）
        - **non-traverser strategy_sum 累积**（Lanctot 2009 §4.1）：`S(I_actor, b) += σ(b)` for all b
        - 返回 `recurse_es(state.next(a'), traverser, π_traverser, π_opp × σ(a'), rng)`
    - 若 actor == traverser：
        - σ = current_strategy(I_traverser)
        - 对每个 action a，递归 `v_a = recurse_es(state.next(a), traverser, π_traverser × σ(a), π_opp, rng)`
        - σ_node = Σ_a σ(a) × v_a
        - **traverser regret 累积**：`R(I_traverser, a) += π_opp × (v_a - σ_node)` for all a
        - 返回 σ_node

**关键不变量**（Lanctot 2009 定理 7）：
- traverser regret 累积乘子是 `π_opp`，与 Vanilla CFR 同；importance weighting 通过 `v_a` 中已经包含的 `1 / π_traverser` 隐式作用。
- non-traverser 决策点遍历所有 action 是 "External" 的字面意义——traverser 视角下 opponent 看作 chance（外采样 = 外人采样）。
- strategy_sum 在 non-traverser 决策点累积（按 σ(b)），traverser 决策点不累积（traverser 决策点在 D-302 非 Linear 下隐含累积，由 `recurse_es` traverser 分支的 σ_node 计算）。

**收敛性**（Lanctot 2009 定理 4）：ES-MCCFR average regret 上界 `O(|I| × √(|A|) / √T)`，与 Vanilla CFR 同阶但常数更大（采样方差）；100M update 量级足够让 average strategy 接近 ε-Nash for ε ≤ 1%（Pluribus 实战参考）。

### regret matching 与 strategy 关系（D-303 + D-306 联合详解）

**单 iter t 上 strategy 计算**（standard regret matching）：

```
function current_strategy(I, regret):
    R_plus = [max(regret[I, a], 0) for a in actions(I)]
    denom = Σ R_plus
    if denom > 0:
        σ(I, a) = R_plus[a] / denom
    else:
        # D-306 退化局面
        σ(I, a) = 1 / |actions(I)|
    return σ
```

**average strategy 计算**（标准 CFR 输出）：

```
function average_strategy(I, strategy_sum):
    denom = Σ strategy_sum[I, b]
    if denom > 0:
        \bar{σ}(I, a) = strategy_sum[I, a] / denom
    else:
        \bar{σ}(I, a) = 1 / |actions(I)|
    return \bar{σ}
```

**数值容差**（path.md §阶段 3 字面 + D-330 锁定）：`|Σ_a σ(I, a) - 1| < 1e-9` 在所有 `(I, t)` 上恒成立。`f64` regret table（D-333 锁定）下，100M update 量级累积误差预计 `< 1e-12`，远低于容差。

---

## 2. 游戏环境（D-310..D-319）

| 编号 | 决策项 | 选定值 |
|---|---|---|
| D-310 | Kuhn Poker 规则 | **标准 Kuhn**：3 张牌 deck `{J, Q, K}`（rank 11/12/13） / 2 player / 各发 1 张私有牌 / 各 ante `1` chip / 1 round betting / 最多 `1` voluntary bet（size = `1` chip）/ player 1 先行动 `{Check, Bet}`；player 2 应对 `{Pass, Bet}`（Check 后 Bet 视为 1 raise，最多 1 raise 链）。Showdown 比 rank（J < Q < K）。Payoff 单位 chip。**InfoSet 数 = 12**（每 player 6 个：3 牌 × 2 公开历史 `["", "pb"]` for player 1 / `["c", "b"]` for player 2）。 |
| D-311 | Leduc Poker 规则 | **标准 Leduc**：6 张牌 deck `{J♠, J♥, Q♠, Q♥, K♠, K♥}`（rank 11/12/13 × suit 0/1） / 2 player / 每人发 1 张私有牌（preflop）+ 1 张公共牌（flop 后翻开） / 各 ante `1` chip / 2 round betting / 每 round 最多 `2` voluntary raise / preflop bet size = `2` chip、postflop bet size = `4` chip / showdown：先比 pair（私有 rank == 公共 rank → 自动赢）再比 rank。**InfoSet 数估算**：preflop 6 × |histories_preflop| + postflop 6 × 6 × |histories_postflop|，~`288` InfoSet（具体由 D-311 Game trait B2 [实现] 枚举确认）。 |
| D-312 | `Game` trait 抽象层 | **统一接口** Kuhn / Leduc / 简化 NLHE 三类游戏：`trait Game { type State; type Action; fn root(&self, rng: &mut dyn RngSource) -> Self::State; fn current_player(state: &Self::State) -> PlayerOrChance; fn info_set(state: &Self::State) -> InfoSetId; fn legal_actions(state: &Self::State) -> Vec<Self::Action>; fn next(state: Self::State, action: Self::Action) -> Self::State; fn is_terminal(state: &Self::State) -> bool; fn payoff(state: &Self::State, player: PlayerId) -> f64; fn chance_distribution(state: &Self::State) -> Vec<(Self::Action, f64)>; }`。具体签名在 `pluribus_stage3_api.md` API-310 锁定。 |
| D-313 | 简化 NLHE 规则范围 | **2-player + 100 BB starting stack + 盲注 0.5/1.0 BB + 完整 4 街** + stage 2 `DefaultActionAbstraction`（5-action）+ stage 2 `PreflopLossless169` + `PostflopBucketAbstraction`（500/500/500 bucket）。复用 stage 1 `GameState` + stage 2 `ActionAbstraction` / `InfoAbstraction` / `BucketTable`，仅在 `SimplifiedNlheGame` 适配层把 stage 1 `GameState` 包装成 `Game` trait state。该决策由用户 batch 1 [决策] 确认，stage 4 6-max blueprint 的真子集。 |
| D-314 | bucket table 依赖（简化 NLHE） | **deferred**（详见 §10 已知未决项）。bucket table 依赖只在 B2/C2 [实现] 真正构造 `SimplifiedNlheGame` 时被消费，A0..B2 期间 §G-batch1 §3.4-batch2 可在 vultr 并行跑完；到 C2 起步时若 v2 528 MB artifact 已 ready 由 D-314-rev1 lock 为 v2，否则 v1 95 KB fallback 由 D-314-rev2 lock + 显式标注 collision carve-out。 |
| D-315 | chance distribution / sampling 接口 | **统一走 stage 1 `RngSource`** 显式注入（继承 D-027 + D-050）。chance node 在 `Game::next` 内部按 D-312 `chance_distribution` 返回的离散分布 + `rng.next_u64()` 采样 1 outcome（D-308 在算法层锁定 1-sample，本条锁定具体接口）。**禁止** `rand::thread_rng()` 隐式调用；任何隐式 RNG 是 stage 3 P0 阻塞 bug。Kuhn / Leduc deck shuffle + 简化 NLHE deal hole / deal board 全部走该路径。 |
| D-316 | terminal payoff utility 计算 | **player 视角整数 chip 净收益直接当 utility**（不归一化、不除以 BB）。`payoff(state, player) -> f64 = (final_stack_chip - initial_stack_chip) as f64`。Kuhn / Leduc 严格零和 `payoff(state, 0) + payoff(state, 1) = 0`；简化 NLHE 含 rake = 0 默认（继承 stage 1）也严格零和。该约定让 exploitability 单位与 path.md §阶段 3 字面 "chips/game" 直接对齐。 |
| D-317 | InfoSet 识别（history → InfoSetId 映射） | **Kuhn / Leduc** 走 stage 3 独立 InfoSet 编码（`KuhnInfoSetId` / `LeducInfoSetId`，类型不同于 stage 2 `InfoSetId`），按 `(player_private_card, public_history_string)` 直接索引（小博弈无 abstraction 需求，全 InfoSet 唯一）。**简化 NLHE** 走 stage 2 `InfoSetId`（D-215 64-bit layout，继承 `PreflopLossless169` / `PostflopBucketAbstraction`）。该差异由 `Game::info_set` 关联类型 + `Trainer<G: Game>` 泛型表达，避免 InfoSet 类型混用。**D-317-rev1（2026-05-13 C2 [实现] 落地中暴露）**：简化 NLHE 路径在 stage 2 `InfoSetId.bucket_id` field bits 12..18 编码 6-bit `legal_actions` availability mask（D-324 在 stage 2 `stack_bucket` 5 桶 + `DefaultActionAbstraction` 可变长输出下不自动满足；本 rev 把 mask 编码到 bucket_id field 上 carve out，不触及 IA-007 reserved 区域）。详见 §10.3 D-317-rev1 lock 段落。 |
| D-318 | `Game::legal_actions` 与 stage 2 `ActionAbstraction` 边界 | **Kuhn / Leduc**：`Game::legal_actions` 直接返回 game-specific action 枚举（`KuhnAction { Check, Bet, Call, Fold }` / `LeducAction { ... }`），不经 stage 2 抽象层。**简化 NLHE**：`SimplifiedNlheGame::legal_actions` **内部调用** stage 2 `DefaultActionAbstraction::actions(&GameState)`，把 5-action `AbstractAction` 直接当 `Game::Action` 返回（不再二次抽象）。该约定保持 Kuhn / Leduc 零 stage 2 依赖、简化 NLHE 全 stack stage 2 抽象。 |
| D-319 | state representation 选型 | **owned clone**（每个 recurse 节点持有独立 `Game::State`），不引入 persistent data structure / state diff / ZipperList。**理由**：① Kuhn / Leduc state size ≤ 100 byte，clone 成本可忽略；② 简化 NLHE state 是 stage 1 `GameState`（继承设计已优化 clone 路径，stage 1 测试 ~5M clone/s）；③ 持久化结构会引入 unsafe / Cow / Rc 等模式与 stage 1 `unsafe_code = "forbid"` 冲突。性能 SLO（D-361）单线程 ≥ 10K update/s 在 owned clone 路径已可达。 |

---

## 3. Regret / strategy 存储（D-320..D-329）

| 编号 | 决策项 | 选定值 |
|---|---|---|
| D-320 | `RegretTable` 容器选型 | **`HashMap<InfoSetIdEnum, Vec<f64>>`** 嵌入 `RegretTable` 结构体；`InfoSetIdEnum` 区分 `KuhnInfoSetId` / `LeducInfoSetId` / `stage2::InfoSetId`（via `Game::InfoSet` 关联类型，泛型 `RegretTable<G: Game>`）。`Vec<f64>` 长度 = `game.legal_actions(state).len()`，按 `Game::Action` 输出顺序对齐索引（继承 stage 2 D-209 deterministic 顺序模式）。**不引入**外部 crate（如 `ahash` / `dashmap`），HashMap 默认 SipHash 即可——CFR 训练 InfoSet 数 << 10⁷ 量级，hash 性能不是瓶颈。 |
| D-321 | thread-safety 模型（多线程 ES-MCCFR） | **deferred**（详见 §10 已知未决项）。具体方案在 batch 3 [实现] 起步前决定，候选：① `parking_lot::RwLock<HashMap>` 简单封装；② `dashmap::DashMap` 细粒度并发；③ thread-local accumulator + 周期 batch merge；④ `crossbeam::SegQueue` snapshot reduce。D-361 多线程 SLO `≥ 50,000 update/s on 4-core`（效率 ≥ 0.5）的可达性由该选型决定。**单线程**（Kuhn / Leduc 必然单线程）不受影响。 |
| D-322 | `StrategyAccumulator` 数据结构 | 与 `RegretTable` 同构（`HashMap<InfoSetIdEnum, Vec<f64>>`），但内容是 strategy_sum 累积值。`average_strategy(I, a) = strategy_sum[I, a] / Σ_b strategy_sum[I, b]`，分母为 0 时回退均匀分布（与 D-331 退化局面一致）。`StrategyAccumulator` 与 `RegretTable` **不**合并成单 HashMap：① 单职责清晰；② checkpoint 序列化时两者可独立压缩；③ regret 在 ES-MCCFR 仅 traverser 决策点累积、strategy_sum 在 non-traverser 决策点累积，并发模型不同。 |
| D-323 | lazy 初始化策略 | **HashMap 默认 lazy**：InfoSet 未访问时不分配 `Vec<f64>`；首次 `current_strategy(I)` 调用时根据 `game.legal_actions(state).len()` 分配 `vec![0.0; n_actions]`（regret）+ `vec![0.0; n_actions]`（strategy_sum）。该策略让 ES-MCCFR 训练初期 InfoSet 增长曲线与训练量 1:1 匹配（不预分配全 InfoSet 集合）。**Kuhn / Leduc**：全 InfoSet 数已知（12 / ~288），可选 eager 预分配（D-323-rev1 候选，仅 Kuhn/Leduc 路径），但 lazy 路径行为相同，A0 默认 lazy 即可。 |
| D-324 | InfoSet → action_count 映射稳定性 | **action_count 必须在训练全程对同一 InfoSetId 恒定**。`Game::legal_actions(state)` 在同一 InfoSet 上必须返回固定长度的 action 集合。Kuhn / Leduc：按 history string 索引时显然恒定；简化 NLHE：stage 2 `DefaultActionAbstraction::actions(&GameState)` 在 same InfoSetId 上必须恒定（继承 stage 2 D-209 deterministic 顺序 + 5-action 结构）。**Trainer 不自适应 action_count 变化**——首次分配后 `Vec<f64>` 长度固定；运行时长度不匹配触发 `TrainerError::ActionCountMismatch { info_set, expected, got }`（P0 阻塞）。 |
| D-325 | 内存上界监控 | **simplified NLHE 100M update 量级 InfoSet 数预计 ~10⁶**（preflop 169 lossless × position × stack × betting_state × postflop 1500 buckets × betting_state），regret + strategy_sum 各 ~10⁶ × 5 × `f64` ≈ 80 MB；training process RSS 预算 `≤ 8 GB`（D-325 锁定 SLO 上界，远高于实际预计）。**Kuhn / Leduc** RSS 预算 `≤ 100 MB`（12 / 288 InfoSet × f64 量级）。`TrainerError::OutOfMemory { rss_bytes, limit }` 在监控触发时返回（P0 阻塞，不试图自适应缩表）。 |
| D-326 | 多 game 隔离 | **每个 game variant 独立 `RegretTable` + `StrategyAccumulator`**，不共享。Kuhn / Leduc / 简化 NLHE 三个 game 训练 checkpoint 互不兼容（D-350 `game_variant` 字段拒绝 mismatch），不允许 Kuhn 训练得到的 regret 注入 Leduc trainer。该约定让多 game 并行训练（CI / dev）安全。 |
| D-327 | regret / strategy_sum checkpoint 序列化格式 | `bincode 1.x` 默认 little-endian + varint integer encoding；HashMap 序列化为 sorted-by-InfoSetIdEnum 顺序（保证跨 host BLAKE3 byte-equal）。**不引入** `serde_json`（文本格式跨 host 浮点格式化漂移破 byte-equal）/ Protocol Buffers / capnp（依赖膨胀）。具体 schema 锁在 D-350（§6 Checkpoint）。 |
| D-328 | query API：current_strategy / average_strategy | **`RegretTable::current_strategy(&self, info_set: &InfoSetIdEnum, n_actions: usize) -> Vec<f64>`** 返回 regret matching 输出（D-303 + D-306 标准 RM）。**`StrategyAccumulator::average_strategy(&self, info_set: &InfoSetIdEnum, n_actions: usize) -> Vec<f64>`** 返回 average strategy（D-304 标准累积）。两者均 stateless（`&self` 不修改内部 state），可多线程并发读。返回 `Vec<f64>` 而非借用，避免生命周期纠缠；性能上 stage 3 不是 query 热路径。 |
| D-329 | 数值容差监控（warn vs panic） | **训练循环每 1M update 抽样 1K 个 InfoSet 检查** `|Σ_a σ(I, a) - 1| < 1e-9`（D-330 容差）；超限触发 `tracing::warn!`（非 panic，避免长跑训练被单点抖动打断）。training 结束后 F3 [报告] 跑 full sweep（全 InfoSet）严格断言超限 0 case；超限 ≥ 1 case 视为 stage 3 P0 阻塞。该约定让浮点累积异常在长跑训练中可观测但不中断训练。 |

---

## 4. Sampling 与 traversal（D-330..D-339）

| 编号 | 决策项 | 选定值 |
|---|---|---|
| D-330 | regret matching 概率 sum 容差 | **`|Σ_a σ(I, a) - 1| < 1e-9`**（path.md §阶段 3 字面）。该容差在所有 `(I, t)` 上恒成立——`f64` regret table（D-333）下 100M update 累积误差预计 `< 1e-12`，远低于容差。检测路径：D-329 训练抽样 + F3 报告 full sweep。退化局面（all-zero）回退均匀分布也满足 `Σ = n_actions × (1/n_actions) = 1.0` 严格无误差。 |
| D-331 | regret matching 退化局面 | 所有 `R(I, a) ≤ 0` 时回退**均匀分布** `σ(I, a) = 1 / |actions(I)|`（CFR 文献标准 convention）。该约定保证训练初期 `R = 0` 时 strategy 不退化为单点分布。**实现要求**：分母 `Σ max(R, 0)` 计算后判 `> 0` 用浮点 strict positive 而非 `> 1e-N` 阈值——浮点 strict positive 在严格 zero regret 时退化正确，在数值噪声 `~1e-300` 时也能走 RM 分支（不影响 D-330 容差，因为 σ 仍 sum 到 1）。 |
| D-332 | 零和约束 sanity check | Vanilla CFR 训练后期（5K iter+）`|EV_player_0 + EV_player_1| < 1e-6`（Kuhn / Leduc 严格零和；简化 NLHE rake = 0 默认也严格零和）。该 sanity check 在 F1 [测试] 落地为 `tests/zero_sum_invariant.rs`，断言每个 game variant 训练结束 EV sum 容差 `< 1e-6`。超限视为 stage 3 P0 阻塞（暗示 cfv 计算 / payoff 计算 / sampling 重要性权重某处错误）。 |
| D-333 | regret / strategy_sum 数值类型 | **`f64`**（非 `f32`）。`f64` 在 100M update 量级累积误差 `< 1e-12`，远低于 D-330 `1e-9` 容差；`f32` 在 ~10⁷ update 后累积误差就可触达 `1e-4` 量级，触发 D-330 容差 fail。`f32` 优化路径在 stage 4+ 出现性能瓶颈时再视情况引入（届时需 D-333-revM 翻面）。该约定继承 path.md §阶段 3 `1e-9` 字面门槛 + Pluribus 论文 §S2 实战 f64。 |
| D-334 | tree traversal 算法 | **DFS recursive**（Vanilla CFR + ES-MCCFR 共用）。Rust stack frame 在 release 模式 ~256 byte / frame，简化 NLHE 最深 ~50 layer（4 街 × 5 action × overhead），总 stack ~15 KB 远低于默认 8 MB stack size。**不**改 iterative + 显式 stack——iterative 让 cfv 累积变成 worklist 模式，调试成本远高于性能收益。stack overflow 监控走 D-325 RSS 上界（间接覆盖）。 |
| D-335 | sampling RNG sub-stream 派生 | **继承 stage 1 D-228**：训练循环 RNG sub-stream 派生走 SplitMix64 finalizer + op_id 表（继承 stage 2 `cluster::rng_substream` 模式）。CFR / MCCFR 训练新增 `op_id` 表项：`OP_KUHN_DEAL = 0x03_00`、`OP_LEDUC_DEAL = 0x03_01`、`OP_NLHE_DEAL = 0x03_02`、`OP_OPP_ACTION_SAMPLE = 0x03_10`、`OP_CHANCE_SAMPLE = 0x03_11`、`OP_TRAVERSER_TIE = 0x03_20`。具体 op_id 表锁在 batch 5 `pluribus_stage3_api.md` API-330。 |
| D-336 | chance sampling 实现 | **`Game::chance_distribution(state) -> Vec<(Action, f64)>` 离散采样**：`rng.next_u64()` 映射到 `[0, 2^64)` → 归一化到 `[0, 1)` → 在累积分布上 binary search 找 outcome。**不**用 `rand::distributions::WeightedIndex`（外部 crate 浮点行为跨版本可能漂移破 byte-equal；与 stage 2 D-250 自实现 k-means 同型政策）。Kuhn deal `1/3` / Leduc deal `1/6` / 简化 NLHE deal `1/52` 等概率均走该路径。 |
| D-337 | opponent action sampling 实现 | **按 `current_strategy(I_opp)` 采样 1 action**（D-309 锁算法层 + 本条锁实现层）：同 D-336 累积分布 binary search。`current_strategy(I)` 返回 `Vec<f64>` 长度 = `n_actions`，sum = 1.0 ± 1e-9（D-330 保证）；binary search 的浮点累积误差不影响 sampling 正确性（误差 < 1e-9 远小于 `1/n_actions` 量级）。 |
| D-338 | counterfactual value (cfv) 计算 | **per-action `Vec<f64>` 累积**：traverser 决策点递归得到每个 action 的 cfv，按 σ 加权得到 σ_node = Σ_a σ(a) × cfv_a；regret 更新 `R(I, a) += π_opp × (cfv_a - σ_node)`。**不**做 cfv baseline correction（baseline / control variates 是 stage 5 实时搜索方差缩减范围，stage 3 不引入）。**精度**：cfv 路径全 f64，累积链长度 = tree depth ~ 50；误差累积 `~50 × 2^-52 ≈ 1.1e-14` 远低于 D-330 容差。 |
| D-339 | terminal value computation | **Game::payoff(state, player) -> f64 = chip_net_change as f64**（D-316 锁定单位）。整数 chip → f64 转换 lossless（chip 范围 ≤ 2^31，f64 mantissa 52 bit 充足）。terminal value 直接是 cfv 起点，无中间归一化。 |

---

## 5. Exploitability 计算（D-340..D-349）

| 编号 | 决策项 | 选定值 |
|---|---|---|
| D-340 | Kuhn exploitability 算法 | **Full-tree backward induction best response**：枚举 player 1 / player 2 各自所有 deterministic 策略（每个 InfoSet 选 1 action），对每个候选 BR 计算 `EV_BR_i = E_{σ_{-i}, σ_i^BR}[utility_i]`，取最大值；exploitability = `(BR_1(σ_2) + BR_2(σ_1)) / 2`。Kuhn 12 InfoSet × 2 action → `2^12 = 4096` 候选 BR，单线程 release `< 100 ms`。**Closed-form anchor**：player 1 Nash EV = `-1/18 ≈ -0.05556`，BR_1(Nash σ_2) = `-1/18`、BR_2(Nash σ_1) = `+1/18`，理论 exploitability = `(−1/18 + 1/18)/2 = 0`；训练 10K iter 后 exploitability `< 0.01` 字面门槛对应 5%-级偏离已是 strong upper bound。 |
| D-341 | Leduc exploitability 算法 + 阈值 | **同 D-340 full-tree backward induction**，~288 InfoSet × ≤ 3 action → 候选 BR 量级超过暴力枚举 (~3^288)，走 backward induction polynomial 路径（对每个 InfoSet 在 backward 中选最大 cfv 的 action）。**阈值**：10K iter 后 exploitability `< 0.1` chips/game（CFR 文献常见 reference）。**curve 单调性**：1K / 2K / 5K / 10K 4 checkpoint exploitability 单调非升，允许相邻两次 ±5% 噪声。 |
| D-342 | 简化 NLHE 验收门槛 | **训练规模 ≥ 100M sampled decision update**（path.md 字面）；**无 panic / NaN / inf**；**单线程吞吐 ≥ 10K update/s release**（D-361）；**4-core 吞吐 ≥ 50K update/s**（D-361，效率 ≥ 0.5）；**fixed-seed + 100M update BLAKE3 byte-equal**（重复跑同 seed 同 host 同 toolchain）。**不**计算精确 exploitability（state space 太大，full BR 不可行）；监控走 D-343 average regret growth。 |
| D-343 | 简化 NLHE average regret growth 监控 | **`max_I avg_regret(I, T) / sqrt(T) ≤ C`** 应保持 bounded（CFR 理论 sublinear growth）；constant `C` 由 stage 3 F3 [报告] 实测落地决定（候选基线 `C ≤ 100` chips/game，类比 path.md §阶段 4 `< 100 mbb/g` 字面 LBR 阈值放大 1000× 量级）。每 1M update 抽样 1K 个 InfoSet 计算 `avg_regret(I, T) = (Σ_a R+(I, a)) / T`，超限触发 `tracing::warn!`（非 panic）。F3 报告 full sweep 给出 `C` 实测值。 |
| D-344 | `BestResponse` 输出格式 | **`(br_strategy: HashMap<InfoSetIdEnum, Vec<f64>>, br_value: f64)`**：strategy 是 one-hot per InfoSet（best action 概率 1，其它 0），value 是 BR 期望收益。让 BestResponse 输出格式与 `current_strategy` / `average_strategy` 同型，便于 cross-replay 复用 visualization 工具。 |
| D-345 | exploitability checkpoint snapshot | **每个 exploitability 计算点保存独立 `tests/data/stage3_exploit_<game>_iter_<N>.txt`**：含 iter 数、player 1 BR EV、player 2 BR EV、exploitability 数值（f64 二进制 + bincode）。该文件作为 stage 3 F1 [测试] regression baseline，重跑同 seed 同 iter 应 byte-equal。**不进 git history**（继承 stage 2 D-251 `artifacts/` gitignore 模式）；通过 git LFS 或 release artifact 分发。 |
| D-346 | LBR (Local Best Response) 是否纳入 stage 3 | **不纳入**。Stage 3 简化 NLHE 不计算 LBR；path.md §阶段 3 字面验收门槛不提 LBR，LBR 是 stage 4 6-max blueprint 字面强约束（§阶段 4 字面 `< 100 mbb/g`）。stage 3 F3 [报告] 仅提及 LBR 作为 stage 4 起步依赖项。**Carve-out**：若 stage 3 F3 时间预算充裕，可破例引入 LBR 作为 stage 4 起步 baseline pre-flight（用户授权，D-346-rev1 翻面）。 |
| D-347 | exploitability 跨 host 一致性 | **fixed seed + 同 toolchain → exploitability 计算结果 byte-equal**（继承 stage 1 + stage 2 头号 determinism 不变量）。**aspirational**：跨架构（x86_64 ↔ aarch64）byte-equal 是 aspirational（继承 stage 1 D-051 / D-052 + stage 2 跨架构 carve-out 模式），不进 stage 3 强制出口；32-seed baseline regression guard 强制。 |
| D-348 | exploitability 计算性能 SLO | **Kuhn 单 iter exploitability < 100 ms release**（4096 候选 BR 暴力枚举上界）；**Leduc 单 iter exploitability < 1 s release**（backward induction polynomial）；**简化 NLHE 不计算 exploitability**（D-342 走 average regret growth 监控替代）。该 SLO 让 F3 [报告] 4 checkpoint exploitability 实测能在 ≤ 1 分钟内完成（Kuhn 4 × 100 ms + Leduc 4 × 1 s ≤ 4.4 s 实际）。 |
| D-349 | mid-training exploitability 报告频率 | **Kuhn / Leduc 在 1K / 2K / 5K / 10K iter 各 1 个 checkpoint**（4 个 sample point）。**简化 NLHE 在 10M / 25M / 50M / 100M update 各 1 个 checkpoint**（4 个 sample point，无 exploitability 数值仅 average regret growth + average strategy BLAKE3）。F3 [报告] 出 4 个 sample point 对照表 + 收敛曲线图（Kuhn / Leduc 用 exploitability，简化 NLHE 用 `max_avg_regret / sqrt(T)` 替代）。 |

---

## 6. Checkpoint（D-350..D-359）

| 编号 | 决策项 | 选定值 |
|---|---|---|
| D-350 | Checkpoint 二进制 schema | **`magic: [u8; 8] = b"PLCKPT\0\0"` + `schema_version: u32 = 1` + `trainer_variant: u8 ∈ {0=VanillaCFR, 1=ESMccfr}` + `game_variant: u8 ∈ {0=Kuhn, 1=Leduc, 2=SimplifiedNlhe}` + `pad: [u8; 6] = 0` + `update_count: u64` + `rng_state: [u8; 32]` (ChaCha20Rng state, 继承 stage 1) + `bucket_table_blake3: [u8; 32]` (仅简化 NLHE 非零，Kuhn/Leduc 全零) + `regret_table_offset: u64` + `strategy_sum_offset: u64` + body (bincode-serialized HashMap<InfoSetIdEnum, Vec<f64>> sorted-by-key) + `trailer_blake3: [u8; 32] = BLAKE3(file[..len-32])` (D-243 模式)`。**total header = 96 bytes**（8-byte aligned）。具体字节布局锁在 `pluribus_stage3_api.md` API-350。 |
| D-351 | `CheckpointError` 错误枚举 | **5 类**（继承 stage 1 §F1 + stage 2 D-247 错误前移模式）：`FileNotFound { path }` / `SchemaMismatch { expected, got }` / `TrainerMismatch { expected: TrainerVariant, got: TrainerVariant }`（含 game_variant 不匹配） / `BucketTableMismatch { expected: [u8; 32], got: [u8; 32] }`（简化 NLHE 加载时 bucket table BLAKE3 hash check） / `Corrupted { offset, reason }`（含 magic bytes / BLAKE3 trailer / offset 表越界 / bincode 反序列化失败）。每条均需 F1 [测试] 覆盖（含 byte-flip smoke）。 |
| D-352 | trailer BLAKE3 自校验 | **eager 校验在 `Checkpoint::open` 命中**（继承 stage 2 `BucketTable::open` D-243 模式）：读取 trailer 32 byte → 计算 `BLAKE3(file[..len-32])` → 比对；不匹配 → `Corrupted { offset: len-32, reason: "trailer BLAKE3 mismatch" }`。**性能**：100M update checkpoint 文件预计 ~80 MB body + 96 byte header + 32 byte trailer ≈ 80 MB；BLAKE3 throughput ~1 GB/s → eager 校验 ~80 ms 单次开销，可接受。 |
| D-353 | checkpoint 写出原子性 | **write-to-temp + atomic rename**：先写 `<path>.tmp` → 计算 trailer BLAKE3 → flush + fsync → `rename(<path>.tmp, <path>)`。该路径让 SIGKILL / OOM / 断电中断的 checkpoint 写出不会留下 partial file 污染既有 `<path>`。**实现**：走 `std::fs::File` + `tempfile` crate（继承 stage 2 D-255 `memmap2` 模式，新引入 `tempfile = "3"` 单一原子写依赖；A0 [决策] 锁定）。 |
| D-354 | 跨 endian / 跨平台兼容 | **bincode 1.x 默认 little-endian + varint integer encoding**（继承 stage 2 D-244 模式）；header 字段全部 little-endian（同 stage 2 D-244）。Big-endian 机器（如某些嵌入式 ARM）不在 stage 3 强制出口范围，但走 bincode 默认走 LE 兼容；aarch64 macOS / Linux 默认 LE 不影响。 |
| D-355 | checkpoint 自动落地频率 | **Kuhn / Leduc 在 1K / 2K / 5K / 10K iter 各 1 次 auto-save**（与 D-349 报告频率对齐）；**简化 NLHE 每 1M update 1 次 auto-save**（100 个 sample point）。CLI `--checkpoint-every N` flag 覆盖默认。Trainer 实现接 `Checkpoint::save_to(path)` API，trainer 内部不维护 checkpoint policy 状态（trainer 与 checkpoint 解耦）。 |
| D-356 | 多 game checkpoint 不兼容 | **`TrainerMismatch` 拒绝 game_variant 不匹配**（D-351）。Kuhn checkpoint 不可作为 Leduc trainer 起点；简化 NLHE 不同 bucket_table（即 D-314-rev1 v2 vs D-314-rev2 v1）的 checkpoint 互不兼容（`BucketTableMismatch` 拒绝）。该约束让 cross-game 误用立即 fail-fast。 |
| D-357 | 跨语言 reader（Python） | **`tools/checkpoint_reader.py`**（继承 stage 1 `tools/history_reader.py` + stage 2 `tools/bucket_table_reader.py` 同型）：minimal bincode + struct 解码，输出 `(schema_version, trainer_variant, game_variant, update_count, regret_table, strategy_sum)`。F3 [报告] 落地，stage 3 主线工作（A1..F2）不依赖。该路径用于 stage 7 评测脚本 / blueprint visualization。 |
| D-358 | incremental vs full snapshot | **full snapshot**（每次 checkpoint 写完整 regret_table + strategy_sum）。**不**做 incremental delta encoding——简化 NLHE 100M update / 100 个 checkpoint = 每 checkpoint 平均增量 ~1% InfoSet 变化，incremental 收益 ≤ 50%；vs full snapshot 的简单实现 + crash-safe 全自包含优势，stage 3 选 full snapshot。Incremental 在 stage 4 100B update 量级再视情况引入（D-358-revM 翻面候选）。 |
| D-359 | backup checkpoint 保留策略 | **保留最近 5 个 + 4 个 milestone**：milestone = 1K/2K/5K/10K iter（Kuhn/Leduc）或 10M/25M/50M/100M update（简化 NLHE，D-349 对齐）；其它 auto-save checkpoint 每 5 个轮换覆盖。`tools/train_cfr.rs --keep-last N` flag 覆盖默认。CI artifact upload 全部 milestone + 最新 1 个 auto-save。 |

---

## 7. 性能 SLO（D-360..D-369）

| 编号 | 决策项 | 选定值 |
|---|---|---|
| D-360 | Kuhn / Leduc Vanilla CFR 时长上界 | **Kuhn 10K iter 单线程 release `< 1 s`**（12 InfoSet × 2 action × 10K iter ~ 240K node visits 量级）。**Leduc 10K iter 单线程 release `< 60 s`**（~288 InfoSet × tree size × 10K iter ~ 数百万 node visits 量级）。该 SLO 让 F1 [测试] 收敛性测试在每次 push 跑 1 次（`#[ignore]` opt-in，与 stage 1 SLO 同型 release 触发）。 |
| D-361 | 简化 NLHE ES-MCCFR 训练吞吐 | **单线程 release `≥ 10,000 update/s`**（100M update / 10K = 10⁴ s ≈ 2.78 h 单 host 可行）。**4-core release `≥ 50,000 update/s`**（多线程效率 ≥ 0.5；具体并发模型 D-321 deferred 决定）。**8-core+**：aspirational，不进强制出口；stage 4 6-max blueprint 起步时再视硬件实测。`tests/perf_slo.rs::stage3_simplified_nlhe_*` 在 vultr 4-core EPYC 实测落地。 |
| D-362 | 重复确定性 SLO | **同 seed 重复跑 BLAKE3 byte-equal**：Kuhn 10K iter 重复 1000 次 BLAKE3 一致；Leduc 10K iter 重复 10 次 BLAKE3 一致；简化 NLHE 100M update 重复 3 次 BLAKE3 一致（每次成本 ~3 h，重复 3 次 ≤ 10 h vultr）。**跨 host 同 toolchain 同 seed BLAKE3 一致**（继承 stage 1 + stage 2 头号 determinism 不变量）。 |
| D-363 | 外部对照口径（OpenSpiel） | **自洽性优先 + OpenSpiel CFR 轻量对照**（继承 stage 2 D-260 模式）。主验收依赖 Kuhn closed-form anchor + Leduc fixed-seed BLAKE3 byte-equal + 简化 NLHE checkpoint round-trip；F3 [报告] 附带 OpenSpiel `algorithms/cfr_py.py` Kuhn / Leduc 收敛曲线对照（D-364 锁定口径）。**OpenSpiel 简化 NLHE 对照不强求**（OpenSpiel 不直接支持 stage 2 bucket abstraction，对照成本高、收益低）。 |
| D-364 | OpenSpiel 收敛轨迹趋势对照 | **收敛轨迹趋势一致**（Kuhn / Leduc 各自 10K iter 内 exploitability 下降）；**不**要求各 iter exploitability 数值 byte-equal——OpenSpiel 实现可能用 regret matching+ 或不同 sampling，数值 byte-equal 不现实。具体 sample point: Kuhn / Leduc 各取 1K / 2K / 5K / 10K iter 4 个 sample point 对照，趋势单调下降即视为 trend match。 |
| D-365 | OpenSpiel 收敛失败 P0 | **OpenSpiel CFR 在 Kuhn 或 Leduc 任一 game 上 exploitability 不下降视为 stage 3 P0 阻塞 bug**（暗示我们的 game environment 实现与标准 CFR ground truth 偏离）。具体 iter 数值差异不阻塞，仅在 F3 报告标注 reference difference。 |
| D-366 | F3 一次性接入 OpenSpiel | **`tools/external_cfr_compare.py` 在 F3 [报告] 起草时一次性接入**（继承 stage 2 D-263 模式），stage 3 主线工作（A1..F2）不依赖 OpenSpiel。`tools/external_cfr_compare.py` 接 PyPI `open_spiel==1.5.x` 或 latest，跑 Kuhn / Leduc CFR 10K iter 输出 exploitability 曲线对照表。 |
| D-367 | bench profile（criterion） | **`benches/stage3.rs`**（与 stage 1 `benches/baseline.rs` + stage 2 同型扩展）：3 个 bench group——`stage3/kuhn_cfr_iter`（单 iter throughput）/ `stage3/leduc_cfr_iter` / `stage3/nlhe_es_mccfr_update`（per-update throughput）。CI 短路径走 `--warm-up-time 1 --measurement-time 1 --sample-size 10 --noplot`；nightly 跑全量。 |
| D-368 | 跨架构 SLO | **aspirational**：x86_64 ↔ aarch64 SLO 数值可能差异 ~30%（继承 stage 1 + stage 2 carve-out）；阶段 3 SLO 仅在 vultr 4-core EPYC-Rome 主验收 host 上强制达成。darwin-aarch64 / GitHub-hosted runner 上的实测仅供参考。 |
| D-369 | perf SLO test harness | **`tests/perf_slo.rs::stage3_*`**（与 stage 1 `stage1_*` + stage 2 `stage2_*` 同型扩展）：release profile + `--ignored` 显式触发，CI nightly 跑 bench-full + 短 bench 在 push 时跑。失败时输出实测吞吐 + 上下文（host CPU / load average）。 |

---

## 8. Crate / 模块 / Cargo.toml（D-370..D-379）

| 编号 | 决策项 | 选定值 |
|---|---|---|
| D-370 | `src/training/` 子模块布局 | **`src/training/` 新增 module 树**：`mod.rs` / `game.rs`（`Game` trait + 3 game 适配） / `kuhn.rs` + `leduc.rs` + `nlhe.rs`（具体游戏） / `regret.rs`（`RegretTable` / `StrategyAccumulator`） / `trainer.rs`（`Trainer` trait + `VanillaCfrTrainer` / `EsMccfrTrainer`） / `sampling.rs`（chance / opponent sampling + RngSource sub-stream 派生） / `best_response.rs`（Kuhn / Leduc BR + exploitability） / `checkpoint.rs`（Checkpoint binary schema + 5 类 error）。**不分 crate**（继承 stage 1 D-010..D-012 + stage 2 §Crate 布局 模式，单 crate 多 module 直到 API 稳定）。 |
| D-371 | `Trainer` trait surface | `trait Trainer<G: Game> { fn step(&mut self, rng: &mut dyn RngSource); fn save_checkpoint(&self, path: &Path) -> Result<(), CheckpointError>; fn load_checkpoint(path: &Path) -> Result<Self, CheckpointError>; fn current_strategy(&self, info_set: &G::InfoSet) -> Vec<f64>; fn average_strategy(&self, info_set: &G::InfoSet) -> Vec<f64>; fn update_count(&self) -> u64; }`。具体签名锁在 `pluribus_stage3_api.md` API-300。`Trainer` impl `Send + Sync`（多线程 ES-MCCFR 前提，D-321 thread-safety 决定具体内部 lock 模型）。 |
| D-372 | `tools/train_cfr.rs` CLI | **CLI 入口**：`cargo run --release --bin train_cfr -- --game {kuhn,leduc,nlhe} --trainer {vanilla,es-mccfr} --iter N --seed S --checkpoint-dir DIR [--resume PATH] [--checkpoint-every N] [--keep-last N]`。具体 flag 锁在 `pluribus_stage3_api.md` API-370。Trainer 选择由 `--game` 自动推断（kuhn/leduc → vanilla，nlhe → es-mccfr）；显式 `--trainer` 覆盖。 |
| D-373 | Cargo.toml 新增依赖 | **新增 3 crate**：① `bincode = "1.3"`（D-327 checkpoint 序列化）；② `tempfile = "3"`（D-353 原子写）；③ thread-safety crate 在 D-321 batch 3 [实现] 之前决定（候选 `parking_lot` 0.12 / `dashmap` 5.5 / `crossbeam` 0.8）。继承 stage 1 `blake3` / `memmap2` / `serde` 既有依赖。**不引入**：① `nalgebra` / `ndarray`（继承 stage 2 D-250 自实现政策）；② `tokio` / `async-std`（CFR 训练 CPU-bound 不需 async）；③ ML 框架（stage 3 范围明确不引入 NN，stage 4-6 blueprint 也由用户路线 [stage4_6 路径](../memory/...) 明确不引 NN）。 |
| D-374 | 与 stage 1 / stage 2 模块边界 | **`src/training/`** 是 stage 3 唯一新增顶层 module。stage 1 `src/core/` / `src/rules/` / `src/eval/` / `src/history/` + stage 2 `src/abstraction/` 全部只读消费，stage 3 不修改。`src/error.rs` 仅追加 `CheckpointError` + `TrainerError` 枚举（继承 stage 1 + stage 2 错误追加不删模式）。`src/lib.rs` 新增 `pub mod training;` + `pub use training::{Trainer, ...}` re-export。 |
| D-375 | 子 module 文件粒度 | **每个 module 文件 ≤ 1000 行**（继承 stage 2 经验，stage 2 `bucket_table.rs` 接近 800 行已达可读性上限）。超过 1000 行的 module 拆 sub-module（如 `trainer.rs` 超 → `trainer/vanilla.rs` + `trainer/es_mccfr.rs` + `trainer/mod.rs`）。 |
| D-376 | 公开 vs 私有 API | **公开**：`Trainer` trait + `Game` trait + `RegretTable` / `StrategyAccumulator` + `BestResponse` trait + `Checkpoint` + `CheckpointError` + `TrainerError` + `KuhnGame` / `LeducGame` / `SimplifiedNlheGame` + `VanillaCfrTrainer` / `EsMccfrTrainer`。**私有**：`sampling.rs` 内部辅助函数（cdf binary search 等）+ `trainer.rs` 内部 cfv 累积辅助。 |
| D-377 | type alias / re-export | **顶层 `src/lib.rs`** 不暴露具体 InfoSet 类型枚举（`InfoSetIdEnum`），而是通过 `Game::InfoSet` 关联类型表达。**`pub use`** re-export 至少 `Trainer / Game / RegretTable / Checkpoint / CheckpointError`（让 downstream `use poker::{Trainer, Game}` 短路径可用）。 |
| D-378 | doc-test 策略 | **`Trainer::step` + `Trainer::save_checkpoint` / `load_checkpoint`** 加 doc-test（Kuhn 10 iter 端到端 + checkpoint round-trip + average_strategy query）。doc-test 在 `cargo test` 默认跑（继承 stage 2 D-249 doc-test 路径）。简化 NLHE doc-test 走 `#[ignore]` opt-in（依赖 bucket table artifact，cargo test 默认不触发）。 |
| D-379 | lints scope | **`src/training/`** 允许浮点（`f64` regret / strategy_sum）；**`src/training/sampling.rs`** 的 RNG 派生 + CDF binary search 子段允许浮点。**继承** stage 1 `unsafe_code = "forbid"` + stage 2 `abstraction::map` `clippy::float_arithmetic` 死锁。stage 3 不扩展 lint 边界（不把 float_arithmetic 死锁扩到 training），但 `tests/api_signatures.rs` 扩展到覆盖 stage 3 公开 API 签名（trip-wire 模式，继承 stage 2 §A0 模式）。 |

---

## 9. 与阶段 1 / 阶段 2 决策的边界

阶段 3 不修改阶段 1 + 阶段 2 已锁定决策；任何冲突走 stage 1 / stage 2 `D-NNN-revM` 修订流程：

- **stage 1 决策继承**：D-001..D-103 全集 + 9 条 D-NNN-revM 修订（D-033-rev1 incomplete raise / D-037-rev1 last_aggressor / D-039-rev1 odd-chip / API-001-rev1 HistoryError / API-004-rev1 GameState::config / API-005-rev1 RngSource::fill_u64s 等）。stage 3 训练循环 sampling、chance node、tie-break 全部走 stage 1 `RngSource` 显式注入；任何 `rand::thread_rng()` 隐式调用是 P0 阻塞 bug。
- **stage 2 决策继承**：D-200..D-283 全集 + stage 2 实施期间 D-NNN-revM 修订（D-218-rev2 真等价类 deferred / D-244-rev2 schema v2 / D-282 host-load carve-out 等）。stage 3 简化 NLHE 训练通过 stage 2 `ActionAbstraction` / `InfoAbstraction` / `BucketTable` 接口消费抽象，不修改 stage 2 任何类型签名。
- **错误枚举追加不删**：stage 3 新增 `CheckpointError`（D-351）+ `TrainerError`（stage 3 内部，B2/C2 [实现] 时落地）。stage 1 `RuleError` / `HistoryError` + stage 2 `BucketTableError` / `EquityError` 只读不删。
- **浮点边界继承**：stage 1 规则路径无浮点 + stage 2 `abstraction::map` 子模块 `clippy::float_arithmetic` 死锁继续生效。stage 3 引入的 f64 regret / strategy 仅限 `src/training/` 子模块，不允许泄露到 stage 1 / stage 2 锁定路径。

---

## 10. 已知未决项（不阻塞 A1）

阶段 3 A0 [决策] batch 1 锁定算法变体 + 验收门槛骨架后仍有以下未决项；列入此处不阻塞 A1 [实现] 脚手架推进，但在 B2/C2 [实现] 真正消费时必须由后续 D-NNN-revM 落地：

- **D-314（bucket table 依赖）** — **已 lock 为 D-314-rev1（2026-05-13，C1 [测试] 起草前）**：bucket table 依赖锁定为 §G-batch1 §3.10 production v3 artifact `artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin`（528 MiB / body BLAKE3 `67ee555439f2c918698650c05f40a7a5e9e812280ceb87fc3c6590add98650cd`）。详见 §10.1 D-314-rev1 lock 段落。**原文（locked 前 / A0 [决策] entry）**：~~简化 NLHE 训练用 stage 2 C2 hash-based 95 KB v1 artifact 还是 §G-batch1 §3.4-batch2 production 528 MB v2 artifact。**A0 [决策] 不锁，runway 给 §G-batch1 §3.4-batch2 在 vultr 并行跑完**：A0..B2 期间（按 stage 2 时间线 2-3 周）若 v2 artifact 已 ready 由 D-314-rev1 lock 为 v2；否则 v1 fallback 由 D-314-rev2 lock 为 v1 + 显式标注已知 collision carve-out。lock 时间窗：B2/C2 [实现] 简化 NLHE `SimplifiedNlheGame` 开始构造之前。该决策 deferred 是用户授权 stage 3 [决策] 优先于 §G-batch1 §3.4-batch2..§4 closure 的直接产物。~~
- ~~**多线程 ES-MCCFR thread-safety 模型**（D-321 候选）：`RegretTable` HashMap 多线程并发写候选方案（① `parking_lot::RwLock<HashMap>` / ② `dashmap::DashMap` / ③ thread-local accumulator + batch merge / ④ `crossbeam::SegQueue` snapshot reduce）。具体选型留 D-321 在 batch 3 落地，影响 D-361 多线程 SLO 实现路径。~~ — **已 lock 为 D-321-rev1（2026-05-13，C2 [实现] 起步前）**：候选 ③ thread-local accumulator + batch merge + C2 commit ship serial-equivalent step_parallel；真并发实现 deferred 到 E2 [实现] 性能优化阶段。详见 §10.2 D-321-rev1 lock 段落。
- **Linear CFR weighting 引入时间窗**（D-302-rev1 候选）：阶段 3 出口（F3 [报告]）若 simplified NLHE 100M update 后 LBR 实测高于 stage 4 起步 acceptance 预期，可选择在 stage 3 出口前引入 Linear CFR weighting 提前消化收敛速度问题。lock 时间窗：F2 [实现] 收尾前 + 用户授权。
- **regret matching+ 引入时间窗**（D-303-rev1 候选）：同 D-302-rev1，与 Linear CFR 联动。lock 时间窗：F2 [实现] 收尾前 + 用户授权。

---

### 10.1 D-314-rev1 lock（C1 [测试] 起草前，2026-05-13）

**触发器**：`pluribus_stage3_workflow.md` §步骤 C1 line 206 字面 "C1 [测试] 起草前由用户决策 lock D-314 为 v1（D-314-rev2）或 v2（D-314-rev1）" + line 441 carry-forward。本 lock 由 C1 [测试] agent 在 2026-05-13 用户授权下落地（与 stage 2 §A-rev0 / stage 3 §B-rev0 同型：D-NNN-revM 文档变更由非 [决策] role 在 workflow 字面授权下执行）。

**lock 决定**：bucket table 依赖 = §G-batch1 §3.10 production **v3** artifact（不是 v1 fallback 也不是 v2 dual-phase MC artifact）。具体参数：

- **artifact 路径**：`artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin`（CLAUDE.md ground truth 路径，gitignore 不进 git history）
- **size**：553,631,520 bytes = **528 MiB**
- **body BLAKE3**：`67ee555439f2c918698650c05f40a7a5e9e812280ceb87fc3c6590add98650cd`（CLI `content_hash`；CLAUDE.md "当前 artifact 基线" 段 ground truth）
- **schema_version**：`2`（§G-batch1 §3.2 bump，与 D-244-rev2 mandate 一致）
- **训练参数**：single-phase full N × per-street ClusterIter::production_default() (flop=2000/turn=5000/river=10000) × river_exact=true（§3.10 990 outcome enumeration）
- **生成 host**：AWS c6a.8xlarge 32-core EPYC 7R13 / 61 GB on-demand 1h 37m

**调用契约**：`SimplifiedNlheGame::new(bucket_table: Arc<BucketTable>)` 调用方负责 `BucketTable::open(artifact_path)` + `Arc::new` 包装；artifact 缺失或 BLAKE3 mismatch 时由 `BucketTable::open` 返回 `BucketTableError`，`SimplifiedNlheGame::new` 走 `TrainerError::UnsupportedBucketTable`。C2 [实现] 落地 `new` 时校验 `schema_version() == 2`（v1 95 KB fallback 走 `D-314-rev2` 已废弃，C2 拒绝 schema_version=1 输入）。

**测试 setup 策略**（C1 [测试] 起草决定）：`tests/cfr_simplified_nlhe.rs` 5 条测试共享 helper `load_v3_artifact_or_skip()`，当 default artifact 路径文件不存在（典型 CI / GitHub-hosted runner 场景）时打印 eprintln 提示并 `return`（pass-with-skip）。本地 dev box / vultr / AWS host 有 artifact 时跑完整测试。CI 上 panic-fail 形态保留给 A1 scaffold 阶段 `SimplifiedNlheGame::new` 自身 `unimplemented!()`（C2 落地后 skip 路径生效）。

**D-356 BucketTableMismatch 校验路径**：以 v3 body hash `67ee5554...` 为 expected；checkpoint round-trip 跨 artifact 不兼容（v3 训练的 checkpoint 不能加载到 v2/v1 BucketTable 上）。具体 expected hash 锁定在 D2 [实现] checkpoint header schema 落地时由 `BucketTable::content_hash()` 动态写入而非编译期常量。

**D-314-rev2（v1 95 KB fallback）废弃路径**：A0 [决策] entry 字面预留的 v1 fallback 在 §G-batch1 §3.10 v3 artifact 落地后不再需要。C2 [实现] 落地 `SimplifiedNlheGame::new` 时直接拒绝 schema_version=1，不留 v1 兼容入口。

**carve-out**：本 lock 由 C1 [测试] agent 落地决策文档变更，属 stage 3 workflow 字面授权的角色越界（与 stage 2 §B-rev1 / stage 3 §B-rev0 同型）。本 §10.1 entry 同 commit 落地，无需独立 `pluribus_stage3_workflow.md` §修订历史 entry 追认（workflow line 206 字面授权 = 修订历史 entry 等价物）。

---

### 10.2 D-321-rev1 lock（C2 [实现] 起步前，2026-05-13）

**触发器**：`pluribus_stage3_workflow.md` §步骤 C2 line 216 字面 "D-321 thread-safety 模型在 C2 [实现] 起步前 lock；C2 commit 锁定具体实现" + line 441 carry-forward + D-321 4 候选并行 deferred。本 lock 由 C2 [实现] agent 在 2026-05-13 用户授权下落地（与 §10.1 D-314-rev1 同型：D-NNN-revM 文档变更由非 [决策] role 在 workflow 字面授权下执行）。

**lock 决定**：thread-safety 模型 = 候选 ③ **thread-local accumulator + batch merge**。C2 commit ship "serial-equivalent step_parallel"（在 rng_pool 上循环单线程 step；不引入实际跨线程同步），真并发实现 deferred 到 E2 [实现] 性能优化阶段。

**理由**：
- C1 [测试] 全 5 条测试（D-313 root / D-318 桥接 / D-317 桥接 / D-342 1K smoke / D-362 1M × 3 BLAKE3 byte-equal）均走单线程 `EsMccfrTrainer::step`，不消费 `step_parallel`。C2 commit 主线交付目标 = "C1 5 条测试全部转绿"，不依赖多线程实现。
- 候选 ③（thread-local accumulator）让 regret accumulator 在每线程独立持有 owned clone，per-step 末端 batch merge 到主表。该路径 vs ① RwLock / ② DashMap 的优势：① / ② 在 lock contention 下 4-core throughput 通常退化到 1.5× 单线程（已知 HashMap 全锁 / 细粒度锁 fan-in pattern）；③ 在 batch merge 间无 lock 接触，4-core 接近 4× 线性 scaling。候选 ④（crossbeam::SegQueue）适合 producer-consumer 模式与 CFR backward induction cfv 累积语义不匹配。
- D-361 多线程 SLO（4-core `≥ 50,000 update/s`，效率 `≥ 0.5`）实测落地在 E1 [测试]；E2 [实现] 可基于实测瓶颈选 ③ 之 sub-variant（如 lock-free merge 或 channel-based reduce）。C2 commit 锁定 ③ 路径方向，sub-variant 由 E2 实测决定，不在 C2 commit 内细化。
- **D-373 依赖**：C2 commit 不引入 `parking_lot` / `dashmap` / `crossbeam` 任一 thread-safety crate（候选 ③ 走 std `Vec<f64>` + `HashMap` clone 即可）。`Cargo.toml [dependencies]` 在 C2 commit 内继续保持 bincode + tempfile 2 crate；E2 [实现] 评估是否需新增 `rayon = "1"`（thread pool） / `crossbeam-channel = "0.5"`（batch merge channel）。

**调用契约**（C2 commit 内 `EsMccfrTrainer::step_parallel` 接口形态）：

```rust
impl<G: Game> EsMccfrTrainer<G> {
    /// C2 [实现] commit ship serial-equivalent fallback（D-321-rev1 lock）：
    /// 在 `rng_pool` 上循环 single-threaded `step`；忽略 `n_threads` 参数。E2
    /// [实现] 落地真并发后翻面为 thread-local accumulator + batch merge 真路径。
    pub fn step_parallel(
        &mut self,
        rng_pool: &mut [Box<dyn RngSource>],
        _n_threads: usize,
    ) -> Result<(), TrainerError> {
        for rng in rng_pool.iter_mut() {
            self.step(rng.as_mut())?;
        }
        Ok(())
    }
}
```

**E2 [实现] 路径预告**（D-321-rev1 之外的 sub-variant 决策窗口）：
1. 替换 `step_parallel` 内部循环为 `std::thread::scope` × n_threads 并行 spawn；每 spawn 持有 thread-local `(RegretTable, StrategyAccumulator)` clone。
2. spawn 结束后 main thread 走 batch merge：遍历每个 thread-local accumulator 的 HashMap entries → 累加到 main accumulator（accumulate 走 D-305 标准 CFR update + D-322 strategy_sum 累积）。
3. step_parallel 一次调用 = n_threads × per-step update（D-307 alternating traverser 在线程间共享 `update_count % n_players` 选 traverser 让每线程 alternate）。
4. 与 D-362 重复确定性兼容性：thread-local accumulator 顺序合并（按 thread id 升序）→ HashMap entries 排序后 merge → 跨 run BLAKE3 byte-equal 不破。

**carve-out**：本 lock 由 C2 [实现] agent 落地决策文档变更，与 §10.1 D-314-rev1 同型 workflow 字面授权角色越界。本 §10.2 entry 同 commit 落地，无需独立 `pluribus_stage3_workflow.md` §修订历史 entry 追认（workflow line 216 字面授权 = 修订历史 entry 等价物）。

---

### 10.3 D-317-rev1 lock（C2 [实现] 落地中暴露，2026-05-13）

**触发器**：C2 [实现] 落地 `EsMccfrTrainer::step` 后跑 `simplified_nlhe_es_mccfr_1k_update_no_panic_no_nan_no_inf` 在第 N 次 update（N ≪ 1000）panic：`RegretTable::get_or_init action_count mismatch: stored 3, requested 2 (D-324)`。根因：D-324 字面 "action_count 训练全程对同一 InfoSetId 恒定" 在 simplified NLHE + `DefaultActionAbstraction` + stage 2 `InfoSetId` 组合下不自动满足。

**根因分析**：
1. **stage 2 `DefaultActionAbstraction`** D-209 输出顺序 `[Fold?, Check?, Call?, Bet|Raise@0.5?, Bet|Raise@1.0?, AllIn?]` 长度可变（`?` skippable）；同 `(GameState, hole)` 输入下输出长度由 `LegalActionSet.fold/check/call/bet_range/raise_range/all_in_amount` 与 cap 决定。
2. **stage 2 `InfoSetId`**（D-215）仅编码 `(bucket_id, position_bucket, stack_bucket, betting_state, street_tag)`，其中 `stack_bucket` 是 5 桶粗分（D-211：< 20 BB / 20..50 / 50..100 / 100..200 / ≥ 200）。同 stack_bucket 内不同 cap 值（如 200 BB vs 350 BB）会让 `raise_range` / `all_in_amount` 触发条件不同 → `DefaultActionAbstraction` 输出长度不同。
3. **D-324 违反**：rollout A 在 same InfoSetId 命中 5 actions、rollout B 命中 2 actions → `RegretTable::get_or_init(I, n)` 第二次调用 panic。

**lock 决定**：D-317-rev1 = stage 3 **carve-out** "把 `legal_actions(state)` 输出的 6-bit availability mask 写入 `InfoSetId.bucket_id` field bits 12..18"。具体规则：

- 6 个 mask bit 对应 D-209 顺序：bit 0=Fold / bit 1=Check / bit 2=Call / bit 3=Bet|Raise@`BetRatio::HALF_POT` / bit 4=Bet|Raise@`BetRatio::FULL_POT` / bit 5=AllIn。
- `bucket_id` field 24 bits 切分：bits 0..12 = 实际 bucket（preflop hand_class 0..169 / postflop bucket 0..500，均 < 4096）+ bits 12..18 = action mask + bits 18..24 = 0 reserved within bucket_id。
- stage 2 `InfoSetId` 整体 64-bit layout 不变：bits 38..64 reserved 仍恒为零（IA-007 不变量满足；本 carve-out 仅触及 bucket_id field 内部位分配，不触及 stage 2 IA-007 reserved 区域）。
- `pack_info_set_id` 调用方式不变（输入仍是 u32 bucket_id，stage 3 调用方在写入 mask 后传入 combined value `base_bucket | (mask << 12)`）。

**调用契约**（C2 commit 内 `src/training/nlhe.rs` 落地）：

```rust
// src/training/nlhe.rs
const ACTION_MASK_SHIFT: u32 = 12;
const ACTION_MASK_BITS: u32 = 6;

fn action_signature_mask(actions: &[AbstractAction]) -> u8 {
    let mut mask = 0u8;
    for a in actions {
        let bit = match a {
            AbstractAction::Fold => 0,
            AbstractAction::Check => 1,
            AbstractAction::Call { .. } => 2,
            AbstractAction::Bet { ratio_label, .. }
            | AbstractAction::Raise { ratio_label, .. } => {
                if ratio_label.as_milli() == BetRatio::HALF_POT.as_milli() { 3 }
                else if ratio_label.as_milli() == BetRatio::FULL_POT.as_milli() { 4 }
                else { 4 }  // default_5_action 仅 HALF_POT / FULL_POT
            }
            AbstractAction::AllIn { .. } => 5,
        };
        mask |= 1u8 << bit;
    }
    mask
}

// Game::info_set 内：
let bucket_id_with_mask = base_bucket | (u32::from(action_mask) << ACTION_MASK_SHIFT);
pack_info_set_id(bucket_id_with_mask, position_bucket, stack_bucket, betting_state, street_tag)
```

**不变量** (D-317-rev1 / D-324 联合)：

1. `base_bucket < 2^12 = 4096`（preflop hand_class ≤ 168 / postflop bucket ≤ 499 均成立）。`debug_assert!` 在 C2 commit 内 enforce。
2. `mask < 2^6 = 64`（D-209 最多 6 个 slot）。`debug_assert!` enforce。
3. 同 (InfoSetId, mask) → 同 `legal_actions().len()`（mask 是 popcount 的 superset；同 mask 必同 popcount）。D-324 严格满足。
4. 不同 (InfoSetId, mask) → CFR 视作两个独立 InfoSet（HashMap key 不同）。该约束让 stack_bucket=4 内不同 cap 状态被自动分离训练，提升 CFR 收敛精度（vs 原 D-317 字面 lossy aggregation）。
5. mask 编码与 D-209 顺序绑定：bit i 对应输出位置 i 的 role；action index 在 trainer RegretTable Vec<f64> 内的角色跨 rollout 一致（D-318 字面 "action 顺序 deterministic 且与 RegretTable Vec<f64> 索引一一对应"）。

**与 IA-007 关系**：`pluribus_stage2_api.md` IA-007 / `tests/info_id_encoding.rs::info_id_reserved_bits_must_be_zero` 仅校验 stage 2 `PreflopLossless169::map` / `PostflopBucketAbstraction::map` 输出的 `InfoSetId.raw() & !((1<<38)-1) == 0`。stage 3 `SimplifiedNlheGame::info_set` 输出的 `InfoSetId` 经 bucket_id field 编码 mask（bits 12..18）后，bits 38..64 仍恒为零——IA-007 数学上仍成立。stage 2 既有测试不受影响。

**与 D-326 关系**：D-326 "多 game 独立 RegretTable"（Kuhn / Leduc / 简化 NLHE 三 game checkpoint 互不兼容）继续成立。simplified NLHE 的 InfoSetId 与 stage 2 `InfoAbstraction::map` 输出的 InfoSetId 同型 u64，但语义不同（stage 3 bucket_id field 复用了 6 bits）；D-326 已经隔离这两类 InfoSetId 池，互不污染。

**与 D-356 关系**：D-356 BucketTableMismatch 拒绝跨 bucket_table 加载（checkpoint header `bucket_table_blake3`）。InfoSetId 编码 mask 的方式仅与 `DefaultActionAbstraction::default_5_action()` 输出有关，不与 bucket_table 内容直接相关；D-356 跨 bucket_table 隔离继续成立。

**stage 4 转出**：stage 4 6-max blueprint 走 Linear CFR / RM+ 翻面（D-302-rev1 / D-303-rev1）时，stage 3 InfoSetId encoding（bits 12..18 action mask）是 stage 4 入口前必须 mat决定的 carry-forward 项。两条候选路径：
1. stage 4 继续走 D-317-rev1 stage 3 encoding（直接复用 stage 3 checkpoint）；
2. stage 4 引入 `D-215-revM` 让 InfoSetId layout 显式预留 action_mask field（stage 2 schema bump）。

具体由 stage 4 起步 [决策] batch 决定，stage 3 F3 [报告] 时只标注 carry-forward 状态。

**carve-out**：本 lock 由 C2 [实现] agent 在 C1 测试触发 D-324 panic 时落地（与 §10.1 / §10.2 同型 workflow 字面授权角色越界）。C1 测试 0 改动（继承 stage 2 §B-rev1 / stage 3 §B-rev0 角色边界政策）；C2 [实现] 落地路径全部在 `src/training/nlhe.rs` 内部 + 同 commit 落地 §10.3 entry。无需独立 `pluribus_stage3_workflow.md` §修订历史 entry 追认（workflow line 478 字面 "C1 test expose 产品代码之外的契约 bug → filed issue 协商 D-NNN-revM 流程" = 本 lock 等价物，本场景 expose 的是 D-324 vs stage 2 InfoSetId granularity 不匹配，stage 3 内部 in-place 修复不波及 stage 1 / stage 2 surface）。

---

## 11. 决策修改流程

继承 `pluribus_stage1_decisions.md` §10 + `pluribus_stage2_decisions.md` §11 修改流程，**D-NNN-revM 追加不删** + 在工作流 issue / PR 中显式标注 + 必要时 bump schema_version。stage 3 引入 `Checkpoint.schema_version`（D-350）作为 stage 3 自己的 serialization version anchor，与 stage 1 `HandHistory.schema_version` + stage 2 `BucketTable.schema_version` 互不冲突。

---

## 12. 与决策文档 / API 文档的对应关系

- `pluribus_stage3_validation.md` §1-§7：anchor path.md §阶段 3 字面 5 条门槛 + stage 3 不变量边界 + 性能 SLO 汇总。决策值由本文档 §1-§8 落地，validation.md 仅引用 D-NNN 编号。
- `pluribus_stage3_api.md`：API-300..API-3xx 锁定 Rust 接口签名（`Trainer` / `Game` / `RegretTable` / `BestResponse` / `Checkpoint` trait）。本文档 §3-§8 决策与 API surface 一一对应；签名变化走 API-NNN-revM 修订流程。
- `pluribus_stage3_workflow.md`：13 步 test-first 流程 + Agent 分工 + 反模式。本文档为 [测试] / [实现] / [报告] 共同 spec。

---

## 参考资料

- Zinkevich, Bowling, Johanson, Piccione, "Regret Minimization in Games with Incomplete Information"（NeurIPS 2007，CFR 原始论文 + Vanilla CFR 算法定义 + Theorem 1 收敛上界）
- Lanctot, Waugh, Zinkevich, Bowling, "Monte Carlo Sampling for Regret Minimization in Extensive Games"（NeurIPS 2009，MCCFR / External Sampling 原始论文 + §4.1 ES 伪代码 + Theorem 4/7 收敛上界）
- Brown, Sandholm, "Solving Imperfect-Information Games via Discounted Regret Minimization"（AAAI 2019，Linear CFR / regret matching+ 起源 — stage 3 不引入，留 stage 4）
- Tammelin, Burch, Johanson, Bowling, "Solving Heads-up Limit Texas Hold'em"（IJCAI 2015，CFR+ Cepheus — stage 3 不引入，留 stage 4）
- Brown, Sandholm, "Superhuman AI for multiplayer poker"（Science 2019，Pluribus 主论文 §S2 描述 ES-MCCFR 在 6-max 上的实战参数）
- OpenSpiel CFR / MCCFR Python 实现：https://github.com/google-deepmind/open_spiel/tree/master/open_spiel/python/algorithms（D-364 F3 [报告] 外部对照参考）

---

## 修订历史

本文档遵循与 `pluribus_stage1_decisions.md` §10 / `pluribus_stage2_decisions.md` §11 相同的 "追加不删" 约定。

阶段 3 实施期间的角色边界 carve-out 追认（B-rev / C-rev / D-rev / E-rev / F-rev 命名风格继承阶段 1 + 阶段 2）落到 `pluribus_stage3_workflow.md` §修订历史，本节不重复记录。

- **2026-05-12（A0 [决策] 起步 batch 2 落地）**：stage 3 A0 [决策] 起步 batch 2 落地 `docs/pluribus_stage3_decisions.md`（本文档）骨架 + §1 算法变体 D-300..D-309 全集。本节首条由 stage 3 A0 [决策] batch 2 commit 落地，与 `pluribus_stage3_validation.md` §修订历史首条 + `pluribus_stage3_workflow.md` §修订历史首条同步。
    - §1 算法变体：D-300 Vanilla CFR for Kuhn / Leduc + Zinkevich 2007 伪代码 + 收敛上界引用；D-301 ES-MCCFR for 简化 NLHE + Lanctot 2009 伪代码 + 收敛上界引用；D-302 非 Linear CFR weighting（Linear 留 stage 4 D-302-revM）；D-303 标准 regret matching（RM+ 留 stage 4 D-303-revM）；D-304 标准 average strategy 累积（Vanilla 精确 reach / ES sampled reach）；D-305 标准 CFR regret update 公式；D-306 regret matching 算法定义边界（退化局面 / 数值容差 / 数值类型 lock 跨 §1/§4 拆分，具体值锁在 D-330 / D-331 / D-333）；D-307 alternating traverser selection；D-308 chance node 采样 1 outcome；D-309 non-traverser action 按当前 strategy 采样 1 action。
    - §2-§4 决策表项 **batch 3 落地**（同一 commit）：§2 游戏环境 D-310 Kuhn 规则 / D-311 Leduc 规则 / D-312 `Game` trait 统一抽象层 / D-313 简化 NLHE 规则范围 / D-314 bucket table 依赖 deferred / D-315 chance distribution 走 stage 1 `RngSource` / D-316 chip 净收益直接当 utility / D-317 InfoSet 识别（Kuhn/Leduc 独立 InfoSet 编码、简化 NLHE 继承 stage 2 `InfoSetId`） / D-318 `Game::legal_actions` 与 stage 2 `ActionAbstraction` 边界 / D-319 owned clone state representation。§3 regret/strategy 存储 D-320 `HashMap<InfoSetIdEnum, Vec<f64>>` / D-321 thread-safety deferred / D-322 `StrategyAccumulator` 独立 / D-323 lazy 初始化 / D-324 action_count 训练全程恒定 / D-325 RSS 上界 ≤ 8 GB (simplified NLHE) / ≤ 100 MB (Kuhn/Leduc) / D-326 多 game 隔离 checkpoint / D-327 bincode 1.x 序列化 / D-328 query API stateless 返回 Vec<f64> / D-329 浮点容差 warn 不 panic。§4 sampling/traversal D-330 sum 容差 1e-9 / D-331 退化均匀分布 / D-332 零和约束 1e-6 / D-333 f64（非 f32）/ D-334 DFS recursive / D-335 RngSource sub-stream 派生 + 6 个 op_id 表项 / D-336 chance sampling 自实现 binary search / D-337 opponent sampling 同型 / D-338 per-action cfv `Vec<f64>` 累积 / D-339 chip → f64 lossless terminal value。
    - §5-§8 决策表项 **batch 4 落地**（同一 commit）：§5 exploitability D-340 Kuhn full-tree backward induction BR + closed-form `-1/18` anchor / D-341 Leduc same algorithm + `< 0.1` 阈值 / D-342 简化 NLHE 验收门槛 4 项 / D-343 average regret growth 监控（`max_avg_regret / sqrt(T) ≤ C`，C 由 F3 实测落地）/ D-344 BestResponse 输出 (one-hot strategy + BR value) / D-345 exploitability checkpoint snapshot artifacts/ + git LFS / D-346 LBR 不纳入 stage 3 / D-347 跨 host BLAKE3 一致 + 跨架构 aspirational / D-348 exploitability 计算 SLO（Kuhn < 100ms / Leduc < 1s）/ D-349 mid-training sample point 4 个。§6 checkpoint D-350 96 byte header + magic `PLCKPT\0\0` + schema_version 1 + trainer_variant + game_variant + bucket_table_blake3 + trailer BLAKE3 / D-351 5 类 `CheckpointError` / D-352 trailer BLAKE3 eager 校验 / D-353 write-to-temp + atomic rename / D-354 bincode 1.x LE 默认 / D-355 auto-save 频率 / D-356 多 game 不兼容 / D-357 Python reader F3 落地 / D-358 full snapshot 不做 incremental / D-359 backup 保留最近 5 + 4 个 milestone。§7 性能 SLO D-360 Kuhn < 1s + Leduc < 60s release / D-361 简化 NLHE 单线程 ≥ 10K update/s + 4-core ≥ 50K update/s / D-362 重复确定性（Kuhn 1000× / Leduc 10× / NLHE 3× BLAKE3 byte-equal）/ D-363 OpenSpiel 自洽性优先 + 轻量对照 / D-364 收敛轨迹趋势对照 / D-365 OpenSpiel 收敛失败 P0 / D-366 F3 一次性接入 / D-367 criterion bench 3 group / D-368 跨架构 aspirational / D-369 `tests/perf_slo.rs::stage3_*`。§8 crate/module D-370 `src/training/` 子模块布局 9 文件 / D-371 `Trainer<G: Game>` trait surface / D-372 `tools/train_cfr.rs` CLI / D-373 新增 3 crate（bincode + tempfile + thread-safety TBD）/ D-374 与 stage 1 / stage 2 模块边界 / D-375 ≤ 1000 行 / 文件 / D-376 公开 vs 私有 API / D-377 type alias 走 `Game::InfoSet` 关联类型 / D-378 doc-test Trainer::step + save/load / D-379 lints scope 不扩展 float_arithmetic 死锁但扩展 api_signatures.rs trip-wire。
    - §9 与 stage 1 / stage 2 决策边界：stage 1 D-001..D-103 + 9 条 revM 全集继承；stage 2 D-200..D-283 全集继承；错误枚举追加不删；浮点边界继承。
    - §10 已知未决项：D-314（bucket table 依赖）deferred 到 B2/C2 [实现] 之前由 D-314-rev1（v2 528 MB）或 D-314-rev2（v1 95 KB fallback + collision carve-out）lock；D-321 多线程 thread-safety 模型 deferred 到 batch 3；D-302-rev1 / D-303-rev1 Linear CFR + RM+ 引入时间窗 deferred 到 F2 [实现] 收尾前 + 用户授权。
    - §11 决策修改流程：继承 stage 1 + stage 2；引入 `Checkpoint.schema_version`（D-350）作为 stage 3 自己的 serialization version anchor。
