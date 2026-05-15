//! Information abstraction（API §2）。
//!
//! `InfoSetId` 64-bit 复合编码 + `InfoAbstraction` trait + `BettingState` /
//! `StreetTag` enum。
//!
//! 不变量 IA-001..IA-007（含 IA-006-rev1）见 `docs/pluribus_stage2_api.md` §2。

use crate::core::Card;
use crate::rules::state::GameState;

/// 复合 InfoSet id。低位编码与 D-215 / D-216 一致，**preflop / postflop 共享
/// 同一 64-bit layout**。
///
/// 字段顺序（低位起）：
///
/// - bit  0..24: `bucket_id`         (24 bit；preflop = hand_class_169 ∈ 0..169；
///   postflop = `BucketTable::lookup` 返回 cluster id ∈ 0..bucket_count(street))
/// - bit 24..28: `position_bucket`   ( 4 bit；0..n_seats-1，支持 2..=9 桌大小)
/// - bit 28..32: `stack_bucket`      ( 4 bit；0..4 = D-211 5 桶；postflop 沿用
///   preflop 起手值)
/// - bit 32..35: `betting_state`     ( 3 bit；0..4 = D-212 5 状态 enum 值)
/// - bit 35..38: `street_tag`        ( 3 bit；0..3 = Preflop/Flop/Turn/River；
///   preflop 显式编码 0 不靠零启发式)
/// - bit 38..64: `reserved`          (26 bit；必须为 0)
#[derive(
    Copy, Clone, Eq, PartialEq, Hash, Debug, Ord, PartialOrd, serde::Serialize, serde::Deserialize,
)]
pub struct InfoSetId(u64);

impl InfoSetId {
    pub fn raw(self) -> u64 {
        self.0
    }

    pub fn street_tag(self) -> StreetTag {
        crate::abstraction::map::unpack_street_tag(self.0)
    }

    pub fn position_bucket(self) -> u8 {
        crate::abstraction::map::unpack_position_bucket(self.0)
    }

    pub fn stack_bucket(self) -> u8 {
        crate::abstraction::map::unpack_stack_bucket(self.0)
    }

    pub fn betting_state(self) -> BettingState {
        crate::abstraction::map::unpack_betting_state(self.0)
    }

    pub fn bucket_id(self) -> u32 {
        crate::abstraction::map::unpack_bucket_id(self.0)
    }

    /// 便捷构造（API §7 桥接）：从 `GameState + hole + 抽象层` → `InfoSetId`。
    /// 等价于 `abs.map(state, hole)`，仅作为 driver 代码的 ergonomic helper。
    pub fn from_game_state<A: InfoAbstraction>(
        state: &GameState,
        hole: [Card; 2],
        abs: &A,
    ) -> InfoSetId {
        abs.map(state, hole)
    }

    /// crate-private 内部构造：仅供 `abstraction::map` 子模块的 `pack_info_set_id`
    /// 调用，外部代码不应该直接构造 `InfoSetId`（必须经 `InfoAbstraction::map`
    /// 路径以保证 D-215 字段语义稳定）。
    pub(crate) fn from_raw_internal(raw: u64) -> InfoSetId {
        InfoSetId(raw)
    }

    /// stage 4 API-423 — 14-action availability mask 写入 reserved 区域（D-423）。
    ///
    /// **C2 \[实现\] lock**（2026-05-15）：mask 区域 = bits 33..47（14-bit
    /// 子段），与 `CLAUDE.md` "stage 4 C2 \[实现\] 起步前 lock" + `pluribus_stage4_
    /// api.md` API-423 字面一致。
    ///
    /// 该区域在 stage 2 D-215 实际 layout 上跨越 `betting_state`（bits 32..35）的
    /// 高 2 bit（33,34）+ `street_tag`（bits 35..38）全 3 bit + reserved（bits
    /// 38..64）的低 9 bit；NlheGame6 路径走 `crate::training::NlheGame6` 的
    /// `info_set` 实现通过 `pack_info_set_id` + `with_14action_mask` 串联编码
    /// （先 pack 出基础 InfoSetId，再写入 mask），mask 写入后**字面禁止**回头
    /// 调用 [`Self::betting_state`] / [`Self::street_tag`] 解包（写入会破坏这两
    /// 字段的 bit 位）— D-423 文档承诺：NlheGame6 路径不消费 unpack 链路，由
    /// mask 本身提供 betting/street 的等价判别力（不同 betting state / street
    /// 必映射到不同 legal_actions subset，进而映射到不同 mask）。
    ///
    /// **stage 3 SimplifiedNlheGame 路径不受影响**：SimplifiedNlhe 走
    /// D-317-rev1 6-bit mask（bits 12..18 写入 bucket_id field），不调用
    /// [`Self::with_14action_mask`]；stage 1/2/3 既有测试套件（`tests/
    /// info_id_encoding.rs` 与 `tests/cfr_simplified_nlhe.rs`）使用的
    /// `betting_state()` / `street_tag()` getter 在 stage 3 InfoSetId 上 byte-equal
    /// 维持。
    pub fn with_14action_mask(self, mask: u16) -> InfoSetId {
        debug_assert!(
            u32::from(mask) < (1u32 << 14),
            "with_14action_mask: mask {mask} 越界 (14-bit 上界 < 16384)"
        );
        let raw = self.0;
        let cleared = raw & !(0x3FFFu64 << 33);
        let with_mask = cleared | ((u64::from(mask) & 0x3FFF) << 33);
        InfoSetId(with_mask)
    }

