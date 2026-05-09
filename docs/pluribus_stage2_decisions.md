# 阶段 2 决策记录

## 文档地位

本文档记录阶段 2（抽象层）的全部技术与规则决策。一旦 commit，后续步骤（A1 / B1 / B2 / ... / F3）的所有 agent 必须严格按此 spec 执行。

任何决策修改必须：
1. 在本文档以 `D-NNN-revM` 形式追加新条目（不删除原条目）
2. 必要时 bump `BucketTable.schema_version`（D-240）或 `HandHistory.schema_version`（继承阶段 1 D-101，仅当抽象层修改影响序列化时触发）
3. 通知所有正在工作的 agent（在工作流 issue / PR 中显式标注）

未在本文档列出的细节，agent 应在 PR 中显式标注 "超出 A0 决策范围"，由决策者补充决策后再实施。

阶段 2 决策编号从 **D-200** 起，与阶段 1 D-NNN（D-001..D-103）不冲突。阶段 1 D-NNN 全集 + D-NNN-revM 修订作为只读 spec 继承到阶段 2，未在本文档显式覆盖的部分以 `pluribus_stage1_decisions.md` 为准。

---

## 1. Action abstraction（D-200..D-209）

| 编号 | 决策项 | 选定值 |
|---|---|---|
| D-200 | 默认 action 集合 | **5-action**（不含 `Fold` / `Check` 互斥剔除前的候选集合）：`{ Fold, Check, Call, Bet/Raise(0.5×pot), Bet/Raise(1.0×pot), AllIn }`，其中 "Bet/Raise(x×pot)" 在本下注轮无前序 bet 时输出 `Bet`、有前序 bet 时输出 `Raise`（继承 stage 1 LA-002 互斥）。详见下方 D-200 详解。 |
| D-201 | off-tree action 映射算法 | **Pseudo-harmonic mapping (PHM)**（Pluribus 论文 §S2 标准）。阶段 2 仅落 stub（接口签名稳定 + nearest-action fallback 实现），完整数值验收 + fuzz 留 stage 6c。 |
| D-202 | `ActionAbstractionConfig` 1–14 raise size 接口 | Rust 结构体 `ActionAbstractionConfig { raise_pot_ratios: Vec<BetRatio> }`，长度 ∈ [1, 14]，每个元素 ∈ (0.0, +∞)；超界由 `ActionAbstractionConfig::new` 返回 `Result<_, ConfigError>`。**阶段 2 仅默认 5-action 强验收**；其它配置仅 smoke test（"配置可加载 + 输出确定性 + 哈希区分性"）。无 TOML / JSON 反序列化层（A0 不预定，stage 4 视消融需求决定）。 |
| D-203 | "pot" 定义 | **pot-relative bet/raise 中 "pot" = 当前 pot + 当前 actor 跟注金额**（即 actor call 完后的 pot）。等价表述：`new_to = max_committed_this_round + ratio × (pot_before_action + (max_committed_this_round - actor.committed_this_round_before))`。该约定与 PokerKit `state.pot_amount(...)` 在 `PotLimitNoLimit` 模式下的语义一致。 |
| D-204 | `Fold` 剔除规则 | 当 `LegalActionSet.check == true`（无前序 bet）时，5-action 输出**剔除** `Fold`（玩家 Free-check 局面下 fold 是 -EV 严格劣势动作）。其他局面 `Fold` 保留（需要 call 才能继续时 fold 合法）。 |
| D-205 | `Bet/Raise(x×pot)` fallback 规则 | bet vs raise 由 stage 1 `LegalActionSet`（LA-002 互斥）决定：`bet_range.is_some()` ⇒ 输出 `Bet`，`raise_range.is_some()` ⇒ 输出 `Raise`。按以下顺序判定（first-match-wins）：① 若 `x×pot < min_to`（违反 D-034 首次开局 / D-035 链式 raise），**替换** `to` 为 `min_to`（合法最小 bet 或 raise，不剔除）；② 若 `x×pot >= committed_this_round + stack`（即超出剩余筹码），**替换**整个动作为 `AllIn { to = committed_this_round + stack }`；③ 否则保留为 `Bet { to = x×pot }` 或 `Raise { to = x×pot }`，向上取整到 chip。规则 ① 与 ② 同时触发时（min_to ≥ committed_this_round + stack）走 `AllIn`。 |
| D-206 | `Bet/Raise(x×pot)` 与显式 `AllIn` 去重 | 若 D-205 fallback 后 `Bet/Raise(0.5×pot)` 与 `Bet/Raise(1.0×pot)` 折叠到相同 `to` 值（典型场景：短码），保留 ratio_label 较小的一份；若进一步与 `AllIn` 折叠（同 `to`），保留一份且枚举 tag 选 `AllIn`（保证抽象动作集合 size 单调收缩，下游 InfoSet 编码不混乱）。 |
| D-207 | 抽象动作 `to` 字段语义 | 抽象动作集合中每个 `Bet` / `Raise` / `Call` / `AllIn` 持有具体 `ChipAmount(to)` 值（不是 ratio 占位符）；与 `Action::Bet/Raise { to }` 同语义。`Bet { to, ratio_label }` 与 `Raise { to, ratio_label }` 中 `ratio_label` 仅作为 InfoSet 编码区分性使用，apply 时取 `to`；`Fold` / `Check` 不带 `to`。 |
| D-208 | 当前 actor 视角下 `effective_stack` 定义 | `effective_stack = min(actor.stack, max(opp.stack for opp in still_active_opps))`，含 actor 自己尚未投入但仍持有的部分。该值用于 D-211 stack bucket 与 D-205 fallback 判定。"still_active_opps" 包含 `Active` 与 `AllIn` 状态（已 all-in 对手对 actor 的 effective stack 没有压制效应，但 still 在 pot 中）；只排除 `Folded`。 |
| D-209 | 抽象动作集合的 deterministic 顺序 | 输出顺序固定为 `[Fold?, Check?, Call?, Bet(0.5×pot)? \| Raise(0.5×pot)?, Bet(1.0×pot)? \| Raise(1.0×pot)?, AllIn?]`（按 D-200 5-action 顺序；同一 ratio 槽位 Bet 与 Raise 互斥，由 LA-002 保证）。`?` 表示该位若 D-204 / D-205 / D-206 剔除 / 折叠后不存在则跳过。该顺序作为 InfoSet 编码契约稳定，任何变更走 D-200-revM。 |

