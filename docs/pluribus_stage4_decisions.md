# 阶段 4 决策记录

## 文档地位

本文档记录阶段 4（6-max NLHE Blueprint 训练）的全部技术与规则决策。一旦 commit，后续步骤（A1 / B1 / B2 / ... / F3）的所有 agent 必须严格按此 spec 执行。

任何决策修改必须：
1. 在本文档以 `D-NNN-revM` 形式追加新条目（不删除原条目）
2. 必要时 bump `Checkpoint.schema_version`（D-470 stage 4 checkpoint schema）或继承 stage 3 `Checkpoint.schema_version`（仅当 stage 4 修改影响序列化时触发）或 `HandHistory.schema_version`（继承 stage 1 D-101）或 `BucketTable.schema_version`（继承 stage 2 D-240）或 stage 2 `InfoSetId` 64-bit layout（D-423 InfoSet bit 扩展 14-action mask 候选）
3. 通知所有正在工作的 agent（在工作流 issue / PR 中显式标注）

未在本文档列出的细节，agent 应在 PR 中显式标注 "超出 A0 决策范围"，由决策者补充决策后再实施。

阶段 4 决策编号从 **D-400** 起，与阶段 1 D-NNN（D-001..D-103）+ 阶段 2 D-NNN（D-200..D-283）+ 阶段 3 D-NNN（D-300..D-379）不冲突。阶段 1 + 阶段 2 + 阶段 3 D-NNN 全集 + D-NNN-revM 修订作为只读 spec 继承到阶段 4，未在本文档显式覆盖的部分以 `pluribus_stage1_decisions.md` + `pluribus_stage2_decisions.md` + `pluribus_stage3_decisions.md` 为准。

---

## 1. 算法变体（D-400..D-409）

| 编号 | 决策项 | 选定值 |
|---|---|---|
| D-400 | 主算法 | **Linear MCCFR + Regret Matching+**（Brown & Sandholm 2019 AAAI Linear CFR + Tammelin et al. 2015 CFR+）。External-Sampling 采样核继承 stage 3 D-301 ES-MCCFR；regret 累积按 Linear weighting（D-401）；strategy 计算按 RM+ clamp（D-402）；average strategy 按 Linear weighted 累积（D-403）。详见 D-400 详解。**翻面 stage 3 D-302**（非 Linear）+ **D-303**（标准 RM）：stage 3 字面 deferred 在 stage 4 A0 翻面 lock。 |
| D-401 | Linear discounting weighting 公式 | **iter t 上 regret 累积乘 `t / (t + 1)` 折扣项 + 当前 update 量乘 `1`（cumulative 形式 t-weighted）**：`R̃_t(I, a) = (t / (t + 1)) × R̃_{t-1}(I, a) + r_t(I, a)`，等价于 cumulative weighted regret `R̃_t(I, a) = Σ_{τ=1..t} (τ / (τ+1) × τ / (τ-1) ... ) × r_τ(I, a)`（Brown & Sandholm 2019 §3.1 字面）。实现路径：每次 update 前对全 InfoSet 的 regret 累积乘 `t / (t + 1)` 衰减；update 后 += 当前 iter regret delta。**性能开销**：全 InfoSet 衰减 = 全 HashMap 扫描，stage 3 v3 InfoSet 量级 ~10⁶ × 14 action = 1.4 × 10⁷ float multiply per iter — 在 D-490 单线程 SLO `≥ 5K update/s` 下衰减成本占 ~30%（按 200 ns/multiply × 14 × 10⁶ = 2.8 ms / iter，5K/s = 200 μs/iter — 矛盾）。**实现 carve-out**：D-401-revM 候选 — 衰减改为 lazy（每次 `current_strategy(I)` query 时延迟应用衰减，分摊到只访问的 InfoSet）；具体在 batch 5 [api] / B2 [实现] 落地前决定。 |
| D-402 | Regret Matching+ clamp 公式 | **每 update 后对 regret 负值 clamp 到 0**：`R^+_t(I, a) = max(R̃_t(I, a), 0)`，strategy 计算用 clamp 后值 `σ_t(I, a) = R^+_{t-1}(I, a) / Σ_b R^+_{t-1}(I, b)`（Tammelin 2015 §3 字面）。**clamp 时机**：D-402 锁定在每次 update 累积完 regret delta + Linear discounting 后立即 clamp（in-place 修改 `R̃` field 不保留负值）；下一 iter `current_strategy(I)` 直接读 clamped value。Stage 3 标准 RM 实现（D-303 + D-306）作为 ablation baseline 保留但不进 production。**与标准 RM 关键差异**：RM+ 不允许 regret 累积到大负值（标准 RM 允许负值长期累积导致 "regret debt" 慢恢复），RM+ 即时丢弃负值收敛更快（实测 2-3× 加速，Brown & Sandholm 2019 §6）。 |
| D-403 | average strategy 累积公式（Linear weighted） | **strategy sum 按 t 加权**：`S_t(I, a) = S_{t-1}(I, a) + t × σ_t(I, a)`（Brown & Sandholm 2019 §3.2 字面 Linear time weighting）。**average strategy** 计算：`σ̄_t(I, a) = S_t(I, a) / Σ_b S_t(I, b)`，分母为 0 时回退均匀分布（继承 stage 3 D-306）。**目的**：对早期低质量 strategy 降权（早期 t 小、权重低，后期 t 大、权重高），相当于隐式 burn-in。**与 stage 3 D-304 关键差异**：stage 3 strategy sum 按 reach probability × σ 累积（unweighted by t）；stage 4 在此基础上额外乘 `t`。 |
| D-404 | counterfactual regret delta 公式 | **继承 stage 3 D-305 标准 CFR regret update**：每 iter t 上对每个 traverser InfoSet I 和 action a，regret delta `r_t(I, a) = π_opp × (cfv_t(I, a) - Σ_b σ_t(I, b) × cfv_t(I, b))`；ES-MCCFR 中 cfv 通过 outcome-level Monte Carlo 估计（继承 stage 3 D-301 详解）。Linear discounting + RM+ 仅作用于 **累积** 公式（D-401 + D-402），单 iter delta 计算不变。 |
| D-405 | sampling 策略选型 | **External-Sampling MCCFR（继承 stage 3 D-301）**。stage 3 §8.1 第 (II) 项 carry-forward (outcome vs external sampling) 在 stage 4 A0 [决策] 评估结论：**maintain external sampling**。理由：(a) 6-player 14-action 状态空间下 outcome sampling 每 trajectory 路径长度 = 6 player × 4 街 × ~3 action depth ≈ 72 hops，traverser 决策点全 14-action 遍历的 external sampling 在 6-player 下反而**比 outcome sampling 快**（external 在 traverser 决策点 1 次递归覆盖 14 个 cfv，outcome 需 14 次独立 trajectory）；(b) 收敛速度方差 external sampling 严格优于 outcome sampling（Lanctot 2009 定理 4 + 7 字面）。**D-301 字面 lock 维持**，不走 D-301-revM。 |
| D-406 | traverser 选择策略（6-player） | **轮换**（alternating），继承 stage 3 D-307 扩展到 n=6：iter t 上 traverser = `(t mod 6)`。**不**采用 uniform random traverser selection（Pluribus 论文 §S2 字面 alternating）。多线程并发场景下，每 worker thread 独立 traverser = `((base_update_count + tid) mod 6)`（继承 stage 3 D-321-rev1 / rev2 alternating 跨线程模式扩展 n=2 → n=6）。 |
| D-407 | chance node 采样 | **继承 stage 3 D-308**：采样 1 outcome，按 chance distribution。**不**做 Public Chance Sampling（PCS）—— PCS 是 stage 6 实时搜索优化范围，stage 4 blueprint 训练保持 outcome sampling 标准形态。 |
| D-408 | non-traverser action 采样 | **继承 stage 3 D-309**：按当前 strategy `σ_t` 采样 1 action（External Sampling 标准）；non-traverser 决策点 `S_t(I_opp, b) += t × σ(b)` 累积（含 D-403 Linear weighting）。 |
| D-409 | warm-up phase（Linear weighting 提前禁用） | **前 1M update 用 standard CFR + RM**（不加 Linear / 不加 RM+ clamp），1M update 后切到 Linear MCCFR + RM+。**理由**：Linear weighting 在 update_count 很小时（前 ~1K update）`t / (t + 1)` 接近 0.5，等价于过强衰减；warm-up phase 让 regret 先达到正常量级再切 Linear。RM+ 在 update_count 很小时也可能误 clamp 正确的初期负 regret 导致策略震荡。**实现**：`EsMccfrTrainer` 状态机内部维护 `warmup_complete: bool` flag；warm-up 期间走 D-302/D-303 stage 3 路径，warm-up 后切 D-400/D-401/D-402 stage 4 路径。Warm-up 长度 1M 是经验值（Pluribus 论文 §S2 / Brown 2020 PhD 论文 §4.2 候选 1K-1M range），D-409 锁定 1M，**碰到收敛异常走 D-409-revM 翻面**。 |

### D-400 详解（Linear MCCFR + RM+ for 6-max NLHE blueprint）

**伪代码**（单 iter，单 traverser，alternating per D-406）：

