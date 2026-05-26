//! Action abstraction（API §1）。
//!
//! `AbstractAction` / `AbstractActionSet` / `ActionAbstractionConfig` / `BetRatio` /
//! `ActionAbstraction` trait + `DefaultActionAbstraction`。
//!
//! 不变量 AA-001..AA-008（含 AA-003-rev1 / AA-004-rev1）见
//! `docs/pluribus_stage2_api.md` §1；A1 阶段所有方法体走 `unimplemented!()`。

use thiserror::Error;

use crate::core::ChipAmount;
use crate::core::Street;
use crate::rules::action::Action;
use crate::rules::state::GameState;

use crate::abstraction::info::StreetTag;

/// 抽象动作。pot ratio 编码进 `Bet` / `Raise` 变体；apply 时取 `to`。
///
/// `Bet` 与 `Raise` 在构造时由 stage 1 `LegalActionSet`（LA-002 互斥）选定：
/// 本下注轮无前序 bet ⇒ `Bet`，已有前序 bet ⇒ `Raise`。该拆分让 `to_concrete()`
/// 无状态可调用（见 §7），同时 D-212 `betting_state` 字段在 `Bet` 与 `Raise`
/// 之间的转移无歧义（`Bet` 把 `Open` 推进到 `FacingBetNoRaise`；`Raise` 把任何
/// 状态推进到 `FacingRaise{1,2,3+}`）。
///
/// `ratio_label` 仅作为 InfoSet 编码区分性（D-207 / D-209），不参与 apply 计算。
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum AbstractAction {
    Fold,
    Check,
    Call {
        to: ChipAmount,
    },
    /// 本下注轮无前序 bet（`legal_actions().bet_range.is_some()`）。
    Bet {
        to: ChipAmount,
        ratio_label: BetRatio,
    },
    /// 本下注轮已有前序 bet（`legal_actions().raise_range.is_some()`）。
    Raise {
        to: ChipAmount,
        ratio_label: BetRatio,
    },
    AllIn {
        to: ChipAmount,
    },
}

/// pot ratio 标签的整数编码，避免 `f64` 进入 `Eq` / `Hash`。
///
/// 内部存 `ratio × 1000` 的 `u32`（D-200 默认值：`Half = 500`、`Full = 1000`）。
/// `ActionAbstractionConfig` 接受 `f64` 输入但内部规整为该整数表示。
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct BetRatio(u32);

impl BetRatio {
    pub const HALF_POT: BetRatio = BetRatio(500);
    pub const FULL_POT: BetRatio = BetRatio(1000);
    pub const TWO_POT: BetRatio = BetRatio(2000);

    /// 量化协议（D-202-rev1 / BetRatio::from_f64-rev1）：
    ///
    /// 1. **rounding mode**：bankers-rounding (half-to-even)，
    ///    `(ratio * 1000.0).round_ties_even() as i64`，再校验范围。
    /// 2. **合法范围**：`ratio ∈ [0.001, 4_294_967.295]`（含端点），量化后
    ///    `u32 ∈ [1, u32::MAX]`；越界（< 0.001 / > 4_294_967.295 / NaN / Inf /
    ///    负数 / 0.0）返回 `None`。
    /// 3. **重复处理**：本函数本身不去重；多输入量化到同一 milli 值由
    ///    `ActionAbstractionConfig::new` 检测，返回 `ConfigError::DuplicateRatio`。
    // `round_ties_even` is stable since Rust 1.77; project Cargo.toml
    // `rust-version = "1.75"` is conservative metadata while
    // `rust-toolchain.toml` pins the actual compiler to 1.95.0 (D-007).
    // The clippy `incompatible_msrv` lint fires on the MSRV metadata read;
    // suppressing it here keeps the IEEE-754 half-to-even semantics required
    // by the D-202-rev1 spec without bumping the Cargo.toml MSRV (a separate
    // policy decision).
    #[allow(clippy::incompatible_msrv)]
    pub fn from_f64(ratio: f64) -> Option<BetRatio> {
        if !ratio.is_finite() || ratio <= 0.0 {
            return None;
        }
        let raw = ratio * 1000.0;
        if !raw.is_finite() || raw < 0.0 || raw > u32::MAX as f64 {
            return None;
        }
        let rounded = raw.round_ties_even();
        if rounded < 1.0 || rounded > u32::MAX as f64 {
            return None;
        }
        Some(BetRatio(rounded as u32))
    }

