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
| D-206 | `Bet/Raise(x×pot)` 与显式 `AllIn` 去重 | 若 D-205 fallback 后 `Bet/Raise(0.5×pot)` 与 `Bet/Raise(1.0×pot)` 折叠到相同 `to` 值（典型场景：短码），保留 ratio_label 较小的一份；若进一步与 `AllIn` 折叠（同 `to`），保留一份且枚举 tag 选 `AllIn`（保证抽象动作集合 size 单调收缩，下游 InfoSet 编码不混乱）。**fold-collapsed `AllIn` 的 betting_state 语义**：经 D-206 折叠落到 `AbstractAction::AllIn { to }` 的动作，其 `to_concrete() → Action::AllIn` 经 stage 1 状态机归一化（D-033 / D-033-rev1）后会再走回 `Bet/Raise { to = committed + stack }` 路径——D-212 `betting_state` 状态转移**与原 `Bet/Raise(x×pot)` 等价**（即如果原本是 1.0×pot Raise 折叠到 AllIn，状态从 `FacingBetNoRaise` → `FacingRaise1` 与未折叠路径一致）。F10 review 担忧的 "tag 是 AllIn 但状态机走 Raise" 不破坏 InfoSet 编码：betting_state 由实际下注金额 + voluntary 性决定，不由 abstract tag 决定。 |
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
| D-217 | preflop 169 等价类编号（A0 锁定 closed-form 公式） | `hand_class_169 ∈ 0..169`，从 canonical `(rank_high, rank_low, suited)` 三元组用 closed-form 公式直接计算。详见下方 **D-217 详解**——A0 锁定公式 + 12 条边界锚点表，B1 [测试] 在 [实现] 之前即可基于本表写完整 1326 → 169 枚举断言（test-first ground truth），不依赖 [实现] 落地具体数表。 |
| D-218 | canonical hand / board id | hole canonical：22 → `(rank=2, rank=2, suited=false)` → 唯一 id；suited 与 offsuit 各异。board canonical：考虑花色对称性等价类，按 rank 多重集 + suit 模式 canonicalize；具体算法 A1 落地。canonical id 是 `u32`，足够覆盖（5-card board canonical 上限 ~134k；7-card 上限 ~1.5M，远在 u32 内）。 |
| D-219 | postflop 不依赖 preflop key 的隔离原则 | postflop bucket 仅依赖 `(street, board, hole)`（特征只看牌力 / 公牌结构），**不嵌入** position / stack / betting_state。preflop key 的位置 / stack / betting_state 信息留在 `InfoSetId` 复合字段里（D-215 / D-216），不渗入 postflop bucket。理由：postflop bucket 表是 cluster 输出，跨手通用；与博弈树位置无关，便于阶段 6 实时搜索复用同一 mmap 表。 |

### D-217 详解

```rust
fn hand_class_169(rank_a: Rank, rank_b: Rank, suited: bool) -> u8 {
    // 排序：high ≥ low（rank 数值越大越强，继承 stage 1 D-001 Rank 枚举：
    // Two = 0, Three = 1, ..., Ace = 12）
    let (high, low) = if (rank_a as u8) >= (rank_b as u8) {
        (rank_a as u8, rank_b as u8)
    } else {
        (rank_b as u8, rank_a as u8)
    };
    if high == low {
        // Pocket pair：class id = rank 数值（22→0, 33→1, ..., AA→12）
        high                              // ∈ 0..13
    } else if suited {
        // Suited：lex order on (high, low) ascending
        13 + high * (high - 1) / 2 + low  // ∈ 13..91
    } else {
        // Offsuit：同 suited 顺序 + offset 78
        91 + high * (high - 1) / 2 + low  // ∈ 91..169
    }
}
```

**段长校验**：13 个 pocket pair + 78 个 suited（`C(13, 2) = 78`）+ 78 个 offsuit = **169 类**总计 ✓。**hole 计数**（D-217 每类 hole 组合数 × 类数 = 1326 起手牌）：每 pocket pair 6 组合（`C(4, 2)` 花色对）、每 suited 4 组合（4 花色）、每 offsuit 12 组合（`4 × 3` 花色对），总和 `13×6 + 78×4 + 78×12 = 78 + 312 + 936 = 1326` ✓。

**边界锚点表**（B1 [测试] `tests/preflop_169.rs` 必须断言以下 12 条，作为公式正确性的最小验证集；其余 157 条由公式直接生成枚举断言）：

| Hand | high | low | suited | class id |
|---|---|---|---|---|
| 22 | 0 (Two) | 0 | n/a | **0** |
| 33 | 1 | 1 | n/a | 1 |
| AA | 12 (Ace) | 12 | n/a | **12** |
| 32s | 1 | 0 | true | **13** |
| 42s | 2 | 0 | true | 14 |
| 43s | 2 | 1 | true | 15 |
| 52s | 3 | 0 | true | 16 |
| AKs | 12 | 11 | true | **90** |
| 32o | 1 | 0 | false | **91** |
| 42o | 2 | 0 | false | 92 |
| 43o | 2 | 1 | false | 93 |
| AKo | 12 | 11 | false | **168** |

**与 D-218 canonical hole id 的关系**：D-218 的 hole canonical id（0..1326）→ D-217 `hand_class_169`（0..169）是多对一映射；逆向映射（`hand_class_169` → 代表性 canonical hole）由 [实现] 选取，不在本决策范围。

**与 BucketTable preflop lookup table 的关系**：D-244 §⑫ preflop 段 `[u32; 1326]` 每个 entry = 该 hole canonical id 的 `hand_class_169` 值（0..168），`BucketTable::lookup(StreetTag::Preflop, _board_canonical_id, hole_canonical_id)` 返回该值。

**与 D-260 / D-261 OpenSpiel 对照的关系**：D-261 sanity check "可能不同顺序但 169 类成员一致"——本 closed-form 公式让 [测试] 与 OpenSpiel 编号无关地枚举我方 169 类成员（13 pair + 78 suited + 78 offsuit），仅做集合相等比对（D-262 P0 阻塞条件）。我方编号顺序锁定后**不**与 OpenSpiel 顺序对齐——CFR 训练只看 InfoSetId byte-equal，不看跨实现编号顺序一致。

---

## 3. Equity & 特征（D-220..D-229）

