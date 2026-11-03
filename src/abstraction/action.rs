//! Action abstraction（API §1）。
//!
//! `AbstractAction` / `AbstractActionSet` / `ActionAbstractionConfig` / `BetRatio` /
//! `ActionAbstraction` trait + `DefaultActionAbstraction`。
//!
//! 不变量 AA-001..AA-008（含 AA-003-rev1 / AA-004-rev1）见
//! `docs/pluribus_stage2_api.md` §1；A1 阶段所有方法体走 `unimplemented!()`。

use thiserror::Error;

use crate::core::rng::{ChaCha20Rng, RngSource};
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

    /// off-tree action 映射（stage 6c：pseudo-harmonic randomized rounding，
    /// 算法版本见 [`OFF_TREE_MAP_ALGORITHM`]）。
    ///
    /// `real_to` 是对手实际下注的 `to` 字段（绝对金额，与 stage 1
    /// `Action::Bet/Raise { to }` 同语义）。
    ///
    /// **纯函数契约**：同 `(state, real_to)` → 同输出（rounding 的随机 draw 由
    /// 局面派生种子驱动，不消费外部 rng）。这是 6c 门槛②「映射结果稳定可复现」
    /// 以及 AIVAT/replay「无状态重放一致」（`aivat_nlhe` §4.5）的硬约束——
    /// 故签名**不**接受 `&mut dyn RngSource`（外部 rng 会在二次调用间推进、破坏可复现）。
    fn map_off_tree(&self, state: &GameState, real_to: ChipAmount) -> AbstractAction;

    /// 配置只读访问。
    fn config(&self) -> &ActionAbstractionConfig;
}

/// 6c off-tree 映射算法的版本标识，写入策略服务版本元数据
/// （`docs/temp/pluribus_path.md` 阶段 6c 门槛①「显式选定 off-tree 映射算法
/// 并写入版本元数据」）。`map_off_tree` 落 `Bet/Raise` 区间时走此算法。
pub const OFF_TREE_MAP_ALGORITHM: &str = "pseudo-harmonic-randomized-rounding-v1";

