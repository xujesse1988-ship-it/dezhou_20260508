//! Hand history 与回放（API §5）。

use crate::core::{Card, ChipAmount, SeatId, Street};
use crate::error::HistoryError;
use crate::rules::action::Action;
use crate::rules::config::TableConfig;
use crate::rules::state::GameState;

/// 一手牌完整记录。
///
/// `replay()` 与 `replay_to()` 基于 `seed` + `actions` 重建中间 / 终局
/// `GameState`，要求与原始记录完全一致（D-028 发牌协议保证发牌确定性）。
#[derive(Clone, Debug)]
pub struct HandHistory {
    /// 当前固定为 1（D-061）。
    pub schema_version: u32,
    pub config: TableConfig,
    /// 用于复现的初始 seed。
    pub seed: u64,
    /// 按发生顺序。AllIn 已归一化为 Bet/Raise/Call。
    pub actions: Vec<RecordedAction>,
    /// 0..=5 张。
    pub board: Vec<Card>,
    /// 长度 = `n_seats`。
    pub hole_cards: Vec<Option<[Card; 2]>>,
    /// 净收益。
    pub final_payouts: Vec<(SeatId, i64)>,
    /// 摊牌顺序，最后激进者在前（D-037）。
    pub showdown_order: Vec<SeatId>,
}

#[derive(Clone, Debug)]
pub struct RecordedAction {
    /// 全手内单调递增。
    pub seq: u32,
    pub seat: SeatId,
    pub street: Street,
    /// AllIn 已归一化为 Bet/Raise/Call。
    pub action: Action,
    /// 该 seat 在本街（= `self.street`）的投入总额；语义见 API §5。
    pub committed_after: ChipAmount,
}

impl HandHistory {
    /// 序列化为 protobuf 字节（schema_version=1）。
    ///
    /// 输出必须 deterministic：相同 `HandHistory` 在所有平台上产生 byte-equal
    /// 字节流（PB-003）。
    pub fn to_proto(&self) -> Vec<u8> {
        unimplemented!()
    }

    /// 从 protobuf 字节反序列化。错误情况见 [`HistoryError`]。
    /// 校验阶段须执行 PB-001 / PB-002 全部检查。
    pub fn from_proto(bytes: &[u8]) -> Result<HandHistory, HistoryError> {
        let _ = bytes;
        unimplemented!()
    }

    /// 完整回放：从 `seed` + `actions` 重建终局 `GameState`。
    ///
    /// 终局状态必须与原始记录完全一致（`board`, `hole_cards`, `payouts`）。
    /// 错误类型为 [`HistoryError`]（API-001-rev1）：
    /// - 记录动作在某 index 处被规则引擎拒绝 → `HistoryError::Rule { index, source }`
    /// - 重新发牌结果与记录的 `board` / `hole_cards` 不一致 → `HistoryError::ReplayDiverged`
    pub fn replay(&self) -> Result<GameState, HistoryError> {
        unimplemented!()
    }

    /// 部分回放：应用 `actions[0..action_index]` 后的中间状态（即"前
    /// `action_index` 个动作已应用"）。
    ///
    /// `action_index = 0` 表示"刚发完手牌、未行动"；`action_index = actions.len()`
    /// 等同 [`replay`](Self::replay)。错误类型说明见 [`replay`](Self::replay)。
    pub fn replay_to(&self, action_index: usize) -> Result<GameState, HistoryError> {
        let _ = action_index;
        unimplemented!()
    }

    /// hand history 的内容指纹。`BLAKE3(self.to_proto())`。
    ///
    /// 由于 `to_proto` 是 deterministic（PB-003），`content_hash` 跨平台稳定，
    /// 适合用于 D-051 跨平台一致性验收与 fuzz roundtrip 比对。
    pub fn content_hash(&self) -> [u8; 32] {
        unimplemented!()
    }
}
