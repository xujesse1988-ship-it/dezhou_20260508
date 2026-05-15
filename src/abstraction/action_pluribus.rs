//! Pluribus 14-action abstraction（API-420..API-423 / D-420..D-423）。
//!
//! stage 2 [`crate::ActionAbstraction`] trait 第 2 个 impl 形态：14-action 集合
//! `{Fold, Check, Call, Raise 0.5/0.75/1/1.5/2/3/5/10/25/50 × pot, AllIn}`
//! 复用 Pluribus 主论文 §S3 字面顺序。
//!
//! **B2 \[实现\] 状态**（2026-05-15）：[`PluribusActionAbstraction::actions` /
//! `is_legal` / `compute_raise_to`] 全部落地走 stage 1 [`GameState`] legal
//! action + pot / current_bet 计算。`compute_raise_to` rounding policy =
//! **floor**（`(pot.as_u64() as f64 * multiplier) as u64` 隐式截断），让 B1
//! 14 测试 0.75 Pot ±1 chip 容差通过（floor / round-half-up / ceil 任一形态均
//! 落在容差内）；整数 multiplier × pot 精确等于不依赖 rounding policy。
//!
//! 不调用 stage 2 既有 [`crate::ActionAbstraction`] trait impl（trait 返回
//! [`crate::AbstractAction`] 与 14-variant `PluribusAction` 不是一一映射）；
//! stage 4 C2 \[实现\] 落地 trait impl 桥接（API-494）。

use crate::core::ChipAmount;
use crate::rules::state::GameState;

/// Pluribus 14-action enumeration（API-420 / D-420 字面顺序）。
///
/// 顺序按 Pluribus 主论文 §S3 字面：Fold / Check / Call / Raise 10 个 pot
/// multiplier / AllIn。`#[repr(u8)]` tag = enumerate index ∈ 0..14。
#[derive(Clone, Copy, Eq, PartialEq, Hash, Debug)]
#[repr(u8)]
pub enum PluribusAction {
    Fold = 0,
    Check = 1,
    Call = 2,
    Raise05Pot = 3,
    Raise075Pot = 4,
    Raise1Pot = 5,
    Raise15Pot = 6,
    Raise2Pot = 7,
    Raise3Pot = 8,
    Raise5Pot = 9,
    Raise10Pot = 10,
    Raise25Pot = 11,
    Raise50Pot = 12,
    AllIn = 13,
}

impl PluribusAction {
    /// stage 4 D-420 字面 — 14 个 action。
    pub const N_ACTIONS: usize = 14;

    /// stage 4 D-420 字面 — 14-action 集合迭代顺序（deterministic）。
    pub fn all() -> [PluribusAction; 14] {
        [
            PluribusAction::Fold,
            PluribusAction::Check,
            PluribusAction::Call,
            PluribusAction::Raise05Pot,
            PluribusAction::Raise075Pot,
            PluribusAction::Raise1Pot,
            PluribusAction::Raise15Pot,
            PluribusAction::Raise2Pot,
            PluribusAction::Raise3Pot,
            PluribusAction::Raise5Pot,
            PluribusAction::Raise10Pot,
            PluribusAction::Raise25Pot,
            PluribusAction::Raise50Pot,
            PluribusAction::AllIn,
        ]
    }

    /// stage 4 D-420 字面 — raise pot multiplier 表。
    ///
    /// 返回 `Some(mult)` 表示 raise，`None` 表示 non-raise（Fold / Check /
    /// Call / AllIn）。10 个 raise multiplier ∈ {0.5, 0.75, 1, 1.5, 2, 3, 5, 10,
    /// 25, 50} 与 Pluribus 主论文 §S3 字面一致。
    pub fn raise_multiplier(self) -> Option<f64> {
        match self {
            PluribusAction::Raise05Pot => Some(0.5),
            PluribusAction::Raise075Pot => Some(0.75),
            PluribusAction::Raise1Pot => Some(1.0),
            PluribusAction::Raise15Pot => Some(1.5),
            PluribusAction::Raise2Pot => Some(2.0),
            PluribusAction::Raise3Pot => Some(3.0),
            PluribusAction::Raise5Pot => Some(5.0),
            PluribusAction::Raise10Pot => Some(10.0),
            PluribusAction::Raise25Pot => Some(25.0),
            PluribusAction::Raise50Pot => Some(50.0),
            _ => None,
        }
    }