    /// 返回内部整数表示（D-200，`milli = ratio × 1000`）。
    pub fn as_milli(self) -> u32 {
        self.0
    }
}

/// 抽象动作集合输出。顺序固定为 D-209：
/// `[Fold?, Check?, Call?, Bet(0.5×pot)? | Raise(0.5×pot)?, Bet(1.0×pot)? | Raise(1.0×pot)?, AllIn?]`
/// `?` 表示不存在则跳过；同一 ratio 槽位 `Bet` 与 `Raise` 互斥（由 stage 1
/// LA-002 保证）。
#[derive(Clone, Debug)]
pub struct AbstractActionSet {
    actions: Vec<AbstractAction>,
}

impl AbstractActionSet {
    pub fn iter(&self) -> std::slice::Iter<'_, AbstractAction> {
        self.actions.iter()
    }

    pub fn len(&self) -> usize {
        self.actions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }

    pub fn contains(&self, action: AbstractAction) -> bool {
        self.actions.contains(&action)
    }

    pub fn as_slice(&self) -> &[AbstractAction] {
        &self.actions
    }

    /// 消费 set，move 出内部 `Vec<AbstractAction>`（避免 `as_slice().to_vec()`
    /// 在 CFR `Game::legal_actions` 热路径上每节点多 alloc 一次 Vec）。
    pub fn into_actions(self) -> Vec<AbstractAction> {
        self.actions
    }
}

/// `ActionAbstractionConfig`：raise size 集合（D-202）。
/// `raise_pot_ratios` 长度 ∈ [1, 14]，每个元素 ∈ (0.0, +∞)。
#[derive(Clone, Debug)]
pub struct ActionAbstractionConfig {
    pub raise_pot_ratios: Vec<BetRatio>,
}

impl ActionAbstractionConfig {
    /// 默认 6-action 配置：`[BetRatio::HALF_POT, BetRatio::FULL_POT, BetRatio::TWO_POT]`。
    /// 3 个分数 raise size + Fold + Check/Call + AllIn → 单决策点最多 6 个抽象动作。
    pub fn default_6_action() -> ActionAbstractionConfig {
        ActionAbstractionConfig {
            raise_pot_ratios: vec![BetRatio::HALF_POT, BetRatio::FULL_POT, BetRatio::TWO_POT],
        }
    }

    /// 自定义构造。长度 / 范围越界 / 量化后 milli 重复均返回 `ConfigError`
    /// （见 §9 BetRatio::from_f64-rev1 量化协议；D-202-rev1）。
    pub fn new(raise_pot_ratios: Vec<f64>) -> Result<ActionAbstractionConfig, ConfigError> {
        let n = raise_pot_ratios.len();
        if !(1..=14).contains(&n) {
            return Err(ConfigError::RaiseCountOutOfRange(n));
        }
        let mut quantized: Vec<BetRatio> = Vec::with_capacity(n);
        for raw in raise_pot_ratios {
            let q = BetRatio::from_f64(raw).ok_or(ConfigError::RaiseRatioInvalid(raw))?;
            if quantized
                .iter()
                .any(|existing| existing.as_milli() == q.as_milli())
            {
                return Err(ConfigError::DuplicateRatio {
                    milli: q.as_milli(),
                });
            }
            quantized.push(q);
        }
        Ok(ActionAbstractionConfig {
            raise_pot_ratios: quantized,
        })
    }

    pub fn raise_count(&self) -> usize {
        self.raise_pot_ratios.len()
    }
}