### D-200 详解

| 候选动作 | tag | pot ratio | 出现条件（before D-204 / D-205 / D-206 处理） |
|---|---|---|---|
| `Fold` | `AbstractAction::Fold` | — | 任意 actor turn |
| `Check` | `AbstractAction::Check` | — | `LegalActionSet.check == true` |
| `Call` | `AbstractAction::Call { to }` | — | `LegalActionSet.call.is_some()` |
| `Bet(0.5×pot)` | `AbstractAction::Bet { to, ratio_label: HALF_POT }` | 0.5 | `LegalActionSet.bet_range.is_some()`（本下注轮无前序 bet） |
| `Raise(0.5×pot)` | `AbstractAction::Raise { to, ratio_label: HALF_POT }` | 0.5 | `LegalActionSet.raise_range.is_some()`（本下注轮已有前序 bet） |
| `Bet(1.0×pot)` | `AbstractAction::Bet { to, ratio_label: FULL_POT }` | 1.0 | 同 `Bet(0.5×pot)` 出现条件 |
| `Raise(1.0×pot)` | `AbstractAction::Raise { to, ratio_label: FULL_POT }` | 1.0 | 同 `Raise(0.5×pot)` 出现条件 |
| `AllIn` | `AbstractAction::AllIn { to }` | — | `LegalActionSet.all_in_amount.is_some()` |

由 stage 1 LA-002（`bet_range` 与 `raise_range` 互斥）保证：同一 actor turn 上同一 ratio 槽位（如 `0.5×pot`）至多出现 `Bet` 或 `Raise` 之一，绝不同时出现。

**D-200 等价口语化表述**：默认 5-action 不是 "5 个 abstract action 变体" 而是 "5 类输出"——`Fold` / `Check` / `Call` / `Bet 或 Raise (含 0.5×pot 和 1.0×pot 两个 ratio_label)` / `AllIn`。"5-action" 命名沿用 path.md §阶段 2 字面，但实际 abstract action 集合 size ≤ 6（含 Fold + 双 ratio + AllIn 上限）；D-204 / D-205 / D-206 处理后 size 通常落在 [2, 5] 区间。

---

## 2. Information abstraction（D-210..D-219）