```
function LinearMccfrRmPlus_iter(t, regret_t, strategy_sum_t, rng, traverser):
    # 步骤 1：Linear discounting（D-401）— eager 实现路径（baseline）
    # 注：D-401-revM lazy 路径在 batch 5 [api] / B2 [实现] 之前评估替代
    if t > 1 and warmup_complete:
        decay_factor = (t - 1) / t   # 等价 t/(t+1) 视角的过去 vs 当前 iter
        for (info_set, regret_vec) in regret_t:
            for a in actions:
                regret_vec[a] *= decay_factor
        for (info_set, strategy_sum_vec) in strategy_sum_t:
            for a in actions:
                strategy_sum_vec[a] *= decay_factor   # 注：Linear weighted strategy sum 同样需 decay
    # 步骤 2：External Sampling 递归（继承 stage 3 D-301 ES-MCCFR）
    recurse_es(root, traverser, π_traverser = 1.0, π_opp = 1.0, rng, t, warmup_complete)
    # 步骤 3：RM+ clamp（D-402）— 在 recurse_es 内 in-place 完成（traverser 决策点 update 后立即 clamp）
    return regret_{t+1}, strategy_sum_{t+1}

function recurse_es(state, traverser, π_traverser, π_opp, rng, t, warmup_complete):
    # terminal / chance / non-traverser 分支继承 stage 3 D-301 详解
    if terminal: return utility(state, traverser)
    if chance: o ~ chance_dist; return recurse_es(state.next(o), traverser, π_traverser, π_opp, rng, t, warmup_complete)
    if actor != traverser:
        σ_opp = current_strategy(I_actor)
        a' ~ σ_opp
        # D-408 Linear weighted strategy sum 累积（仅 warmup_complete 后）
        for b in actions:
            S(I_actor, b) += t × σ_opp(b)   if warmup_complete else += σ_opp(b)
        return recurse_es(state.next(a'), traverser, π_traverser, π_opp × σ_opp(a'), rng, t, warmup_complete)
    # actor == traverser
    σ = current_strategy(I_traverser)
    cfvs = []
    for a in actions:
        v_a = recurse_es(state.next(a), traverser, π_traverser × σ(a), π_opp, rng, t, warmup_complete)
        cfvs.append(v_a)
    σ_node = Σ_a σ(a) × cfvs[a]
    # D-404 regret delta 累积
    for a in actions:
        delta = π_opp × (cfvs[a] - σ_node)
        if warmup_complete:
            R(I_traverser, a) += delta
            R(I_traverser, a) = max(R(I_traverser, a), 0)   # D-402 RM+ clamp
        else:
            R(I_traverser, a) += delta   # warm-up = stage 3 standard RM 路径
    return σ_node
```

**关键不变量**：
- Linear weighting 同时作用于 regret + strategy sum（D-401 + D-403）；只 weighting 一个而不另一个 = 不对称偏置（Brown & Sandholm 2019 §3.3 反例）。
- RM+ clamp 必须在 regret 累积**之后** 立即应用（D-402）；clamp 在 query `current_strategy(I)` 时延迟应用 = 错误实现（regret 持续累积大负值，clamp 后丢失正确的近期 positive regret 信号）。
- warm-up 边界（D-409）由 `warmup_complete: bool` flag 控制；前 1M update 走 stage 3 standard CFR + RM 路径，与 stage 3 `EsMccfrTrainer::step` 字面**完全等价**（warm-up phase BLAKE3 byte-equal 与 stage 3 1M update anchor 必须一致 — D-409 carry-over 不变量）。

**收敛性**（Brown & Sandholm 2019 定理 1 + Tammelin 2015 定理 2）：Linear CFR + RM+ average regret 上界为 standard CFR + RM 的 `1 / (2 × √2)` 倍（理论 1.41× 加速）；实战 6-max NLHE 测得 2-5× 加速（Pluribus 论文 §S2 / Brown PhD 论文 §4.6）。stage 4 first usable 10⁹ update 预期对应 standard CFR + RM 5 × 10⁹ update 收敛质量。

---

## 2. 多人 NLHE 规则与 Traverser 协议（D-410..D-419）

| 编号 | 决策项 | 选定值 |
|---|---|---|
| D-410 | 6-max NLHE 规则 | **6-player Texas Hold'em No-Limit 100 BB starting stack + 盲注 0.5/1.0 BB + 无 rake + 无 ante + 完整 4 街**（继承 stage 1 D-022 默认 + n_seats=6 路径）。Button rotation 每手左移 1 seat（继承 stage 1 D-032）；行动顺序 SB → BB → UTG → MP → CO → BTN preflop / SB → BB → UTG → MP → CO → BTN postflop（继承 stage 1 D-022b first_postflop_actor 多人通用规则）。Showdown 走 stage 1 `HandEvaluator` 7-card lookup（继承 stage 1 D-072）。 |
| D-411 | `NlheGame6` Game trait 实现 | **stage 3 `Game` trait 的第 4 个 impl**（继承 KuhnGame / LeducGame / SimplifiedNlheGame）。`Game::VARIANT = GameVariant::Nlhe6Max`（stage 3 D-373-rev1 enum 追加变体，stage 3 字面 `GameVariant { Kuhn, Leduc, SimplifiedNlhe, Nlhe6Max }` 4 个变体）。`Game::State = stage1::GameState`（与 SimplifiedNlheGame 共享，区别在 `n_seats = 6` config）；`Game::Action = stage2::abstract::AbstractAction`（继承 stage 2 抽象 action，14-action 通过 D-420 `PluribusActionAbstraction` 实现）；`Game::InfoSet = stage2::InfoSetId`（继承 stage 2 64-bit + D-423 14-action mask 扩展）。 |
| D-412 | 6-player alternating traverser 状态机 | **每 trainer 内部维护 6 套独立 `RegretTable` + `StrategyAccumulator`**（每 traverser 1 套，索引 `[0..6)`）。`EsMccfrTrainer::step` 内部 `traverser = (update_count mod 6)`；多线程 `step_parallel` 内每 worker thread `traverser = ((base_update_count + tid) mod 6)`。**6 套表互不共享数据**——traverser 0 的 regret 不影响 traverser 1 的 strategy 计算。该约定让 6-player 训练等价于 6 个独立的 single-player problem，每 traverser 解一个 BR-against-opponents（其它 5 个 traverser 走当前 strategy）。 |
| D-413 | seat position 与 traverser 映射 | **不一一映射**（继承 stage 3 D-412 模式）。traverser 是 trainer 内部 player index `[0..6)`，与 `SeatId` 物理 seat 解耦；button rotation 在 `GameState::root(&mut rng)` 内部按 stage 1 D-032 处理，trainer 不感知物理 button position。每 hand 起始时 trainer 选 traverser_index，runtime 通过 `GameState::actor_at_seat(seat_id) -> PlayerId` 映射到具体 InfoSet。 |
| D-414 | 6 traverser 是否共享 strategy（policy invariance） | **不共享**。stage 4 blueprint 训练 6 个 traverser 各自 converge 到自己的 strategy；policy invariance（"所有 player 走同一个 blueprint" 的实战 simplification）由 stage 6 实时 search 阶段统一处理（每 player 在搜索时调用 traverser 0 的 strategy 作为 default opponent model）。stage 4 验收按**每 traverser 独立** LBR + Slumbot 评测，每 traverser 必须独立通过门槛（避免 traverser 0 优秀但 traverser 5 失败的虚假通过）。 |
| D-415 | stage 1 GameState n_seats=6 路径 byte-equal 维持 | **stage 1 测试套件全 0 failed 锚点**（继承 CLAUDE.md 不变量）。stage 4 引入 6-player 路径不修改 stage 1 `GameState::apply` / `GameState::root` 实现；任何修改走 stage 1 `D-NNN-revM` 流程 + 用户授权（与 stage 3 D-022b-rev1 同型跨 stage 1 carve-out 模式）。stage 4 B1 [测试] 必须覆盖 6-player 完整 4 街 1000 hand smoke 跑过 + BLAKE3 byte-equal regression。 |
| D-416 | 6-player heads-up 退化路径 | **n_seats=2 走 stage 1 D-022b-rev1 HU NLHE 语义**（继承 stage 3 SimplifiedNlheGame 实现）；n_seats=6 走 stage 1 默认 multi-seat 分支。stage 4 训练 default `NlheGame6 { n_seats: 6 }` 走 multi-seat 路径；HU vs Slumbot 评测时（D-460）用 `NlheGame6 { n_seats: 2 }` 退化路径走 HU 分支，**复用 stage 3 SimplifiedNlheGame BLAKE3 anchor 不变量**（stage 3 1M update × 3 anchor 必须在 stage 4 commit byte-equal 维持）。 |
| D-417 | first to act in betting round | **continued from stage 1 D-022b-rev1 multi-seat**：preflop 行动顺序起点 = SB+2 即 UTG（左于 button 第 3 位）；postflop 行动顺序起点 = SB（左于 button 第 1 位）。这是 stage 1 D-022b 通用规则在 n_seats=6 的字面实例化，**stage 4 不引入新约定**。 |
| D-418 | side pot 处理（6-player） | **stage 1 D-038 side pot 算法继承**。6-player 路径下 side pot 数量 ≤ 5（每个 all-in player 创建 1 个 side pot 上限），stage 1 测试 `tests/side_pots.rs` 8 active 测试已覆盖到 4-player side pot；stage 4 B1 [测试] 新增 1000 random 6-player all-in 场景 fuzz 验证 side pot 在 6-player 路径 byte-equal 不退化。 |
| D-419 | rake / ante / structure variation | **rake = 0 / ante = 0 / no straddle**（继承 stage 3 SimplifiedNlheGame 简化）。Pluribus 论文 §1 字面 "no rake" 训练；real-money rake 模型由 stage 8 策略服务化阶段引入（path.md §阶段 8 字面 "API 输入..."）。stage 4 训练**严格零和**（D-410 + D-419 lock）便于 LBR / Slumbot 评测的 mbb/g 计算无 rake 修正。 |

---

## 3. Action / Info Abstraction（D-420..D-429）