/// 配置错误（D-202-rev1 含 `DuplicateRatio` 变体）。
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("raise_pot_ratios length out of range: expected [1, 14], got {0}")]
    RaiseCountOutOfRange(usize),

    #[error("raise pot ratio not positive finite: {0}")]
    RaiseRatioInvalid(f64),

    /// `BucketConfig::new` 越界：每条街 bucket 数应 ∈ [10, 10_000]（D-214）。
    #[error("bucket count out of range for {street:?}: expected [10, 10_000], got {got}")]
    BucketCountOutOfRange { street: StreetTag, got: u32 },

    /// 多个 `raise_pot_ratios` 元素经 `BetRatio::from_f64` 量化后落到同一 milli 值
    /// （D-202-rev1 / BetRatio::from_f64-rev1）。caller 责任去重，避免 D-209
    /// 输出顺序与 `raise_count()` 不一致。
    #[error("duplicate raise pot ratio after quantization: milli = {milli}")]
    DuplicateRatio { milli: u32 },
}

/// Action abstraction trait（API §1）。
pub trait ActionAbstraction: Send + Sync {
    /// 给定当前 `GameState`，返回抽象动作集合（D-200..D-209 全部 fallback 已应用）。
    fn abstract_actions(&self, state: &GameState) -> AbstractActionSet;

    /// off-tree action 映射（D-201 PHM stub；stage 2 仅占位实现，stage 6c 完整数值验证）。
    ///
    /// `real_to` 是对手实际下注的 `to` 字段（绝对金额，与 stage 1
    /// `Action::Bet/Raise { to }` 同语义）。
    fn map_off_tree(&self, state: &GameState, real_to: ChipAmount) -> AbstractAction;

    /// 配置只读访问。
    fn config(&self) -> &ActionAbstractionConfig;
}

/// 默认 5-action 抽象（D-200）。
pub struct DefaultActionAbstraction {
    config: ActionAbstractionConfig,
}

impl DefaultActionAbstraction {
    pub fn new(config: ActionAbstractionConfig) -> DefaultActionAbstraction {
        DefaultActionAbstraction { config }
    }

    pub fn default_6_action() -> DefaultActionAbstraction {
        DefaultActionAbstraction::new(ActionAbstractionConfig::default_6_action())
    }
}