| 编号 | 决策项 | 选定值 |
|---|---|---|
| D-210 | preflop position bucket（6-max） | **6 桶**：`{ BTN, SB, BB, UTG, MP, CO }`，与 stage 1 `Position` 枚举（`pluribus_stage1_api.md` §1）byte-equal。stage 2 仅 6-max 强验收；2..=9 其它桌大小走 "seat distance from button mod n_seats" 通用映射，桶数等于 `n_seats`，仅 smoke test。 |
| D-211 | preflop `effective_stack` bucket 边界 | **5 桶**：`[0, 20) BB / [20, 50) BB / [50, 100) BB / [100, 200) BB / [200, +∞) BB`。`effective_stack` 单位 BB（chip / `big_blind`），向下取整。**preflop 起手时**计算（postflop bucket 不依赖 stack bucket，stack 只进 preflop key）。stage 2 默认 100 BB（D-022），落入 `[100, 200)` 桶。边界值采用左闭右开。 |
| D-212 | `betting_state` bucket（preflop + postflop 统一）| **5 状态**：`{ open, facing_bet_no_raise, facing_raise_1, facing_raise_2, facing_raise_3plus }`，3 bit 编码。该字段同时决定 actor 当前合法动作集（关键：`open` 局面 actor 可 `Check / Bet`，`facing_bet_no_raise` 局面 actor 必须 `Fold / Call / Raise`，二者合法动作集**不同**——若仅以 raise count = 0 编码会让两类局面同 InfoSetId 但合法动作集不同，CFR regret 矩阵跨 GameState 错位）。**preflop 语义**：`open` = BB 在 limpers / walks 后有 check option 的局面；`facing_bet_no_raise` = 非 BB 位首次面对 BB 强制下注（无 voluntary raise，仅盲注 + 任意数量 limp）；`facing_raise_k` = 当前下注轮已发生 k 次 voluntary raise（含 incomplete short all-in 视为 1 次）；`facing_raise_3plus` 吸收 ≥ 3 次。**postflop 语义**：`open` = 本街无任何 voluntary bet，actor 可 check 或 bet；`facing_bet_no_raise` = 本街已有 opening bet 但无 raise；`facing_raise_k` = 本街已有 k 次 voluntary raise。盲注本身不算 voluntary aggression（继承 stage 1 D-037）；preflop limp 不算 raise。 |
| D-213 | postflop 默认 bucket 数 | **flop = 500, turn = 500, river = 500**（path.md §阶段 2 字面 ≥ 500）。`BucketConfig` 接口允许每条街独立配置 bucket 数 ∈ [10, 10_000]；**stage 2 验收只跑 500/500/500**，其它配置只做 "配置可加载 + 写出 bucket table + bucket id 范围正确" smoke。 |
| D-214 | postflop `BucketConfig` API | `pub struct BucketConfig { pub flop: u32, pub turn: u32, pub river: u32 }`，构造时校验每条街 ∈ [10, 10_000]。`BucketConfig::default_500_500_500()` 返回默认配置。配置变更时 `BucketTable.schema_version` 不 bump，但 `feature_set_id`（D-240）随特征组合变化。 |
| D-215 | InfoSet key 统一 64-bit layout | 单一 `u64` `InfoSetId` 字段顺序（低位起，跨 preflop / postflop **共用同一 layout**，避免 stage 1 `Street` enum 与抽象层语义解耦）：① `bucket_id`（**24 bit**，preflop 取值 = `hand_class_169` ∈ 0..169，postflop 取值 = `BucketTable::lookup` 返回的 cluster id ∈ 0..`bucket_count(street)`；24 bit 上限 16M 覆盖 D-214 当前 [10, 10_000] 与未来 stage 3+ 扩 bucket 数 / 街合并编码）；② `position_bucket`（**4 bit**，0..n_seats-1，支持 D-030 全部 2..=9 桌大小）；③ `stack_bucket`（**4 bit** 留 slack，0..4 = D-211 5 桶；postflop **沿用 preflop 起手 stack bucket**——postflop 不重算 effective_stack 进 InfoSet）；④ `betting_state`（**3 bit**，0..4 = D-212 5 状态 enum 值）；⑤ `street_tag`（**3 bit**，0..3 = `Preflop / Flop / Turn / River`，preflop 显式编码 `street_tag = 0` 而非靠 "其余字段为 0" 启发式判断）；⑥ `reserved`（**26 bit**，必须为 0；任何非零位写入是 P0 阻塞 bug）。该 64-bit 编码字节级稳定，下游 CFR 可直接对 `InfoSetId.raw()` 做 hash key。完整 betting tree path 编码（如未来 4-bet pot vs 5-bet pot 树分裂）留 stage 3 决策，届时通过 `betting_state` 5 状态扩展或新增 history-compressed bit 实现。 |
| D-216 | preflop / postflop bucket_id 来源差异 | preflop：`bucket_id = hand_class_169` 直接映射，不经 k-means（继承 D-217 编号 + D-239 lossless）。postflop：`bucket_id = BucketTable::lookup(street_tag, board_canonical_id, hole_canonical_id)` 由 mmap 命中返回；street 间 bucket id 命名空间独立（flop bucket 17 与 turn bucket 17 是不同 InfoSet，由 `street_tag` 字段消歧）。两路径下 `bucket_id` 字段宽度都是 D-215 的 24 bit。InfoSetId 跨街 byte-equal 仅在 (bucket_id, position, stack, betting_state, street_tag) 五元组完全相同时成立——这正是 CFR 训练所需的语义。 |
| D-217 | preflop 169 等价类编号 | `hand_class_169 ∈ 0..169`：`0..13` = pocket pairs（22, 33, ..., AA 升序）；`13..91` = suited（按高牌主排序、低牌副排序：32s, 42s, 43s, 52s, ..., AKs）；`91..169` = offsuit（同顺序）。具体编号表落在 `tests/preflop_169.rs` 的 lookup 断言里，A0 仅锁原则；A1 [实现] 落具体数表。 |
| D-218 | canonical hand / board id | hole canonical：22 → `(rank=2, rank=2, suited=false)` → 唯一 id；suited 与 offsuit 各异。board canonical：考虑花色对称性等价类，按 rank 多重集 + suit 模式 canonicalize；具体算法 A1 落地。canonical id 是 `u32`，足够覆盖（5-card board canonical 上限 ~134k；7-card 上限 ~1.5M，远在 u32 内）。 |
| D-219 | postflop 不依赖 preflop key 的隔离原则 | postflop bucket 仅依赖 `(street, board, hole)`（特征只看牌力 / 公牌结构），**不嵌入** position / stack / betting_state。preflop key 的位置 / stack / betting_state 信息留在 `InfoSetId` 复合字段里（D-215 / D-216），不渗入 postflop bucket。理由：postflop bucket 表是 cluster 输出，跨手通用；与博弈树位置无关，便于阶段 6 实时搜索复用同一 mmap 表。 |

---

## 3. Equity & 特征（D-220..D-229）