| 编号 | 决策项 | 选定值 |
|---|---|---|
| D-220 | equity Monte Carlo 默认 iter 数 | **10,000 iter / hand**（`MonteCarloEquity::default()`）。CI 短测试可降到 1,000；clustering 训练路径必须用默认 10k。Monte Carlo 标准误差在 10k iter 下约 `sqrt(0.25/10000) ≈ 0.005`（0.5%）。 |
| D-220a | equity 反对称容差（pairwise 路径）| **反对称只在 pairwise equity 上成立**：`equity_vs_hand(A, B, board, rng) + equity_vs_hand(B, A, board, rng) = 1`（modulo ties counted as 0.5/0.5）。**注意**：`equity(hole, board, rng)`（hand-vs-uniform-random-hole，EHS 路径）**不**满足反对称——`equity(A, board) + equity(B, board) ≠ 1`，因为对手是 uniform random over remaining cards 不是固定的 B。EQ-001 反对称断言必须用 `equity_vs_hand` 接口（API §3）。**容差**：① **postflop**（`board.len() ≥ 3`）：outer 枚举全部未发公共牌（river: 0 rollout / turn: 44 张未发 / flop: `C(45, 2) = 990` 个 (turn, river) 对，注意此处比 D-227 EHS² 少 2 张，因为 opp_hole 占 2 张），无 RNG 消费，确定性；antisymmetry 在 IEEE-754 reorder 容忍下 `\|sum - 1\| ≤ 1e-9`。② **preflop**（`board.len() == 0`）：outer Monte Carlo over `C(48, 5) = 1,712,304` 个 (flop, turn, river) 组合，消费 RngSource；同 RngSource state（fresh sub-stream from D-228 same op_id + sub_index）下 `\|sum - 1\| ≤ 1e-9`，不同 sub-stream 下 Monte Carlo 噪声容忍 `\|sum - 1\| ≤ 0.005`（`iter = 10_000`）/ `≤ 0.02`（`iter = 1_000`）；标准误差近似 `sqrt(0.25 / iter)`。容差用于 `tests/equity_self_consistency.rs` 反对称断言；postflop 不应触发宽 tolerance 路径。 |
| D-221 | 默认特征组合（postflop clustering） | **EHS² + OCHS** 双特征 concat，作为 k-means 输入向量。EHS² 标量 1 维；OCHS 向量 N=8 维（D-222）；总输入维度 = 9。distribution-aware histogram **不进默认**（path.md "可选" 字面），仅作为 stage 4 消融对照接入。 |
| D-222 | OCHS opponent cluster 数 | **N = 8**（Brown & Sandholm 2014 "Strategy-Based Warm Starting for Real-Time Hold'em Poker" 论文使用值；与 Pluribus 实战一致）。8 个 opponent cluster 在 stage 2 启动时通过 preflop 169 上的 EHS 一维 k-means 自训练（同 RngSource seed → 8 cluster centroid byte-equal）。N 配置接口预留为 `OchsConfig { n_opp_clusters: u8 }`，但 stage 2 只跑 N=8。 |
| D-223 | EHS / EHS² 计算路径 | **EHS** = `Pr(我方 7-card final hand strength > 对手随机 hole 7-card final hand strength)`，Monte Carlo 联合采样 over (对手 hole, 未发公牌)，对手 hole uniform over remaining unknown cards（即排除我方 hole + 当前 board）。**EHS²** = `E[EHS_at_river² \| current_state]`，**outer** 枚举未发公牌（确定性，无 RNG，详见 D-227），每条 rollout 在补完的 river 状态下计算 inner EHS 然后平方求均值。river 状态下 outer rollout = 0，退化为 `inner_EHS²`（inner EHS 仍走 Monte Carlo over 对手 hole）。 |
| D-224 | 特征数值范围与 NaN 处理 | EHS / EHS² ∈ [0.0, 1.0]；OCHS 每维 ∈ [0.0, 1.0]。任何 NaN / Inf 出现视为 P0 阻塞 bug（继承 stage 1 D-026 "禁浮点" 精神在 cluster 路径的等价物：浮点允许，但只允许 finite）。`MonteCarloEquity::compute(...)` 返回 `f64`，调用方在写入 bucket table 前必须断言 finite。 |
| D-225 | equity 离散化前的浮点边界 | clustering / equity 计算允许 `f32` / `f64`；写入 mmap bucket table 时 centroid 量化到 `u8`（D-241），bucket id 量化到 `u32`。运行时映射热路径（`abstraction::map`）只读 `u32` bucket id，禁止浮点（D-252）。 |
| D-226 | hand-vs-range equity 接口 | 阶段 2 仅实现 `equity(hole, board, rng)`（hand-vs-uniform-random-hole）；`equity(hole, board, opp_range, rng)` range-aware 版接口预留但 stage 2 不实现，留 stage 4 决策。 |
| D-227 | EHS² 计算 rollout 数（outer enumeration） | **采样口径**：outer 是 "已知我方 hole + 当前 board" 视角下未发**公共牌**枚举；对手 hole 不在 outer 维度，而在 inner equity 内部 Monte Carlo（uniform over remaining cards 排除我方 hole + 完整 board）。**rollout 数**：river 状态 outer = 0 rollout（无未发公共牌），EHS² 退化为 `inner_EHS²`；turn 状态 outer = **46 张**未发 river 卡全枚举（52 - 2 hole - 4 board）；flop 状态 outer = **`C(47, 2) = 1081` 个 (turn, river) 无序对**全枚举（52 - 2 hole - 3 board = 47 张未发，选 2）。outer 全部确定性枚举（无 RNG），inner equity 在每个 outer 评估点走 Monte Carlo（消耗 RngSource，默认 D-220 iter；sub-stream seed 派生见 D-228）。**flop 1081 < 默认 inner iter 10000 不可比**——两者维度不同：outer 是确定性枚举数，inner iter 是每个 outer 点上的 Monte Carlo 样本数，总评估次数 ≈ outer × inner。 |
| D-228 | RngSource sub-stream 派生协议（公开 contract，与 stage 1 D-028 同型） | stage 2 多个 RngSource 消费点（k-means++ 初始化 / 空 cluster split / EHS² inner equity / equity / OCHS warmup / 三街并行 fork）必须从单一 `training_seed: u64` 派生独立 sub-stream，**任何 [实现] 偏离视为违反公开 contract**：① **派生函数**：`derive_substream_seed(master_seed: u64, op_id: u32, sub_index: u32) -> u64` 走 SplitMix64 finalizer：`let tag = ((op_id as u64) << 32) \| (sub_index as u64); let mut x = master_seed ^ tag; x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9); x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb); x ^ (x >> 31)`。② **op_id 表**（u32 常量，命名空间按高 16 位分类、低 16 位区分街 / 子操作；新增 op_id 必须 D-228-revM）：`OCHS_WARMUP = 0x0001_0000`、`CLUSTER_MAIN_FLOP = 0x0002_0001` / `_TURN = 0x0002_0002` / `_RIVER = 0x0002_0003`、`KMEANS_PP_INIT_FLOP = 0x0003_0001` / `_TURN = 0x0003_0002` / `_RIVER = 0x0003_0003`、`EMPTY_CLUSTER_SPLIT_FLOP = 0x0004_0001` / `_TURN = 0x0004_0002` / `_RIVER = 0x0004_0003`、`EQUITY_MONTE_CARLO = 0x0005_0000`、`EHS2_INNER_EQUITY_FLOP = 0x0006_0001` / `_TURN = 0x0006_0002` / `_RIVER = 0x0006_0003`、`OCHS_FEATURE_INNER = 0x0007_0000`。③ **sub_index 语义**：iter / outer-enum-index / split-attempt-index 等线性整数；caller 在 op_id 的 doc comment 里固化具体 sub_index 含义。④ **使用方式**：sub_seed → `ChaCha20Rng::from_seed(sub_seed)`（继承 stage 1 D-028 RNG 实例化），不允许直接 `next_u64()` master 后用其 raw bits 当 sub_seed。⑤ **公开性**：派生函数 + op_id 表作为公开 API 在 `abstraction::cluster` 模块导出，便于 [测试] 在 `tests/clustering_determinism.rs` 等测试中独立构造 sub-stream 验证 byte-equal。任何修改必须走 D-228-revM 流程并 bump `BucketTable.schema_version`（因 sub-stream 派生改变会让相同 `(training_seed, BucketConfig)` 输出不同 BLAKE3 trailer，破坏 D-237 byte-equal 不变量）。 |