impl ActionAbstraction for DefaultActionAbstraction {
    fn abstract_actions(&self, state: &GameState) -> AbstractActionSet {
        if state.current_player().is_none() {
            return AbstractActionSet {
                actions: Vec::new(),
            };
        }
        let la = state.legal_actions();
        let actor_seat = state
            .current_player()
            .expect("current_player checked above");
        let actor = &state.players()[actor_seat.0 as usize];
        let committed_this_round = actor.committed_this_round;
        let max_committed = state
            .players()
            .iter()
            .map(|p| p.committed_this_round)
            .max()
            .unwrap_or(ChipAmount::ZERO);
        let cap = la
            .all_in_amount
            .unwrap_or(committed_this_round + actor.stack);
        let pot_before = state.pot();
        // pot_after_call_size = pot_before + (max_committed - committed_this_round)
        let to_call_delta = if max_committed > committed_this_round {
            max_committed - committed_this_round
        } else {
            ChipAmount::ZERO
        };
        let pot_after_call = pot_before + to_call_delta;

        // D-209 顺序构建候选: Fold / Check / Call / Bet|Raise(0.5×) / Bet|Raise(1.0×) / AllIn
        let mut actions: Vec<AbstractAction> = Vec::with_capacity(6);

        // D-204：free-check 局面剔除 Fold
        if la.fold && !la.check {
            actions.push(AbstractAction::Fold);
        }
        if la.check {
            actions.push(AbstractAction::Check);
        }
        if let Some(call_to) = la.call {
            actions.push(AbstractAction::Call { to: call_to });
        }

        // D-205 / AA-003-rev1：每个 raise_pot_ratio 计算 candidate_to + first-match-wins fallback
        for &ratio in &self.config.raise_pot_ratios {
            // Skip if neither bet_range nor raise_range is available (no aggression possible).
            if la.bet_range.is_none() && la.raise_range.is_none() {
                continue;
            }
            let min_to = la
                .bet_range
                .map(|(min, _)| min)
                .or(la.raise_range.map(|(min, _)| min))
                .expect("bet_range or raise_range checked above");
            // candidate_to = max_committed + ceil(ratio_milli * pot_after_call / 1000)
            let milli = ratio.as_milli() as u64;
            let pot_chips = pot_after_call.as_u64();
            let scaled = (milli as u128) * (pot_chips as u128);
            let ratio_part_ceil = scaled.div_ceil(1000) as u64;
            let mut candidate_to = max_committed + ChipAmount::new(ratio_part_ceil);
            // ① floor to min_to
            if candidate_to < min_to {
                candidate_to = min_to;
            }
            // ② ceil to AllIn cap
            if candidate_to >= cap {
                // Will be handled by AllIn slot below; emit nothing here so AllIn priority
                // (AA-003-rev1 ②) absorbs the candidate.
                continue;
            }
            // ③ otherwise: Bet { to } / Raise { to }
            let candidate = if la.bet_range.is_some() {
                AbstractAction::Bet {
                    to: candidate_to,
                    ratio_label: ratio,
                }
            } else {
                AbstractAction::Raise {
                    to: candidate_to,
                    ratio_label: ratio,
                }
            };
            actions.push(candidate);
        }

        // AllIn slot
        if let Some(all_in_to) = la.all_in_amount {
            actions.push(AbstractAction::AllIn { to: all_in_to });
        }

        // AA-004-rev1 折叠去重（first-match-wins）：
        //   ① AllIn 优先：任意 Call/Bet/Raise 的 to == cap 折入 AllIn 槽，移除前者。
        //   ② Bet/Raise(0.5×) 与 Bet/Raise(1.0×) 同 to 时保留 ratio_label 较小的一份。
        //   ③ Call vs Bet/Raise 不会折叠（D-034 / D-035 严格不等）。
        if actions
            .iter()
            .any(|a| matches!(a, AbstractAction::AllIn { .. }))
        {
            actions.retain(|a| match a {
                AbstractAction::Call { to } => *to != cap,
                AbstractAction::Bet { to, .. } => *to != cap,
                AbstractAction::Raise { to, .. } => *to != cap,
                _ => true,
            });
        }
        // ② 同 to 的 Bet/Raise 去重，保留较小 ratio_label
        let mut deduped: Vec<AbstractAction> = Vec::with_capacity(actions.len());
        for action in actions {
            match action {
                AbstractAction::Bet { to, ratio_label } => {
                    if let Some(idx) = deduped
                        .iter()
                        .position(|a| matches!(a, AbstractAction::Bet { to: t, .. } if *t == to))
                    {
                        if let AbstractAction::Bet {
                            ratio_label: existing,
                            ..
                        } = deduped[idx]
                        {
                            if ratio_label.as_milli() < existing.as_milli() {
                                deduped[idx] = AbstractAction::Bet { to, ratio_label };
                            }
                        }
                    } else {
                        deduped.push(action);
                    }
                }
                AbstractAction::Raise { to, ratio_label } => {
                    if let Some(idx) = deduped
                        .iter()
                        .position(|a| matches!(a, AbstractAction::Raise { to: t, .. } if *t == to))
                    {
                        if let AbstractAction::Raise {
                            ratio_label: existing,
                            ..
                        } = deduped[idx]
                        {
                            if ratio_label.as_milli() < existing.as_milli() {
                                deduped[idx] = AbstractAction::Raise { to, ratio_label };
                            }
                        }
                    } else {
                        deduped.push(action);
                    }
                }
                _ => deduped.push(action),
            }
        }

        AbstractActionSet { actions: deduped }
    }