| 编号 | 决策项 | 选定值 |
|---|---|---|
| D-220 | equity Monte Carlo 默认 iter 数 | **10,000 iter / hand**（`MonteCarloEquity::default()`）。CI 短测试可降到 1,000；clustering 训练路径必须用默认 10k。Monte Carlo 标准误差在 10k iter 下约 `sqrt(0.25/10000) ≈ 0.005`（0.5%）。 |
| D-220a | equity 反对称容差 | `\|equity(A, B \| board) + equity(B, A \| board) - 1\| ≤ 0.005` 在 `iter = 10,000` 时；`iter = 1,000` 时容差放宽到 0.02。容差由 D-220 决定（标准误差近似 `sqrt(0.25/iter)`）。该容差用于 `tests/equity_self_consistency.rs` 反对称断言。 |
| D-221 | 默认特征组合（postflop clustering） | **EHS² + OCHS** 双特征 concat，作为 k-means 输入向量。EHS² 标量 1 维；OCHS 向量 N=8 维（D-222）；总输入维度 = 9。distribution-aware histogram **不进默认**（path.md "可选" 字面），仅作为 stage 4 消融对照接入。 |
| D-222 | OCHS opponent cluster 数 | **N = 8**（Brown & Sandholm 2014 "Strategy-Based Warm Starting for Real-Time Hold'em Poker" 论文使用值；与 Pluribus 实战一致）。8 个 opponent cluster 在 stage 2 启动时通过 preflop 169 上的 EHS 一维 k-means 自训练（同 RngSource seed → 8 cluster centroid byte-equal）。N 配置接口预留为 `OchsConfig { n_opp_clusters: u8 }`，但 stage 2 只跑 N=8。 |
| D-223 | EHS / EHS² 计算路径 | **EHS** = `Pr(我方 7-card final hand strength > 对手随机 hole 7-card final hand strength)`，Monte Carlo 联合采样 over (对手 hole, 未发公牌)，对手 hole uniform over remaining unknown cards（即排除我方 hole + 当前 board）。**EHS²** = `E[EHS_at_river² \| current_state]`，**outer** 枚举未发公牌（确定性，无 RNG，详见 D-227），每条 rollout 在补完的 river 状态下计算 inner EHS 然后平方求均值。river 状态下 outer rollout = 0，退化为 `inner_EHS²`（inner EHS 仍走 Monte Carlo over 对手 hole）。 |
| D-224 | 特征数值范围与 NaN 处理 | EHS / EHS² ∈ [0.0, 1.0]；OCHS 每维 ∈ [0.0, 1.0]。任何 NaN / Inf 出现视为 P0 阻塞 bug（继承 stage 1 D-026 "禁浮点" 精神在 cluster 路径的等价物：浮点允许，但只允许 finite）。`MonteCarloEquity::compute(...)` 返回 `f64`，调用方在写入 bucket table 前必须断言 finite。 |
| D-225 | equity 离散化前的浮点边界 | clustering / equity 计算允许 `f32` / `f64`；写入 mmap bucket table 时 centroid 量化到 `u8`（D-241），bucket id 量化到 `u32`。运行时映射热路径（`abstraction::map`）只读 `u32` bucket id，禁止浮点（D-252）。 |
| D-226 | hand-vs-range equity 接口 | 阶段 2 仅实现 `equity(hole, board, rng)`（hand-vs-uniform-random-hole）；`equity(hole, board, opp_range, rng)` range-aware 版接口预留但 stage 2 不实现，留 stage 4 决策。 |
| D-227 | EHS² 计算 rollout 数（outer enumeration） | **采样口径**：outer 是 "已知我方 hole + 当前 board" 视角下未发**公共牌**枚举；对手 hole 不在 outer 维度，而在 inner equity 内部 Monte Carlo（uniform over remaining cards 排除我方 hole + 完整 board）。**rollout 数**：river 状态 outer = 0 rollout（无未发公共牌），EHS² 退化为 `inner_EHS²`；turn 状态 outer = **46 张**未发 river 卡全枚举（52 - 2 hole - 4 board）；flop 状态 outer = **`C(47, 2) = 1081` 个 (turn, river) 无序对**全枚举（52 - 2 hole - 3 board = 47 张未发，选 2）。outer 全部确定性枚举（无 RNG），inner equity 在每个 outer 评估点走 Monte Carlo（消耗 RngSource，默认 D-220 iter）。**flop 1081 < 默认 inner iter 10000 不可比**——两者维度不同：outer 是确定性枚举数，inner iter 是每个 outer 点上的 Monte Carlo 样本数，总评估次数 ≈ outer × inner。 |

---

## 4. Clustering（D-230..D-239）

| 编号 | 决策项 | 选定值 |
|---|---|---|
| D-230 | postflop clustering 算法 | **k-means + L2 距离**（在特征空间 R⁹ = EHS² ⊕ OCHS⁸ 上）。**不**直接使用 EMD 距离作为 k-means 内部度量（EMD 在 R⁹ 上等价于加权 L1，对 9 维向量收益不明显且收敛慢）。**bucket 间距下限**（D-233）使用 EMD 测度（评估 bucket 之间整体 hole-set 分布距离），与 k-means 内部 L2 距离分离。等价表述："聚类用 L2，验收用 EMD"。 |
| D-231 | k-means 初始化 | **k-means++**（Arthur & Vassilvitskii 2007）+ 显式 `RngSource` 注入。任何 `rand::thread_rng()` 隐式调用是 stage 2 P0 阻塞 bug（继承 stage 1 D-027 / D-050）。 |
| D-232 | k-means 收敛门槛 | `max_iter = 100` 与 `centroid_shift_l_inf ≤ 1e-4`（任意一个先满足即停）二者并集为 OR 收敛判据。`centroid_shift_l_inf` = max over centroids of max over dimensions of `\|c_new - c_old\|`。max_iter=100 确保最坏情况下耗时可控（500 bucket × 9 维 × 100 iter ≈ 可控）。 |
| D-233 | bucket 间 EMD 阈值 `T_emd` | **`T_emd ≥ 0.02`**（衡量相邻 bucket id 间 all-in equity 分布的 1D EMD；分布在 [0,1] 区间）。"相邻" = bucket id `(k, k+1)`。每条街 500 bucket → 499 对相邻；任一对 EMD < 0.02 视为聚类质量不足，回归到 [测试] 指出聚类未达验收 → [实现] 重新调参。 |
| D-234 | EMD 距离计算（1D） | 1D EMD 在 [0, 1] 区间用 sorted CDF 差分积分计算，O(n log n) sort + O(n) 累加。所有 EMD 计算路径走同一函数 `emd_1d_unit_interval(samples_a, samples_b) -> f64`，确保 byte-equal。 |
| D-235 | k-means 内部确定性 | 同 seed clustering 重复 10 次 bucket centroid byte-equal。**k-means++ 抽样**：浮点距离平方 `d2[i]` 不可直接 `as u64`（特征 ∈ [0,1]⁹ 时 d2 ∈ [0, 9]，转 u64 会截断到 0..9 严重扭曲分布）。确定性流程：① **量化** `d2_q[i] = (d2[i].clamp(0.0, D2_MAX) / D2_MAX * (1u64 << 40) as f64) as u64`，其中 `D2_MAX = 9.0`（特征上限：9 维 [0,1] 区间，d2 上限 9）；量化后 `d2_q[i] ∈ [0, 2^40]`。② **累积** `cum_q[i] = sum_{j ≤ i} d2_q[j]`（u64 安全：N ≤ 10000 个候选点 × 2^40 上限 ≈ 2^54，远低于 u64 溢出 2^64）。③ **零和 fallback**：若 `cum_q[N-1] == 0`（所有未选点 d2 量化后均为 0，极少发生），取**最小 index** 的未选点。④ 否则 sample：`r = rng.next_u64() % cum_q[N-1]`，二分查找最小 i 使得 `cum_q[i] > r`。**k-means 重分配 tie-break**：数据点到多个 cluster 距离严格相等时取小 cluster id（确定性 tie-break）。 |
| D-236 | k-means 失败处理 | 若收敛后某 cluster 为空（k-means++ 极少见但非 0 概率）：从最大 cluster 中按 L2 距离最远点切出，保证 0 空 bucket（验收硬条件，validation §3 字面）。该 split 路径需 RngSource tie-break：距离严格相等时取最小 sample id。 |
| D-236b | 训练完成后 bucket 重编号 | k-means 输出的 cluster id 由初始化顺序决定，**不天然具备强度顺序**。训练完成后必须按 bucket 内 EHS 中位数升序重编号 cluster id（**0 = 最弱 / N-1 = 最强**），重编号后的 lookup table 与 centroid data 按新 id 顺序写入 mmap。**tie-break**：① EHS 中位数严格相等时按 centroid 向量字典序（u8 quantized 后的字节序，D-241）；② centroid 字节序也相等时按旧 cluster id 升序。该步骤是 D-233 "相邻 bucket EMD ≥ T_emd" 与 validation §3 "bucket id ↔ EHS 中位数单调一致" 同时成立的前提；任何 [实现] 跳过 D-236b 直接写 bucket table 都会让 [测试] EMD / 单调性断言批量 fail。重编号是最后一步，发生在 D-243 BLAKE3 trailer 计算之前。 |
| D-237 | 训练 RngSource seed 编码 | bucket table 训练 seed 是 `u64`，写入 `BucketTable.metadata.training_seed`。任何同 `(BucketConfig, training_seed, feature_set_id)` 组合训练出的 bucket table 必须 BLAKE3 byte-equal（D-243）。 |
| D-238 | 多街训练顺序 | flop / turn / river 三条街**独立训练**（不依赖彼此 bucket id），可并行。每条街用独立 RngSource fork（`stream_id = 0/1/2`）保证跨并行执行 byte-equal（继承 stage 1 D-054 多线程一致性精神）。 |
| D-239 | preflop 169 不进 clustering | preflop 169 是组合数学 lossless 等价类（D-217），**不**经 k-means。preflop bucket id 直接 = `hand_class_169`（0..169）。bucket table 中 preflop 段：lookup table `[u32; 1326]`（每个 hole canonical id → 0..168 bucket id），**不存** cluster centroid（lossless 无需）。bucket count 固定 169，**header 不显式存 preflop bucket count 字段**（reader 直接返回常量 169）；与 D-244 header 中的 `flop_count / turn_count / river_count` 三字段无关——后者只描述 postflop 三条街的 k-means 输出 bucket 数。 |