---

## 4. Clustering（D-230..D-239）

| 编号 | 决策项 | 选定值 |
|---|---|---|
| D-230 | postflop clustering 算法 | **k-means + L2 距离**（在特征空间 R⁹ = EHS² ⊕ OCHS⁸ 上）。**不**直接使用 EMD 距离作为 k-means 内部度量（EMD 在 R⁹ 上等价于加权 L1，对 9 维向量收益不明显且收敛慢）。**bucket 间距下限**（D-233）使用 EMD 测度（评估 bucket 之间整体 hole-set 分布距离），与 k-means 内部 L2 距离分离。等价表述："聚类用 L2，验收用 EMD"。 |
| D-231 | k-means 初始化 | **k-means++**（Arthur & Vassilvitskii 2007）+ 显式 `RngSource` 注入。任何 `rand::thread_rng()` 隐式调用是 stage 2 P0 阻塞 bug（继承 stage 1 D-027 / D-050）。 |
| D-232 | k-means 收敛门槛 | `max_iter = 100` 与 `centroid_shift_l_inf ≤ 1e-4`（任意一个先满足即停）二者并集为 OR 收敛判据。`centroid_shift_l_inf` = max over centroids of max over dimensions of `\|c_new - c_old\|`。max_iter=100 确保最坏情况下耗时可控（500 bucket × 9 维 × 100 iter ≈ 可控）。 |
| D-233 | bucket 间 EMD 阈值 `T_emd` | **`T_emd ≥ 0.02`**（衡量相邻 bucket id 间 all-in equity 分布的 1D EMD；分布在 [0,1] 区间）。"相邻" = bucket id `(k, k+1)`。每条街 500 bucket → 499 对相邻；任一对 EMD < 0.02 视为聚类质量不足，回归到 [测试] 指出聚类未达验收 → [实现] 重新调参。 |
| D-234 | EMD 距离计算（1D） | 1D EMD 在 [0, 1] 区间用 sorted CDF 差分积分计算，O(n log n) sort + O(n) 累加。所有 EMD 计算路径走同一函数 `emd_1d_unit_interval(samples_a, samples_b) -> f64`，确保 byte-equal。 |
| D-235 | k-means 内部确定性 | 同 seed clustering 重复 10 次 bucket centroid byte-equal。**k-means++ 抽样**：浮点距离平方 `d2[i]` 不可直接 `as u64`（特征 ∈ [0,1]⁹ 时 d2 ∈ [0, 9]，转 u64 会截断到 0..9 严重扭曲分布）。确定性流程：① **量化** `d2_q[i] = (d2[i].clamp(0.0, D2_MAX) / D2_MAX * (1u64 << 40) as f64) as u64`，其中 `D2_MAX = 9.0`（特征上限：9 维 [0,1] 区间，d2 上限 9）；量化后 `d2_q[i] ∈ [0, 2^40]`。② **累积** `cum_q[i] = sum_{j ≤ i} d2_q[j]`（u64 安全：**`N ≤ 2_000_000` 候选点上限**——postflop 训练 candidate set 由 [实现] 控制不超过该数；OCHS warmup N=169 远低；候选总和上限 = `2_000_000 × 2^40 ≈ 2^61`，留 8× 缓冲在 u64 内 [F15 修复]）。如未来 stage 4 消融需要 N > 2_000_000，须走 D-235-revM 切换到 `cum_q: u128` 或减小 SCALE。③ **零和 fallback**：若 `cum_q[N-1] == 0`（所有未选点 d2 量化后均为 0，极少发生），取**最小 index** 的未选点。④ 否则 sample：`r = rng.next_u64() % cum_q[N-1]`，二分查找最小 i 使得 `cum_q[i] > r`。**k-means 重分配 tie-break**：数据点到多个 cluster 距离严格相等时取小 cluster id（确定性 tie-break）。 |
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
| D-243 | BLAKE3 自校验 | BLAKE3 hash 计算范围：**文件全体除最后 32 字节** = `[file[0..len-32]]`。最后 32 字节存 BLAKE3 hash 本身。`BucketTable::open(path)` **eager 校验**：读 mmap → 计算 hash → 比对 → 不匹配返回 `BucketTableError::Corrupted`。eager 校验的代价 < 全文件 mmap 读一遍（500/500/500 配置下 bucket table ~10MB，BLAKE3 ~3GB/s 单核约 3ms，可接受）。**schema_version bump 与 BLAKE3 reproducibility 关系**：因 BLAKE3 覆盖整个 file body 含 header layout，任何 `schema_version` bump（含 D-241 量化方式 u8 → f32 切换、D-244 字段顺序调整）会让相同 `(training_seed, BucketConfig, feature_set_id)` 输出不同 BLAKE3，破坏 stage 4+ 把 BLAKE3 写进 commit message / report 后的回退重训练能力。**stage 2 不解决该耦合**——v1 only，无切换问题。如 stage 3+ 需要 schema-independent training-input fingerprint（让 v1 → v2 schema bump 后仍能审计 "是否同 training input 重训练"），届时引入 `training_input_hash: [u8; 32]` 字段（仅覆盖 training_seed + BucketConfig + feature_set_id + sub-stream 派生协议 D-228，不覆盖 file layout）作为 D-243-rev1 的 hook，与 BLAKE3 trailer 共存。F16 review 留作 stage 3 评估项。 |
| D-244 | 文件总体 layout（含变长段绝对偏移表） | header 80 字节定长（8-byte aligned），含变长段的绝对偏移表，让 reader **不依赖前段精确 size 即可寻址**（解决 BT-007 byte flip 在变长段定位失败导致 panic 的结构性缺口）：<br><br>**header（80 bytes，全部 little-endian）**：<br>① `magic: [u8; 8] = b"PLBKT\0\0\0"`（D-240）<br>② `schema_version: u32`（D-240）<br>③ `feature_set_id: u32`（D-240）<br>④ `bucket_count_flop / turn / river: u32 × 3`（D-214；preflop 固定 169 不存）<br>⑤ `n_canonical_flop / turn / river: u32 × 3`（每条街 canonical board id 数量上限；preflop 固定 1326 不存；用于 reader bound check 与 lookup_table size 推导）<br>⑥ `n_dims: u8`（centroid 向量维度，feature_set_id=1 对应 D-221 EHS² + OCHS(N=8) = **9 维**；feature_set_id 改 N 时同步 bump）<br>⑦ `pad: [u8; 7] = 0`（8-byte 对齐填充）<br>⑧ `training_seed: u64`（D-237）<br>⑨ `centroid_metadata_offset: u64`、`centroid_data_offset: u64`、`lookup_table_offset: u64`（三段绝对字节偏移，从文件起始算；reader 用此寻址，不计算前段累积 size；offset 越界 / 不递增 / 不 8-byte 对齐均视为 `BucketTableError::Corrupted`）<br><br>**body（变长，按 header 偏移定位）**：<br>⑩ `centroid_metadata`（始于 ⑨ centroid_metadata_offset）：每条 postflop 街 × `n_dims` × `(min: f32, max: f32)`，共 `3 × n_dims × 8` 字节（feature_set_id=1 时 = 216 字节）<br>⑪ `centroid_data`（始于 ⑨ centroid_data_offset）：每条 postflop 街 × bucket_count(street) × `n_dims × u8` quantized centroid（D-241），按 D-236b 重编号顺序排列<br>⑫ `lookup_table`（始于 ⑨ lookup_table_offset）：preflop `[u32; 1326]`（D-239 / D-245）+ flop `[u32; n_canonical_flop]` + turn `[u32; n_canonical_turn]` + river `[u32; n_canonical_river]`，所有 entry = bucket id<br><br>**trailer（32 bytes）**：<br>⑬ `blake3: [u8; 32]` = BLAKE3(`file_body[..len-32]`)（D-243）<br><br>任何字段顺序调整 / header 字段增删 / 段语义改变必须 bump `schema_version`。reader 实现负责按 ⑨ 偏移表定位三段，不允许 const-bake 段大小推算。 |
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

