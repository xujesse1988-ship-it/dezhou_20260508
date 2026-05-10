//! 手牌评估器（API §6）。

use crate::core::Card;

/// 不透明手牌强度。数值越大越强；同值代表同强度（split pot）。
///
/// 注意：不要求 `HandRank` 数值跨不同 evaluator 实现一致；只要求**同一 evaluator
/// 内部全序稳定**。
#[derive(Copy, Clone, Eq, PartialEq, PartialOrd, Ord, Hash, Debug)]
pub struct HandRank(pub u32);

impl HandRank {
    pub fn category(self) -> HandCategory {
        match self.0 / RANK_BASE {
            0 => HandCategory::HighCard,
            1 => HandCategory::OnePair,
            2 => HandCategory::TwoPair,
            3 => HandCategory::Trips,
            4 => HandCategory::Straight,
            5 => HandCategory::Flush,
            6 => HandCategory::FullHouse,
            7 => HandCategory::Quads,
            8 => HandCategory::StraightFlush,
            _ => HandCategory::RoyalFlush,
        }
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum HandCategory {
    HighCard,
    OnePair,
    TwoPair,
    Trips,
    Straight,
    Flush,
    FullHouse,
    Quads,
    StraightFlush,
    RoyalFlush,
}

/// 评估器接口。同一 trait 同时支持 5/6/7-card。
///
/// `eval6` / `eval7` 必须返回所有 5-card 子集中最强的 `HandRank`；
/// 三个接口对相同 5-card 输入必须返回相同 `HandRank`。
pub trait HandEvaluator: Send + Sync {
    fn eval5(&self, cards: &[Card; 5]) -> HandRank;
    fn eval6(&self, cards: &[Card; 6]) -> HandRank;
    fn eval7(&self, cards: &[Card; 7]) -> HandRank;
}

const RANK_BASE: u32 = 13_u32.pow(5); // 371_293
const RANK_P4: u32 = 13_u32.pow(4); // 28_561
const RANK_P3: u32 = 13_u32.pow(3); // 2_197
const RANK_P2: u32 = 13_u32.pow(2); // 169
const RANK_P1: u32 = 13;

/// 6-max NLHE 评估器。
///
/// **算法**（E2 替换；E1 留下的朴素 C(7,5) = 21 × eval5 实现已淘汰）：
///
/// 1. 单 pass 折叠 N 张牌为 5 个 13-bit u16 掩码：
///    - `by_suit[4]` —— 每花色 13-bit rank set（用于 flush / straight flush）。
///    - `all_mask` —— 出现 ≥1 次的 rank。
///    - `pair_mask` —— 出现 ≥2 次的 rank（两对、葫芦 pair 部分）。
///    - `trip_mask` —— 出现 ≥3 次的 rank。
///    - `quad_mask` —— 出现 = 4 次的 rank。
///
///    每张牌只读一次，常量级位运算更新：旧 `all_mask & bit != 0` ⇒ pair 起；
///    旧 `pair_mask & bit != 0` ⇒ trip 起；旧 `trip_mask & bit != 0` ⇒ quad 起。
/// 2. 类别判定全部走位运算：
///    - flush ⇐ 任一 `by_suit[s].count_ones() >= 5`，flush 牌型 = 该花色掩码
///      `top 5` bit。
///    - straight ⇐ 13-bit 掩码查 `STRAIGHT_HIGH_TABLE`（8 KiB const 内嵌
///      表，含 wheel）。
///    - quads/full house/trips/two pair/one pair/high card 由 4 个 mask 取
///      `highest_bit` + `mask & !bit` 链得到。
/// 3. 编码沿用 `category * 13^5 + base-13(kickers)`，与历史朴素实现的
///    `HandRank` 数值**完全一致**——`tests/evaluator.rs` 的 `eval7 ==
///    max(eval5 over 7-choose-5)` 等价断言因此免修改。
///
/// 复杂度：eval5/6/7 均 O(1)（`N` 次 histogram + 至多 5 次 `highest_bit` +
/// 1 次 8 KiB 表查）。零分配；零浮点；零 unsafe（`lints.rust unsafe_code =
/// "forbid"`）。release 模式单线程吞吐 ≥ 10M eval/s 满足 validation §8 SLO。
///
/// 类型名 `NaiveHandEvaluator` 是 B2/E1 历史命名（朴素 C(7,5) 实现）；E2 替换
/// 内核但保留类型名以避免破坏 `tests/perf_slo.rs`（`[测试]` 文件，`[实现]`
/// agent 不得修改）与 `tests/evaluator.rs` 的
/// `use poker::eval::NaiveHandEvaluator` 引用。
#[derive(Copy, Clone, Debug, Default)]
pub struct NaiveHandEvaluator;

impl HandEvaluator for NaiveHandEvaluator {
    #[inline]
    fn eval5(&self, cards: &[Card; 5]) -> HandRank {
        eval_inner(cards)
    }

    #[inline]
    fn eval6(&self, cards: &[Card; 6]) -> HandRank {
        eval_inner(cards)
    }

    #[inline]
    fn eval7(&self, cards: &[Card; 7]) -> HandRank {
        eval_inner(cards)
    }
}

#[inline(always)]
pub(crate) fn eval7(cards: &[Card; 7]) -> HandRank {
    eval_inner::<7>(cards)
}

/// 主评估路径。const-generic 让 LLVM 在 5/6/7-card 三个调用点完全展开
/// histogram 循环。
#[inline(always)]
pub(crate) fn eval_inner<const N: usize>(cards: &[Card; N]) -> HandRank {
    let mut by_suit = [0u16; 4];
    let mut all_mask: u16 = 0; // count >= 1
    let mut pair_mask: u16 = 0; // count >= 2
    let mut trip_mask: u16 = 0; // count >= 3
    let mut quad_mask: u16 = 0; // count == 4

    let mut i = 0usize;
    while i < N {
        let v = cards[i].to_u8();
        let r = (v >> 2) as u32;
        let s = (v & 0b11) as usize;
        let bit: u16 = 1u16 << r;
        by_suit[s] |= bit;
        // 阶梯式提升：先看更高一档是否已置位（bit 已在该档里），再点亮下一档。
        // 每张牌的 4 行更新流水线无依赖反链，便于 ILP。
        quad_mask |= trip_mask & bit;
        trip_mask |= pair_mask & bit;
        pair_mask |= all_mask & bit;
        all_mask |= bit;
        i += 1;
    }

    // Flush：4 花色中至多一花 ≥5（5+5=10 > 7 张）。
    let mut flush_mask: u16 = 0;
    if by_suit[0].count_ones() >= 5 {
        flush_mask = by_suit[0];
    } else if by_suit[1].count_ones() >= 5 {
        flush_mask = by_suit[1];
    } else if by_suit[2].count_ones() >= 5 {
        flush_mask = by_suit[2];
    } else if by_suit[3].count_ones() >= 5 {
        flush_mask = by_suit[3];
    }

    if flush_mask != 0 {
        let sf_high = STRAIGHT_HIGH_TABLE[flush_mask as usize];
        if sf_high != STRAIGHT_NONE {
            if sf_high == 12 {
                // Royal flush.
                return HandRank(9 * RANK_BASE + 12 * RANK_P4);
            }
            return HandRank(8 * RANK_BASE + (sf_high as u32) * RANK_P4);
        }
    }

    // Quads：highest_bit(quad_mask) + 1 kicker。
    if quad_mask != 0 {
        let q = highest_bit(quad_mask);
        let k_mask = all_mask & !(1u16 << q);
        let k = highest_bit(k_mask);
        return HandRank(7 * RANK_BASE + q * RANK_P4 + k * RANK_P3);
    }

    // Full house：trip_mask 不空且 (pair_mask \ trip 顶位) 不空。
    if trip_mask != 0 {
        let t = highest_bit(trip_mask);
        let pair_part_mask = pair_mask & !(1u16 << t);
        if pair_part_mask != 0 {
            let p = highest_bit(pair_part_mask);
            return HandRank(6 * RANK_BASE + t * RANK_P4 + p * RANK_P3);
        }
    }

    // Flush（非直）：从 flush_mask 取高 5 bit。
    if flush_mask != 0 {
        let kickers = top5(flush_mask);
        return HandRank(
            5 * RANK_BASE
                + kickers[0] * RANK_P4
                + kickers[1] * RANK_P3
                + kickers[2] * RANK_P2
                + kickers[3] * RANK_P1
                + kickers[4],
        );
    }

    // Straight：13-bit all_mask 查 STRAIGHT_HIGH_TABLE（含 wheel）。
    let s_high = STRAIGHT_HIGH_TABLE[all_mask as usize];
    if s_high != STRAIGHT_NONE {
        return HandRank(4 * RANK_BASE + (s_high as u32) * RANK_P4);
    }

    // Trips（无 FH）：trip + 2 高 kicker。
    if trip_mask != 0 {
        let t = highest_bit(trip_mask);
        let mut km = all_mask & !(1u16 << t);
        let k1 = highest_bit(km);
        km &= !(1u16 << k1);
        let k2 = highest_bit(km);
        return HandRank(3 * RANK_BASE + t * RANK_P4 + k1 * RANK_P3 + k2 * RANK_P2);
    }

    // Two pair：pair_mask 至少 2 个 bit（trips 已分流到 FH，到这里 pair_mask
    // ⊆ "count == 2"）。p1/p2 + 1 kicker（最高非对位 rank）。
    if pair_mask.count_ones() >= 2 {
        let p1 = highest_bit(pair_mask);
        let pm = pair_mask & !(1u16 << p1);
        let p2 = highest_bit(pm);
        let k_mask = all_mask & !(1u16 << p1) & !(1u16 << p2);
        let k = if k_mask == 0 { 0 } else { highest_bit(k_mask) };
        return HandRank(2 * RANK_BASE + p1 * RANK_P4 + p2 * RANK_P3 + k * RANK_P2);
    }

    // One pair：pair + 3 高 kicker。
    if pair_mask != 0 {
        let p = highest_bit(pair_mask);
        let mut km = all_mask & !(1u16 << p);
        let k1 = if km == 0 { 0 } else { highest_bit(km) };
        if k1 != 0 || km != 0 {
            km &= !(1u16 << k1);
        }
        let k2 = if km == 0 { 0 } else { highest_bit(km) };
        if k2 != 0 || km != 0 {
            km &= !(1u16 << k2);
        }
        let k3 = if km == 0 { 0 } else { highest_bit(km) };
        return HandRank(RANK_BASE + p * RANK_P4 + k1 * RANK_P3 + k2 * RANK_P2 + k3 * RANK_P1);
    }

    // High card：top 5 distinct ranks。N == 5/6/7 都至少 5 distinct（无 pair 分支）。
    let kickers = top5(all_mask);
    HandRank(
        kickers[0] * RANK_P4
            + kickers[1] * RANK_P3
            + kickers[2] * RANK_P2
            + kickers[3] * RANK_P1
            + kickers[4],
    )
}

/// `mask` 必须非零。返回最高位的位置（0..=15）。底层走 `leading_zeros`，x86_64
/// 上一条 BSR/LZCNT。
#[inline(always)]
fn highest_bit(mask: u16) -> u32 {
    debug_assert!(mask != 0);
    15 - mask.leading_zeros()
}

/// 取 13-bit 掩码的最高 5 个 bit 位置（高位优先）。掩码 popcnt 不足 5 时高位
/// 优先填，余位补 0。
#[inline(always)]
fn top5(mask: u16) -> [u32; 5] {
    let mut out = [0u32; 5];
    let mut m = mask;
    let mut i = 0usize;
    while i < 5 && m != 0 {
        let h = highest_bit(m);
        out[i] = h;
        m &= !(1u16 << h);
        i += 1;
    }
    out
}

const STRAIGHT_NONE: u8 = 0xFF; // sentinel from STRAIGHT_HIGH_TABLE

/// 13-bit rank mask → straight 高位（`0..=12`）；`STRAIGHT_NONE = 0xFF` 表无直。
///
/// 编译期 const 计算：8192 项 × u8 = 8 KiB，stage-1 整二进制内嵌（rodata 段）；
/// 与 D-026「无浮点 / 无 unsafe」+ `lints.rust unsafe_code = "forbid"` 兼容。
/// 含 wheel（A-2-3-4-5，high = rank(5) = 3）。
const STRAIGHT_HIGH_TABLE: [u8; 8192] = build_straight_table();

const fn build_straight_table() -> [u8; 8192] {
    let mut table = [STRAIGHT_NONE; 8192];
    let mut mask: u16 = 0;
    while mask < 8192 {
        // 9 个高位窗口：high = 12..=4 找首个匹配的 5-连位窗。
        let mut high: u8 = 12;
        let mut found: u8 = STRAIGHT_NONE;
        loop {
            let needed: u16 = 0b11111u16 << (high - 4);
            if mask & needed == needed {
                found = high;
                break;
            }
            if high == 4 {
                break;
            }
            high -= 1;
        }
        if found == STRAIGHT_NONE {
            // Wheel: A(rank 12) + 2/3/4/5 (rank 0/1/2/3) → high = rank(5) = 3。
            let wheel: u16 = (1u16 << 12) | 0b1111;
            if mask & wheel == wheel {
                found = 3;
            }
        }
        table[mask as usize] = found;
        mask += 1;
    }
    table
}
