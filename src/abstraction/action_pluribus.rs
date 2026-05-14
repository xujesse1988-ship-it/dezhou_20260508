//! Pluribus 14-action abstraction（API-420..API-423 / D-420..D-423）。
//!
//! stage 2 [`crate::ActionAbstraction`] trait 第 2 个 impl 形态：14-action 集合
//! `{Fold, Check, Call, Raise 0.5/0.75/1/1.5/2/3/5/10/25/50 × pot, AllIn}`
//! 复用 Pluribus 主论文 §S3 字面顺序。
//!
//! **A1 \[实现\] 状态**：[`PluribusAction`] 14-variant enum + [`PluribusAction::all`]
//! / [`PluribusAction::raise_multiplier`] 公开 const helper 落地；
//! [`PluribusActionAbstraction`] struct + `actions` / `is_legal` / `compute_raise_to`
//! 内部方法签名锁，方法体 `unimplemented!()` 占位，B2 \[实现\] 落地走 stage 1
//! [`crate::GameState`] legal action + pot / current_bet 计算。
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
///
/// **A1 \[实现\] 状态**：struct 签名锁；`actions` / `is_legal` /
/// `compute_raise_to` 全 `unimplemented!()`。B2 \[实现\] 落地走 stage 1
/// [`GameState`] legal action 计算。
#[derive(Clone, Copy, Debug, Default)]
pub struct PluribusActionAbstraction;

impl PluribusActionAbstraction {
    /// stage 4 D-420 字面 — 列出 `state` 上合法的 14-action 子集。
    ///
    /// 走 stage 1 [`GameState`] legal action + pot / current_bet 计算（D-422
    /// 字面 raise size 走 stage 1 [`GameState::apply`] byte-equal 验证）。
    ///
    /// **A1 \[实现\] 状态**：方法体 `unimplemented!()`，B2 \[实现\] 落地。
    pub fn actions(&self, state: &GameState) -> Vec<PluribusAction> {
        let _ = state;
        unimplemented!(
            "stage 4 A1 [实现] scaffold: PluribusActionAbstraction::actions 落地 B2 [实现] D-420"
        )
    }

    /// stage 4 D-420 + D-422 — 判定 [`PluribusAction`] 在 `state` 上是否 legal。
    ///
    /// 走 stage 1 [`GameState`] betting state + stack / pot 计算；超过 stack 的
    /// raise size 自动转 [`PluribusAction::AllIn`]（与 stage 1 D-022 字面继承）。
    /// 不满足 min raise（stage 1 D-033）的 raise size 自动剔除。
    ///
    /// **A1 \[实现\] 状态**：方法体 `unimplemented!()`，B2 \[实现\] 落地。
    pub fn is_legal(&self, action: &PluribusAction, state: &GameState) -> bool {
        let _ = (action, state);
        unimplemented!(
            "stage 4 A1 [实现] scaffold: PluribusActionAbstraction::is_legal 落地 B2 [实现] D-420"
        )
    }

    /// stage 4 D-420 — raise size 计算：`raise_to = current_bet + multiplier × pot_size`。
    ///
    /// `multiplier` 来自 [`PluribusAction::raise_multiplier`] 的 `Some(_)` 值；
    /// non-raise [`PluribusAction`]（Fold / Check / Call / AllIn）由 caller
    /// `unreachable!()` 拒绝（不进入本路径）。
    ///
    /// **A1 \[实现\] 状态**：方法体 `unimplemented!()`，B2 \[实现\] 落地走 stage 1
    /// [`GameState::pot`] + per-player committed_this_round 计算 current_bet。
    pub fn compute_raise_to(&self, state: &GameState, multiplier: f64) -> ChipAmount {
        let _ = (state, multiplier);
        unimplemented!(
            "stage 4 A1 [实现] scaffold: PluribusActionAbstraction::compute_raise_to 落地 B2 [实现] D-420"
        )
    }
}