#### A0 关闭（2026-05-09）

A0 [决策] 关闭，本文档 D-200..D-283（含 D-220a / D-236b / D-228 sub-stream 派生协议）全部锁定数值。同 commit 落地 `docs/pluribus_stage2_api.md` API-200..API-302、`docs/pluribus_stage2_validation.md` §修订历史首条占位补完、`docs/pluribus_stage2_workflow.md` §修订历史首条 carry forward、`CLAUDE.md` 状态翻 "stage 2 A0 closed"。

A0 起步起 review 子 agent 提出 12 处独立 spec drift（编号 F7..F18），通过 5 笔 commit 落地 11 处修正：

| 编号 | 修正主题 | commit |
|---|---|---|
| F7 / F8 / F9 / F17 | InfoSet 编码 + 类型一致性（D-215 统一 64-bit layout / `StreetTag` vs `Street` 隔离 / `BettingState` 5 状态展开 / `position_bucket` 4 bit 支持 2..=9 桌大小） | `3f62842` batch 1 |
| F11 / F13 | RngSource sub-stream 派生协议（D-228 SplitMix64 finalizer + op_id 表）+ bucket table header 80-byte 偏移表（D-244 §⑨ 解决 BT-007 byte flip 变长段定位 panic） | `96e3b9c` batch 2 |
| F14 | D-217 169 hand class closed-form 公式 + 12 条边界锚点表（B1 [测试] 在 [实现] 之前直接基于公式枚举断言） | `1e57942` batch 3 |
| F10 / F15 / F16 | D-206 fold-collapsed `AllIn` 的 `betting_state` 状态转移澄清 / D-235 N ≤ 2_000_000 候选点上限 + 量化 SCALE=2^40 / D-243 schema_version vs BLAKE3 reproducibility 耦合标注（v1 only 不解决，stage 3 hook） | `622204f` batch 4 |
| F18 | D-220a / EQ-001 `equity_vs_hand` pairwise 接口（API §3 trait 新增第三个方法，反对称只在 pairwise 路径成立——`equity(hole, board, rng)` random-opp 数学上不满足反对称） | `9b7085d` batch 5 |
| F12 | 维持不修（理论 P3：feature 精度 ~5e-3 远高于 d2 量化失效阈值 1e-12，工程不触发） | — |

A0 §修订历史 首条 carry forward 阶段 1 处理政策清单（与 `pluribus_stage2_workflow.md` §修订历史 首条同步）：

- §B-rev1 §3：[实现] 步骤越界改测试 → 当 commit 显式追认；不静默扩散到下一步。
- §B-rev1 §4：每个步骤关闭后必须有一笔 `docs(CLAUDE.md): X 完成后状态同步` 把仓库状态、出口数据、修订历史索引补齐。
- §C-rev1：零产品代码改动的 [实现] 步骤同样需要书面 closure；测试规模扩展属于 [测试] 角色，即使 "只是改个常数"。
- §D-rev0 §1–§3：`D-NNN-revM` 翻语义时主动评估测试反弹；carve-out 范围最小化；测试文件改名 / 删除 / 大幅重写仍属 [测试] 范畴。
- §F-rev1：错误前移到序列化解码阶段（如 `from_proto` / `BucketTable::open`）是 [实现] agent 单点不变量收口的优选模式。

