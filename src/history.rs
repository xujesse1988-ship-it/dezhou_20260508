//! Hand history 与回放（API §5）。

use crate::core::{Card, ChipAmount, SeatId, Street};
use crate::error::HistoryError;
use crate::rules::action::Action;
use crate::rules::config::TableConfig;
use crate::rules::state::GameState;
use prost::Message;

/// 一手牌完整记录。
///
/// `replay()` 与 `replay_to()` 基于 `seed` + `actions` 重建中间 / 终局
/// `GameState`，要求与原始记录完全一致（D-028 发牌协议保证发牌确定性）。
///
/// **回放与街转换时序**（详见 `docs/pluribus_stage1_api.md` §5）：
///
/// - 公共牌（flop / turn / river）的发牌**不占** `actions` 序列中的位置，
///   不产生 `RecordedAction`。
/// - 触发某街最后动作的 `apply` 调用内顺序执行：reset 所有玩家
///   `committed_this_round` → 发下街公共牌 → 切 `street` → 选下街第一行动者
///   （postflop = SB 起，preflop = UTG 起）。
/// - **全员 all-in 跳轮（D-036）多街快进**：若该 `apply` 后剩余 `Active`
///   玩家 ≤ 1 名，状态机在同一 `apply` 调用内连续发完所有未发公共牌
///   （直至 `board.len() == 5`）→ 切 `Showdown` → 计算 `payouts` →
///   `is_terminal == true`，期间不产生新 `RecordedAction`。`replay_to(k)`
///   若第 k 个动作触发该分支，返回的 `GameState` 已处于终局。
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
    /// 该 seat 在本街（= `self.street`）的投入总额。**取该动作 apply 完成、
    /// 本街 `committed_this_round` 尚未被街转换重置之前的快照值**：
    ///
    /// - 未触发街转换的动作：等价于 apply 后 `player.committed_this_round`。
    /// - 触发街转换的动作（即本街最后一个动作）：等价于"如果本街不重置，
    ///   apply 后 `player.committed_this_round` 应有的值"。
    ///
    /// 各 `Action` 变体下的具体取值：
    ///
    /// - `Fold` / `Check`：`committed_after` = 该 seat 进入本动作前的
    ///   `committed_this_round`（本动作不改变投入额）。
    /// - `Call` / `Bet { to }` / `Raise { to }`：`committed_after = to`。
    ///
    /// 该定义保证 `committed_after` 在 `replay` / `replay_to` 中可被独立
    /// 校验，不依赖于"街转换 reset 是否已发生"的内部时序。
    pub committed_after: ChipAmount,
}

impl HandHistory {
    /// 序列化为 protobuf 字节（schema_version=1）。
    ///
    /// 输出必须 deterministic：相同 `HandHistory` 在所有平台上产生 byte-equal
    /// 字节流（PB-003）。
    pub fn to_proto(&self) -> Vec<u8> {
        let proto = proto::HandHistory {
            schema_version: self.schema_version,
            config: Some(config_to_proto(&self.config)),
            seed: self.seed,
            actions: self.actions.iter().map(action_to_proto).collect(),
            board: self.board.iter().map(|c| c.to_u8() as u32).collect(),
            hole_cards: self.hole_cards.iter().map(hole_cards_to_proto).collect(),
            final_payouts: self
                .final_payouts
                .iter()
                .map(|(seat, amount)| proto::Payout {
                    seat: seat.0 as u32,
                    amount: *amount,
                })
                .collect(),
            showdown_order: self.showdown_order.iter().map(|s| s.0 as u32).collect(),
        };
        proto.encode_to_vec()
    }