    fn map_off_tree(&self, state: &GameState, real_to: ChipAmount) -> AbstractAction {
        // D-201 PHM stub（issue #8 §出口）。stage 2 占位实现，stage 6c 替换为
        // Pluribus §S2 完整 pseudo-harmonic mapping。要求：相同 (state, real_to)
        // → 相同输出（确定性 + no-panic），数值正确性留 stage 6c。
        //
        // 算法：
        //   ① real_to ≥ cap                → AllIn { to: cap }
        //   ② real_to ≤ max_committed      → Call (或 Check / Fold 兜底)
        //   ③ 无 bet_range / raise_range   → Call / Fold 兜底（防御）
        //   ④ 否则 pick `raise_pot_ratios` 中 target_to 与 real_to 最接近的 ratio：
        //         target_to(r) = max_committed + ceil(r.milli × pot_after_call / 1000)
        //      tie-break：milli 较小者先（与 AA-004-rev1 同 to 折叠 ratio_label
        //      较小一致）。输出 `Bet | Raise { to: real_to, ratio_label }`（LA-002
        //      互斥：bet_range Some → Bet，否则 Raise）。

        let Some(actor_seat) = state.current_player() else {
            return AbstractAction::Fold;
        };
        let la = state.legal_actions();
        let actor = &state.players()[actor_seat.0 as usize];
        let committed_this_round = actor.committed_this_round;
        let max_committed = state
            .players()
            .iter()
            .map(|p| p.committed_this_round)
            .max()
            .unwrap_or(ChipAmount::ZERO);
        let cap = la
            .all_in_amount
            .unwrap_or(committed_this_round + actor.stack);

        // ① cap 上界：AllIn 优先。
        if real_to >= cap {
            return AbstractAction::AllIn { to: cap };
        }

        // ② 非 aggression 区间：real_to ≤ max_committed。
        if real_to <= max_committed {
            if let Some(call_to) = la.call {
                return AbstractAction::Call { to: call_to };
            }
            if la.check {
                return AbstractAction::Check;
            }
            return AbstractAction::Fold;
        }

        // ③ 无 bet/raise legal（terminal / all-in 跳轮 / 防御）。
        if la.bet_range.is_none() && la.raise_range.is_none() {
            if let Some(call_to) = la.call {
                return AbstractAction::Call { to: call_to };
            }
            if la.check {
                return AbstractAction::Check;
            }
            return AbstractAction::Fold;
        }

        // ④ 找最近 ratio。pot_after_call = pot() + (max_committed - committed_this_round)。
        let pot_before = state.pot();
        let to_call_delta = if max_committed > committed_this_round {
            max_committed - committed_this_round
        } else {
            ChipAmount::ZERO
        };
        let pot_after_call = pot_before + to_call_delta;

        let real_to_chips = real_to.as_u64();
        let max_committed_chips = max_committed.as_u64();
        let pot_chips = pot_after_call.as_u64();

        // best = (ratio, distance)；遍历显式 tie-break smaller milli first。
        let mut best: Option<(BetRatio, u64)> = None;
        for &ratio in &self.config.raise_pot_ratios {
            let milli = ratio.as_milli() as u128;
            let scaled = milli * (pot_chips as u128);
            let ratio_part_ceil = scaled.div_ceil(1000) as u64;
            let target_to_chips = max_committed_chips.saturating_add(ratio_part_ceil);
            let distance = target_to_chips.abs_diff(real_to_chips);
            match best {
                None => best = Some((ratio, distance)),
                Some((existing_ratio, existing_distance)) => {
                    if distance < existing_distance
                        || (distance == existing_distance
                            && ratio.as_milli() < existing_ratio.as_milli())
                    {
                        best = Some((ratio, distance));
                    }
                }
            }
        }
        let chosen_ratio = best
            .expect("raise_pot_ratios 非空 (D-202 长度 ∈ [1, 14])")
            .0;

        if la.bet_range.is_some() {
            AbstractAction::Bet {
                to: real_to,
                ratio_label: chosen_ratio,
            }
        } else {
            AbstractAction::Raise {
                to: real_to,
                ratio_label: chosen_ratio,
            }
        }
    }

