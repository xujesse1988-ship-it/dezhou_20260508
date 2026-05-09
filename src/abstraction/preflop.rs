//! Preflop 169 lossless 抽象（API §2）。
//!
//! `PreflopLossless169` + `canonical_hole_id` helper（D-217 / D-218-rev1）。

use crate::core::Card;
use crate::rules::state::GameState;

use crate::abstraction::info::{InfoAbstraction, InfoSetId};

/// preflop hole 单维 canonical id ∈ 0..1326（花色对称归一化）。
/// `BucketTable::lookup(StreetTag::Preflop, _)` 入参由本函数计算。
pub fn canonical_hole_id(_hole: [Card; 2]) -> u32 {
    unimplemented!("A1 stub; B2 implements per D-218-rev1 (花色对称归一化)")
}

/// 169 lossless 等价类抽象（D-217）。
pub struct PreflopLossless169 {
    #[allow(dead_code)] // A1 stub; B2 fills lookup table.
    _opaque: (),
}

impl PreflopLossless169 {
    pub fn new() -> PreflopLossless169 {
        unimplemented!("A1 stub; B2 implements per D-217 169 hand class closed-form")
    }

    /// 169 lossless 等价类编号（D-217）：
    ///
    /// - `0..13` = pocket pairs（22, 33, ..., AA 升序）
    /// - `13..91` = suited（按高牌主排序、低牌副排序：32s 起，AKs 终）
    /// - `91..169` = offsuit（同顺序）
    pub fn hand_class(&self, _hole: [Card; 2]) -> u8 {
        unimplemented!("A1 stub; B2 implements per D-217")
    }

    /// 169 类总 hole 计数：pairs 6 / suited 4 / offsuit 12，总和 1326。
    pub fn hole_count_in_class(_class: u8) -> u8 {
        unimplemented!("A1 stub; B2 implements per D-217")
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
    fn map(&self, _state: &GameState, _hole: [Card; 2]) -> InfoSetId {
        unimplemented!("A1 stub; B2 implements per D-215 / D-217")
    }
}