| 编号 | 决策项 | 选定值 |
|---|---|---|
| D-420 | Action abstraction = Pluribus 14-action | **`PluribusActionAbstraction` 实现 stage 2 `ActionAbstraction` trait**：14 个 action `{ fold, check, call, raise(0.5×pot), raise(0.75×pot), raise(1×pot), raise(1.5×pot), raise(2×pot), raise(3×pot), raise(5×pot), raise(10×pot), raise(25×pot), raise(50×pot), all_in }`（Pluribus 论文 §2.2 字面）。stage 2 5-action `DefaultActionAbstraction` 在 stage 4 期间作为 ablation baseline 保留但不进 production。**raise size 计算**：`raise_to = current_bet + (raise_multiplier × pot_size)`；不满足 min raise（stage 1 D-033）的 raise size 在 `legal_actions` 内自动剔除；超过 stack 的 raise size 自动转 all_in。**Action enumeration 顺序**：固定 `[fold, check, call, r05, r075, r1, r15, r2, r3, r5, r10, r25, r50, all_in]`，stage 2 trait surface 不变（D-209 deterministic 顺序继承）。 |
| D-421 | preflop 独立 action set | **不引入**（A0 lock 选 stage 2 trait 单一 abstraction 视角）。Pluribus 论文 §S2 preflop 实际用更细 abstraction（3×BB / 4×BB / 5×BB / pot raise 等），但 stage 4 A0 选**统一 14-action**简化实现复杂度；preflop range 表达力损失通过 turn/river postflop bucket 自动 capture（preflop range 进入 postflop 后通过 bucket 隔离到不同 InfoSet，CFR 训练自然分流不同 preflop entry size 的策略）。**D-421-revM 候选**：若 stage 4 F1 [测试] LBR 实测 first usable `10⁹` update 后 `> 300 mbb/g` 显著超 D-451 first usable 阈值 200 mbb/g，可考虑 D-421-revM 翻面引入 preflop 独立 action set；具体在 F2 [实现] 起步前 + 用户授权 lock。 |
| D-422 | stage 1 `GameState::apply` 14-action 验证 | **跨 stage 1 边界 + stage 1 测试套件 byte-equal 锚点**（继承 stage 4 D-415 + stage 3 §8.1 第 (III) 项 carry-forward）。stage 4 B1 [测试] 落地 `tests/nlhe_6max_raise_sizes.rs`（候选名）覆盖 14-action 全 raise sizes 在 stage 1 `GameState::apply` 路径下：(a) min raise 检查（继承 stage 1 D-033）；(b) incomplete raise 不 reopen raise option（继承 stage 1 D-033-rev1）；(c) `Action::Raise { to }` 绝对量约定（继承 stage 1 D-026）；(d) all-in short raise 不 reopen 已经行动玩家（继承 stage 1 D-033-rev1）；(e) raise size 超 stack 自动转 all_in（stage 4 B1 [测试] 锁定 stage 1 现有行为 byte-equal）。**stage 4 不修改 stage 1 `GameState::apply` 实现**；任何 14-action 测试发现 stage 1 实现缺陷 → 走 stage 1 `D-NNN-revM` 流程 + 用户授权（与 stage 3 D-022b-rev1 同型跨 stage 1 carve-out 模式）。 |
| D-423 | InfoSet bit 编码扩展（14-action mask） | **stage 3 D-317-rev1 6-bit mask（bits 12..18）→ stage 4 14-bit mask 翻面候选**。stage 2 `InfoSetId` 64-bit layout 在 stage 3 D-317-rev1 lock 后剩余 reserved bits 已经收紧；stage 4 14-action mask 14 bit 不能 fit 在 stage 3 6-bit mask 区域。**候选方案**（具体在 batch 5 [api] 锁定 D-423-rev0）：(a) 复用 stage 2 IA-007 reserved 14 bits（**首选**，stage 2 D-218 InfoSetId 64-bit layout reserved 区域字面充分）；(b) bucket_id field 收紧给 mask 让位（破 stage 2 D-218 bucket_id 21-bit 字面，需 D-218-rev3 翻面 + stage 2 D-NNN-revM 流程 + 用户授权）；(c) InfoSetId 升 128-bit（破 stage 2 schema_version 1 → 2，最大破坏面 — checkpoint format 破坏不向前兼容）。**stage 4 A0 [决策] 锁定首选 (a) reserved 14 bits 占用 mask**，pending batch 5 [api] D-423-rev0 验证 stage 2 IA-007 reserved 14 bits 是否字面正确（stage 2 api.md IA-007 锁定细节复查）。 |
| D-424 | bucket table 依赖（stage 4） | **复用 stage 3 D-314-rev1 v3 production artifact**：`artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin`（528 MiB / body BLAKE3 `67ee555439f2c918698650c05f40a7a5e9e812280ceb87fc3c6590add98650cd` / schema_version=2）。stage 4 训练不重训 bucket table。**理由**：v3 落地 stage 3 §G-batch1 §3.10，含 D-218-rev2 真等价类 + sqrt-scaled K=500，bucket quality 实测 19 测试 10 passed / 9 failed（已知偏离 carry-forward 到 stage 4 §8.1 (VI)）。stage 4 用 v3 起步训练；若 first usable `10⁹` update 后 LBR 实测无法达到 200 mbb/g 阈值，evaluate bucket table re-train（D-218-rev3 / D-244-rev3 / 等）作为 stage 4 §F3 carve-out。 |
| D-425 | bucket table 版本一致性检查 | **训练 checkpoint 中存 bucket_table_blake3**（继承 stage 3 D-350 字段）。每次 checkpoint load 时校验当前 mounted bucket_table 与 checkpoint 中 bucket_table_blake3 匹配，不匹配 → `CheckpointError::BucketTableMismatch`（继承 stage 3 D-351 错误枚举）。**stage 4 训练全程使用同一 v3 artifact**（D-424），不允许中途切换 bucket table。 |
| D-426 | preflop lossless abstraction | **继承 stage 2 `PreflopLossless169`**（169 lossless preflop bucket，stage 2 D-213）。**不**扩展（stage 2 D-213 已经字面 lossless preflop，stage 4 无 quality 提升空间）。 |
| D-427 | postflop bucket abstraction | **继承 stage 2 `PostflopBucketAbstraction`**（500/500/500 buckets，stage 2 D-217）。stage 4 不修改。 |
| D-428 | bucket lookup 性能 SLO 在 stage 4 路径 | **继承 stage 2 D-281 P95 153 ns**；stage 4 训练每 update 涉及多次 bucket lookup（每 traverser 决策点 14 action × 6 player × 4 街 ~ 200 bucket lookup/update），stage 4 D-490 单线程 SLO `≥ 5K update/s` 下 bucket lookup 累计耗时 ~30 ms/update × 5K/s = 50 ms/s = 5%，可接受。bucket table mmap warm-up（stage 2 §G-batch1 §3.10 验证 mmap 1 次 lookup 足够 warm）继承到 stage 4。 |
| D-429 | abstraction 中途升级 prohibited | **stage 4 训练全程 bucket table mid-training 不升级**（继承 stage 3 D-356）。Bucket table 任何 schema 升级（如 D-218-rev3 真等价类 v4 artifact 落地）需要从 scratch 重训 blueprint；不允许 hot-swap bucket table 在已训练 checkpoint 上继续 — 会破坏 InfoSet 一致性（同一 history 在新旧 bucket 下映射到不同 InfoSet，regret table 失效）。 |

---

## 4. Regret / Strategy 存储扩展（D-430..D-439）

| 编号 | 决策项 | 选定值 |
|---|---|---|
| D-430 | 6-traverser × 14-action `RegretTable` 容器扩展 | **6 套独立 `RegretTable<NlheGame6>`** 每 traverser 1 套（D-412 锁定）；每 `RegretTable` 内部 `HashMap<InfoSetId, Vec<f64>>` 与 stage 3 D-320 同型。`Vec<f64>` 长度 = `n_actions = 14`（stage 3 D-324 InfoSet → action_count 映射稳定性继承）。**HashMap 选型**：A0 锁定继承 stage 3 默认 `std::collections::HashMap` SipHash；stage 4 batch 2-4 评估 `FxHashMap` 替代（stage 3 §8.1 第 (II)/Y 项 carry-forward — 估计 10-20% throughput 收益），D-430-revM 翻面候选在 B2 [实现] 落地前 lock。 |
| D-431 | 内存上界监控 | **6-traverser × InfoSet ~10⁷-10⁸ × 14 action × f64 = 6 × 10⁸ × 112 byte = 67 GB peak**（worst case，full InfoSet enumeration）；实际 InfoSet 触达率取决于 6-player × 14-action × 4 街树形 reachability，估计 1-10% sparse → peak RSS 1-7 GB。**D-431 SLO 上界 = 32 GB**（D-490 host AWS c7a.8xlarge 32 vCPU / 64 GB RAM 字面预算 50%）；超限触发 `TrainerError::OutOfMemory { rss_bytes, limit }`（P0 阻塞，不试图自适应缩表 — 继承 stage 3 D-325 模式）。**RSS 监控走 `/proc/self/status` VmRSS**（Linux），trainer `metrics()` 接口暴露 `peak_rss_bytes` 字段（D-471）。 |
| D-432 | sparse vs dense storage | **HashMap sparse**（继承 stage 3 D-323 lazy 初始化）。InfoSet 未访问时不分配 `Vec<f64>`；首次 `current_strategy(I)` 调用时根据 `game.legal_actions(state).len()` 分配 `vec![0.0; n_actions]` (regret) + `vec![0.0; n_actions]` (strategy_sum)。**不**做 dense pre-allocation — 6-player × InfoSet 10⁸ × 14 × f64 × 2 (regret + strategy_sum) × 6 traverser = 1.34 TB peak，不可承受。stage 5 紧凑存储（path.md §阶段 5）是该 carve-out 的真正解。 |
| D-433 | Linear weighted regret 数值类型 | **`f64`**（非 `f32`，继承 stage 3 D-333）。Linear weighting 累积时 `regret × (t-1)/t` 在 t ~10⁹ 时 decay factor `(10⁹ - 1) / 10⁹ ≈ 1.0 - 10⁻⁹`；f64 mantissa 52 bit 精度 ~2.2e-16，远远低于 decay step → 不丢失精度。f32 mantissa 23 bit 精度 ~5.96e-8，在 1M update 后即累积超 D-450 LBR 阈值量级，**不可用**。f32 在 stage 5 紧凑存储阶段视情况引入（届时需 D-433-revM 翻面 + stage 5 D-NNN）。 |
| D-434 | RM+ clamp 数值精度 | **clamp 时机 D-402 锁定 update 后 in-place clamp**；浮点 max 比较精确（f64 严格 `> 0` 判定）。退化局面（所有 R+ = 0）走 D-431 均匀分布回退（继承 stage 3 D-331）。**clamp 不引入精度损失**（`max(x, 0)` 等价 `if x > 0 then x else 0`，纯比较）。 |
| D-435 | Linear weighting decay 实现策略 | **A0 锁定 eager decay（baseline）+ D-401-revM lazy decay 评估**：A0 路径每 iter 起始扫描全 HashMap × 6 traverser × 2 (regret + strategy_sum) 应用 decay factor，性能开销估计 ~30% 单线程 throughput。**D-401-revM lazy decay 评估**（B2 [实现] 起步前 lock）：每个 InfoSet entry 内存中存 `(value, last_update_count_t)` tuple；`current_strategy(I)` query 时延迟应用 decay `value × Π_{τ=last_t..current_t} (τ-1)/τ = value × (last_t-1) / (current_t-1)` 一次性 catch-up。lazy 实现复杂度高但 throughput 收益 30%+，stage 4 batch 2-4 评估翻面。 |
| D-436 | regret / strategy_sum bincode 序列化 | **继承 stage 3 D-327** bincode 1.x little-endian + varint integer encoding + HashMap sorted-by-InfoSetId。stage 4 checkpoint schema 扩展（D-470）增加 6-traverser 维度（每 checkpoint 含 6 套 RegretTable + 6 套 StrategyAccumulator）。 |
| D-437 | query API：current_strategy / average_strategy | **继承 stage 3 D-328**：`RegretTable<NlheGame6>::current_strategy(&self, info_set: &InfoSetId, n_actions: usize = 14) -> Vec<f64>` + `StrategyAccumulator<NlheGame6>::average_strategy(...)`。stage 4 新增 `Trainer::current_strategy_for_traverser(&self, traverser: PlayerId, info_set: &InfoSetId) -> Vec<f64>`（6-traverser routing），具体签名锁在 batch 5 `pluribus_stage4_api.md` API-430。 |
| D-438 | RegretTable 跨 traverser 隔离强约束 | **不允许跨 traverser InfoSet 共享 read/write**：traverser 0 的 `RegretTable[0]::current_strategy(I)` 不读取 `RegretTable[1]` 的内容。该约定让多线程 alternating traverser 路径 thread-safe 自动满足（thread 0 写 traverser_index_0 的 table，thread 1 写 traverser_index_1 的 table，互不冲突）。**实现**：`EsMccfrTrainer` 持有 `regret_tables: [RegretTable<NlheGame6>; 6]` 数组（编译期固定 6 长度）。 |
| D-439 | 数值容差监控（warn vs panic） | **继承 stage 3 D-329**：训练循环每 `10⁶` update 抽样 `1K` 个 InfoSet 检查 `|Σ_a σ(I, a) - 1| < 1e-9`（D-330 stage 3 容差继承）；超限触发 `tracing::warn!`（非 panic）。F3 [报告] full sweep 严格断言超限 0 case。 |