    fn config(&self) -> &ActionAbstractionConfig {
        &self.config
    }
}

/// 按街分派的 action abstraction：preflop / flop / turn / river 各持一个
/// [`DefaultActionAbstraction`]，[`abstract_actions`](ActionAbstraction::abstract_actions)
/// / [`map_off_tree`](ActionAbstraction::map_off_tree) 按 `state.street()` 选对应街配置。
///
/// 动机（`docs/temp/nlhe_dense_infoset_table_plan_2026_05_26.md` 前置 P）：bet-size
/// 扩张目标 profile 要求各街不同 raise 集合（flop `{0.33,0.66,1,2}`、其余
/// `{0.5,1,2}`），而单个 `DefaultActionAbstraction` 只有一组全局 ratio。
///
/// - [`uniform`](Self::uniform)：四条街共用同一 config，输出与单个
///   `DefaultActionAbstraction::new(config)` **byte-identical**（用于保持旧路径不变
///   + 重构对照）。
/// - [`per_street`](Self::per_street)：`[preflop, flop, turn, river]` 各一组 config。
///
/// `Street::Showdown`（terminal）不应作为决策节点进入；万一传入，落到 river 槽位的
/// 底层 abstraction，由其 `current_player().is_none()` 守卫返回空集 / `Fold`，与
/// `DefaultActionAbstraction` 行为一致。
pub struct StreetActionAbstraction {
    /// index = `Street as usize`（Preflop=0, Flop=1, Turn=2, River=3）。
    by_street: [DefaultActionAbstraction; 4],
}

impl StreetActionAbstraction {
    /// 四条街共用同一 config；输出等价单个 `DefaultActionAbstraction::new(config)`。
    pub fn uniform(config: ActionAbstractionConfig) -> StreetActionAbstraction {
        StreetActionAbstraction {
            by_street: [
                DefaultActionAbstraction::new(config.clone()),
                DefaultActionAbstraction::new(config.clone()),
                DefaultActionAbstraction::new(config.clone()),
                DefaultActionAbstraction::new(config),
            ],
        }
    }

    /// per-street raise 集合：`[preflop, flop, turn, river]`。
    pub fn per_street(configs: [ActionAbstractionConfig; 4]) -> StreetActionAbstraction {
        let [preflop, flop, turn, river] = configs;
        StreetActionAbstraction {
            by_street: [
                DefaultActionAbstraction::new(preflop),
                DefaultActionAbstraction::new(flop),
                DefaultActionAbstraction::new(turn),
                DefaultActionAbstraction::new(river),
            ],
        }
    }

    /// 全街 `{0.5,1,2}`（= [`DefaultActionAbstraction::default_6_action`] 的 per-street
    /// 包装）。简化 NLHE 生产默认路径，与历史行为 byte-equal。
    pub fn default_6_action() -> StreetActionAbstraction {
        StreetActionAbstraction::uniform(ActionAbstractionConfig::default_6_action())
    }

    /// 指定街对应的底层 abstraction。`Showdown` 复用 river 槽位（见类型文档）。
    fn abs_for_street(&self, street: Street) -> &DefaultActionAbstraction {
        let idx = match street {
            Street::Preflop => 0,
            Street::Flop => 1,
            Street::Turn => 2,
            Street::River | Street::Showdown => 3,
        };
        &self.by_street[idx]
    }

    /// 指定街的 config 只读访问（诊断 / 测试）。
    pub fn config_for(&self, street: Street) -> &ActionAbstractionConfig {
        self.abs_for_street(street).config()
    }
}

impl ActionAbstraction for StreetActionAbstraction {
    fn abstract_actions(&self, state: &GameState) -> AbstractActionSet {
        self.abs_for_street(state.street()).abstract_actions(state)
    }

