//! 运行时映射热路径子模块（D-252 / D-273）。
//!
//! 本模块持有 `InfoAbstraction::map` / `BucketTable::lookup` 等运行时映射调用的
//! 内部实现细节（`InfoSetId` 位编码、preflop / postflop bucket id 派发等）。
//! 所有计算必须为整数路径——浮点特征提取 / clustering 在 sibling 模块
//! `abstraction::feature` / `abstraction::cluster` / `abstraction::equity` 完成，
//! 输出量化为 `u8` 写入 mmap bucket table 后由本模块以整数 key lookup。
//!
//! D-252 锁死的实现手段：本模块文件顶 `#![deny(clippy::float_arithmetic)]` inner
//! attribute，让 clippy 在本模块内任何 `f32` / `f64` 算术触发硬错（即使 `cargo
//! clippy` 无 `-D warnings`）。该约束保护 stage 4+ CFR / 实时搜索 driver 在
//! `map` 路径上的 byte-equal 跨架构稳定性（继承 stage 1 D-026 整数边界精神到
//! stage 2 抽象层）。
//!
//! D-254 内部子模块隔离：本模块不在 `lib.rs` 顶层 re-export，仅通过
//! `PreflopLossless169::map` / `PostflopBucketAbstraction::map` 间接对外。

#![deny(clippy::float_arithmetic)]

use crate::abstraction::info::{BettingState, InfoSetId, StreetTag};

// InfoSetId 64-bit layout（D-215，低位起）：
//   bit  0..24: bucket_id          (24 bit)
//   bit 24..28: position_bucket    ( 4 bit)
//   bit 28..32: stack_bucket       ( 4 bit)
//   bit 32..35: betting_state      ( 3 bit)
//   bit 35..38: street_tag         ( 3 bit)
//   bit 38..64: reserved           (26 bit, must be 0)

const BUCKET_ID_SHIFT: u32 = 0;
const POSITION_SHIFT: u32 = 24;
const STACK_SHIFT: u32 = 28;
const BETTING_STATE_SHIFT: u32 = 32;
const STREET_TAG_SHIFT: u32 = 35;

const BUCKET_ID_MASK: u64 = (1u64 << 24) - 1;
const POSITION_MASK: u64 = (1u64 << 4) - 1;
const STACK_MASK: u64 = (1u64 << 4) - 1;
const BETTING_STATE_MASK: u64 = (1u64 << 3) - 1;
const STREET_TAG_MASK: u64 = (1u64 << 3) - 1;

pub(crate) fn pack_info_set_id(
    bucket_id: u32,
    position_bucket: u8,
    stack_bucket: u8,
    betting_state: BettingState,
    street_tag: StreetTag,
) -> InfoSetId {
    debug_assert!(
        u64::from(bucket_id) <= BUCKET_ID_MASK,
        "bucket_id must fit in 24 bits"
    );
    debug_assert!(
        u64::from(position_bucket) <= POSITION_MASK,
        "position_bucket must fit in 4 bits"
    );
    debug_assert!(
        u64::from(stack_bucket) <= STACK_MASK,
        "stack_bucket must fit in 4 bits"
    );
    let raw = (u64::from(bucket_id) & BUCKET_ID_MASK) << BUCKET_ID_SHIFT
        | (u64::from(position_bucket) & POSITION_MASK) << POSITION_SHIFT
        | (u64::from(stack_bucket) & STACK_MASK) << STACK_SHIFT
        | (u64::from(betting_state as u8) & BETTING_STATE_MASK) << BETTING_STATE_SHIFT
        | (u64::from(street_tag as u8) & STREET_TAG_MASK) << STREET_TAG_SHIFT;
    InfoSetId::from_raw_internal(raw)
}

pub(crate) fn unpack_bucket_id(raw: u64) -> u32 {
    ((raw >> BUCKET_ID_SHIFT) & BUCKET_ID_MASK) as u32
}

pub(crate) fn unpack_position_bucket(raw: u64) -> u8 {
    ((raw >> POSITION_SHIFT) & POSITION_MASK) as u8
}

pub(crate) fn unpack_stack_bucket(raw: u64) -> u8 {
    ((raw >> STACK_SHIFT) & STACK_MASK) as u8
}

pub(crate) fn unpack_betting_state(raw: u64) -> BettingState {
    let bits = ((raw >> BETTING_STATE_SHIFT) & BETTING_STATE_MASK) as u8;
    match bits {
        0 => BettingState::Open,
        1 => BettingState::FacingBetNoRaise,
        2 => BettingState::FacingRaise1,
        3 => BettingState::FacingRaise2,
        4 => BettingState::FacingRaise3Plus,
        _ => panic!("InfoSetId: invalid betting_state bits {bits}"),
    }
}

pub(crate) fn unpack_street_tag(raw: u64) -> StreetTag {
    let bits = ((raw >> STREET_TAG_SHIFT) & STREET_TAG_MASK) as u8;
    match bits {
        0 => StreetTag::Preflop,
        1 => StreetTag::Flop,
        2 => StreetTag::Turn,
        3 => StreetTag::River,
        _ => panic!("InfoSetId: invalid street_tag bits {bits}"),
    }
}