A0 角色边界审计：仅修 `docs/` 下 4 份新文档（`pluribus_stage2_decisions.md` / `pluribus_stage2_api.md` 起草 + `pluribus_stage2_validation.md` 占位补完 + `pluribus_stage2_workflow.md` §修订历史同步）+ `CLAUDE.md` 状态翻面；`src/` / `tests/` / `benches/` / `fuzz/` / `tools/` / `proto/` **未修改一行**——A0 [决策] role 0 越界。

下一步：B1 [测试]（`tests/api_signatures.rs` 阶段 2 trait 签名编译断言起步 + `tests/preflop_169.rs` 1326 → 169 完整覆盖 + `tests/equity_self_consistency.rs` 反对称 harness + `tests/scenarios.rs` 阶段 2 版 10–15 个 driving 场景 + `tests/clustering_determinism.rs` / `tests/bucket_quality.rs` harness 骨架）。前置 A1 [实现]（API 骨架代码化 + `unimplemented!()` 占位 + `Cargo.toml` 追加 dev-dep + `abstraction::map` 子模块 `clippy::float_arithmetic` inner attribute）由 A0 → B1 之间 [实现] agent 落地。

#### A0 review 修正 batch 6（2026-05-09，A1 起步前）

A0 关闭后另一轮 review 暴露 9 处独立 spec drift（编号 F19..F27），按 §11 修订流程追加 rev 条目，原条目保留。本 batch 与 stage-2 §A-rev0（workflow 修订历史首条）同 commit 落地，目的：避免 A1 [实现] / B1 [测试] 起步后再回头修订契约。

| 编号 | 严重度 | 修正主题 | 修订条目 |
|---|---|---|---|
| F19 | P0 | postflop bucket lookup table 索引空间不足以表达 (board, hole) 联合映射 | D-216-rev1 + D-218-rev1 + D-244-rev1（API 同步：BT-005-rev1 / lookup 签名） |
| F20 | P0 | `Call` 与 `AllIn` 相同 `to` 时的去重优先级缺失 | D-206-rev1（API 同步：AA-004-rev1） |
| F21 | P1 | postflop `stack_bucket` 沿用 preflop 起手值的来源不可重建 | D-211-rev1（API 同步：InfoAbstraction trait 新增 hand-start snapshot 来源约束） |
| F22 | P1 | terminal `InfoSetId` 行为无法由 `(GameState, hole)` 重建 | API IA-006-rev1（决策侧仅备注，不新建 D-NNN-revM） |
| F23 | P1 | `equity_vs_hand` 重叠返回 NaN 与 D-224 finite invariant 冲突 | D-224-rev1（API 同步：EquityCalculator-rev1 / EQ-002-rev1） |
| F24 | P2 | AA-003 漏写 `min_to` 与 stack 同时触发优先级 | API AA-003-rev1（决策侧 D-205 原文已含优先级，API 同步收紧） |
| F25 | P2 | preflop pairwise equity 反对称 RNG 契约可执行性不足 | D-220a-rev1（API 同步：EQ-001-rev1） |
| F26 | P2 | D-253 顶层 re-export 列表与 API §6 不一致 | D-253-rev1 |
| F27 | P2 | `BetRatio` 量化规则（rounding / 越界 / duplicate）未锁定 | D-202-rev1（API 同步：BetRatio::from_f64 -rev1） |

##### D-216-rev1（2026-05-09，F19）

**背景**：D-216 原文规定 `bucket_id = BucketTable::lookup(street_tag, board_canonical_id, hole_canonical_id)`，但 D-244 原文 lookup_table postflop 段长度仅为 `n_canonical_<street>`（单维 board canonical 数量），无法在同一 board canonical 下区分多个 hole canonical，结构性表达不出 (board, hole) → bucket 映射。

**新规则**：postflop bucket lookup 改用**联合 canonical observation id**：

```text
canonical_observation_id(street, board, hole) -> u32
```

返回 (board, hole) 对在花色对称 + rank 多重集等价类下的唯一 id ∈ `[0, n_canonical_observation(street))`。该函数公开导出（`abstraction::postflop::canonical_observation_id`），作为 `BucketTable::lookup` 的输入计算来源。`board_canonical_id` 与 `hole_canonical_id` 单维 canonical 仍由 D-218-rev1 保留作为子函数（`canonical_board_id` / `canonical_hole_id` on board），但**不再单独驱动** lookup 索引。

`canonical_observation_id` 落地约束（与 D-218-rev1 共生）：① 同一 (board, hole) 在 [实现] 重复调用 byte-equal；② 花色重命名 / 同 rank 内花色置换不改变 id；③ id 紧凑分布（无空洞）；④ id 上限：flop ≤ ~1.3M、turn ≤ ~14M、river ≤ ~123M（保守上界，使用 `(board, hole)` 联合等价类计数，由 [实现] A1 给出实际数）。

**影响**：① D-218 保留单维 canonical 子函数（hole-only / board-only），追加联合 canonical observation id 函数；② D-244 header 字段 `n_canonical_<street>` 语义改为 `n_canonical_observation_<street>`（lookup_table 长度跟随）；③ API §4 `BucketTable::lookup` 签名由 `(street, board_id, hole_id)` 改为 `(street, observation_id)`，与 D-216-rev1 同步——见 API §9 `BT-005-rev1` / lookup 签名修订；④ 不影响 preflop 路径——preflop 仍以 hole canonical id（0..1326）单维索引，D-239 / D-245 不变；⑤ `bucket_table.schema_version` 在本 rev 之内**不 bump**（A0 第一版 spec 锁定前的语义澄清，no on-disk file 已写出），但若 stage 2 commit 后再走该 rev，必须 bump。

**与 D-219 的关系**：postflop bucket 仍仅依赖 (street, board, hole)，不嵌入 position / stack / betting_state；D-216-rev1 只改变 lookup 索引方式，**不改变** D-219 隔离原则。

##### D-218-rev1（2026-05-09，F19）

**背景**：D-218 原文只定义 hole canonical id 与 board canonical id 两个单维概念，未明确 postflop bucket lookup 所需的 (board, hole) 联合 canonical id。

**新规则**：D-218 在原文基础上扩展为三层 canonical id：

| 层级 | 函数 | 返回类型 | 用途 |
|---|---|---|---|
| L1 单维 hole | `canonical_hole_id(hole) -> u32` | 0..1326 | preflop lookup 唯一索引；postflop 联合 canonical 的 hole 子分量 |
| L1 单维 board | `canonical_board_id(street, board) -> u32` | 见下 | 内部缓存 / 可观测调试入口（不直接驱动 lookup） |
| L2 联合 observation | `canonical_observation_id(street, board, hole) -> u32` | 0..n_canonical_observation(street) | **postflop lookup 唯一索引**（替代 D-218 原 (board_id, hole_id) 二维方案） |