    fn map_off_tree(&self, state: &GameState, real_to: ChipAmount) -> AbstractAction {
        self.abs_for_street(state.street())
            .map_off_tree(state, real_to)
    }

    /// 返回 preflop 街的 config 作为代表（仅在 `uniform` 下对全街成立）。
    fn config(&self) -> &ActionAbstractionConfig {
        self.by_street[0].config()
    }
}

// ===========================================================================
// §7 桥接：AbstractAction → stage 1 Action
// ===========================================================================

impl AbstractAction {
    /// `AbstractAction` → 实际可 apply 的 `Action`（stage 1 类型）。**无状态**——
    /// `AbstractAction::Bet` / `Raise` 在构造时已由 stage 1 `LegalActionSet` 区分，
    /// 转换无歧义。映射规则：
    ///
    /// - `Fold` → `Action::Fold`
    /// - `Check` → `Action::Check`
    /// - `Call { .. }` → `Action::Call`（stage 1 `Action::Call` 不带 `to`，跟注
    ///   金额由 state machine 推导）
    /// - `Bet { to, .. }` → `Action::Bet { to }`
    /// - `Raise { to, .. }` → `Action::Raise { to }`
    /// - `AllIn { .. }` → `Action::AllIn`（state machine 自动归一化，`to` 字段
    ///   作为 InfoSet 编码标签即可丢弃）
    pub fn to_concrete(self) -> Action {
        match self {
            AbstractAction::Fold => Action::Fold,
            AbstractAction::Check => Action::Check,
            AbstractAction::Call { .. } => Action::Call,
            AbstractAction::Bet { to, .. } => Action::Bet { to },
            AbstractAction::Raise { to, .. } => Action::Raise { to },
            AbstractAction::AllIn { .. } => Action::AllIn,
        }
    }
}

// ===========================================================================
// StreetActionAbstraction 按街分派单元测试（dense 前置 P 外部对照）
// ===========================================================================

#[cfg(test)]
mod street_abstraction_tests {
    use super::*;
    use crate::core::Street;
    use crate::rules::action::Action;
    use crate::rules::config::TableConfig;
    use crate::rules::state::GameState;

    /// 沿 check/call 被动线推进到 `target` 街的首个决策节点（HU 200BB）。
    /// 不打 fold/raise，因此 preflop→flop→turn→river 全可达，river 之前不进 terminal。
    fn decision_state_on_street(target: Street, seed: u64) -> GameState {
        let cfg = TableConfig::default_hu_200bb();
        let mut s = GameState::new(&cfg, seed);
        let mut guard = 0;
        while s.street() != target {
            assert!(
                s.current_player().is_some(),
                "被动线在到达 {target:?} 前不应进入 terminal"
            );
            let la = s.legal_actions();
            // preflop SB 面对 BB 无 check → Call；postflop 首 actor 可 Check。
            let action = if la.check {
                Action::Check
            } else {
                Action::Call
            };
            s.apply(action).expect("被动 action 必合法");
            guard += 1;
            assert!(guard < 64, "推进循环失控（target={target:?}）");
        }
        assert!(
            s.current_player().is_some(),
            "{target:?} 街状态必须是决策节点"
        );
        s
    }

    const ALL_STREETS: [Street; 4] = [Street::Preflop, Street::Flop, Street::Turn, Street::River];

    /// uniform(cfg) 在每条街都与单个 `DefaultActionAbstraction::new(cfg)` byte-equal
    /// （证明 per-street 包装不改既有全街同一组行为；前置 P 重构对照）。
    #[test]
    fn uniform_matches_single_default_abstraction_on_every_street() {
        let cfg = ActionAbstractionConfig::default_6_action();
        let uniform = StreetActionAbstraction::uniform(cfg.clone());
        let single = DefaultActionAbstraction::new(cfg);
        for &street in &ALL_STREETS {
            for seed in [1u64, 7, 0xC0FFEE] {
                let s = decision_state_on_street(street, seed);
                assert_eq!(
                    uniform.abstract_actions(&s).as_slice(),
                    single.abstract_actions(&s).as_slice(),
                    "uniform 与单街 abstraction 在 {street:?} (seed={seed:#x}) 输出不一致"
                );
            }
        }
    }