---

## 5. 训练规模与 Checkpoint Cadence（D-440..D-449）

| 编号 | 决策项 | 选定值 |
|---|---|---|
| D-440 | first usable blueprint 训练规模 | **`1,000,000,000`（`10⁹`）次 sampled decision update**（path.md §阶段 4 字面）。**目的**：打通全流水线 + 工程稳定性 + 监控告警 + LBR/Slumbot 评测桥接 + 初步消融对照。**不具备实战质量**（path.md 字面）。**训练时长预算**：D-490 SLO 4-core 15K update/s = 10⁹ / 15K = ~18.5 h；32-vCPU AWS c7a.8xlarge 20K update/s = 10⁹ / 20K = ~14 h。**stage 4 A0 [决策] 锁定 first usable 单次连续运行 ≤ 24 h**（与 D-461 24h continuous run 字面对齐），可能需要 D-491 host 选型支持。 |
| D-441 | production blueprint 训练规模 | **`100,000,000,000`（`10¹¹`）次 sampled decision update**（path.md §阶段 4 字面，Pluribus 论文规模）。**训练时长预算**：10¹¹ / 20K update/s ≈ 1400 h ≈ 58 days AWS c7a.8xlarge 连续运行（spot instance 必然中断，需要 on-demand 长 wall-time + 多次 checkpoint resume）。**stage 4 主线 A0..F3 验收 only requires first usable 10⁹**；production 10¹¹ 训练 **deferred 到 stage 4 F3 [报告] 闭合之后**由用户授权 D-441-rev0 启动；最终 production blueprint artifact 作为 stage 5 → 6 切换输入。**stage 4 F3 [报告] 验收清单显式分离**：first usable 验收 13 项门槛全 PASS（D-450..D-490 配套）；production 验收作为 carve-out carry-forward 到 stage 5 起步并行清单。 |
| D-442 | checkpoint cadence（first usable）| **每 `10⁸` update 写一次完整 checkpoint**（first usable 10⁹ update 共写 10 次）；每次 checkpoint 大小估计 30-50 GB（6 traverser × InfoSet 10⁶-10⁷ × 14 action × f64 × 2 + header）。**写入磁盘位置**：D-447 锁定（local NVMe scratch + S3 sync）。 |
| D-443 | checkpoint cadence（production）| **每 `10⁹` update 写一次完整 checkpoint**（production 10¹¹ update 共写 100 次）；间隔放宽到 ~14 h（与 first usable cadence 等价 wall-clock 间隔），avoid checkpoint write IO 占据训练 throughput。**最后一次 checkpoint** 是 final blueprint artifact，进 GitHub Release。 |
| D-444 | checkpoint resume 协议 | **继承 stage 3 D-350 full snapshot 模式**（不引入 incremental delta）。Resume 时校验：(a) `bucket_table_blake3` 匹配（D-425）；(b) `trainer_variant = ESMccfrLinearRmPlus`（D-470 stage 4 schema 扩展）；(c) `game_variant = Nlhe6Max`；(d) `warmup_complete` flag 状态（D-409）+ `update_count` 状态 + 6-traverser RegretTable/StrategyAccumulator full snapshot byte-equal。 |
| D-445 | resume BLAKE3 round-trip 不变量 | **继承 stage 3 D-350 + D-462**：训练 N update → 保存 checkpoint → 进程退出 → 加载 checkpoint → 继续训练 M update → 最终 6 套 RegretTable + 6 套 StrategyAccumulator BLAKE3 与不中断对照训练 byte-equal。stage 4 F1 [测试] `tests/checkpoint_round_trip_6max.rs` 落地 5 个测试覆盖（继承 stage 3 D1 [测试] 模式扩展到 6-traverser + 14-action）。 |
| D-446 | warm-up phase checkpoint 兼容 | **D-409 warm-up phase 1M update**：warm-up 内 checkpoint 字段含 `warmup_complete: bool = false`；warm-up 后 checkpoint `warmup_complete = true`。Resume 时按 flag 路由 trainer 实现（warm-up 走 stage 3 standard CFR + RM，warm-up 后走 stage 4 Linear MCCFR + RM+）。**stage 4 checkpoint 加载 stage 3 stage3-v1.0 tag 训练的 1M update SimplifiedNlheGame anchor**：**不兼容**（game_variant mismatch — SimplifiedNlheGame ≠ Nlhe6Max），`TrainerMismatch { expected: Nlhe6Max, got: SimplifiedNlhe }` 拒绝 — D-356 + stage 4 D-444 字面继承。 |
| D-447 | checkpoint 存储位置 | **A0 lock 主路径 local NVMe scratch**（AWS c7a.8xlarge 配 2x 300 GB NVMe SSD，足够 100 个 checkpoint × 30-50 GB / 2 = 1.5 TB）；**辅助路径 S3 sync**（每 10⁸ update checkpoint 写完 trigger background `aws s3 cp` upload 到 `s3://pluribus-stage4-checkpoints/`）。**S3 sync 失败不阻塞训练**（trainer 仅 emit warn，下次 checkpoint 重试）。**D-447-revM 候选**：GCS / Backblaze B2 替代 S3 cost 优化，由 batch 2-4 / B2 决策；stage 4 A0 选 AWS S3 起步与 D-491 host 选型耦合。 |
| D-448 | checkpoint 保留策略 | **保留最近 5 个 + 全部 milestone**：milestone = 10⁸/3×10⁸/5×10⁸/10⁹ update（first usable 4 个 sample point，与 D-449 LBR 采样对齐）。其它 auto-save checkpoint 每 5 个轮换覆盖。`tools/train_cfr.rs --keep-last N` flag 覆盖默认。**production 训练**保留 10⁹/10¹⁰/5×10¹⁰/10¹¹ 4 个 milestone（4 × 50 GB = 200 GB）+ 最新 5 个（5 × 50 GB = 250 GB） + S3 全量。 |
| D-449 | checkpoint metadata schema 扩展 | **stage 3 D-350 96-byte header 扩展到 stage 4 128-byte header**：新增 `traverser_count: u8 = 6` + `linear_weighting_enabled: u8 ∈ {0=stage3, 1=stage4}` + `rm_plus_enabled: u8 ∈ {0=stage3, 1=stage4}` + `warmup_complete: u8 ∈ {0, 1}` + `pad: [u8; 28] = 0` 字段。`Checkpoint.schema_version: u32 = 2`（stage 3 schema_version=1 → stage 4 schema_version=2 升级 — 破 stage 3 checkpoint backward compatibility，**显式不向前兼容**），stage 4 checkpoint load 时校验 schema_version ≥ 2，stage 3 trainer 加载 stage 4 checkpoint → `SchemaMismatch { expected: 1, got: 2 }` 拒绝；stage 4 trainer 加载 stage 3 checkpoint → `SchemaMismatch { expected: 2, got: 1 }` 拒绝。 |

---

## 6. LBR Exploitability（D-450..D-459）