---

## 5. Bucket table 文件格式（D-240..D-249）

| 编号 | 决策项 | 选定值 |
|---|---|---|
| D-240 | magic bytes + schema_version | 文件 header 前 16 字节：`magic: [u8; 8] = b"PLBKT\0\0\0"`（5 字节 ASCII + 3 字节 zero pad）+ `schema_version: u32 = 1`（little-endian）+ `feature_set_id: u32 = 1`（little-endian）。`feature_set_id = 1` 对应 D-221 EHS² + OCHS(N=8) 默认组合；后续特征组合（如加 histogram）bump 到 2。 |
| D-241 | centroid 量化 | **u8 quantized（默认）**：每维独立 min/max 量化到 [0, 255]，min/max 作为 metadata 存（每维 2× f32 = 8 字节）。读取时反量化：`x = min + (q / 255.0) * (max - min)`。f32 raw 备选不在 stage 2 默认；feature_set_id 不区分量化方式（量化 vs raw 走 schema_version bump，而非 feature_set_id）。**理由**：u8 量化让跨架构 byte-equal 不依赖 IEEE-754 严格一致；stage 1 D-051 / D-052 跨架构现状下，u8 量化是最稳定的选择。量化误差 ≤ 0.4%（1/255），远低于 D-233 EMD 阈值 0.02，对 bucket 边界质量无显著影响。 |
| D-242 | 文件路径 + 命名 + 分发渠道 | 默认输出：`artifacts/bucket_table_{git_short_hash}_{config_hash}.bin`。`git_short_hash` = 训练 commit 的 git short hash（7 字符），`config_hash` = `BLAKE3(BucketConfig + feature_set_id + training_seed)` 前 8 字符。**分发渠道**：stage 2 F3 决定具体渠道（GitHub release artifact 优先 / git LFS 备选）；A0 仅锁 "**不进 git history**" + "命名包含 git hash 与 config hash 以便审计"。 |
| D-243 | BLAKE3 自校验 | BLAKE3 hash 计算范围：**文件全体除最后 32 字节** = `[file[0..len-32]]`。最后 32 字节存 BLAKE3 hash 本身。`BucketTable::open(path)` **eager 校验**：读 mmap → 计算 hash → 比对 → 不匹配返回 `BucketTableError::Corrupted`。eager 校验的代价 < 全文件 mmap 读一遍（500/500/500 配置下 bucket table ~10MB，BLAKE3 ~3GB/s 单核约 3ms，可接受）。 |
| D-244 | 文件总体 layout | 顺序：`[magic(8) + schema_version(4) + feature_set_id(4)] [BucketConfig.flop(u32) + turn(u32) + river(u32)] [training_seed(u64)] [centroid_metadata(min/max per dim per street)] [centroid_data(u8 quantized)] [lookup_table(canonical_id → bucket_id mapping per street)] [BLAKE3(32)]`。详细字段顺序由 A0 → A1 文档化（API §4），任何字段顺序调整必须 bump `schema_version`。 |
| D-245 | preflop 段在 bucket table 中的存在性 | bucket table **包含 preflop 段**（即使 preflop bucket 是组合 lossless 169），方便 mmap 单一 artifact 加载完整抽象。preflop 段 lookup table = `[u32; 1326]`（每个 hole canonical id → 0..168），无 centroid（D-239）。 |
| D-246 | bucket table v1 → v2 兼容性 | v1 reader **必须显式拒绝** v2 文件（schema_version > 1）并返回 `BucketTableError::SchemaMismatch { expected: 1, got: 2 }`。v2 reader 可选支持 v1 文件（向后兼容升级路径）；A0 阶段不要求 v2 reader 实现，只锁定 v1 reader 的拒绝路径。继承 stage 1 D-062 schema 兼容精神。 |
| D-247 | mmap 加载错误路径 | 5 类错误（继承 validation §5 字面；与 stage 1 §F1 错误路径同型）：`FileNotFound { path }` / `SchemaMismatch { expected, got }` / `FeatureSetMismatch { expected, got }` / `Corrupted { offset, reason }`（含 BLAKE3 不匹配） / `SizeMismatch { expected, got }`（mmap 边界 / 截断）。错误消息使用 `&'static str` 或 `String`（与 stage 1 `RuleError` / `HistoryError` 同型）。 |
| D-248 | bucket table 文件不进 git history | `artifacts/` 加入 `.gitignore`（D-251）。stage 2 commit 不附带 bucket table 二进制；training 由 `tools/train_bucket_table.rs` 在本地 / CI / release pipeline 运行，artifact 通过 release / git LFS 分发。继承 stage 1 `.venv-pokerkit/` gitignore 精神。 |
| D-249 | Python 跨语言读取 | `tools/bucket_table_reader.py` 用纯 Python（无 protoc / mmap C 扩展依赖）读 D-244 文件格式，至少能解码 magic / schema_version / feature_set_id / BucketConfig / preflop lookup / 任意 1k 个 postflop canonical_id → bucket_id。继承 stage 1 `tools/history_reader.py` 同型（minimal proto3 decoder 风格）。 |