    /// per_street([pre, flop, turn, river]) 在每条街分派到对应 config：输出与该街
    /// 单个 `DefaultActionAbstraction::new(street_cfg)` 完全一致（按街分派正确性）。
    #[test]
    fn per_street_dispatches_to_matching_street_config() {
        let pre = ActionAbstractionConfig::new(vec![0.5, 1.0, 2.0]).unwrap();
        let flop = ActionAbstractionConfig::new(vec![0.33, 0.66, 1.0, 2.0]).unwrap();
        let turn = ActionAbstractionConfig::new(vec![0.75, 1.5]).unwrap();
        let river = ActionAbstractionConfig::new(vec![1.0]).unwrap();
        let street_abs = StreetActionAbstraction::per_street([
            pre.clone(),
            flop.clone(),
            turn.clone(),
            river.clone(),
        ]);
        let by_street = [
            (Street::Preflop, pre),
            (Street::Flop, flop),
            (Street::Turn, turn),
            (Street::River, river),
        ];
        for (street, cfg) in by_street {
            let single = DefaultActionAbstraction::new(cfg);
            for seed in [3u64, 11, 0xBEEF] {
                let s = decision_state_on_street(street, seed);
                assert_eq!(
                    street_abs.abstract_actions(&s).as_slice(),
                    single.abstract_actions(&s).as_slice(),
                    "{street:?} (seed={seed:#x}) 未分派到对应街 config"
                );
            }
        }
    }

    /// 判别性检查：flop 状态下 per_street 的输出 ≠ 若全街都用 preflop config 的输出。
    /// 证明分派**确实按街选**，而非固定用某一组 ratio。fresh-flop pot=2BB 下
    /// flop `{0.33,0.66,1,2}` 出 4 个 bet、preflop `{0.5,1,2}` 出 3 个 → 长度不同。
    #[test]
    fn per_street_flop_differs_from_preflop_config() {
        let pre = ActionAbstractionConfig::new(vec![0.5, 1.0, 2.0]).unwrap();
        let flop = ActionAbstractionConfig::new(vec![0.33, 0.66, 1.0, 2.0]).unwrap();
        let street_abs =
            StreetActionAbstraction::per_street([pre.clone(), flop, pre.clone(), pre.clone()]);
        let pre_only = DefaultActionAbstraction::new(pre);
        let s = decision_state_on_street(Street::Flop, 5);
        assert_ne!(
            street_abs.abstract_actions(&s).as_slice(),
            pre_only.abstract_actions(&s).as_slice(),
            "flop 分派应取 flop 4-size 集，与 preflop 3-size 集不同"
        );
    }

    /// `config_for` 取对应街配置；uniform 下全街同 raise_count，per_street 各取各。
    #[test]
    fn config_for_returns_per_street_config() {
        let pre = ActionAbstractionConfig::new(vec![0.5, 1.0, 2.0]).unwrap();
        let flop = ActionAbstractionConfig::new(vec![0.33, 0.66, 1.0, 2.0]).unwrap();
        let turn = ActionAbstractionConfig::new(vec![0.75, 1.5]).unwrap();
        let river = ActionAbstractionConfig::new(vec![1.0]).unwrap();
        let abs = StreetActionAbstraction::per_street([pre, flop, turn, river]);
        assert_eq!(abs.config_for(Street::Preflop).raise_count(), 3);
        assert_eq!(abs.config_for(Street::Flop).raise_count(), 4);
        assert_eq!(abs.config_for(Street::Turn).raise_count(), 2);
        assert_eq!(abs.config_for(Street::River).raise_count(), 1);
        // Showdown 复用 river 槽位（见类型文档）。
        assert_eq!(abs.config_for(Street::Showdown).raise_count(), 1);
    }
}