| 编号 | 决策项 | 选定值 |
|---|---|---|
| D-450 | LBR (Local Best Response) 算法 | **Lisý & Bowling 2017 AAAI workshop paper 原始算法**。每决策点对 traverser 枚举 local best response（call / fold / 几个 raise sizes from 14-action 集合），假设之后所有玩家走 blueprint。LBR 是真实 exploitability 的 **upper bound**（不是 lower bound）；LBR 下降 = blueprint 抗剥削能力上升。**算法核心**：(a) 选 1 个 LBR-player；(b) game tree 上对 LBR-player 决策点枚举 14 action，对 LBR-player 自己之后的决策点用 myopic best response（不向后看），对 opponents 用 blueprint strategy；(c) 取所有候选 LBR strategy 中 EV 最大者作为 LBR best response value。 |
| D-451 | LBR 阈值 | **first usable**: LBR 上界 `< 200 mbb/g`（先允许略松作为 first usable sanity）；**production**: LBR 上界 `< 100 mbb/g`（path.md §阶段 4 字面阈值）。**stage 4 F3 [报告] 仅验收 first usable 200 mbb/g 阈值**；production 100 mbb/g 阈值 deferred 到 D-441-rev0 production 训练完成时验收。 |
| D-452 | LBR 计算频率 | **first usable 训练期间每 `10⁷` update 计算一次 LBR**（first usable 10⁹ update 内共计算 100 次）；曲线必须**单调非升**（允许相邻两次 ±10% 噪声，连续 3 个采样点 trend up 触发 trainer 告警，与 D-470 监控告警耦合）。**LBR 计算路径**：sample 1000 hand → 每 hand 选 1 个 LBR-player → 计算 LBR best response value → average over 1000 hand & 6 LBR-player → mbb/g 单位输出（继承 D-461 单位）。 |
| D-453 | LBR 实现选型 | **A0 锁定 Rust 自实现** + OpenSpiel Python 对照作为 sanity check。**理由**：(a) `pyo3` Rust ↔ Python bridge stage 3 PokerKit 对照实测 cross-language data marshaling overhead `~10%`（继承 stage 2 §C-rev1 carve-out），LBR 100 个采样点 × 1000 hand × 6 LBR-player = `6 × 10⁵` LBR computation per training run，cross-language overhead 不可承受；(b) Rust 自实现可与 stage 3 `BestResponse` trait 复用（继承 D-344 输出格式 + D-348 性能 SLO），workload 与 stage 3 Kuhn / Leduc backward induction 同型；(c) OpenSpiel `algorithms/exploitability_descent.py` 实现作为 stage 4 F3 [报告] 一次性 sanity check（与 stage 3 D-366 模式同型），不在 stage 4 主线训练路径调用。 |
| D-454 | LBR 性能 SLO | **6-player NLHE LBR computation P95 `< 30 s`** for 1000 hand × 6 LBR-player（候选机器 4-core EPYC）。stage 4 训练 100 个 LBR 采样点共耗 `~50 min`，与训练总 wall time 14-18 h 占比 `~5%`，可接受。LBR computation 走 `--release --ignored` profile（与 stage 3 perf SLO 同型）。 |
| D-455 | LBR myopic best response horizon | **myopic horizon = 1 决策点**（继承 Lisý & Bowling 2017 字面 LBR）。LBR-player 在第 1 个决策点选 best response，之后所有 LBR-player 决策点走 blueprint（避免 LBR upper bound 退化为真实 exploitability — 真实 exploitability 计算不可行）。horizon = 0（pure blueprint）= no LBR；horizon = ∞ (full BR) = full exploitability 不可计算。 |
| D-456 | LBR action set | **14-action**（继承 D-420 PluribusActionAbstraction）。LBR-player 在决策点枚举 14 action 取 max EV action 作为 best response。**off-tree LBR action**（如 0.6×pot raise）不在 LBR 集合内 — off-tree handling 是 stage 6c 范围（path.md §阶段 6c 字面 pseudo-harmonic mapping 等）。 |
| D-457 | LBR 对照 OpenSpiel sanity | **F3 [报告] 一次性接入 OpenSpiel `algorithms/exploitability_descent.py`**：stage 4 first usable 训练完成的 blueprint 输出 OpenSpiel-compatible policy 文件 → OpenSpiel 计算 LBR 上界 → 与我们 Rust 自实现 LBR 上界对照。**容差**：mbb/g 差异 `< 10%`（OpenSpiel 与我们实现可能有 myopic horizon 细节差异 + 不同采样数 + 不同 LBR action set 范围，10% 是合理 trust threshold）。**OpenSpiel LBR 差异 ≥ 10%** 触发 F3 [报告] 标注 reference difference，不阻塞 stage 4 出口（继承 stage 3 D-365 OpenSpiel 收敛失败 P0 但数值差异不阻塞 模式）。 |
| D-458 | LBR best response action 输出 | **`(lbr_strategy: HashMap<InfoSetId, Vec<f64>>, lbr_value_mbbg: f64)`**：strategy 是 one-hot per InfoSet（lbr-player 决策点 best action 概率 1，其它 0），value 是 LBR 上界 mbb/g。让 LBR 输出格式与 stage 3 `BestResponse` 同型（D-344 模式继承到 stage 4）。 |
| D-459 | LBR cross-traverser 报告 | **每 traverser 独立 LBR 上界**（6 个数字，D-414 字面 6 traverser 不共享 strategy）+ 6-traverser average LBR 上界（单数字，作为 stage 4 F3 主验收口径）。**主验收门槛**：6-traverser average LBR `< 200 mbb/g` first usable；6-traverser **任一** traverser LBR `> 500 mbb/g` 视为虚假通过（stage 4 F3 [报告] §carve-out 已知偏离 标注）。 |

---

## 7. Slumbot / 开源 Bot 对战评测（D-460..D-469）

| 编号 | 决策项 | 选定值 |
|---|---|---|
| D-460 | 对手选型 | **A0 锁定 Slumbot HU NLHE bot**（http://www.slumbot.com/，Eric Jackson 2017 AAAI Computer Poker Competition winner，公开 HTTP API）。6-max blueprint 退化到 HU 1v1 对战 Slumbot（n_seats=6 走 n_seats=2 退化分支，D-416 lock）。**理由**：(a) Slumbot HU API 公开稳定，HU NLHE 评测协议成熟；(b) 6-max 同等水平开源 bot 不存在（OpenSpiel 内置 6-max baseline 是 random / call-station 等弱基线，不达 Slumbot 水平）；(c) HU 评测 mbb/g 误差 standard error 比 6-max 评测小 3× 左右（HU duplicate dealing + 2-player 方差缩减效果远好于 6-player）。**stage 4 F3 [报告] 主验收走 Slumbot HU**；自训练 5 副本 6-max self-play 锦标赛作为辅助验收 ablation。 |
| D-461 | 100K 手评测协议 | **path.md §阶段 4 字面 100K 手不输 + 95% CI 不显著为负**。**stage 4 first usable 阈值**：`mean ≥ -10 mbb/g` 且 95% CI 下界 `≥ -30 mbb/g`（允许略弱于 Slumbot 作为 first usable sanity 不显著输）。**stage 4 production 阈值**：`mean ≥ 0 mbb/g`（不输）+ 95% CI 下界 `≥ -10 mbb/g`。**duplicate dealing**：每 hand 走 2 方向（blueprint = SB then BB / Slumbot = SB then BB），降方差。**重复 5 次取均值**：固定 seed 5 套 5 × 100K hand → 5 次评测 → 取 mean + standard error。 |
| D-462 | mbb/g 单位与置信区间计算 | **mbb/g = milli big blinds per game**（1 mbb = 0.001 BB）。100K 手 6-max NLHE 的 mbb/g standard error 经验值约 `5-10 mbb/g`（HU duplicate dealing 后约 `2-5 mbb/g`），95% CI = `mean ± 1.96 × SE`。**stage 4 F3 [报告] 输出**：`mean`, `standard_error`, `95% CI [low, high]`, `N hands`, `Sharpe ratio = mean / SE`（参考统计量）。 |
| D-463 | Slumbot HTTP API bridge | **`SlumbotBridge` Rust struct + tokio HTTP client**（候选 `reqwest` 0.11，stage 4 candidate dependency）。Slumbot API endpoint `http://www.slumbot.com/api/...`（具体 endpoint 在 batch 5 [api] / F2 [实现] 起步前 lock — Slumbot 可能 rate-limit、可能需要 API key、可能离线维护）。**stage 4 A0 [决策] 锁定 Slumbot 在线 API 主路径**；**D-463-revM 候选**：Slumbot 不可用时切 OpenSpiel-trained HU baseline （F2 起步前 lock）。 |
| D-464 | Slumbot evaluation profile | **`tests/slumbot_eval.rs::*`**（release + `--ignored` 显式触发）。CI nightly 跑 1 次 100K hand 评测（约 wall time 12-24 h，依 Slumbot API rate-limit）。stage 4 F3 [报告] 主验收 5 × 100K hand 实测耗 ~5 × 18 h = 90 h（4 days），可与 production 训练并行（不阻塞 stage 4 F3 closure）。 |
| D-465 | Slumbot 评测失败的 P0 / carve-out 边界 | **Slumbot API 不可用 / 5 次评测 < 5 完成（< 3 次完成视为 evaluation infrastructure fail）** → stage 4 carve-out（D-463-revM 切 OpenSpiel HU baseline 替代）。**Slumbot 评测完成但 first usable 阈值 `mean ≥ -10` 未达** → **stage 4 P0 阻塞 carve-out**（要求 stage 4 F3 [报告] §carve-out 已知偏离 列入 + carry-forward 到 stage 4 §F3-rev1 / stage 5 起步并行清单，不阻塞 stage 5 起步）。**注**：stage 3 §8.1 第 1 条 D-361 NLHE 双 fail 已建立 "F3 closure 可携带已知偏离" 同型 carve-out 模式。 |
| D-466 | 5-副本 6-max self-play 锦标赛（辅助验收） | **5 份独立 stage 4 first usable blueprint × 不同 seed**（{seed_0, seed_1, ..., seed_4}）→ 6-max self-play 1M hand → 计算每副本平均 mbb/g + standard error。**目的**：辅助验收 first usable blueprint 一致性 — 5 副本之间 mbb/g 应统计上 indistinguishable（`|delta| < 10 mbb/g`）；显著差异表明 blueprint 训练 stochastic noise 过大（CFR converge 未充分）。**Stage 4 主验收不强制**（path.md §阶段 4 字面未提及 multi-seed self-play），列入 stage 4 F3 [报告] §附录可选验收。 |
| D-467 | 6-max baseline opponent 集合（path.md §阶段 4 字面） | **3 类 baseline**（D-480 锁定）：(1) random opponent（均匀分布所有 legal action）；(2) call-station（99% call / 1% fold）；(3) tight-aggressive (TAG)（preflop top 20% range raise，postflop 70% c-bet，其它 fold）。**评测**：1M 手 6-max 评测 5 blueprint copies + 1 baseline opponent；blueprint 必须 `mean ≥ +500 / +200 / +50 mbb/g` 对 random / call-station / TAG（D-480 阈值）。 |
| D-468 | head-to-head 评测 seed 管理 | **固定 seed 序列**：每次 100K hand 评测固定 master seed `s`，blueprint actions seed = `s` / Slumbot replies seed = Slumbot 自管（black box）；duplicate dealing 内部 hand-level seed = `hash(s, hand_id)`（继承 stage 1 D-228 sub-stream 模式）。重复 5 次评测使用 master seeds = `{42, 43, 44, 45, 46}`（stage 4 batch 2-4 / B2 [实现] / F2 起步前若调整需 D-468-revM）。 |
| D-469 | head-to-head 评测的 fold equity 校验 | **sanity check**：blueprint vs Slumbot 评测的 fold% / showdown win% / preflop aggression frequency 等 game-stage metrics 必须落在 known healthy range（Slumbot 公开评测平均 fold rate 32-36% / showdown ratio 25-30% / preflop 3-bet 6-10%）。stage 4 F3 [报告] 输出 game-stage metrics 表 + 与 Slumbot 公开 mean 对照；显著偏离（如 blueprint 5% fold rate 暗示 over-aggressive 病态）触发 F3 [报告] §carve-out 标注。 |

---

## 8. 多人 CFR 收敛监控（D-470..D-479）

