//! Preflop 169 lossless 抽象（API §2）。
//!
//! `PreflopLossless169` + `canonical_hole_id` helper（D-217 / D-218-rev1）。

use crate::core::{Card, ChipAmount, Street};
use crate::rules::action::Action;
use crate::rules::state::GameState;

use crate::abstraction::info::{BettingState, InfoAbstraction, InfoSetId, StreetTag};
use crate::abstraction::map::pack_info_set_id;

/// preflop hole 单维 canonical id ∈ 0..1326（花色对称归一化）。
/// `BucketTable::lookup(StreetTag::Preflop, _)` 入参由本函数计算。
pub fn canonical_hole_id(hole: [Card; 2]) -> u32 {
    let (lo, hi) = order_pair(hole);
    pair_combination_index(lo, hi)
}

/// 169 lossless 等价类抽象（D-217）。
pub struct PreflopLossless169 {
    _opaque: (),
}

impl PreflopLossless169 {
    pub fn new() -> PreflopLossless169 {
        PreflopLossless169 { _opaque: () }
    }

    /// 169 lossless 等价类编号（D-217）：
    ///
    /// - `0..13` = pocket pairs（22, 33, ..., AA 升序）
    /// - `13..91` = suited（按高牌主排序、低牌副排序：32s 起，AKs 终）
    /// - `91..169` = offsuit（同顺序）
    pub fn hand_class(&self, hole: [Card; 2]) -> u8 {
        let suited = hole[0].suit() == hole[1].suit();
        let a = hole[0].rank() as u8;
        let b = hole[1].rank() as u8;
        let (high, low) = if a >= b { (a, b) } else { (b, a) };
        if high == low {
            high
        } else if suited {
            13 + high * (high - 1) / 2 + low
        } else {
            91 + high * (high - 1) / 2 + low
        }
    }

    /// 169 类总 hole 计数：pairs 6 / suited 4 / offsuit 12，总和 1326。
    pub fn hole_count_in_class(class: u8) -> u8 {
        match class {
            0..=12 => 6,
            13..=90 => 4,
            91..=168 => 12,
            _ => panic!("PreflopLossless169::hole_count_in_class: class {class} >= 169"),
        }
    }
}

impl Default for PreflopLossless169 {
    fn default() -> Self {
        Self::new()
    }
}

impl InfoAbstraction for PreflopLossless169 {
    /// preflop 路径：`(hand_class_169, position_bucket, stack_bucket, betting_state)`
    /// 复合到 `InfoSetId`（D-215 统一 64-bit 编码，`bucket_id = hand_class_169`，
    /// `street_tag = StreetTag::Preflop`）。
    fn map(&self, state: &GameState, hole: [Card; 2]) -> InfoSetId {
        let actor_seat = state
            .current_player()
            .expect("InfoAbstraction::map called on terminal state (IA-006-rev1)");
        let class = self.hand_class(hole);
        let position_bucket = compute_position_bucket(state, actor_seat);
        let stack_bucket = compute_stack_bucket(state, actor_seat);
        let betting_state = compute_betting_state(state);
        let street_tag = compute_street_tag(state.street());
        pack_info_set_id(
            u32::from(class),
            position_bucket,
            stack_bucket,
            betting_state,
            street_tag,
        )
    }
}

// ============================================================================
// 内部 helper（pub(crate) 以便 postflop 路径复用）
// ============================================================================

pub(crate) fn compute_position_bucket(state: &GameState, actor_seat: crate::core::SeatId) -> u8 {
    let cfg = state.config();
    let n_seats = cfg.n_seats as usize;
    ((actor_seat.0 as usize + n_seats - cfg.button_seat.0 as usize) % n_seats) as u8
}

pub(crate) fn compute_stack_bucket(state: &GameState, actor_seat: crate::core::SeatId) -> u8 {
    let cfg = state.config();
    let starting_stack = cfg.starting_stacks[actor_seat.0 as usize];
    let big_blind = cfg.big_blind;
    let bb_units = if big_blind == ChipAmount::ZERO {
        0
    } else {
        starting_stack.as_u64() / big_blind.as_u64()
    };
    if bb_units < 20 {
        0
    } else if bb_units < 50 {
        1
    } else if bb_units < 100 {
        2
    } else if bb_units < 200 {
        3
    } else {
        4
    }
}

pub(crate) fn compute_betting_state(state: &GameState) -> BettingState {
    let street = state.street();
    let actions_this_street = state
        .hand_history()
        .actions
        .iter()
        .filter(|a| a.street == street);
    let mut bet_count = 0u32;
    let mut raise_count = 0u32;
    for action in actions_this_street {
        match action.action {
            Action::Bet { .. } => bet_count += 1,
            Action::Raise { .. } => raise_count += 1,
            _ => {}
        }
    }

    // Compute "raises beyond opening": preflop's BB blind serves as the opening
    // bet, so each Raise is one raise-beyond-opening; postflop's first Bet is
    // the opening, subsequent Raise are raises-beyond-opening.
    if street == Street::Preflop {
        if raise_count == 0 {
            // No voluntary raise this street: BB walking with check option → Open;
            // otherwise non-BB facing forced BB blind → FacingBetNoRaise.
            if state.legal_actions().check {
                BettingState::Open
            } else {
                BettingState::FacingBetNoRaise
            }
        } else {
            match raise_count {
                1 => BettingState::FacingRaise1,
                2 => BettingState::FacingRaise2,
                _ => BettingState::FacingRaise3Plus,
            }
        }
    } else {
        // Postflop
        if bet_count == 0 && raise_count == 0 {
            BettingState::Open
        } else if bet_count >= 1 && raise_count == 0 {
            BettingState::FacingBetNoRaise
        } else {
            match raise_count {
                1 => BettingState::FacingRaise1,
                2 => BettingState::FacingRaise2,
                _ => BettingState::FacingRaise3Plus,
            }
        }
    }
}

pub(crate) fn compute_street_tag(street: Street) -> StreetTag {
    match street {
        Street::Preflop => StreetTag::Preflop,
        Street::Flop => StreetTag::Flop,
        Street::Turn => StreetTag::Turn,
        Street::River => StreetTag::River,
        Street::Showdown => panic!(
            "InfoAbstraction::map called on Showdown state (IA-006-rev1: caller must filter \
             terminal states)"
        ),
    }
}

fn order_pair(hole: [Card; 2]) -> (u8, u8) {
    let a = hole[0].to_u8();
    let b = hole[1].to_u8();
    if a < b {
        (a, b)
    } else if a > b {
        (b, a)
    } else {
        panic!("canonical_hole_id: duplicate cards in hole {hole:?}")
    }
}

fn pair_combination_index(lo: u8, hi: u8) -> u32 {
    debug_assert!(lo < hi && hi < 52);
    let lo = u32::from(lo);
    let hi = u32::from(hi);
    // sum_{i=0..lo} (51 - i) = 51*lo - lo*(lo-1)/2
    let prefix = 51 * lo - lo * lo.saturating_sub(1) / 2;
    prefix + (hi - lo - 1)
}