    /// 从 protobuf 字节反序列化。错误情况见 [`HistoryError`]。
    /// 校验阶段须执行 PB-001 / PB-002 全部检查。
    pub fn from_proto(bytes: &[u8]) -> Result<HandHistory, HistoryError> {
        let proto = proto::HandHistory::decode(bytes)
            .map_err(|e| HistoryError::InvalidProto(e.to_string()))?;
        if proto.schema_version != 1 {
            return Err(HistoryError::SchemaVersionMismatch {
                found: proto.schema_version,
                supported: 1,
            });
        }
        let config = config_from_proto(
            proto
                .config
                .as_ref()
                .ok_or_else(|| HistoryError::Corrupted("missing config".into()))?,
        )?;
        let board = proto
            .board
            .iter()
            .map(|&v| card_from_u32(v, "board"))
            .collect::<Result<Vec<_>, _>>()?;
        let hole_cards = proto
            .hole_cards
            .iter()
            .map(hole_cards_from_proto)
            .collect::<Result<Vec<_>, _>>()?;
        if hole_cards.len() != config.n_seats as usize {
            return Err(HistoryError::Corrupted(format!(
                "hole_cards length {} != n_seats {}",
                hole_cards.len(),
                config.n_seats
            )));
        }
        let actions = proto
            .actions
            .iter()
            .map(action_from_proto)
            .collect::<Result<Vec<_>, _>>()?;
        let final_payouts = proto
            .final_payouts
            .iter()
            .map(|p| Ok((seat_from_u32(p.seat, "payout.seat")?, p.amount)))
            .collect::<Result<Vec<_>, HistoryError>>()?;
        let showdown_order = proto
            .showdown_order
            .iter()
            .map(|&v| seat_from_u32(v, "showdown_order"))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(HandHistory {
            schema_version: proto.schema_version,
            config,
            seed: proto.seed,
            actions,
            board,
            hole_cards,
            final_payouts,
            showdown_order,
        })
    }

    /// 完整回放：从 `seed` + `actions` 重建终局 `GameState`。
    ///
    /// 终局状态必须与原始记录完全一致（`board`, `hole_cards`, `payouts`）。
    /// 错误类型为 [`HistoryError`]（API-001-rev1）：
    /// - 记录动作在某 index 处被规则引擎拒绝 → `HistoryError::Rule { index, source }`
    /// - 重新发牌结果与记录的 `board` / `hole_cards` 不一致 → `HistoryError::ReplayDiverged`
    pub fn replay(&self) -> Result<GameState, HistoryError> {
        self.replay_to(self.actions.len())
    }

    /// 部分回放：应用 `actions[0..action_index]` 后的中间状态（即"前
    /// `action_index` 个动作已应用"）。
    ///
    /// `action_index = 0` 表示"刚发完手牌、未行动"；`action_index = actions.len()`
    /// 等同 [`replay`](Self::replay)。错误类型说明见 [`replay`](Self::replay)。
    pub fn replay_to(&self, action_index: usize) -> Result<GameState, HistoryError> {
        if action_index > self.actions.len() {
            return Err(HistoryError::Corrupted(format!(
                "action_index {action_index} > actions.len() {}",
                self.actions.len()
            )));
        }
        let mut state = GameState::new(&self.config, self.seed);
        if state.hand_history().hole_cards != self.hole_cards {
            return Err(HistoryError::ReplayDiverged {
                index: 0,
                reason: "hole cards diverged from seed".into(),
            });
        }
        for (index, action) in self.actions.iter().take(action_index).enumerate() {
            state
                .apply(action.action)
                .map_err(|source| HistoryError::Rule { index, source })?;
        }
        if action_index == self.actions.len() {
            if state.board() != self.board.as_slice() {
                return Err(HistoryError::ReplayDiverged {
                    index: action_index,
                    reason: "board diverged".into(),
                });
            }
            if state.payouts().unwrap_or_default() != self.final_payouts {
                return Err(HistoryError::ReplayDiverged {
                    index: action_index,
                    reason: "payouts diverged".into(),
                });
            }
        }
        Ok(state)
    }

    /// hand history 的内容指纹。`BLAKE3(self.to_proto())`。
    ///
    /// 由于 `to_proto` 是 deterministic（PB-003），`content_hash` 跨平台稳定，
    /// 适合用于 D-051 跨平台一致性验收与 fuzz roundtrip 比对。
    pub fn content_hash(&self) -> [u8; 32] {
        *blake3::hash(&self.to_proto()).as_bytes()
    }
}

