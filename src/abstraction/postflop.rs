//! Postflop bucket abstraction（API §2）。
//!
//! `PostflopBucketAbstraction` + `canonical_observation_id` helper
//! （D-216-rev1 / D-218-rev1 / D-244-rev1）。

use crate::core::Card;
use crate::rules::state::GameState;

use crate::abstraction::bucket_table::{BucketConfig, BucketTable};
use crate::abstraction::info::{InfoAbstraction, InfoSetId, StreetTag};

/// postflop 联合 (board, hole) canonical observation id ∈
/// 0..n_canonical_observation(street)（花色对称等价类）。
/// `BucketTable::lookup(street, _)` postflop 入参由本函数计算。
///
/// 仅对 `StreetTag::{Flop, Turn, River}` 有效（`board.len() ∈ {3, 4, 5}`）；
/// `StreetTag::Preflop` 调用 panic（caller 应改用 `canonical_hole_id`）。
pub fn canonical_observation_id(_street: StreetTag, _board: &[Card], _hole: [Card; 2]) -> u32 {
    unimplemented!("A1 stub; B2 implements per D-218-rev1 (联合 (board, hole) 花色对称等价类)")
}

/// mmap-backed postflop bucket abstraction（D-213 / D-214 / D-216 / D-218-rev1 /
/// D-219）。
pub struct PostflopBucketAbstraction {
    #[allow(dead_code)] // A1 stub; B2 fills lookup logic.
    table: BucketTable,
    /// canonical id 计算缓存等内部字段（B2 填充）。
    #[allow(dead_code)]
    _opaque: (),
}

impl PostflopBucketAbstraction {
    /// 从 mmap-loaded `BucketTable` 构造。
    pub fn new(_table: BucketTable) -> PostflopBucketAbstraction {
        unimplemented!("A1 stub; B2 implements")
    }

    /// 仅对 flop / turn / river 街生效；preflop 应走 `PreflopLossless169`。
    /// 内部走 `canonical_observation_id(street, board, hole)` →
    /// `BucketTable::lookup`。
    pub fn bucket_id(&self, _state: &GameState, _hole: [Card; 2]) -> u32 {
        unimplemented!("A1 stub; B2 implements per D-216-rev1 / D-218-rev1 / D-219")
    }

    pub fn config(&self) -> BucketConfig {
        unimplemented!("A1 stub; B2 implements")
    }
}

impl InfoAbstraction for PostflopBucketAbstraction {
    /// postflop 路径：`(street, board, hole) → bucket_id`（mmap），与 preflop key
    /// 字段（position / stack / betting_state / street_tag）合并到 `InfoSetId`
    /// （D-215 统一 64-bit 编码；`bucket_id` 由 `BucketTable::lookup` 命中得到）。
    fn map(&self, _state: &GameState, _hole: [Card; 2]) -> InfoSetId {
        unimplemented!("A1 stub; B2 implements per D-215 / D-216-rev1 / D-219")
    }
}
