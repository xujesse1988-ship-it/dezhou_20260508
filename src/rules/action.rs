//! 动作枚举与合法动作集合（API §2）。

use crate::core::ChipAmount;

/// 玩家动作。
///
/// 语义见 API §2：`Bet { to }` / `Raise { to }` 的 `to` 是该玩家本下注轮投入的
/// **绝对总额**（包含此动作之前已投入的盲注 / call / 之前被加注的额度）。
/// 应用动作后该玩家的 `committed_this_round` 必须严格等于 `to`。
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum Action {
    Fold,
    Check,
    Call,
    /// 当前下注轮无前序 bet 时的下注。`to` = 本轮投入总额（绝对值）。
    Bet {
        to: ChipAmount,
    },
    /// 前序已有 bet 时的加注。`to` = 本轮投入总额（绝对值）。
    Raise {
        to: ChipAmount,
    },
    /// 全部剩余筹码。状态机内部归一化为 Bet/Raise/Call，
    /// `HandHistory.actions` 中存储归一化后的最终动作。
    AllIn,
}

/// 合法动作集合。每条字段独立，`None` 表示该动作不合法。
///
/// **不变量**（实现 agent 必须保证、测试 agent 在 invariant suite 中验证；
/// `docs/pluribus_stage1_api.md` §2）：
///
/// - **LA-001** `check` 与 `call` 互斥：当前下注轮 `committed_this_round` 与
///   `max_committed_this_round` 相等时只能 `check`（`call = None`）；不等时
///   只能 `call`（`check = false`）。即 `check && call.is_some()` 永远为 false。
/// - **LA-002** `bet_range` 与 `raise_range` 互斥：本轮 `max_committed_this_round
///   == 0`（无前序 bet）时 `raise_range = None`；`> 0` 时 `bet_range = None`。
///   即 `bet_range.is_some() && raise_range.is_some()` 永远为 false。
/// - **LA-003** `fold` 永远合法（除非 `current_player == None`）：
///   `current_player().is_some() => fold == true`。
/// - **LA-004** `call` 与 `check` 至少有一个真：`current_player().is_some()` 时
///   `check || call.is_some()` 必须为 true。
/// - **LA-005** `bet_range.min_to >= BB`（首次开局，D-034）；`raise_range.min_to`
///   满足 D-035 链式 min raise 约束。
/// - **LA-006** `bet_range / raise_range` 的 `max_to <= committed_this_round
///   + stack`（不可下注超出剩余筹码 + 本轮已投入）。
/// - **LA-007** `all_in_amount` 当且仅当 `stack > 0` 时为 `Some`；其值
///   `= committed_this_round + stack`。
/// - **LA-008** `current_player() == None`（terminal / all-in 跳轮）时所有
///   字段为 `false / None`（"空集合"）。
#[derive(Clone, Debug)]
pub struct LegalActionSet {
    pub fold: bool,
    pub check: bool,
    /// 跟注所需金额（绝对，不是差额）。
    pub call: Option<ChipAmount>,
    /// `(min_to, max_to)`。本轮无前序 bet 时使用。
    pub bet_range: Option<(ChipAmount, ChipAmount)>,
    /// `(min_to, max_to)`。本轮已有前序 bet 时使用，含 short all-in 不重开 raise 的约束（D-033）。
    pub raise_range: Option<(ChipAmount, ChipAmount)>,
    /// 全 all-in 时的等效 `to` 值；`stack > 0` 时为 `Some`。
    pub all_in_amount: Option<ChipAmount>,
}