fn config_to_proto(config: &TableConfig) -> proto::TableConfig {
    proto::TableConfig {
        n_seats: config.n_seats as u32,
        starting_stacks: config.starting_stacks.iter().map(|c| c.as_u64()).collect(),
        small_blind: config.small_blind.as_u64(),
        big_blind: config.big_blind.as_u64(),
        ante: config.ante.as_u64(),
        button_seat: config.button_seat.0 as u32,
    }
}

fn config_from_proto(config: &proto::TableConfig) -> Result<TableConfig, HistoryError> {
    if !(2..=9).contains(&config.n_seats) {
        return Err(HistoryError::Corrupted(format!(
            "n_seats out of range: {}",
            config.n_seats
        )));
    }
    if config.starting_stacks.len() != config.n_seats as usize {
        return Err(HistoryError::Corrupted(format!(
            "starting_stacks length {} != n_seats {}",
            config.starting_stacks.len(),
            config.n_seats
        )));
    }
    Ok(TableConfig {
        n_seats: config.n_seats as u8,
        starting_stacks: config
            .starting_stacks
            .iter()
            .map(|&chips| ChipAmount::new(chips))
            .collect(),
        small_blind: ChipAmount::new(config.small_blind),
        big_blind: ChipAmount::new(config.big_blind),
        ante: ChipAmount::new(config.ante),
        button_seat: seat_from_u32(config.button_seat, "button_seat")?,
    })
}

fn action_to_proto(action: &RecordedAction) -> proto::RecordedAction {
    let (kind, to) = match action.action {
        Action::Fold => (proto::ActionKind::Fold, 0),
        Action::Check => (proto::ActionKind::Check, 0),
        Action::Call => (proto::ActionKind::Call, action.committed_after.as_u64()),
        Action::Bet { to } => (proto::ActionKind::Bet, to.as_u64()),
        Action::Raise { to } => (proto::ActionKind::Raise, to.as_u64()),
        Action::AllIn => unreachable!("AllIn must be normalized before history write"),
    };
    proto::RecordedAction {
        seq: action.seq,
        seat: action.seat.0 as u32,
        street: street_to_proto(action.street) as i32,
        kind: kind as i32,
        to,
        committed_after: action.committed_after.as_u64(),
    }
}

fn action_from_proto(action: &proto::RecordedAction) -> Result<RecordedAction, HistoryError> {
    let street = street_from_proto(action.street)?;
    let kind = proto::ActionKind::try_from(action.kind)
        .map_err(|_| HistoryError::Corrupted(format!("unknown action kind {}", action.kind)))?;
    let action_value = match kind {
        proto::ActionKind::Unspecified => {
            return Err(HistoryError::Corrupted(
                "action kind unspecified".to_string(),
            ));
        }
        proto::ActionKind::Fold => Action::Fold,
        proto::ActionKind::Check => Action::Check,
        proto::ActionKind::Call => Action::Call,
        proto::ActionKind::Bet => Action::Bet {
            to: ChipAmount::new(action.to),
        },
        proto::ActionKind::Raise => Action::Raise {
            to: ChipAmount::new(action.to),
        },
    };
    Ok(RecordedAction {
        seq: action.seq,
        seat: seat_from_u32(action.seat, "action.seat")?,
        street,
        action: action_value,
        committed_after: ChipAmount::new(action.committed_after),
    })
}

fn hole_cards_to_proto(hole: &Option<[Card; 2]>) -> proto::HoleCards {
    match hole {
        Some([a, b]) => proto::HoleCards {
            present: true,
            c0: a.to_u8() as u32,
            c1: b.to_u8() as u32,
        },
        None => proto::HoleCards {
            present: false,
            c0: 0,
            c1: 0,
        },
    }
}

fn hole_cards_from_proto(hole: &proto::HoleCards) -> Result<Option<[Card; 2]>, HistoryError> {
    if !hole.present {
        return Ok(None);
    }
    Ok(Some([
        card_from_u32(hole.c0, "hole.c0")?,
        card_from_u32(hole.c1, "hole.c1")?,
    ]))
}