    /// stage 4 API-423 — 读回 14-action availability mask（D-423）。
    ///
    /// 反 [`Self::with_14action_mask`] 写入路径；`SimplifiedNlheGame` 路径上
    /// （未调用 with_14action_mask）返回 0（reserved 区域 stage 2 D-215 字面初
    /// 始化全零，写 mask 前读出 0）。
    pub fn legal_actions_mask_14(self) -> u16 {
        ((self.0 >> 33) & 0x3FFF) as u16
    }
}

/// 当前下注轮的合法动作集语义（D-212）。preflop 与 postflop 共用同一枚举。
///
/// 该字段直接决定 actor 的合法动作集——`Open` 局面 actor 可 `Check / Bet`，
/// `FacingBetNoRaise` 局面 actor 必须 `Fold / Call / Raise`，二者**不同**；
/// 仅以 raise count = 0 编码会让两类局面同 `InfoSetId` 但合法动作集不同，
/// CFR regret 矩阵跨 `GameState` 错位（F17 修复）。
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
#[repr(u8)]
pub enum BettingState {
    /// preflop: BB 在 limpers / walks 后有 check option；
    /// postflop: 本街无 voluntary bet。
    Open = 0,
    /// preflop: 非 BB 位首次面对 BB 强制下注（无 voluntary raise）；
    /// postflop: 本街已有 opening bet 但无 raise。
    FacingBetNoRaise = 1,
    /// 本下注轮已发生 1 次 voluntary raise（含 incomplete short all-in）。
    FacingRaise1 = 2,
    FacingRaise2 = 3,
    /// ≥ 3 次 voluntary raise 吸收。
    FacingRaise3Plus = 4,
}

/// 街标记（D-216）。`StreetTag` 仅含 4 个 betting 街变体，不含 `Showdown`——
/// caller 必须在调用前把 stage 1 `Street::Showdown` 局面分流（Showdown 不存在
/// InfoSet 决策点）。
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
#[repr(u8)]
pub enum StreetTag {
    Preflop = 0,
    Flop = 1,
    Turn = 2,
    River = 3,
}

/// Information abstraction trait（API §2）。
pub trait InfoAbstraction: Send + Sync {
    /// `(GameState, hole_cards)` → `InfoSet id`。
    ///
    /// **前置条件**（IA-006-rev1）：`state.current_player().is_some()`（即非
    /// terminal 且非 all-in 跳轮 state）。违反前置条件 panic（debug + release
    /// 一致，与 stage 1 `ChipAmount::Sub` 同型）。caller 必须在 CFR / 实时搜索
    /// driver 中先判断 `state.current_player().is_none()` 跳过 InfoSet 编码——
    /// terminal 局面没有 actor 决策点，InfoSet 概念不可达。
    ///
    /// **stack_bucket 来源**（D-211-rev1）：实现必须从 `state.config()` 引用 +
    /// `state.actor_seat()` 计算 `effective_stack_at_hand_start`，**不允许**从
    /// `state.player(seat).stack`（当前剩余筹码）推算。同手内 preflop / flop /
    /// turn / river 调用结果 `stack_bucket` 字段 byte-equal。如 stage 1
    /// `GameState` 当前未公开 `config()` getter，B2 \[实现\] 必须走 stage 1
    /// `API-NNN-revM` 流程在 `pluribus_stage1_api.md` 添加只读 getter（A1 阶段
    /// 只产签名，不触发该 rev）。
    ///
    /// 整条调用路径**禁止浮点**（D-273 / D-252）；postflop 走 mmap bucket lookup
    /// 命中整数 bucket id；preflop 走组合 lookup 表。
    fn map(&self, state: &GameState, hole: [Card; 2]) -> InfoSetId;
}