| 编号 | 决策项 | 选定值 |
|---|---|---|
| D-470 | average regret growth rate 监控 | **每 `10⁵` update 采样一次 `max_I R̃_t(I) / sqrt(T)` (over all touched InfoSets)**，曲线必须 **非递增** 趋势（允许相邻 ±5% 噪声）。**线性增长**（`R̃_t ∝ T`，比 `sqrt(T)` 快）= P0 阻塞 bug（暗示 trainer 实现错误 / Linear weighting bug / RM+ clamp 失效）；连续 5 个采样点 trend up（`r_t > 1.05 × r_{t-10⁵}`） → trainer `metrics()` 返回告警 → CLI `tools/train_cfr.rs` 决策是否 abort。 |
| D-471 | 策略 entropy 监控 | **每 `10⁵` update 采样一次 `H(σ_t) = - Σ_I Σ_a σ_t(I, a) × log σ_t(I, a)`** averaged over reachable InfoSets。Entropy 应在训练初期高（接近 `log(14) ≈ 2.64` for 14-action uniform）+ 训练后期单调下降（策略集中到 dominant actions）。**Entropy 突然回升 ≥ 5% 连续 3 采样点** = blueprint oscillation 候选信号，触发 trainer warn（不 abort）。 |
| D-472 | 动作概率震荡幅度监控 | **每 `10⁵` update 采样一次 `Σ_I Σ_a |σ_t(I, a) - σ_{t-10⁵}(I, a)|`** averaged over reachable InfoSets（average strategy 变化量绝对值）。震荡幅度应单调下降；连续 5 个采样点震荡幅度增加 = 训练异常告警。 |
| D-473 | 监控告警实现路径 | **trainer 不主动 abort**（让 CLI / 用户决策）。Trainer 提供 `metrics() -> &TrainingMetrics` 公开 read-only 接口（API-440 lock，batch 5 [api] 落地），含字段：`avg_regret_growth_rate: f64` / `policy_entropy: f64` / `policy_oscillation: f64` / `peak_rss_bytes: u64` / `update_count: u64` / `wall_clock_seconds: f64` / `last_alarm_kind: Option<AlarmKind>`。CLI 每 `10⁵` update 拉取 `metrics()` 输出训练日志 + 触发告警时显式标记。 |
| D-474 | 训练日志格式 | **JSONL 行格式**（每 `10⁵` update 一行 JSON），写入 `--log-file PATH`（默认 stdout）。字段：`{"t": u64, "update_count": u64, "wall_clock_seconds": f64, "avg_regret_growth": f64, "policy_entropy": f64, "policy_oscillation": f64, "peak_rss_bytes": u64, "alarms": [...]}`。**JSONL 解析**：F3 [报告] 用 Python `pandas` 读 JSONL → 输出收敛曲线图 + 监控指标统计。**JSONL not BLAKE3 byte-equal**（浮点格式化跨 host 漂移），仅作为人类可读训练 log，不进 BLAKE3 anchor 集合（continued from stage 3 D-327 政策）。 |
| D-475 | reachable InfoSet 估算 | **每 `10⁵` update 抽样 1000 random hand → 走 blueprint strategy → 记录访问到的 InfoSet count**。D-470 / D-471 / D-472 监控指标的 "reachable InfoSet" 集合按这个 sample-based 估算（vs 全 InfoSet enumeration 不可行）。Sample noise estimated `~5%` 对 monotonicity check 不影响。 |
| D-476 | 监控成本预算 | **监控开销 ≤ 5% 单线程 throughput**（D-490 SLO 5K update/s = 200 μs/update；监控每 `10⁵` update 1 次，分摊 = ~5 ns/update，远低于 5%）。**主要成本**：reachable InfoSet 抽样 1000 hand × ~50 InfoSet/hand × 14 action × hash lookup ≈ 5K hash lookup / 监控点 ≈ 1 ms / 10⁵ update = 1e-5 / update = 0.005% overhead。 |
| D-477 | 监控指标 baseline 实测 | **stage 4 F3 [报告] 必须输出 first usable 训练全程 D-470 / D-471 / D-472 三条曲线**（first usable 10⁹ update / 监控点 10⁵ = 10⁴ data point per metric × 3 metric = 30K data point）+ 监控 vs LBR 曲线相关性分析（LBR 100 点 D-452 sample 与 monitoring 10⁴ 点 D-470 sample 时间对齐校验）。 |
| D-478 | EV sanity check（零和约束） | **训练后期每 traverser EV 应满足 `Σ_traverser EV(traverser) ≈ 0` (within `1e-3 mbb/g` precision)**（6-player 零和约束，继承 stage 3 D-332 模式）。**stage 4 训练 1M update warm-up 后**每 `10⁸` update 抽样 1000 hand 计算 6-traverser EV sum → 超 `1e-3 mbb/g` → 触发 P0 阻塞告警（暗示 cfv 计算 / payoff / sampling importance weight 某处错误）。 |
| D-479 | 告警 stack trace + InfoSet dump | **trainer 告警时 dump 当前 update_count + 最大 regret growth InfoSet ID + 该 InfoSet 的 regret/strategy 状态** to `--alarm-dump-dir PATH`，便于事后定位（特别是 RegretTable HashMap bucket key collision、Linear discounting decay factor 算错、RM+ clamp 误触发 等 silent bug 复现）。 |

---

## 9. Baseline Sanity Check（D-480..D-489）

| 编号 | 决策项 | 选定值 |
|---|---|---|
| D-480 | 3 类 baseline opponent 定义 | **(1) random opponent**: 决策点均匀采样 legal action（14-action 14 路径均匀分布；fold 也按 legal 与否包含/排除）；**(2) call-station opponent**: 99% call/check（如 fold 是 legal 但显然劣选），1% 随机其它（避免 always-call 死局）；**(3) tight-aggressive (TAG) opponent**: preflop top 20% range raise（`KK+ / AKs+` 等 hard-coded range，stage 4 batch 2-4 实测落地具体 range），其它 fold；postflop 70% c-bet（按 pot 比例），未 c-bet 走 check-fold；turn / river top hands 加倍 aggression。 |
| D-481 | 1M 手评测协议 | **6-player 4-blueprint + 2-opponent**（5 个 seat 中 4 个 blueprint copies + 1 opponent seat，position 轮转每 hand 左移；6 个 seat 1 个 opponent 验证 blueprint vs opponent 整体表现）。1M 手 + duplicate dealing + 固定 seed `[42, 43, 44]` 重复 3 次取均值。**stage 4 first usable 阈值**：blueprint `mean ≥ +500 / +200 / +50 mbb/g` 对 random / call-station / TAG（95% CI 下界 `> 0`，必要非充分条件）。 |
| D-482 | 阈值理由 | **+500 mbb/g vs random**：random opponent 不学习，blueprint 任何 above-50% strategy 都该轻松 +500 mbb/g（参考：bot vs random 实测通常 +1000 to +3000 mbb/g 量级，500 是 floor）。**+200 mbb/g vs call-station**：call-station 不弃牌，blueprint 用 value bet thin 大量 thin value，参考量级 +500-1500 mbb/g，200 是 floor。**+50 mbb/g vs TAG**：TAG 是 imperfect baseline 而非 weak opponent，blueprint 需要利用 TAG 的 over-fold + under-bluff 漏洞获利，参考量级 +100-300 mbb/g，50 是 floor。 |
| D-483 | baseline opponent 实现 | **`RandomOpponent` / `CallStationOpponent` / `TagOpponent` 实现 trait `Opponent6Max`**（候选 trait surface 在 batch 5 [api] lock）：`fn act(&mut self, state: &GameState, rng: &mut dyn RngSource) -> AbstractAction`。**所有 opponent 走 stage 1 `RngSource`** 显式注入（继承 D-027 / D-050）。 |
| D-484 | baseline 评测的 BLAKE3 byte-equal | **固定 seed × 固定 1M hand × 同 host 同 toolchain → blueprint vs baseline mbb/g 结果 byte-equal**（继承 stage 1 D-051 / stage 2 / stage 3 D-362 determinism 模式）。`tests/baseline_eval.rs::*` 包含 byte-equal regression assertion（5 个 hash anchor: vs random / vs call-station / vs TAG / cross-traverser-average / single-traverser-best）。 |
| D-485 | baseline 评测的运行时预算 | **1M hand × ~50 decision/hand × 14 action lookup × ~50 ns/lookup = ~35 s/run**（c7a.8xlarge 32-vCPU 估算）。3 类 baseline × 3 seed = 9 run × 35 s = ~5 min wall time；可并行 32 vCPU 跑 9 run < 1 min wall time。**Total baseline sanity 验收 1-2 min wall time**，可在 stage 4 F1 [测试] CI 触发。 |
| D-486 | baseline 中的 fold 边界 case | **call-station opponent 在 all-in 决策点也 100% call**（不论 stack ratio）；random opponent 在 all-in 决策点 14-action 14 路径均匀（含 fold）；TAG 在 all-in 决策点按 hand strength 决策（top 30% range call all-in，其它 fold）。该约定让 baseline 行为可预期 + 自然失败 in 极端 corner case。 |
| D-487 | baseline 在 6-player vs HU 行为差异 | **3 类 baseline 同样适用 HU 退化路径**（D-416）。Slumbot 评测路径上不替换 Slumbot 用 baseline（D-460 Slumbot 是 specific bot），但 stage 4 F3 [报告] 附录可输出 HU 退化 blueprint vs 3 类 baseline 1M hand 结果作为 cross-format 一致性 sanity check。 |
| D-488 | random baseline 的 "random" 定义边界 | **random opponent 是 uniform random over `legal_actions` 集合**，不是 uniform random over all 14 abstract action（部分 action 在某些 state 不 legal — 如 already all-in 不能 raise）。该约定让 random 至少满足 stage 1 rule legality；vs random 评测不会因 illegal action 误判。 |
| D-489 | baseline 验收 carve-out | **3 类 baseline 任一未达 D-481 阈值 → stage 4 F3 [报告] §carve-out 已知偏离**；**两类或以上未达** → stage 4 出口 P0 阻塞（blueprint 不达 "必要非充分" baseline 显著说明 blueprint 实战质量严重不足，不允许进 stage 5）。Random / call-station 必过；TAG 是 borderline（TAG 在 mbb/g `+30..+60` range 允许 ±20% noise，`+50 mbb/g` 阈值有 carve-out 余地，D-489 lock）。 |