fn card_from_u32(value: u32, field: &'static str) -> Result<Card, HistoryError> {
    let value_u8 = u8::try_from(value)
        .map_err(|_| HistoryError::Corrupted(format!("{field} card out of range: {value}")))?;
    Card::from_u8(value_u8)
        .ok_or_else(|| HistoryError::Corrupted(format!("{field} card out of range: {value}")))
}

fn seat_from_u32(value: u32, field: &'static str) -> Result<SeatId, HistoryError> {
    let seat = u8::try_from(value)
        .map_err(|_| HistoryError::Corrupted(format!("{field} seat out of range: {value}")))?;
    Ok(SeatId(seat))
}

fn street_to_proto(street: Street) -> proto::Street {
    match street {
        Street::Preflop => proto::Street::Preflop,
        Street::Flop => proto::Street::Flop,
        Street::Turn => proto::Street::Turn,
        Street::River => proto::Street::River,
        Street::Showdown => proto::Street::Showdown,
    }
}

fn street_from_proto(value: i32) -> Result<Street, HistoryError> {
    match proto::Street::try_from(value)
        .map_err(|_| HistoryError::Corrupted(format!("unknown street {value}")))?
    {
        proto::Street::Unspecified => {
            Err(HistoryError::Corrupted("street unspecified".to_string()))
        }
        proto::Street::Preflop => Ok(Street::Preflop),
        proto::Street::Flop => Ok(Street::Flop),
        proto::Street::Turn => Ok(Street::Turn),
        proto::Street::River => Ok(Street::River),
        proto::Street::Showdown => Ok(Street::Showdown),
    }
}

mod proto {
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct HandHistory {
        #[prost(uint32, tag = "1")]
        pub schema_version: u32,
        #[prost(message, optional, tag = "2")]
        pub config: Option<TableConfig>,
        #[prost(uint64, tag = "3")]
        pub seed: u64,
        #[prost(message, repeated, tag = "4")]
        pub actions: Vec<RecordedAction>,
        #[prost(uint32, repeated, tag = "5")]
        pub board: Vec<u32>,
        #[prost(message, repeated, tag = "6")]
        pub hole_cards: Vec<HoleCards>,
        #[prost(message, repeated, tag = "7")]
        pub final_payouts: Vec<Payout>,
        #[prost(uint32, repeated, tag = "8")]
        pub showdown_order: Vec<u32>,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct TableConfig {
        #[prost(uint32, tag = "1")]
        pub n_seats: u32,
        #[prost(uint64, repeated, tag = "2")]
        pub starting_stacks: Vec<u64>,
        #[prost(uint64, tag = "3")]
        pub small_blind: u64,
        #[prost(uint64, tag = "4")]
        pub big_blind: u64,
        #[prost(uint64, tag = "5")]
        pub ante: u64,
        #[prost(uint32, tag = "6")]
        pub button_seat: u32,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct RecordedAction {
        #[prost(uint32, tag = "1")]
        pub seq: u32,
        #[prost(uint32, tag = "2")]
        pub seat: u32,
        #[prost(enumeration = "Street", tag = "3")]
        pub street: i32,
        #[prost(enumeration = "ActionKind", tag = "4")]
        pub kind: i32,
        #[prost(uint64, tag = "5")]
        pub to: u64,
        #[prost(uint64, tag = "6")]
        pub committed_after: u64,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, prost::Enumeration)]
    #[repr(i32)]
    pub enum ActionKind {
        Unspecified = 0,
        Fold = 1,
        Check = 2,
        Call = 3,
        Bet = 4,
        Raise = 5,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, prost::Enumeration)]
    #[repr(i32)]
    pub enum Street {
        Unspecified = 0,
        Preflop = 1,
        Flop = 2,
        Turn = 3,
        River = 4,
        Showdown = 5,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct HoleCards {
        #[prost(bool, tag = "1")]
        pub present: bool,
        #[prost(uint32, tag = "2")]
        pub c0: u32,
        #[prost(uint32, tag = "3")]
        pub c1: u32,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Payout {
        #[prost(uint32, tag = "1")]
        pub seat: u32,
        #[prost(sint64, tag = "2")]
        pub amount: i64,
    }
}