---

## 6. Crate / 模块 / Cargo.toml（D-250..D-259）

| 编号 | 决策项 | 选定值 |
|---|---|---|
| D-250 | 是否引入 `ndarray` / 其它 numerics crate | **不引入**。clustering / EMD / equity 全部用 `Vec<f32>` / `Vec<f64>` / `Vec<u8>` + 手工索引实现。理由：① 外部 numerics crate 浮点行为可能跨版本漂移破 clustering determinism（D-235）；② stage 2 特征维度 ≤ 9，手工实现性能足够；③ 减少 dependency surface 降低 cargo audit 噪声。memmap2 可引入（mmap 加载是不可避免的系统接口）。 |
| D-251 | `artifacts/` 目录 + `.gitignore` | `artifacts/` 加入 `.gitignore`；目录由 `tools/train_bucket_table.rs` 按需创建。bucket table mmap artifact 严格不进 git history（D-248）。 |
| D-252 | `abstraction::map` 子模块 `clippy::float_arithmetic` | 子模块 root 文件（`src/abstraction/map/mod.rs`）顶部加 `#![deny(clippy::float_arithmetic)]` 内部属性。Cargo.toml `[lints]` 不能 per-module 配置 lint，所以走 inner attribute 路径。该子模块所有代码必须能通过 `cargo clippy --all-targets -- -D warnings -D clippy::float_arithmetic`；其它 abstraction 子模块（`cluster` / `equity` / `feature`）不强制此 lint。 |
| D-253 | 模块导出粒度 | `src/abstraction/mod.rs` re-export：`ActionAbstraction` / `DefaultActionAbstraction` / `AbstractAction` / `AbstractActionSet` / `ActionAbstractionConfig` / `InfoAbstraction` / `InfoSetId` / `PreflopLossless169` / `PostflopBucketAbstraction` / `EquityCalculator` / `MonteCarloEquity` / `BucketTable` / `BucketConfig` / `BucketTableError`。顶层 `lib.rs` 追加 `pub mod abstraction;` 与具体类型 re-export（详见 `pluribus_stage2_api.md` §6）。 |
| D-254 | 内部子模块隔离 | `abstraction::cluster` / `abstraction::equity` / `abstraction::feature` 内部类型不 re-export 到顶层；只通过 trait 接口暴露。运行时映射热路径只走 `abstraction::map` 子模块，任何 `cluster` / `equity` 调用都是 offline training path（CLI / 测试）。 |
| D-255 | `Cargo.toml` 新增 dependencies | stage 2 候选新增（A1 落地）：`memmap2 = "0.9"`（mmap 加载，必需）；`thiserror`（已在 stage 1）继续用于 `BucketTableError`；`blake3`（已在 stage 1）继续用于 D-243 自校验。**不引入**：`ndarray` / `linfa-clustering` / `kmeans` / equity 库（理由见 D-250 / D-230）。 |
| D-256 | dev-dependencies 新增 | stage 2 候选新增（B1 落地）：`tempfile` 用于 `tests/bucket_table_corruption.rs` 写入临时 mmap 文件做 byte flip；`proptest`（已在 stage 1）继续用于 cluster determinism property test。无 stage 2 专属 dev-dep。 |
| D-257 | feature flag | stage 2 不引入 feature flag（继承 stage 1 D-013 精神，仅 `xvalidate` 模块的 PokerKit 依赖通过 feature 隔离）。clustering / mmap / equity 全部默认编译。 |
| D-258 | 性能 SLO 文件位置 | stage 2 SLO 断言追加到 `tests/perf_slo.rs`（继承 stage 1 文件，新增 stage2_* 命名前缀），与 stage 1 5 条 SLO 共存。`#[ignore]` + release profile 触发模式不变。 |
| D-259 | bench harness 文件位置 | stage 2 bench 追加到 `benches/baseline.rs`（继承 stage 1 文件，新增 `abstraction/*` 命名前缀），与 stage 1 5 条 bench 共存。 |

---

## 7. 外部对照（D-260..D-269）