---

## 10. 性能 SLO 与训练 Host（D-490..D-499）

| 编号 | 决策项 | 选定值 |
|---|---|---|
| D-490 | stage 4 单线程 / 4-core / 32-vCPU 训练吞吐 SLO | **单线程 release `≥ 5,000 update/s`**（继承 stage 3 D-361 单线程 SLO 退化 1/2，因 14-action + 6-player 路径长度增加 2-3×；stage 3 §8.1 第 (I)..(III) carry-forward 优化全做完后回升 ≥ 10K）。**4-core release `≥ 15,000 update/s`**（效率 ≥ 0.75，继承 stage 3 E2-rev1 vultr 4-core 1.78× efficiency 估计；stage 4 rayon thread pool + append-only delta merge 继承 stage 3 D-321-rev2 真并发模式）。**32-vCPU AWS c7a.8xlarge release `≥ 20,000 update/s`**（效率 ≥ 0.13；32-vCPU 受限于 HashMap contention + AWS Hyperthread sibling 竞争 + L3 cache pressure，实际 efficiency 估计 0.1-0.2 之间）。`tests/perf_slo.rs::stage4_*` 释放 + `--ignored` opt-in 触发，c7a.8xlarge 实测落地。 |
| D-491 | 训练 host 选型 | **A0 lock = AWS / vultr cloud on-demand**（不走 Hetzner bare-metal）：(a) **first usable 10⁹ update 主 host = AWS c7a.8xlarge**（32 vCPU EPYC 9R14 / 64 GB DDR5 / 2x 300 GB NVMe / on-demand $1.63/h on-demand × 14 h = $23 / first usable run）；备用 = vultr Bare Metal 64-core 单 socket EPYC $300/月；(b) **production 10¹¹ update 主 host = AWS c7a.16xlarge**（64 vCPU / 128 GB DDR5 / on-demand $3.27/h × 1400 h ≈ $4600 single run）；on-demand 必须长 wall-time，spot 中断风险不可承受；(c) **24h continuous run 验证 host = AWS c7a.4xlarge**（16 vCPU / 32 GB / on-demand $0.82/h × 24 h ≈ $20）。**memory project_stage4_6_path.md 路线**：stage 4 cloud on-demand → stage 5/6 Hetzner bare-metal 候选保留（cost 优化路径，stage 5 起步前 evaluate）。 |
| D-492 | LBR computation host 选型 | **复用 D-491 训练 host**（LBR 100 采样点分摊到训练 wall time 内 ~5% overhead，不需要独立 host）。Off-line LBR re-compute（如 F3 [报告] 起草时） 走 4-core vultr EPYC fallback（继承 stage 3 vultr host 模式）。 |
| D-493 | Slumbot evaluation host 选型 | **dev box 或 vultr 4-core** 即可（Slumbot HTTP API rate-limit 是瓶颈，本地 host CPU 不构成瓶颈）。100K hand × 5 次评测约 90 h wall time，rate-limit ≈ 1 hand/s 决定的；不需要 AWS 大实例。 |
| D-494 | baseline sanity host 选型 | **dev box 即可**（1-2 min wall time / D-485 估算）。CI nightly 跑 1 次。 |
| D-495 | 跨架构 SLO | **aspirational**：x86_64 ↔ aarch64 SLO 数值可能差异 ~30%（继承 stage 1 + stage 2 + stage 3 D-368 carve-out）；stage 4 SLO 仅在 D-491 lock 的 AWS x86_64 host 上强制达成。darwin-aarch64 / GitHub-hosted runner 上的实测仅供参考。 |
| D-496 | bench profile (criterion) | **`benches/stage4.rs`**（与 stage 1 / 2 / 3 同型扩展）：3 个 bench group — `stage4/nlhe_6max_es_mccfr_linear_rm_plus_update`（per-update throughput） / `stage4/lbr_compute_1000_hand`（LBR computation cost） / `stage4/baseline_eval_1000_hand`（baseline opponent evaluation cost）。CI 短路径走 `--warm-up-time 1 --measurement-time 1 --sample-size 10 --noplot`；nightly 跑全量。 |
| D-497 | 24h continuous run 验证 | **`tests/training_24h_continuous.rs::stage4_six_max_24h_no_crash`**（release + `--ignored` 显式触发）。**测试形态**：固定 seed + 24h wall time 启动 `EsMccfrTrainer::step` 循环 + 每 `10⁶` update 调用 `metrics()` 写日志 + 每 `10⁸` update 写 checkpoint + 退出前最后一次 checkpoint。**验收门槛**：24h 内无 panic / NaN / inf / RSS 增量 `< 5 GB` / 全部 checkpoint round-trip BLAKE3 byte-equal / 全部 monitoring 阈值未越界。host = D-491 AWS c7a.4xlarge（16 vCPU / 32 GB / 24 h × $0.82/h ≈ $20）。**stage 4 F1 [测试] / F2 [实现] 落地**。 |
| D-498 | nightly fuzz host | **AWS / vultr cloud on-demand**（继承 D-491）。CI nightly fuzz = stage 4 24h continuous + 全 panic / NaN 监控 + checkpoint round-trip + monitoring 阈值；连续 7 天无 panic 是 stage 4 carve-out（继承 stage 1 + stage 2 + stage 3 24h fuzz carve-out 模式）。 |
| D-499 | perf SLO test harness | **`tests/perf_slo.rs::stage4_*`**（与 stage 1 / 2 / 3 同型扩展）：release profile + `--ignored` 显式触发，CI nightly 跑 bench-full + 短 bench 在 push 时跑。失败时输出实测吞吐 + 上下文（host CPU / load average / vCPU count）。 |

---

## 11. 与阶段 1 / 阶段 2 / 阶段 3 决策的边界

阶段 4 不修改阶段 1 + 阶段 2 + 阶段 3 已锁定决策；任何冲突走 stage 1 / stage 2 / stage 3 `D-NNN-revM` 修订流程：

- **stage 1 决策继承**：D-001..D-103 全集 + D-NNN-revM 修订（D-033-rev1 / D-037-rev1 / D-039-rev1 / D-022b-rev1 / API-001-rev1 / API-004-rev1 / API-005-rev1）。stage 4 训练循环 sampling、chance node、tie-break 全部走 stage 1 `RngSource` 显式注入；任何 `rand::thread_rng()` 隐式调用是 stage 4 的 P0 阻塞 bug。14-action raise sizes 走 stage 1 `GameState::apply` byte-equal 不退化锚点（D-422）。
- **stage 2 决策继承**：D-200..D-283 全集 + stage 2 D-NNN-revM 修订（D-218-rev2 真等价类 / D-244-rev2 schema v2 / D-282 host-load carve-out / 等）。stage 4 14-action 通过新增 `PluribusActionAbstraction` impl stage 2 `ActionAbstraction` trait 第 2 个 impl，stage 2 trait surface 不变（D-220 lock）。InfoSet bit 编码扩展走 D-423 复用 IA-007 reserved 14 bits（首选）或破 stage 2 D-218 走 D-218-rev3 翻面 + 用户授权。
- **stage 3 决策继承 / 翻面**：D-300..D-379 全集 + stage 3 D-NNN-revM 修订（D-014-rev1 / D-022b-rev1 / D-321-rev1 / D-321-rev2 / D-317-rev1 / D-373-rev1 / D-373-rev2 / API-300-rev1 等）。stage 4 翻面：(a) **D-302** 非 Linear → stage 4 D-400 / D-401 Linear MCCFR；(b) **D-303** 标准 RM → stage 4 D-400 / D-402 RM+；(c) **D-304** 标准 strategy 累积 → stage 4 D-403 Linear weighted 累积；(d) **D-317-rev1** 6-bit mask → stage 4 D-423 14-bit mask（候选复用 IA-007 reserved）。**stage 3 字面 carry-forward (II) outcome vs external sampling**：stage 4 D-405 评估结论维持 external sampling，D-301 字面 lock 不翻面。
- **错误枚举追加不删**：stage 4 新增候选 `TrainerError::LinearWeightOverflow` / `TrainerError::LbrComputeFailed` / `TrainerError::SlumbotConnectionFailed` / `TrainerError::PreflopActionAbstractionMismatch`（batch 5 [api] 落地）。stage 1 `RuleError` / `HistoryError` + stage 2 `BucketTableError` / `EquityError` + stage 3 `CheckpointError` / `TrainerError` 只读不删。
- **浮点边界继承**：stage 1 规则路径无浮点 + stage 2 `abstraction::map` 子模块 `clippy::float_arithmetic` 死锁继续生效 + stage 3 `src/training/` 子模块允许浮点。stage 4 引入的 Linear weighting + RM+ + monitoring metrics 全部 f64，仅限 `src/training/` 子模块 + stage 4 新增 `src/lbr/` + `src/eval/` 子模块（candidate 路径，batch 5 [api] lock）。

---

## 12. 已知未决项（不阻塞 A1）

阶段 4 A0 [决策] batch 1 锁定算法变体 + 验收门槛骨架后仍有以下未决项；列入此处不阻塞 A1 [实现] 脚手架推进，但在 B2/C2 [实现] 真正消费时必须由后续 D-NNN-revM 落地：