L1 单维 board canonical id 上界（D-218 原文 "5-card board canonical 上限 ~134k；7-card 上限 ~1.5M" 保留作为单维参考；联合 observation 的实际枚举数由 [实现] A1 落地枚举确认并写入 `n_canonical_observation_<street>`）。

**影响**：① 公开 API 新增 `canonical_observation_id`；② `BucketTable::lookup` 入参由二维变单维（API §4 同步 BT-005-rev1）；③ `tests/canonical_observation.rs` 由 B1 [测试] 起草，枚举验证花色对称等价类完整性 + 1k 随机 (board, hole) 重复调用 byte-equal；④ `tools/bucket_table_reader.py` Python reader 同步更新（D-249）。

##### D-244-rev1（2026-05-09，F19）

**背景**：D-244 原文 header 字段 `n_canonical_flop / turn / river` 为单维 board canonical 数量，与 D-216-rev1 联合 observation id 索引不匹配；lookup_table postflop 段长度对应不上 (board, hole) 联合空间。

**新规则**：

1. **header 字段语义**（offset 不变，保持 80-byte header 布局兼容）：原 `n_canonical_flop / turn / river`（offset 0x1C / 0x20 / 0x24）三字段语义改为 **`n_canonical_observation_flop / turn / river`**——每条 postflop 街联合 (board, hole) canonical observation id 总数。
2. **lookup_table 段长度**：postflop 三街分别为 `[u32; n_canonical_observation_<street>]`；`lookup_table_<street>[observation_canonical_id] = bucket_id`。
3. **bound check**：BT-008 header 偏移表完整性约束扩展——`n_canonical_observation_<street>` 上限由 D-218-rev1 联合等价类枚举上界给出（保守值：flop ≤ 2_000_000、turn ≤ 20_000_000、river ≤ 200_000_000；`> 这些上界` 视为 `Corrupted`，A1 实测后可收紧）；header 字段被解读为 `n_canonical_observation` 后必须 ≥ 1。
4. **schema_version 处理**：本 rev 不 bump `schema_version`（v1 文件尚未生成，A0 锁定阶段语义澄清）；若本 rev 在已写出 v1 bucket table artifact 后落地，必须 bump 到 v2。
5. **F13 偏移表完整性**：BT-008 仍按 §⑨ 偏移表强制；`size = lookup_table_offset + 4 × (1326 + n_canonical_observation_flop + n_canonical_observation_turn + n_canonical_observation_river) + 32`（`+32` 为 BLAKE3 trailer），任何字段越界返回 `BucketTableError::Corrupted`。

**影响**：① `tools/bucket_table_reader.py` 更新字段名 / 长度推导；② D-247 错误路径 `Corrupted { offset, reason }` 增加 "n_canonical_observation_<street> out of range" / "lookup_table size mismatch" 两类原因（仍 5 类不变体，仅 reason 字符串扩展）；③ centroid_metadata / centroid_data 段不受影响（postflop bucket 数 `bucket_count_<street>` 不变）。

##### D-206-rev1（2026-05-09，F20）

**背景**：D-206 原文仅约束 `Bet/Raise(0.5×pot)` / `Bet/Raise(1.0×pot)` 之间的折叠 + 与 `AllIn` 折叠时的 tag 选择，**未约束** `Call { to }` 与 `AllIn { to }` 在相同 `to` 值（典型场景：玩家面对下注，剩余 stack 恰等于跟注金额——`call = stack`）下的去重。AA-004 又要求所有带 `to` 的 `AbstractAction` 实例 `to` 字段互不相等，导致语义冲突——是保留 `Call`、保留 `AllIn`，还是两者都保留？

**新规则**：扩展 D-206 折叠优先级规则到完整链：

1. **基线候选生成**（before 折叠）：按 D-200 详解表生成所有候选 `AbstractAction` 实例，每个实例的 `to` 字段按 D-205 fallback 计算后填入。
2. **折叠优先级**（first-match-wins，从高到低）：
   - **`AllIn` 优先级最高**：若任何带 `to` 的候选（`Call` / `Bet` / `Raise`）的 `to` 等于 `committed_this_round + stack`（即等价于把剩余筹码全推到当前下注轮，与 stage 1 `Action::AllIn` 状态机归一化后等价），**整组合并为 `AllIn { to }`**。tag 始终落到 `AllIn`。**例外**：AA-002 已剔除 `Fold` 的 free-check 局面下若 `Check` 是仍然合法的非 `to` 动作，`Check` 不参与折叠（不带 `to`，不与 `AllIn` 冲突）。
   - **`Call` 与 `Bet/Raise(x×pot)` 折叠**：理论上不可能——`Call` 的 `to` 等于 `max_committed_this_round`，`Bet/Raise` 的 `to` 不能小于 `min_to ≥ max_committed_this_round + last_full_raise_size`（D-034 / D-035），二者 `to` 严格不等；如果 [实现] 因 D-205 fallback 把 `Bet/Raise` 折叠到 stack 上限，已被上一条 `AllIn` 优先级吸收。
   - **`Bet/Raise(0.5×pot)` 与 `Bet/Raise(1.0×pot)` 之间折叠**：保留 ratio_label 较小的一份（沿用 D-206 原规则）。
3. **去重不变量**：经上述优先级处理后，最终 `AbstractActionSet` 内任意两个带 `to` 的动作 `to` 字段互不相等（AA-004 不变量满足）。

**等价口语化**：当 `Call { to = X }` 与 `AllIn { to = X }` 同时候选时（all-in call），保留 **`AllIn { to = X }`**——因为 stage 1 `Action::Call` 在剩余 stack 等于 call 金额时实际触发 all-in 状态转移，`AllIn` 的 betting_state 状态转移（D-206 原文最后一段）与 `Call` 等价但 InfoSet 编码标签更精确（避免 "字面 Call 实际 all-in" 的混淆）。

**影响**：① API §1 AA-004 不变量同步收紧——`AbstractAction { Call, Bet, Raise, AllIn }` 任意两个实例 `to` 字段不等；② AA-001 D-209 输出顺序中 `Call` 槽位若被 `AllIn` 优先级吸收则**消失**（不出现），输出顺序保持 `[Fold?, Check?, Call?, Bet/Raise(0.5×)?, Bet/Raise(1.0×)?, AllIn?]` 中 `?` 跳过的语义不变；③ `tests/scenarios_extended.rs` 阶段 2 版必须含至少 2 条 all-in call 场景（短码 BTN call 大 raise / 短码 BB call 3-bet）断言 `Call` 不出现而 `AllIn` 出现。