| 编号 | 决策项 | 选定值 |
|---|---|---|
| D-260 | 外部 abstraction 参考选定 | **自洽性优先 + OpenSpiel 轻量对照**。主验收依赖内部不变量（preflop 169 lossless / bucket 内方差 < 0.05 / 相邻 bucket EMD ≥ 0.02 / clustering BLAKE3 byte-equal / 1M mapping determinism）。F3 验收报告**附带** OpenSpiel poker abstractions 公开 bucket 数与 preflop 169 类**对照** sanity check（不要求 bucket 分配 byte-equal，只对照 lossless 信任锚 + bucket 数量级）。Slumbot bucket 数据获取不确定，**不强求**接入；如未来 stage 4 训练时发现 abstraction 质量与公开 bot 显著偏离，追加 D-260-revM 重新评估接入工作量。 |
| D-261 | OpenSpiel 对照口径 | OpenSpiel `python/algorithms/exploitability_descent` 与 `games/universal_poker` 提供的 abstraction：F3 报告对照其 preflop 169 类编号顺序（与 D-217 比对：可能不同顺序但 169 类成员一致），与 5-action / 6-action 默认配置（path.md 字面匹配）。**不**做 postflop bucket 一一对照（OpenSpiel postflop 默认配置与我方 500/500/500 不同，且 bucket 边界本就因 cluster seed 不同而异）。 |
| D-262 | 外部对照失败处理 | 若 OpenSpiel sanity check 暴露 preflop 169 类成员**显著差异**（≥ 1 类不一致），视为 stage 2 P0 bug——169 lossless 是组合数学唯一解，不允许实现差异。bucket 数量 / postflop 边界差异不阻塞，仅在 F3 报告中标注。 |
| D-263 | 外部对照接入时间点 | 不在 stage 2 中段引入；F3 [报告] 起草时由报告者一次性接入对照 sanity 脚本（`tools/external_compare.py`）。stage 2 主线工作（A1..F2）不依赖 OpenSpiel，避免 dependency 引入晚期翻车。 |

---

## 8. 与阶段 1 决策 / API 的边界