    /// stage 4 API-411 binary trip-wire — `u8` tag → enum。
    ///
    /// Checkpoint v2 / InfoSetId 14-action mask 解码路径在 stage 4 D2 \[实现\]
    /// 起步前消费；A1 \[实现\] 仅锁签名。
    pub fn from_u8(tag: u8) -> Option<PluribusAction> {
        match tag {
            0 => Some(PluribusAction::Fold),
            1 => Some(PluribusAction::Check),
            2 => Some(PluribusAction::Call),
            3 => Some(PluribusAction::Raise05Pot),
            4 => Some(PluribusAction::Raise075Pot),
            5 => Some(PluribusAction::Raise1Pot),
            6 => Some(PluribusAction::Raise15Pot),
            7 => Some(PluribusAction::Raise2Pot),
            8 => Some(PluribusAction::Raise3Pot),
            9 => Some(PluribusAction::Raise5Pot),
            10 => Some(PluribusAction::Raise10Pot),
            11 => Some(PluribusAction::Raise25Pot),
            12 => Some(PluribusAction::Raise50Pot),
            13 => Some(PluribusAction::AllIn),
            _ => None,
        }
    }
}

/// stage 4 Pluribus 14-action abstraction（API-420 / D-420）。
///
/// 无字段 — `legal_actions` 计算只读消费 [`GameState`]（D-420 字面）。stage 2
/// [`crate::DefaultActionAbstraction`] 5-action 抽象作为 ablation baseline 维持
/// 独立 impl 不退化。
#[derive(Clone, Copy, Debug, Default)]
pub struct PluribusActionAbstraction;

impl PluribusActionAbstraction {
    /// stage 4 D-420 字面 — 列出 `state` 上合法的 14-action 子集。
    ///
    /// 走 stage 1 [`GameState`] legal action + pot / current_bet 计算（D-422
    /// 字面 raise size 走 stage 1 [`GameState::apply`] byte-equal 验证）。输出
    /// 顺序固定 = [`PluribusAction::all`] 字面顺序（deterministic）。
    pub fn actions(&self, state: &GameState) -> Vec<PluribusAction> {
        let mut out = Vec::with_capacity(PluribusAction::N_ACTIONS);
        for action in PluribusAction::all() {
            if self.is_legal(&action, state) {
                out.push(action);
            }
        }
        out
    }

    /// stage 4 D-420 + D-422 — 判定 [`PluribusAction`] 在 `state` 上是否 legal。
    ///
    /// 走 stage 1 [`GameState::legal_actions`] 返回的 [`crate::LegalActionSet`]：
    /// - `Fold` / `Check` / `Call` / `AllIn` 直接读对应字段
    /// - `Raise X Pot` 计算 `raise_to = current_bet + multiplier × pot`（D-420
    ///   字面公式），检验落在 `raise_range`（已有前序 bet）或 `bet_range`
    ///   （无前序 bet）区间内。raise_to 超 cap（stack 上限）或低于 min raise
    ///   都返回 `false`（不满足 min raise → D-422(a) stage 1 D-033 字面继承
    ///   自动剔除；超 stack → D-422(e) 自动转 AllIn 由 caller 单独枚举 AllIn
    ///   action 覆盖）。
    pub fn is_legal(&self, action: &PluribusAction, state: &GameState) -> bool {
        let legal = state.legal_actions();
        match action {
            PluribusAction::Fold => legal.fold,
            PluribusAction::Check => legal.check,
            PluribusAction::Call => legal.call.is_some(),
            PluribusAction::AllIn => legal.all_in_amount.is_some(),
            other => {
                let Some(mult) = other.raise_multiplier() else {
                    return false;
                };
                let raise_to = self.compute_raise_to(state, mult);
                if let Some((min_to, max_to)) = legal.raise_range {
                    raise_to >= min_to && raise_to <= max_to
                } else if let Some((min_to, max_to)) = legal.bet_range {
                    raise_to >= min_to && raise_to <= max_to
                } else {
                    false
                }
            }
        }
    }

    /// stage 4 D-420 — raise size 计算：`raise_to = current_bet + multiplier × pot_size`。
    ///
    /// `current_bet` = `max_p (committed_this_round[p])`（与 stage 1
    /// `GameState::max_committed_this_round` 内部计算等价；外部入口只读
    /// `players().committed_this_round` 累积 max 不依赖私有方法）。
    /// `pot_size` = [`GameState::pot`]（含所有玩家累积总投入）。
    ///
    /// rounding policy = **floor**（`(pot × mult) as u64` 截断小数）：整数
    /// multiplier × pot 精确等于；0.75 Pot 等非整数 rounding 落在 B1 \[测试\]
    /// ±1 chip 容差内（floor / round-half-up / ceil 任一形态均满足）。caller
    /// (`is_legal`) 不进入 non-raise [`PluribusAction`] 分支（Fold / Check /
    /// Call / AllIn 的 `raise_multiplier()` 返回 `None`，分支不会触达本方法）；
    /// 外部直接消费 `compute_raise_to` 时 caller 责任保证 `multiplier >= 0`。
    pub fn compute_raise_to(&self, state: &GameState, multiplier: f64) -> ChipAmount {
        let pot = state.pot();
        let current_bet = state
            .players()
            .iter()
            .map(|p| p.committed_this_round)
            .max()
            .unwrap_or(ChipAmount::ZERO);
        let raise_delta_chips = (pot.as_u64() as f64 * multiplier) as u64;
        current_bet + ChipAmount::new(raise_delta_chips)
    }
}