##### D-211-rev1（2026-05-09，F21）

**背景**：D-211 原文规定 "preflop 起手时计算 stack_bucket，postflop 沿用"，但 `InfoAbstraction::map(state, hole)` 只接收当前 `&GameState + hole`，没有保存 "起手 effective_stack bucket" 的入口；postflop 街调用时只能从当前 stack 重新计算（违反 "沿用" 字面）或从 history 第一条状态反推（无 history 入参），来源不可重建。

**新规则**：钉住 stack_bucket 的来源 = **`TableConfig::initial_stack(seat) / big_blind`**（与 stage 1 D-022 / D-022b 默认 100 BB 配套；stage 2 默认 6-max 100 BB 配置 → 全部 actor `stack_bucket = 3`，对应 `[100, 200) BB`）。

具体推导：

1. `effective_stack_at_hand_start(actor) = min(initial_stack(actor), max(initial_stack(opp) for opp in 同手所有对手))`（hand-start snapshot 视角，所有 seat 都还没行动 / 折叠）；
2. `stack_bucket = bucket_of_bb(effective_stack_at_hand_start(actor) / big_blind)`，按 D-211 5 桶边界向下取整。

**入口要求**（API 同步约束）：`InfoAbstraction::map` 实现必须从 `GameState` 暴露的 `TableConfig` 引用（`state.config()` 返回 `&TableConfig`）+ `state.actor_seat()` 计算 `effective_stack_at_hand_start`，**不允许**从当前 `state.player(seat).stack` 推算（partial call / all-in 后当前 stack 不再是 hand-start 值）。如 stage 1 `GameState` 当前未暴露 `config()` 公开 getter，必须走 stage 1 `API-NNN-revM` 流程在 `pluribus_stage1_api.md` 增加只读 getter（继承 D-271 约束）；本 rev 假设 stage 1 GameState `config()` getter 可访问（见 API IA-stack-bucket-revM 同步）。

**约束范围**：本 rev 仅适用于 stage 2 验收路径（fixed initial stack 100 BB）。stage 4+ 引入 cash-game-style 中途变 stack 时，需走 D-211-rev2 引入显式 hand-start snapshot 字段；stage 2 不预留该入口（避免过度抽象）。

**影响**：① `InfoAbstraction::map` 实现路径必须经 `state.config()`；② 同手内 preflop / flop / turn / river / showdown 调用 `map(state, hole)` 在 (固定 stack 假设) 下 `stack_bucket` 字段 byte-equal；③ B1 [测试] `tests/info_id_encoding.rs` 在 100 BB / 200 BB / 50 BB 三种 TableConfig 下断言 stack_bucket 落入正确桶（3 / 4 / 2）。

##### D-220a-rev1（2026-05-09，F25）

**背景**：D-220a 原文规定 preflop pairwise equity 反对称在 "同 RngSource state（fresh sub-stream from D-228 same op_id + sub_index）下 `|sum - 1| ≤ 1e-9`"，但 `EquityCalculator::equity_vs_hand` 签名只接 `&mut dyn RngSource`——若 [测试] 顺序复用同一个 `&mut rng` 调用两次，第一次会推进 rng 状态，第二次采到不同 future board 集合，严格反对称不成立。

**新规则**：preflop pairwise equity 反对称的 RNG 契约钉为 **"两个独立 `RngSource`，各自从同一 `derive_substream_seed(master_seed, EQUITY_MONTE_CARLO, sub_index)` 构造"**：

```rust
// B1 [测试] tests/equity_self_consistency.rs 的标准模式：
let master_seed: u64 = 0xA0B1C2D3_E4F50617;
let sub_index: u32 = 0;  // 任意固定值
let sub_seed = derive_substream_seed(master_seed, EQUITY_MONTE_CARLO, sub_index);

let mut rng_ab = ChaCha20Rng::from_seed(sub_seed.to_le_bytes_or_seed_array());
let mut rng_ba = ChaCha20Rng::from_seed(sub_seed.to_le_bytes_or_seed_array());

let eq_ab = calc.equity_vs_hand(A, B, &[], &mut rng_ab);
let eq_ba = calc.equity_vs_hand(B, A, &[], &mut rng_ba);

assert!((eq_ab + eq_ba - 1.0).abs() <= 1e-9);  // 严格容差路径
```

**容差分流**（替代 D-220a 原 "fresh sub-stream" 单句表述）：

| 路径 | RNG 配置 | 容差 |
|---|---|---|
| postflop（board.len() ≥ 3） | 不消耗 RNG（确定性枚举） | `\|sum - 1\| ≤ 1e-9` |
| preflop strict | 两个独立 RngSource，从同一 sub_seed 构造（如上 pseudo code） | `\|sum - 1\| ≤ 1e-9` |
| preflop noisy（10k iter） | 两个独立 RngSource，从不同 sub_seed 构造 | `\|sum - 1\| ≤ 0.005`（标准误差 ≈ `sqrt(0.25 / 10000)`） |
| preflop noisy（1k iter） | 两个独立 RngSource，从不同 sub_seed 构造 | `\|sum - 1\| ≤ 0.02` |

**禁止模式**：顺序复用同一 `&mut rng` 调用两次后做严格反对称断言——明确写入 EQ-001-rev1（API 同步）作为反例。`tests/equity_self_consistency.rs` 必须按上述 pseudo code 模式构造 RngSource。

**影响**：① B1 [测试] 反对称 harness 不再有 "fresh sub-stream" 歧义解读空间；② `derive_substream_seed` 输出 `u64` → `[u8; 32]` ChaCha20 seed 的转换路径由 [实现] 在 A1 落地（典型：`u64 → [u8; 8] → 重复 4 次填充 [u8; 32]`，或 SplitMix64 派生 4 次填充；具体 byte 路径在 D-228 op_id 表的 doc comment 里固化）；③ EQ-001 不变量在 API §3 同步 EQ-001-rev1 收紧。

##### D-224-rev1（2026-05-09，F23）

**背景**：D-224 / EQ-002 规定 "任何 NaN / Inf 视为 P0 阻塞 bug"；API §3 `equity_vs_hand` 注释又规定 "`opp_hole` 与 `hole ∪ board` 重叠时返回 NaN"。两条规则同时生效会让 [测试] 无法判断 NaN 是合法错误返回还是 invariant violation。

**新规则**：D-224 finite invariant 收紧到 **"合法、无重叠输入"** 路径，并把 invalid input 的错误返回从 NaN 改为 `Result<f64, EquityError>`：