/// 6c：pseudo-harmonic randomized rounding 的确定性种子。
///
/// 把当前**公共**局面（街 / 底池 / 各家本轮投入与剩余筹码 / board）连同
/// `real_to` 整数混进一个 `u64`。两条性质同时成立：
/// - **纯函数**（同 `(state, real_to)` → 同种子 → 同 draw）→ 满足 6c 门槛②
///   「映射结果稳定可复现」+ 既有 `map_off_tree` 二次调用 `m1 == m2` 契约 +
///   AIVAT/replay 的「无状态重放一致」（`aivat_nlhe` §4.5）。
/// - **局面相关**→ 把 rounding 边界在不同决策点之间打散，使「卡边界对手」
///   无法对单一算术中点 cliff 系统性套利（6c 门槛④抗剥削证据）。
///
/// 纯整数混合（splitmix64 finalizer），无浮点（invariants §1：抽象层禁浮点）。
fn phm_round_seed(state: &GameState, real_to: ChipAmount) -> u64 {
    #[inline]
    fn mix(z: u64) -> u64 {
        let z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        let z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    let street_code: u64 = match state.street() {
        Street::Preflop => 1,
        Street::Flop => 2,
        Street::Turn => 3,
        Street::River => 4,
        Street::Showdown => 5,
    };
    let mut acc = mix(real_to.as_u64() ^ 0x9e37_79b9_7f4a_7c15);
    acc = mix(acc ^ street_code);
    acc = mix(acc ^ state.pot().as_u64());
    for p in state.players() {
        acc = mix(acc.rotate_left(7) ^ p.committed_this_round.as_u64());
        acc = mix(acc.rotate_left(13) ^ p.stack.as_u64());
    }
    for (i, card) in state.board().iter().enumerate() {
        acc = mix(acc ^ ((card.to_u8() as u64) << ((i % 8) * 8)));
    }
    acc
}

/// 6c：在相邻两档 abstract ratio `lower < upper` 之间做 pseudo-harmonic
/// randomized rounding（Ganzfried-Sandholm）。调用前提 `a < x < b`（严格落在
/// 两档之间；命中端点由调用方按确定性分支处理）。
///
/// `x_milli` = 实际 raise-above-call 占 `pot_after_call` 的 milli 比例
/// （与 `target_to(r) = max_committed + r_milli × pot / 1000` 同坐标系）。
/// 映射到 `lower`（较小档）的概率
/// `f_lower(x) = (B - x)(1 + A) / ((B - A)(1 + x))`，其 50% 交叉点
/// `x* = (A + B + 2AB)/(A + B + 2)`（**不是**算术中点、**不是**几何均值——
/// 后两者更可剥削，见设计 §5.e）。draw 由 [`phm_round_seed`] 确定性种子驱动的
/// `ChaCha20Rng`（`RngSource`，byte-equal 可复现）。
///
/// 全整数 milli 运算（`1` pot = `1000` milli），无浮点；`saturating_mul` +
/// 受限左移使任意配置都 no-panic（6c 门槛②「无非法/越界」）。
fn pseudo_harmonic_pick(
    lower: BetRatio,
    upper: BetRatio,
    x_milli: u64,
    state: &GameState,
    real_to: ChipAmount,
) -> BetRatio {
    let picks_lower = pseudo_harmonic_picks_lower(
        lower.as_milli() as u64,
        upper.as_milli() as u64,
        x_milli,
        state,
        real_to,
        OffTreeRounding::Randomized,
    );
    if picks_lower {
        lower
    } else {
        upper
    }
}

/// [`pseudo_harmonic_pick`] 的 milli 版：两档以 milli 坐标给出（`a_milli < x_milli < b_milli`），
/// 返回是否取较小档。`Randomized` 与 [`pseudo_harmonic_pick`] 逐位相同（同种子、同判据）；
/// `Nearest` 取 `f_lower ≥ 1/2` 的那一档（确定性，同一 pseudo-harmonic 度量下的「最近档」）。
fn pseudo_harmonic_picks_lower(
    a_milli: u64,
    b_milli: u64,
    x_milli: u64,
    state: &GameState,
    real_to: ChipAmount,
    rounding: OffTreeRounding,
) -> bool {
    let a = a_milli as u128;
    let b = b_milli as u128;
    let x = x_milli as u128;
    // den > 0（b > a）；num ∈ (0, den)（a < x < b）→ f_lower ∈ (0, 1)。
    // num = P(map→lower) 分子；den = 分母。
    let num = (b - x) * (1000 + a);
    let den = (b - a) * (1000 + x);
    match rounding {
        OffTreeRounding::Randomized => {
            // r ∈ [0, 2^64)；map→lower ⟺ r/2^64 < num/den ⟺ r·den < num·2^64。
            let r = ChaCha20Rng::from_seed(phm_round_seed(state, real_to)).next_u64() as u128;
            let lhs = r.saturating_mul(den);
            let rhs = num << 64; // shift amount 64 < 128 → 不 panic；realistic ratio 下无丢位。
            lhs < rhs
        }
        OffTreeRounding::Nearest => num.saturating_mul(2) >= den,
    }
}

/// 真实 Bet/Raise 落在两个抽象档之间时怎么取舍（[`StreetActionAbstraction::map_aggressive_onto_node`]）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OffTreeRounding {
    /// pseudo-harmonic randomized rounding（与 [`ActionAbstraction::map_off_tree`] 同种子同判据）。
    /// 用于**对手**动作：随机化让对手没法卡固定的取整边界。
    Randomized,
    /// 确定性取 pseudo-harmonic 概率 ≥ 1/2 的那一档。用于 **hero 自己**的动作：hero 只是把建议
    /// 尺寸凑了整（实盘 001131：建议 6875、实下 7000），不需要防剥削，随机取到远端档只会把导航
    /// 带到错误的下注线上。
    Nearest,
}

/// 第 `ratio` 档在 `(max_committed, pot_after_call)` 几何下的目标 `to`（ceil，再抬到 `min_to`）。
/// [`DefaultActionAbstraction::abstract_actions`] 出档与
/// [`DefaultActionAbstraction::map_aggressive_onto_node`] 判「该档是否被并进 AllIn」共用这一份。
fn ratio_target_to(
    ratio: BetRatio,
    max_committed: ChipAmount,
    pot_after_call: ChipAmount,
    min_to: ChipAmount,
) -> ChipAmount {
    // candidate_to = max_committed + ceil(ratio_milli * pot_after_call / 1000)
    let milli = ratio.as_milli() as u64;
    let pot_chips = pot_after_call.as_u64();
    let scaled = (milli as u128) * (pot_chips as u128);
    let ratio_part_ceil = scaled.div_ceil(1000) as u64;
    let candidate_to = max_committed + ChipAmount::new(ratio_part_ceil);
    // ① floor to min_to
    if candidate_to < min_to {
        min_to
    } else {
        candidate_to
    }
}