| 编号 | 决策项 | 选定值 |
|---|---|---|
| D-270 | 阶段 1 D-NNN 全集 + D-NNN-revM 修订 | **只读继承**。stage 2 [实现] agent 发现 stage 1 决策不够用 → 走 stage 1 `D-NNN-revM` 修订流程（在 `pluribus_stage1_decisions.md` §10 修订历史追加），**不允许**直接在本文档覆盖 stage 1 决策。 |
| D-271 | 阶段 1 API-NNN 全集 + API-NNN-revM 修订 | **只读继承**。stage 2 [实现] agent 发现 stage 1 API 签名不够用 → 走 stage 1 `API-NNN-revM` 流程修订 `pluribus_stage1_api.md`，**不允许** stage 2 [实现] agent 顺手改 stage 1 API。 |
| D-272 | stage 1 全套测试在 stage 2 commit 上不允许回归 | stage 2 任何 commit 必须保持 `stage1-v1.0` tag 上的 `cargo test`（默认 104 active / 19 ignored / 0 failed）+ `cargo test --release -- --ignored`（19/19 全绿）+ `cargo fmt --all --check` / `cargo clippy --all-targets -- -D warnings` / `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 全绿。CI 必须把 stage 1 测试纳入 stage 2 PR check（继承 stage 1 §F3 出口检查清单）。 |
| D-273 | 浮点边界继承 + 扩展 | stage 1 D-026 "规则引擎 / 评估器 / hand history / 抽象映射全程整数；任何 PR 引入 `f32`/`f64` 必须 reject" 中 "**抽象映射**" 在 stage 2 收紧为 "`abstraction::map` 子模块 + 运行时映射热路径"。clustering / equity 离线训练路径**允许**浮点（D-225）；运行时只读 mmap 整数 bucket id（D-252）。任何 PR 引入浮点到 `abstraction::map` 子模块必须 reject（继承 stage 1 D-026 精神）。 |
| D-274 | RngSource 显式注入继承 | stage 1 D-027 / D-050 "禁全局 rng / 显式 RngSource" 在 stage 2 全部 clustering / Monte Carlo / k-means++ 路径全部继承。任何 `rand::thread_rng()` / `OsRng` 调用是 stage 2 P0 阻塞 bug。`MonteCarloEquity` / `KMeansClusterer` / `OchsClusterer` 接口都必须接受 `&mut dyn RngSource`。 |
| D-275 | `unsafe_code = "forbid"` 继承 | stage 1 `Cargo.toml [lints.rust] unsafe_code = "forbid"` 继承到 stage 2。`memmap2` 内部使用 unsafe 但通过 crate 边界封装，stage 2 代码不直接写 `unsafe { ... }`。任何 stage 2 PR 引入 `unsafe` 必须 reject。 |
| D-276 | `HandHistory.schema_version` 不被 stage 2 修改 | stage 2 不动 hand history 序列化（stage 1 锁定 schema_version=1）。stage 2 引入新的 `BucketTable.schema_version`（D-240），与 hand history schema 完全独立；后者不 bump。 |

---

## 9. 性能 SLO（最终目标，E2 后达到）

| 编号 | 决策项 | 选定值 |
|---|---|---|
| D-280 | 抽象映射运行时吞吐 | 单线程 `≥ 100,000 mapping/s`（path.md §阶段 2 字面）。`(GameState, hole) → InfoSet id` 全路径。 |
| D-281 | bucket lookup latency | mmap 命中路径 `(street, board_canonical, hole_canonical) → bucket_id` `P95 ≤ 10 μs` 单次查表（`tests/perf_slo.rs::stage2_bucket_lookup_latency`）。 |
| D-282 | equity Monte Carlo 离线吞吐 | 默认 10k iter / hand → `≥ 1,000 hand/s` 单线程（`tests/perf_slo.rs::stage2_equity_monte_carlo_throughput`）。仅用于 clustering 训练；运行时映射禁止触发 Monte Carlo（D-225）。 |
| D-283 | clustering 训练总时间 | flop 500 + turn 500 + river 500 全套 clustering 在单线程 8 核 host 上 **`≤ 12 小时`**（"一次过夜跑出 bucket table"）。该 SLO 不阻塞 stage 2 出口（仅作为 D-280..D-282 之外的工程口径），违反时由 [实现] 评估并行加速或减少 OCHS opponent cluster 抽样数。 |
| D-284 | bench 工具 | criterion（继承 stage 1 D-094）。CI 短跑 30 秒内、夜间全量。stage 2 新增 bench 追加到 `benches/baseline.rs`（D-259）。 |

---

## 10. 已知未决项（不阻塞 A1）

以下事项目前未做最终决策，留待后续步骤再确认：

- **D-202 配置序列化**：是否引入 TOML / JSON 反序列化层供 CFR 训练 driver 加载非默认 raise size 配置 — 由 stage 4 决定（届时再决定是否新增 D-202-revM）。
- **D-226 hand-vs-range equity**：range-aware equity 接口实现 — 由 stage 4 决定（CFR 训练时若需要才接入）。
- **D-246 v2 reader**：bucket table v2 reader 的具体升级路径 — 由 stage 2 schema 第一次 bump 时决定。
- **跨架构 1M 一致性**：bucket table 在 x86_64 vs ARM64 上 byte-equal — aspirational，与 stage 1 D-052 同型（仅 32-seed baseline 强制；1M 留 carve-out）。
- **D-260-revM**：若 stage 4 训练发现 abstraction 质量与公开 bot 显著偏离 → 重新评估 Slumbot bucket 接入。

---

## 11. 决策修改流程

继承阶段 1 §10 D-100..D-103 流程：

- 任何决策修改必须在本文档以追加 `D-NNN-revM` 条目的形式记录，**不删除原条目**
- 修改若影响 `BucketTable.schema_version` 兼容性，必须 bump `schema_version` 并提供升级器（继承 D-101 精神，`BucketTable` 替代 `HandHistory`）
- 修改若影响 API 签名，必须同步修改 `pluribus_stage2_api.md`
- 决策修改 PR 必须经过决策者 review 后合入

---

### 修订历史

阶段 2 实施过程中的决策修订（含 carry forward 阶段 1 处理政策）按时间线追加到本节，遵循阶段 1 §10 修订历史 同样 "追加不删" 约定。

阶段 2 §修订历史 首条新增项必须显式 carry forward 阶段 1 提炼的处理政策清单（与 `pluribus_stage2_workflow.md` §修订历史 首条同步）：

- §B-rev1 §3：[实现] 步骤越界改测试 → 当 commit 显式追认；不静默扩散到下一步。
- §B-rev1 §4：每个步骤关闭后必须有一笔 `docs(CLAUDE.md): X 完成后状态同步` 把仓库状态、出口数据、修订历史索引补齐。
- §C-rev1：零产品代码改动的 [实现] 步骤同样需要书面 closure；测试规模扩展属于 [测试] 角色，即使 "只是改个常数"。
- §D-rev0 §1–§3：`D-NNN-revM` 翻语义时主动评估测试反弹；carve-out 范围最小化；测试文件改名 / 删除 / 大幅重写仍属 [测试] 范畴。
- §F-rev1：错误前移到序列化解码阶段（如 `from_proto` / `BucketTable::open`）是 [实现] agent 单点不变量收口的优选模式。

（本节首条由 A0 [决策] 关闭后填入，记录 D-200..D-283 锁定数值与 `pluribus_stage2_validation.md` §修订历史首条同步。）

---

## 12. 与决策文档 / API 文档的对应关系

| 本文档段落 | 关联 API 段落（`pluribus_stage2_api.md`） | 关联 validation 段落（`pluribus_stage2_validation.md`） |
|---|---|---|
| §1 Action abstraction（D-200..D-209） | §1 Action abstraction（API-200..） | §1 Action abstraction |
| §2 Information abstraction（D-210..D-219） | §2 Information abstraction（API-210..） | §2 preflop 169 lossless / §3 postflop bucket |
| §3 Equity & 特征（D-220..D-229） | §3 Equity Calculator（API-220..） | §3 postflop bucket（特征） |
| §4 Clustering（D-230..D-239） | （内部模块，无公开 API；通过 `tools/train_bucket_table.rs` 入口） | §3 postflop bucket（聚类质量） |
| §5 Bucket table 文件格式（D-240..D-249） | §4 Bucket Table（API-240..） | §5 Bucket lookup table 持久化与 schema |
| §6 Crate / 模块 / Cargo.toml（D-250..D-259） | §6 模块导出（API-250..） | — |
| §7 外部对照（D-260..D-269） | （F3 报告对照脚本，无 API） | §通过标准 末段 + §参考资料 |
| §8 阶段 1 边界（D-270..D-279） | §7 与 stage 1 类型桥接（API-270..） | §7 与阶段 1 的不变量边界 |
| §9 性能 SLO（D-280..D-289） | （perf_slo 测试断言） | §4 抽象映射性能 SLO + §8 SLO 汇总 |

---

## 参考资料

- 阶段 2 验收门槛：`pluribus_stage2_validation.md`
- 阶段 2 实施流程：`pluribus_stage2_workflow.md`
- 阶段 2 API 契约：`pluribus_stage2_api.md`
- 阶段 1 决策记录（只读继承）：`pluribus_stage1_decisions.md`
- 阶段 1 API 契约（只读继承）：`pluribus_stage1_api.md`
- 阶段 1 验收报告：`pluribus_stage1_report.md`
- 整体路径：`pluribus_path.md`
- Pluribus 主论文 §"Action and information abstraction"：https://noambrown.github.io/papers/19-Science-Superhuman.pdf
- Ganzfried & Sandholm, "Potential-Aware Imperfect-Recall Abstraction with Earth Mover's Distance"
- Brown & Sandholm, "Strategy-Based Warm Starting for Real-Time Hold'em Poker"（OCHS N=8 来源）
- Arthur & Vassilvitskii, "k-means++: The Advantages of Careful Seeding"（D-231 算法来源）
- OpenSpiel poker abstractions（D-260 sanity 对照）：https://github.com/google-deepmind/open_spiel
- memmap2：https://github.com/RazrFalcon/memmap2-rs