1. `equity_vs_hand` / `equity` / `ehs_squared` / `ochs` 全部接口签名由返回 `f64` 改为 `Result<f64, EquityError>`（OCHS 改为 `Result<Vec<f64>, EquityError>`），与 stage 1 `RuleError` / `HistoryError` 同型。
2. `EquityError` 5 类（继承 stage 1 错误同型设计）：
   - `OverlapHole { card }` — `opp_hole` 与 `hole` 重叠
   - `OverlapBoard { card }` — `hole` 或 `opp_hole` 与 `board` 重叠
   - `InvalidBoardLen { got }` — `board.len() ∉ {0, 3, 4, 5}`
   - `IterTooLow { got, min: 1 }` — Monte Carlo `iter == 0`（默认 10k 不触发，stage 4 消融可触发）
   - `Internal(String)` — 评估器内部错误透传（继承 stage 1 `HandEvaluator` 错误，可能性极低）
3. **invariant 收紧**：D-224 / EQ-002 改为——返回 `Ok(x)` 时 `x ∈ [0.0, 1.0]` 且 finite；任何 NaN / Inf 出现在 `Ok` 路径是 P0 阻塞 bug。`Err(EquityError)` 路径不进入 feature / bucket 写入，由 caller 在 clustering 训练前过滤（`MonteCarloEquity::ochs(...)?` 用 `?` 操作符传播错误终止 cluster 数据点）。

**影响**：① API §3 `EquityCalculator` trait 4 个方法签名全部加 `Result` 包装（API §9 EquityCalculator-rev1 同步）；② `tests/equity_self_consistency.rs` 反对称断言路径用 `unwrap()` 解包合法输入；③ fuzz 测试 invariant 用 `match result { Ok(x) => assert!(x.is_finite()), Err(_) => {} }` 模式；④ 不影响 stage 1 类型 / API（错误类型新增，不复用 stage 1 错误体系）；⑤ `feature_set_id` 不 bump（错误返回仅影响接口层，不影响 bucket table 二进制布局）。

##### D-202-rev1（2026-05-09，F27）

**背景**：D-202 / `BetRatio::from_f64` 仅约束 `raise_pot_ratios` 元素 ∈ (0.0, +∞)，但 `BetRatio` 内部 `u32 = ratio × 1000` 量化对 < 0.001 的小数 / > `u32::MAX / 1000 ≈ 4.29M` 的大数 / 多输入映射到同一 milli 值的去重 / rounding 模式（round / floor / ceil）全部未定义。stage 2 默认 5-action 不触发，但 stage 4 消融配置加载非默认 ratio 会让 [实现] 与 [测试] 各自做 ad-hoc 选择。

**新规则**：

1. **rounding mode**：`from_f64(ratio)` 走 **bankers-rounding（half-to-even）** 量化到 `(ratio * 1000.0).round_ties_even() as i64`，再校验范围。理由：half-to-even 让 0.5 / 1.5 / 2.5 等 "整数 + 半" 输入分布对称舍入，避免单向偏差累积；`f64 → i64 → u32` 的中间 `i64` 检测越界。
2. **合法范围**：`from_f64` 接受 `ratio ∈ [0.001, 4_294_967.295]`（含端点），即量化后 `u32 ∈ [1, u32::MAX]`；越界（`< 0.001` / `> 4_294_967.295` / NaN / Inf / 负数 / 0.0）返回 `None`。
3. **重复 milli 去重**：`ActionAbstractionConfig::new(raise_pot_ratios: Vec<f64>)` 内部 `Vec<f64> → Vec<BetRatio>` 转换中，若任意两元素量化后 milli 值相同，**返回 `Err(ConfigError::DuplicateRatio { milli })`**——caller 责任去重。理由：量化导致的隐式去重会让 `raise_pot_ratios.len()` 与 `raise_count()` 不一致，破坏 D-209 输出顺序契约。
4. **误差量级**：half-to-even rounding 引入 ≤ 0.5 milli = 0.0005 ratio 误差；与 D-205 fallback "向上取整到 chip" 的整数级误差相比可忽略；与 D-220 Monte Carlo standard error 0.005 相比远低（量化误差不进入 equity / clustering 路径，仅影响 betting_state 转移点的 to 值取整边界，由 D-205 ② all-in 折叠路径自然吸收）。

**影响**：① API §1 `BetRatio::from_f64` 文字注释收紧（API 同步 BetRatio::from_f64-rev1）；② `ConfigError` 增加 `DuplicateRatio { milli: u32 }` 变体；③ `tests/action_abstraction.rs` 阶段 2 版必须断言 `BetRatio::from_f64(0.5005)` 量化到 `BetRatio::HALF_POT.as_milli() == 500` 且 `from_f64(0.5004)` 也量化到 500（half-to-even），与 `from_f64(0.5)` 严格相等；④ default 5-action 配置 `[BetRatio::HALF_POT, BetRatio::FULL_POT]` (= [500, 1000]) 不触发任何边界路径。

##### D-253-rev1（2026-05-09，F26）

**背景**：D-253 顶层 re-export 列表与 API §6 实际 re-export 不一致——D-253 漏写 `BetRatio` / `ConfigError` / `BettingState` / `StreetTag` 四个类型，但这四个类型出现在公开签名（`BetRatio` 在 `ActionAbstractionConfig`、`ConfigError` 在 `new` 返回、`BettingState` / `StreetTag` 在 `InfoSetId` getter 与 `BucketTable::lookup`）中。caller / [测试] 必须能通过 `use poker::*` 直接使用这四个类型。

**新规则**：D-253 re-export 列表补齐为：

```text
ActionAbstraction / DefaultActionAbstraction / AbstractAction / AbstractActionSet /
ActionAbstractionConfig / BetRatio (★新增) / ConfigError (★新增) /
InfoAbstraction / InfoSetId / BettingState (★新增) / StreetTag (★新增) /
PreflopLossless169 / PostflopBucketAbstraction / EquityCalculator / EquityError (★新增 D-224-rev1 配套) /
MonteCarloEquity / BucketTable / BucketConfig / BucketTableError
```

与 API §6 一致；本 rev **以 API §6 为准**，D-253 文字同步。

**影响**：① 公开 API surface 多 5 个 re-export（`BetRatio` / `ConfigError` / `BettingState` / `StreetTag` / `EquityError`）；② `tests/api_signatures.rs` 阶段 2 版必须断言这 5 个类型可从 crate 顶层导入；③ `abstraction::cluster::rng_substream` 公开 contract（D-228）保持不变（`derive_substream_seed` + op_id 命名常量），不在 D-253 re-export 列表内但通过 `pub use abstraction::cluster::rng_substream;` 模块级别暴露。

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