/// 默认 5-action 抽象（D-200）。
#[derive(Clone)]
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

    /// 把真实 Bet/Raise（`real_to`，非 all-in）映射到**某个树节点实际有的**进攻档上。
    ///
    /// 与 [`ActionAbstraction::map_off_tree`] 的区别：后者在配置菜单的 ratio 之间取整，取到的档
    /// 可能在这个节点并不存在——典型是该档目标额 ≥ 全下额、被 AA-004 并进了 AllIn 槽，或被建树
    /// 规则剪掉。调用方过去把「不存在」一律当 AllIn（实盘 001131：转牌下 7000、半池档 6875，
    /// 2.4% 概率取到 1pot 档 13750 ≥ 全下 13400 → 当成全下 → 后面失同步）。这里候选只取节点里
    /// 真实存在的档：
    ///
    /// - 配置 ratio `r` 的 Bet/Raise 在节点里（`allowed`）→ 候选，坐标 = `r` 的 milli（与
    ///   `map_off_tree` 同坐标，所以节点档齐全、无档被并进全下时结果与 `map_off_tree` 逐位相同）；
    /// - `r` 的目标额 ≥ 全下额（被并进 AllIn）且节点有 AllIn → AllIn 作候选，坐标 = 全下额本身的
    ///   milli（按全下的真实金额参与比较）；
    /// - 其余（被建树规则剪掉的档）不作候选。
    ///
    /// 在候选里按 [`map_off_tree`](ActionAbstraction::map_off_tree) 同样的规则取上下邻：命中端点取该档，
    /// 夹在中间按 `rounding` 取舍，超出两端就近。返回 `Bet/Raise { to: real_to, .. }` 或
    /// `AllIn { to: 全下额 }`。`real_to ≥ 全下额` → AllIn（节点没有则 `None`）；
    /// `real_to ≤ 本轮最高投入` / 不能进攻 / 无候选 → `None`（调用方按原兜底处理）。纯函数。
    pub fn map_aggressive_onto_node(
        &self,
        state: &GameState,
        real_to: ChipAmount,
        allowed: &dyn Fn(&AbstractAction) -> bool,
        rounding: OffTreeRounding,
    ) -> Option<AbstractAction> {
        let actor_seat = state.current_player()?;
        let la = state.legal_actions();
        let actor = &state.players()[actor_seat.0 as usize];
        let max_committed = state
            .players()
            .iter()
            .map(|p| p.committed_this_round)
            .max()
            .unwrap_or(ChipAmount::ZERO);
        let cap = la
            .all_in_amount
            .unwrap_or(actor.committed_this_round + actor.stack);
        let all_in = AbstractAction::AllIn { to: cap };
        if real_to >= cap {
            return allowed(&all_in).then_some(all_in);
        }
        if real_to <= max_committed {
            return None;
        }
        let min_to = la
            .bet_range
            .map(|(min, _)| min)
            .or(la.raise_range.map(|(min, _)| min))?;
        let is_bet = la.bet_range.is_some();
        let to_call_delta = if max_committed > actor.committed_this_round {
            max_committed - actor.committed_this_round
        } else {
            ChipAmount::ZERO
        };
        let pot_after_call = state.pot() + to_call_delta;
        let pot_chips = pot_after_call.as_u64();
        // 与 map_off_tree 同口径：raise-above-call 占 pot_after_call 的 milli（floor）。
        let milli_of = |to: ChipAmount| -> u64 {
            to.as_u64()
                .saturating_sub(max_committed.as_u64())
                .saturating_mul(1000)
                .checked_div(pot_chips)
                .unwrap_or(0)
        };
        let x_milli = milli_of(real_to);

        let mut cands: Vec<(u64, AbstractAction)> =
            Vec::with_capacity(self.config.raise_pot_ratios.len() + 1);
        for &ratio in &self.config.raise_pot_ratios {
            let on_menu = if is_bet {
                AbstractAction::Bet {
                    to: real_to,
                    ratio_label: ratio,
                }
            } else {
                AbstractAction::Raise {
                    to: real_to,
                    ratio_label: ratio,
                }
            };
            if allowed(&on_menu) {
                cands.push((ratio.as_milli() as u64, on_menu));
            } else if ratio_target_to(ratio, max_committed, pot_after_call, min_to) >= cap
                && allowed(&all_in)
                && !cands
                    .iter()
                    .any(|(_, a)| matches!(a, AbstractAction::AllIn { .. }))
            {
                cands.push((milli_of(cap), all_in));
            }
        }

        let lower = cands
            .iter()
            .filter(|(m, _)| *m <= x_milli)
            .max_by_key(|(m, _)| *m)
            .copied();
        let upper = cands
            .iter()
            .filter(|(m, _)| *m >= x_milli)
            .min_by_key(|(m, _)| *m)
            .copied();
        match (lower, upper) {
            (Some((a, act)), Some((b, _))) if a == b => Some(act),
            (Some((a, lo)), Some((b, hi))) => Some(
                if pseudo_harmonic_picks_lower(a, b, x_milli, state, real_to, rounding) {
                    lo
                } else {
                    hi
                },
            ),
            (None, Some((_, hi))) => Some(hi),
            (Some((_, lo)), None) => Some(lo),
            (None, None) => None,
        }
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
            // ① candidate_to（ceil，floor 到 min_to）
            let candidate_to = ratio_target_to(ratio, max_committed, pot_after_call, min_to);
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
        // stage 6c：pseudo-harmonic randomized rounding（Ganzfried-Sandholm，
        // 算法版本 OFF_TREE_MAP_ALGORITHM）。纯函数（同 (state, real_to) → 同输出，
        // 见 trait 文档「纯函数契约」），no-panic。
        //
        // 算法：
        //   ① real_to ≥ cap                → AllIn { to: cap }
        //   ② real_to ≤ max_committed      → Call (或 Check / Fold 兜底)
        //   ③ 无 bet_range / raise_range   → Call / Fold 兜底（防御）
        //   ④ 把 real_to 折成 pot-fraction x，落在相邻两档 ratio A<x<B 之间时按
        //      pseudo-harmonic 概率 f_A(x) 随机 round 到 A 或 B（draw 由局面派生
        //      种子驱动，见 pseudo_harmonic_pick）；落在菜单端点之外就近 clamp。
        //      输出 `Bet | Raise { to: real_to, ratio_label }`（LA-002 互斥：
        //      bet_range Some → Bet，否则 Raise）。

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

        // ④ pseudo-harmonic randomized rounding。
        //    pot_after_call = pot() + (max_committed - committed_this_round)，与
        //    target_to(r) = max_committed + r_milli × pot_after_call / 1000 同坐标系。
        let pot_before = state.pot();
        let to_call_delta = if max_committed > committed_this_round {
            max_committed - committed_this_round
        } else {
            ChipAmount::ZERO
        };
        let pot_after_call = pot_before + to_call_delta;
        let pot_chips = pot_after_call.as_u64();

        // x = raise-above-call 占 pot_after_call 的 milli 比例（floor 量化）。
        // case ② 已挡 real_to ≤ max_committed → 此处 real_to > max_committed；
        // pot_chips == 0 真实局面不可达，checked_div 防御性退到最小 x（落最小档）。
        let x_milli = real_to
            .as_u64()
            .saturating_sub(max_committed.as_u64())
            .saturating_mul(1000)
            .checked_div(pot_chips)
            .unwrap_or(0);

        // 相邻两档：lower = milli ≤ x 的最大档；upper = milli ≥ x 的最小档。
        // ratio 菜单非排序（ActionAbstractionConfig::new 按输入序去重）→ 全扫。
        let lower = self
            .config
            .raise_pot_ratios
            .iter()
            .copied()
            .filter(|r| (r.as_milli() as u64) <= x_milli)
            .max_by_key(|r| r.as_milli());
        let upper = self
            .config
            .raise_pot_ratios
            .iter()
            .copied()
            .filter(|r| (r.as_milli() as u64) >= x_milli)
            .min_by_key(|r| r.as_milli());

        let chosen_ratio = match (lower, upper) {
            // 命中某档（x 恰等该 milli，lower==upper）→ 该档（确定性）。
            (Some(a), Some(b)) if a.as_milli() == b.as_milli() => a,
            // 严格落在 a < x < b 之间 → pseudo-harmonic randomized rounding。
            (Some(a), Some(b)) => pseudo_harmonic_pick(a, b, x_milli, state, real_to),
            // x 低于最小档 → clamp 到最小档（保留 raise 语义：real_to 已投入，不下塌
            //   为 Call；A=0 虚拟下邻会丢失 real_to chips，见设计 §5.e）。
            (None, Some(b)) => b,
            // x 高于最大档（但 < cap，已在 ① 处理）→ clamp 到最大档。
            (Some(a), None) => a,
            // ratio 菜单非空（D-202 长度 ∈ [1,14]）→ 不可达；防御退首档不 panic。
            (None, None) => self.config.raise_pot_ratios[0],
        };

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
#[derive(Clone)]
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

    /// 按街分派的 [`DefaultActionAbstraction::map_aggressive_onto_node`]。
    pub fn map_aggressive_onto_node(
        &self,
        state: &GameState,
        real_to: ChipAmount,
        allowed: &dyn Fn(&AbstractAction) -> bool,
        rounding: OffTreeRounding,
    ) -> Option<AbstractAction> {
        self.abs_for_street(state.street())
            .map_aggressive_onto_node(state, real_to, allowed, rounding)
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

    /// 节点里的抽象动作是否含与 `a` 同类同档的一项（只比 kind + ratio_label，不比 `to`）。
    fn same_slot(set: &AbstractActionSet, a: &AbstractAction) -> bool {
        set.as_slice().iter().any(|b| match (a, b) {
            (
                AbstractAction::Bet { ratio_label: x, .. },
                AbstractAction::Bet { ratio_label: y, .. },
            )
            | (
                AbstractAction::Raise { ratio_label: x, .. },
                AbstractAction::Raise { ratio_label: y, .. },
            ) => x == y,
            (AbstractAction::AllIn { .. }, AbstractAction::AllIn { .. }) => true,
            _ => false,
        })
    }

    /// 节点档齐全、无档被并进全下时，`map_aggressive_onto_node(Randomized)` 与 `map_off_tree`
    /// 逐位相同（对手下注的旧行为不变）。HU 200BB flop 起点：底池 200、各 19900，{0.5,1,2}pot
    /// 都远小于全下。
    #[test]
    fn map_onto_node_randomized_equals_map_off_tree_when_menu_intact() {
        let abs = StreetActionAbstraction::default_6_action();
        for seed in [3u64, 11, 0xBEEF] {
            let s = decision_state_on_street(Street::Flop, seed);
            let set = abs.abstract_actions(&s);
            assert_eq!(set.as_slice().len(), 5, "check + 3 档 + allin");
            let allowed = |a: &AbstractAction| same_slot(&set, a);
            for to in (1..=1_000u64).map(ChipAmount::new) {
                let old = abs.map_off_tree(&s, to);
                let new = abs
                    .map_aggressive_onto_node(&s, to, &allowed, OffTreeRounding::Randomized)
                    .expect("能下注的点必有候选");
                assert_eq!(new, old, "seed={seed:#x} to={to:?}");
            }
        }
    }

    /// HU 短码：翻前 SB 加到 400、BB 跟 → flop 底池 800、各剩 600。{0.5,1}pot 里 1pot（800）
    /// ≥ 全下 600、被并进 AllIn，节点只有 {Check, Bet0.5(400), AllIn(600)}。
    fn short_flop_state() -> GameState {
        let mut cfg = TableConfig::default_hu_200bb();
        cfg.starting_stacks = vec![ChipAmount::new(1_000); 2];
        let mut s = GameState::new(&cfg, 7);
        s.apply(Action::Raise {
            to: ChipAmount::new(400),
        })
        .unwrap();
        s.apply(Action::Call).unwrap();
        assert_eq!(s.street(), Street::Flop);
        assert_eq!(s.pot(), ChipAmount::new(800));
        s
    }

    /// 被并进 AllIn 的档按全下真实金额参与比较：下 420（半池 400 稍多）——hero（Nearest）恒落半池；
    /// 对手（Randomized）只会在 {半池, AllIn} 里取，绝不出节点里不存在的 1pot；贴近全下的 590 →
    /// Nearest 落 AllIn。旧 `map_off_tree` 在配置菜单上取整，会取到节点没有的 1pot。
    #[test]
    fn map_onto_node_collapsed_slot_uses_real_allin_size() {
        let abs =
            StreetActionAbstraction::uniform(ActionAbstractionConfig::new(vec![0.5, 1.0]).unwrap());
        let s = short_flop_state();
        let set = abs.abstract_actions(&s);
        let allowed = |a: &AbstractAction| same_slot(&set, a);
        let half = AbstractAction::Bet {
            to: ChipAmount::new(420),
            ratio_label: BetRatio::HALF_POT,
        };
        assert!(allowed(&half));
        assert!(!allowed(&AbstractAction::Bet {
            to: ChipAmount::new(800),
            ratio_label: BetRatio::FULL_POT,
        }));
        let near = |to: u64| {
            abs.map_aggressive_onto_node(
                &s,
                ChipAmount::new(to),
                &allowed,
                OffTreeRounding::Nearest,
            )
        };
        assert_eq!(near(420), Some(half));
        assert_eq!(
            near(590),
            Some(AbstractAction::AllIn {
                to: ChipAmount::new(600)
            })
        );
        assert_eq!(
            near(600),
            Some(AbstractAction::AllIn {
                to: ChipAmount::new(600)
            })
        );
        let mut saw_full_in_old = false;
        for to in 401..600u64 {
            let r = abs
                .map_aggressive_onto_node(
                    &s,
                    ChipAmount::new(to),
                    &allowed,
                    OffTreeRounding::Randomized,
                )
                .expect("有候选");
            assert!(allowed(&r), "to={to}：取到节点里不存在的档 {r:?}");
            let old = abs.map_off_tree(&s, ChipAmount::new(to));
            saw_full_in_old |= matches!(old, AbstractAction::Bet { ratio_label, .. } if ratio_label == BetRatio::FULL_POT);
        }
        assert!(
            saw_full_in_old,
            "旧映射在这段确实会取到节点没有的 1pot（本测试的对照）"
        );
    }

    /// Nearest 是确定性的 pseudo-harmonic 取舍：50% 交叉点 x* = (A+B+2AB)/(A+B+2)（A=0.5、B=1 →
    /// x* ≈ 0.714），x* 以下取较小档、以上取较大档。HU flop 底池 200、档齐全：下 0.69 / 0.72 pot。
    #[test]
    fn map_onto_node_nearest_splits_at_pseudo_harmonic_midpoint() {
        let abs =
            StreetActionAbstraction::uniform(ActionAbstractionConfig::new(vec![0.5, 1.0]).unwrap());
        let s = decision_state_on_street(Street::Flop, 3);
        assert_eq!(s.pot(), ChipAmount::new(200));
        let set = abs.abstract_actions(&s);
        let allowed = |a: &AbstractAction| same_slot(&set, a);
        let label = |to: u64| match abs.map_aggressive_onto_node(
            &s,
            ChipAmount::new(to),
            &allowed,
            OffTreeRounding::Nearest,
        ) {
            Some(AbstractAction::Bet { ratio_label, .. }) => ratio_label,
            other => panic!("应为 Bet，得 {other:?}"),
        };
        assert_eq!(label(138), BetRatio::HALF_POT); // 0.69 pot
        assert_eq!(label(144), BetRatio::FULL_POT); // 0.72 pot
    }

    /// 被建树规则剪掉（不在节点、也没被并进全下）的档不作候选：节点只认 {1pot, AllIn} 时，
    /// 半池附近的下注落 1pot（就近），不再「不在节点 → 当 AllIn」。
    #[test]
    fn map_onto_node_skips_pruned_slots() {
        let abs =
            StreetActionAbstraction::uniform(ActionAbstractionConfig::new(vec![0.5, 1.0]).unwrap());
        let s = decision_state_on_street(Street::Flop, 3);
        let allowed = |a: &AbstractAction| {
            matches!(a, AbstractAction::AllIn { .. })
                || matches!(a, AbstractAction::Bet { ratio_label, .. } if *ratio_label == BetRatio::FULL_POT)
        };
        for rounding in [OffTreeRounding::Nearest, OffTreeRounding::Randomized] {
            let r = abs.map_aggressive_onto_node(&s, ChipAmount::new(100), &allowed, rounding);
            assert!(
                matches!(r, Some(AbstractAction::Bet { ratio_label, .. }) if ratio_label == BetRatio::FULL_POT),
                "{rounding:?}：得 {r:?}"
            );
        }
    }
}