- **D-401-revM Linear weighting decay 实现策略**（eager vs lazy） — A0 锁定 eager baseline；lazy 评估在 batch 5 [api] / B2 [实现] 起步前 lock。Lazy 实现复杂度高但 throughput 收益估计 30%+。
- **D-421-revM preflop 独立 action set** — A0 锁定不引入；若 stage 4 F1 [测试] LBR 实测 first usable LBR > 300 mbb/g 显著超 D-451 first usable 阈值 200 mbb/g，evaluate D-421-revM 翻面引入 preflop 独立 action set；具体在 F2 [实现] 起步前 + 用户授权 lock。
- **D-423-rev0 InfoSet bit 编码 14-action mask 具体方案** — A0 lock 首选 (a) 复用 stage 2 IA-007 reserved 14 bits；batch 5 [api] 验证 stage 2 api.md IA-007 reserved 14 bits 是否字面正确 + 锁定 D-423-rev0。
- **D-430-revM `FxHashMap` 替代 `std::HashMap`** — A0 锁定继承 stage 3 默认 SipHash；stage 4 batch 2-4 评估 `FxHashMap` 替代（继承 stage 3 §8.1 第 (II)/Y 项 carry-forward — 估计 10-20% throughput 收益）。
- **D-447-revM checkpoint 存储位置** — A0 lock 主路径 AWS S3 + local NVMe；stage 5 stage 起步前 evaluate GCS / Backblaze B2 cost 优化。
- **D-453-revM LBR 实现选型 OpenSpiel bridge fallback** — A0 lock Rust 自实现；如 Rust 实现 stage 4 F2 [实现] 落地遇到 LBR 算法 corner case 不可避免（如 myopic horizon = 2 corner case），考虑 D-453-revM 切 OpenSpiel bridge fallback。
- **D-463-revM Slumbot API 不可用 fallback** — A0 lock Slumbot HTTP API 主路径；如 stage 4 F2 [实现] 起步前 Slumbot API 不可用（rate-limit / 维护中 / API key gate），evaluate D-463-revM 切 OpenSpiel-trained HU baseline。
- **D-441-rev0 production 10¹¹ 训练触发** — A0 lock production 训练 deferred 到 stage 4 F3 [报告] 闭合后用户授权触发；stage 4 主线不阻塞。具体 host 选型 + budget approval + checkpoint cadence 在 D-441-rev0 lock。
- **D-409-revM warm-up 长度** — A0 lock warm-up 1M update；如 stage 4 F1 [测试] 实测 1M update 后切 Linear + RM+ 触发显著收敛震荡，evaluate D-409-revM 翻面 warm-up 长度。
- **stage 3 §8.1 carry-forward 7 项**：(I) perf flamegraph hot path / (II) outcome vs external sampling（D-405 A0 评估结论 maintain external，可能 batch 2-4 重新评估） / (III) stage 1 `GameState::apply` micro-opt 跨 stage 1 / (IV) stage 3 D-361-revM 阈值翻面（stage 4 D-490 替代，不直接翻面 stage 3 D-361 字面） / (V) stage 3 D-362 100M anchor 恢复 / (VI) stage 2 bucket quality 12 条 #[ignore] 转 active / (VII) stage 2 `pluribus_stage2_report.md` §8 carve-out 翻面 — 7 项 carry-forward 到 stage 4 主线 13 步 + 并行清单分流处理。

---

## 13. 决策修改流程

继承 stage 1 / stage 2 / stage 3 决策修改流程：

1. 任何决策修改在本文档以 `D-NNN-revM` 形式追加新条目（不删除原条目）+ `pluribus_stage4_decisions.md` §修订历史追加 entry。
2. 必要时 bump `Checkpoint.schema_version`（D-449 stage 4 schema_version = 2）或继承的 stage 1 `HandHistory.schema_version` / stage 2 `BucketTable.schema_version`。
3. 影响 API 签名走 `pluribus_stage4_api.md` API-NNN-revM 同 PR 流程 + `tests/api_signatures.rs` 同 PR 更新 trip-wire。
4. 跨 stage 边界修改（如 D-422 修改 stage 1 `GameState::apply` 或 D-423 修改 stage 2 `InfoSetId` 64-bit layout）需要：(a) 在被修改 stage 的 decisions.md 同 PR 追加 D-NNN-revM；(b) 在 stage 1 / stage 2 / stage 3 测试套件全 0 failed byte-equal 维持；(c) 用户书面授权（与 stage 3 D-022b-rev1 / stage 2 D-218-rev2 / stage 3 D-321-rev2 同型跨 stage carve-out 模式）。
5. 通知所有正在工作的 agent（在工作流 issue / PR 中显式标注）。

---

## 14. 决策 ↔ API 对应关系

| 决策号段 | 主要决策 | API 文档对应 |
|---|---|---|
| D-400..D-409 | Linear MCCFR + RM+ + warm-up + external sampling 选型 | API-400 (`Trainer` trait 扩展) / API-401 (`EsMccfrTrainer::with_linear_rm_plus()` builder) / API-405 (warm-up flag) |
| D-410..D-419 | 6-player NLHE 规则 + Game trait impl + traverser 协议 | API-410 (`NlheGame6` struct) / API-411 (`Game::VARIANT` 新枚举 `Nlhe6Max`) / API-412 (6-traverser routing) |
| D-420..D-429 | 14-action abstraction + InfoSet bit + bucket table 复用 | API-420 (`PluribusActionAbstraction` struct) / API-423 (InfoSetId 14-bit mask) / API-424 (bucket table 复用) |
| D-430..D-439 | 6-traverser RegretTable 扩展 + 内存上界 + 序列化 | API-430 (`RegretTable<NlheGame6>` + 6-traverser routing) / API-431 (RSS 监控接口) |
| D-440..D-449 | first usable / production 阈值 + checkpoint cadence + schema 扩展 | API-440 (`TrainingMetrics` struct) / API-441 (`Checkpoint` schema v2) / API-444 (resume protocol) |
| D-450..D-459 | LBR 算法 + 阈值 + 实现 | API-450 (`LbrEvaluator` struct) / API-453 (LBR 输出格式) / API-457 (OpenSpiel bridge sanity) |
| D-460..D-469 | Slumbot bridge + 100K 手协议 + duplicate dealing | API-460 (`SlumbotBridge` struct) / API-461 (`Head2HeadResult` struct) / API-462 (mbb/g 计算) |
| D-470..D-479 | 监控 metrics + 告警 + 训练日志 | API-470 (`TrainingMetrics` 字段) / API-473 (`AlarmKind` enum) / API-474 (JSONL 日志格式) |
| D-480..D-489 | 3 类 baseline opponent + 1M 手协议 + carve-out 边界 | API-480 (`Opponent6Max` trait) / API-483 (Random/CallStation/TAG impl) |
| D-490..D-499 | 单线程/4-core/32-vCPU SLO + AWS host + 24h continuous run | API-490 (`PerfSloHarness` 扩展) / API-497 (`Stage4ContinuousRun` test harness) |

---

## 参考资料

- Pluribus 主论文：https://noambrown.github.io/papers/19-Science-Superhuman.pdf §2 Algorithm / §S2 Training procedure / §S4 Real-time search
- Pluribus 补充材料：https://noambrown.github.io/papers/19-Science-Superhuman_Supp.pdf §S2 MCCFR algorithm / §S3 Action abstraction / §S5 Blueprint training cost
- Brown, Sandholm, "Solving Imperfect-Information Games via Discounted Regret Minimization"（AAAI 2019，Linear CFR / Linear discounting / Regret Matching+ 主要 reference）
- Tammelin, Burch, Johanson, Bowling, "Solving Heads-up Limit Texas Hold'em"（IJCAI 2015，CFR+ Cepheus / Regret Matching+ 公认实现）
- Brown, "Equilibrium Finding for Large Adversarial Imperfect-Information Games"（PhD 论文 2020，CMU，详细 Pluribus 实现细节）
- Lisý, Bowling, "Equilibrium Approximation Quality of Current No-Limit Poker Bots"（AAAI 2017 Workshop，LBR 原始论文）
- Burch, Schmid, Moravčík, Morrill, Bowling, "AIVAT: A New Variance Reduction Technique for Agent Evaluation in Imperfect Information Games"（AAAI 2018，AIVAT 方差缩减，stage 7 评测 prep）
- Lanctot, Waugh, Zinkevich, Bowling, "Monte Carlo Sampling for Regret Minimization in Extensive Games"（NeurIPS 2009，External Sampling MCCFR，继承 stage 3）
- Zinkevich, Bowling, Johanson, Piccione, "Regret Minimization in Games with Incomplete Information"（NeurIPS 2007，CFR 原始论文，继承 stage 3）
- Slumbot：http://www.slumbot.com/  Eric Jackson 2017 AAAI Computer Poker Competition HU NLHE bot
- OpenSpiel CFR / LBR / AIVAT Python 实现：https://github.com/google-deepmind/open_spiel/tree/master/open_spiel/python/algorithms

---

## 修订历史

本文档遵循与 `pluribus_stage1_decisions.md` §10 / `pluribus_stage2_decisions.md` §11 / `pluribus_stage3_decisions.md` §11 相同的"追加不删"约定。任何 D-NNN-revM 修订追加在本节，按时间倒序排列。

- **2026-05-14（A0 [决策] 起步 batch 2-4 落地）**：stage 4 A0 [决策] 起步 batch 2-4 落地 `docs/pluribus_stage4_decisions.md`（本文档）骨架 — D-400..D-499 全决策表项 + §1 算法详解（Linear MCCFR + RM+ 伪代码 + 关键不变量 + 收敛性） + §11 stage 1/2/3 边界 + §12 已知未决项 10 条（D-401-revM / D-421-revM / D-423-rev0 / D-430-revM / D-447-revM / D-453-revM / D-463-revM / D-441-rev0 / D-409-revM + stage 3 §8.1 carry-forward 7 项） + §13 决策修改流程 + §14 决策 ↔ API 对应关系。**核心 lock**：(a) **D-400 / D-401 / D-402 / D-403** Linear MCCFR + RM+ + Linear weighted strategy sum（翻面 stage 3 D-302 / D-303 / D-304 stage 3 字面 deferred）；(b) **D-409** warm-up 1M update（前 1M update 走 stage 3 standard CFR + RM 保 BLAKE3 byte-equal anchor 不破）；(c) **D-410 / D-411 / D-412** 6-player NLHE + alternating traverser + 6 套独立 RegretTable；(d) **D-420** Pluribus 14-action（fold/check/call/10 raise sizes/all-in）；(e) **D-424** bucket table 复用 stage 3 v3 production artifact 528 MiB；(f) **D-440 / D-441** first usable 10⁹ + production 10¹¹ 双阈值分离（production deferred 到 stage 5 起步并行清单）；(g) **D-449** Checkpoint schema_version 1→2 升级（不向前兼容）；(h) **D-450 / D-451** LBR < 200 first usable / < 100 production mbb/g；(i) **D-460 / D-461** Slumbot HU 退化路径 100K 手 mean ≥ -10 mbb/g first usable；(j) **D-470 / D-471 / D-472** 3 条独立监控 + warn-only（trainer 不主动 abort）；(k) **D-480 / D-481** 3 类 baseline 必要非充分；(l) **D-490 / D-491** 单线程 5K / 4-core 15K / 32-vCPU 20K update/s + AWS c7a.8xlarge host。本节首条由 stage 4 A0 [决策] batch 2-4 commit 落地，与 `pluribus_stage4_validation.md` §修订历史 + `pluribus_stage4_workflow.md` §修订历史 + `CLAUDE.md` "stage 4 A0 起步 batch 1-4 closed" 状态翻面同步。
