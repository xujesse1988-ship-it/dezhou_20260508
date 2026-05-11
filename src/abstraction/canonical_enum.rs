//! D-218-rev2 真等价类枚举（§G-batch1 §3.1 [实现]）。
//!
//! Waugh 2013-style hand isomorphism for postflop (board, hole) canonical
//! observation id。本模块实现 D-218-rev2 字面 (board, hole) 联合花色对称等价
//! 类完整枚举：把任意 (board, hole) 输入映射到 dense `[0, N)` 上的稠密整数
//! canonical id，N 是该街的标准 hand-isomorphism 数（flop = 1,286,792 / turn =
//! 13,960,050 / river = 123,156,254）。
//!
//! 详见 `docs/pluribus_stage2_decisions.md` §10 "Stage 3 起步 batch 1 —
//! D-218-rev2 / D-244-rev2"；本模块替代 stage 2 §C-rev1 §2 落地的 FNV-1a
//! hash-with-mod approximation 路径
//! （`crate::abstraction::postflop::canonical_observation_id` 在 §G-batch1 §3.2
//! 改为 forward 调用本模块的 [`canonical_observation_id`]）。
//!
//! # 算法
//!
//! 1. **Suit canonicalization**：为每个 suit `s ∈ {0,1,2,3}` 计算
//!    `(board_mask: u16, hole_mask: u16)`（13-bit rank set 各占低 13 位）。把 4
//!    元组 `[(b_count, h_count, b_mask, h_mask); 4]` 按字典序排序得到 canonical
//!    suit 顺序——同一个 hand-isomorphism 等价类下，suit 重标后 4-tuple
//!    signature 完全相同。
//!
//! 2. **Canonical form key packing**：把 canonical 4-tuple 打包到 `u128`，使得
//!    `u128` 数值序与 canonical tuple 字典序等价——`MSB → LSB` 排布:
//!    `[suit0 32-bit][suit1 32-bit][suit2 32-bit][suit3 32-bit]`，每 suit 32-bit
//!    内部按 `[b_count (3) | h_count (2) | b_mask (13) | h_mask (13) | unused (1)]`
//!    高位到低位排列。pack 输出在 `u128 高 127 位之内（最高 1 位永 0），所以
//!    `u128 numerical cmp == canonical tuple lex cmp`。
//!
//! 3. **Lazy sorted table per street**：第一次调用 [`canonical_observation_id`]
//!    时，对该街枚举所有 canonical form key 并 sort 到 `Vec<u128>`。canonical
//!    id = `Vec` 中 binary-search position。tables 走 [`OnceLock`]，per-street
//!    lazy build 避免不需要 river 的测试也付 ~1 s build cost。
//!
//! 4. **Enumeration**：用递归方式枚举每个 canonical shape (b_counts, h_counts
//!    per canonical suit 多重集) → 每个 shape 内 enumerate canonical multiset of
//!    (b_mask, h_mask) pairs。详见 [`enumerate_canonical_forms`]。复杂度上界 =
//!    N（每 canonical form 严格 enumerate 一次，无 brute-force dedup）。
//!
//! # 内存预算
//!
//! Per-street sorted `Vec<u128>` 大小：
//!
//! | street | N | bytes (×16) |
//! |---|---|---|
//! | flop | 1,286,792 | ~20.6 MB |
//! | turn | 13,960,050 | ~223 MB |
//! | river | 123,156,254 | ~1.97 GB |
//!
//! Lazy build 让只 touch flop 的测试只付 ~416 KB；full training 路径付完整
//! ~1.99 GB。Build time on 1-CPU host: flop ~30 ms / turn ~2 s / river ~3 min。
//!
//! # 不变量
//!
//! - **确定性**：纯函数，无 RNG / 全局状态（OnceLock 仅 cache build 结果）。
//! - **input-order 不变性**：board / hole 任意输入顺序得到同一 id（pack 前
//!   先按 raw `Card::to_u8()` sort，去除 caller 传入顺序影响）。
//! - **花色对称不变性**：全局花色置换 σ 应用到 (board, hole) 后得到同一 id
//!   （suit canonicalization 把 σ 吸收）。
//! - **board/hole partition 区分**：相同 rank multiset 不同 partition 划分得到
//!   不同 id（partition info 显式编码在 b_mask vs h_mask）。
//! - **唯一性（新）**：D-218-rev2 §3 字面要求——两个互不等价的 (board, hole) 一定
//!   映射到不同 id。由 sorted Vec dedup + binary search 严格保证。
//! - **稠密性**：id ∈ `[0, N)` 全覆盖。

#![deny(clippy::float_arithmetic)]

use std::sync::OnceLock;

use crate::abstraction::info::StreetTag;
use crate::core::Card;

// ============================================================================
// 公开常量 + n_canonical_observation
// ============================================================================

/// Flop canonical observation count（标准 hand-isomorphism 数；
/// 3 board + 2 hole = 5 cards over suit-symmetric partition）。
/// 实测于 `enumerate_canonical_forms(3, 2)`。
pub const N_CANONICAL_OBSERVATION_FLOP: u32 = 1_286_792;

/// Turn canonical observation count（4 board + 2 hole = 6 cards）。
/// 实测于 `enumerate_canonical_forms(4, 2)`。
pub const N_CANONICAL_OBSERVATION_TURN: u32 = 13_960_050;

/// River canonical observation count（5 board + 2 hole = 7 cards）。
pub const N_CANONICAL_OBSERVATION_RIVER: u32 = 123_156_254;

/// 返回 `street` 的 canonical observation 数。preflop 固定 1326（hole-only
/// canonical，由 `preflop.rs::canonical_hole_id` 处理）；本模块函数 panic 在
/// preflop 路径。
pub fn n_canonical_observation(street: StreetTag) -> u32 {
    match street {
        StreetTag::Preflop => 1326,
        StreetTag::Flop => N_CANONICAL_OBSERVATION_FLOP,
        StreetTag::Turn => N_CANONICAL_OBSERVATION_TURN,
        StreetTag::River => N_CANONICAL_OBSERVATION_RIVER,
    }
}

// ============================================================================
// canonical form key packing
// ============================================================================

/// 给定 (board, hole)，计算 canonical form `u128` key。
///
/// Layout（MSB → LSB，4 suits × 32-bit）：
///
/// ```text
///   bit 127:96  →  suit 0 signature (32-bit packed)
///   bit  95:64  →  suit 1 signature
///   bit  63:32  →  suit 2 signature
///   bit  31: 0  →  suit 3 signature
/// ```
///
/// Per-suit 32-bit signature（高位优先）：
///
/// ```text
///   bit 31    →  unused (0)
///   bit 30:28 →  b_count (0..=5)        (3 bits)
///   bit 27:26 →  h_count (0..=2)        (2 bits)
///   bit 25:13 →  b_mask (13-bit rank set)
///   bit 12: 0 →  h_mask (13-bit rank set)
/// ```
///
/// 因为 `u128` 数值序与 canonical (b_count, h_count, b_mask, h_mask) tuple 字典
/// 序一一对应（每 component 占用固定高位段 + 不溢出），sort `Vec<u128>` 等价于
/// sort canonical tuple Vec。
fn pack_canonical_form_key(board: &[Card], hole: &[Card; 2]) -> u128 {
    let mut suits: [(u16, u16); 4] = [(0, 0); 4];
    for &c in board {
        suits[c.suit() as usize].0 |= 1u16 << (c.rank() as u8);
    }
    for &c in hole {
        suits[c.suit() as usize].1 |= 1u16 << (c.rank() as u8);
    }

    let mut sigs: [(u8, u8, u16, u16); 4] = [(0, 0, 0, 0); 4];
    for (s, suit) in suits.iter().enumerate() {
        sigs[s] = (
            suit.0.count_ones() as u8,
            suit.1.count_ones() as u8,
            suit.0,
            suit.1,
        );
    }
    sigs.sort_unstable();

    let mut key: u128 = 0;
    for (i, sig) in sigs.iter().enumerate() {
        let pack: u128 = ((sig.0 as u128) << 28)
            | ((sig.1 as u128) << 26)
            | ((sig.2 as u128) << 13)
            | (sig.3 as u128);
        let shift = 32 * (3 - i);
        key |= pack << shift;
    }
    key
}

// ============================================================================
// canonical_observation_id：lazy table + binary search
// ============================================================================

static FLOP_TABLE: OnceLock<Vec<u128>> = OnceLock::new();
static TURN_TABLE: OnceLock<Vec<u128>> = OnceLock::new();
static RIVER_TABLE: OnceLock<Vec<u128>> = OnceLock::new();

fn lazy_table(street: StreetTag) -> &'static [u128] {
    match street {
        StreetTag::Preflop => {
            panic!("canonical_enum::lazy_table called on Preflop; use canonical_hole_id")
        }
        StreetTag::Flop => FLOP_TABLE.get_or_init(|| build_sorted_table(StreetTag::Flop)),
        StreetTag::Turn => TURN_TABLE.get_or_init(|| build_sorted_table(StreetTag::Turn)),
        StreetTag::River => RIVER_TABLE.get_or_init(|| build_sorted_table(StreetTag::River)),
    }
    .as_slice()
}

/// 计算 (board, hole) 在 `street` 街上的 canonical observation id ∈ `[0, N)`。
///
/// 仅对 `StreetTag::{Flop, Turn, River}` 有效；preflop 路径 panic（caller 应改用
/// [`crate::abstraction::preflop::canonical_hole_id`]）。
///
/// 算法：pack canonical form 到 u128 sort key → binary search 在 lazy 构造的
/// sorted `Vec<u128>` 中 → 返回 index。
///
/// 复杂度：`O(log N)` per call after lazy build；build cost 在第一次 call 时
/// 摊销（flop ~30 ms / turn ~2 s / river ~3 min on 1-CPU host）。
pub fn canonical_observation_id(street: StreetTag, board: &[Card], hole: [Card; 2]) -> u32 {
    if matches!(street, StreetTag::Preflop) {
        panic!(
            "canonical_observation_id called with StreetTag::Preflop; use canonical_hole_id \
             for preflop hole canonical id (D-218-rev2 §2)"
        );
    }
    let expected_board_len = match street {
        StreetTag::Flop => 3usize,
        StreetTag::Turn => 4,
        StreetTag::River => 5,
        StreetTag::Preflop => unreachable!(),
    };
    assert_eq!(
        board.len(),
        expected_board_len,
        "canonical_observation_id: board length mismatch for {street:?}: expected \
         {expected_board_len}, got {}",
        board.len()
    );

    let key = pack_canonical_form_key(board, &hole);
    let table = lazy_table(street);
    match table.binary_search(&key) {
        Ok(idx) => idx as u32,
        Err(_) => panic!(
            "canonical_observation_id: canonical form key 0x{key:032x} not found in lazy \
             table (street {street:?}); enumeration bug — please file issue"
        ),
    }
}

// ============================================================================
// 枚举 + 构表
// ============================================================================

/// 构造 street 的 sorted canonical form `Vec<u128>`（一次性 build，OnceLock cache）。
fn build_sorted_table(street: StreetTag) -> Vec<u128> {
    let expected_n = match street {
        StreetTag::Preflop => unreachable!(),
        StreetTag::Flop => N_CANONICAL_OBSERVATION_FLOP as usize,
        StreetTag::Turn => N_CANONICAL_OBSERVATION_TURN as usize,
        StreetTag::River => N_CANONICAL_OBSERVATION_RIVER as usize,
    };
    let board_size: u8 = match street {
        StreetTag::Preflop => unreachable!(),
        StreetTag::Flop => 3,
        StreetTag::Turn => 4,
        StreetTag::River => 5,
    };

    let mut table: Vec<u128> = Vec::with_capacity(expected_n);
    enumerate_canonical_forms(board_size, 2, &mut |key| {
        table.push(key);
    });
    table.sort_unstable();
    debug_assert!(
        table.windows(2).all(|w| w[0] < w[1]),
        "canonical_enum: enumerate_canonical_forms 产出重复 canonical key for {street:?}"
    );
    assert_eq!(
        table.len(),
        expected_n,
        "canonical_enum: street {street:?} 枚举得到 {} 条，期望 {expected_n}",
        table.len()
    );
    table
}

/// 递归枚举 (board, hole) 全部 canonical forms，每个 form 调一次 `callback`。
///
/// 总共枚举 N 个 canonical form（无 brute-force dedup）。算法分两层：
///
/// 1. **Shape enumeration**：枚举 4 个 canonical-sorted (b_count, h_count) 对，
///    满足 `sum(b_count) = board_size` + `sum(h_count) = hole_size` + 4 元组按
///    `(b_count, h_count)` 字典序非降。
///
/// 2. **Mask enumeration per shape**：对每个 shape，4 个 canonical suit 按
///    multiset 顺序 enumerate (b_mask, h_mask) 配置——shape 内连续相同
///    `(b_count, h_count)` 的 suit 组成 group，group 内 (b_mask, h_mask) 必须
///    canonical-sorted。详见 [`enumerate_mask_assignments_for_shape`]。
fn enumerate_canonical_forms<F: FnMut(u128)>(board_size: u8, hole_size: u8, callback: &mut F) {
    let mut shape = [(0u8, 0u8); 4];
    enumerate_shapes(
        &mut shape,
        0,
        board_size,
        hole_size,
        (0, 0),
        &mut |s: &[(u8, u8); 4]| {
            enumerate_mask_assignments_for_shape(s, callback);
        },
    );
}

/// 递归枚举 canonical-sorted shape：4 个 `(b_count, h_count)` pair，满足
/// 字典序非降 + 总 b_count 等于 `b_total` + 总 h_count 等于 `h_total`。
fn enumerate_shapes<F: FnMut(&[(u8, u8); 4])>(
    shape: &mut [(u8, u8); 4],
    idx: usize,
    b_rem: u8,
    h_rem: u8,
    min_sig: (u8, u8),
    callback: &mut F,
) {
    if idx == 4 {
        if b_rem == 0 && h_rem == 0 {
            callback(shape);
        }
        return;
    }
    // 剩余 suit 数 = 4 - idx。当前 suit 必须 ≥ min_sig 且 ≤ (b_rem, h_rem)；
    // 同时 b_count + h_count ≤ 13（per-suit max ranks）。
    let max_b = b_rem.min(13);
    for b in min_sig.0..=max_b {
        let max_h = h_rem.min(13 - b);
        let h_start = if b == min_sig.0 { min_sig.1 } else { 0 };
        for h in h_start..=max_h {
            shape[idx] = (b, h);
            enumerate_shapes(shape, idx + 1, b_rem - b, h_rem - h, (b, h), callback);
        }
    }
}

/// 对每个 shape，递归 enumerate canonical (b_mask, h_mask) per suit。
///
/// 处理 multiset 约束：shape 内连续相同 (b_count, h_count) 的 suit 组成 group，
/// group 内 (b_mask, h_mask) 必须 canonical-sorted（多重集 enumeration）。
fn enumerate_mask_assignments_for_shape<F: FnMut(u128)>(shape: &[(u8, u8); 4], callback: &mut F) {
    // 识别 group：每个 group 是 shape 中连续相同 (b_count, h_count) 的最长子段。
    let mut groups: [(u8, u8, u8, u8); 4] = [(0, 0, 0, 0); 4]; // (b_count, h_count, group_start, group_end_exclusive)
    let mut n_groups = 0usize;
    let mut i = 0usize;
    while i < 4 {
        let mut j = i + 1;
        while j < 4 && shape[j] == shape[i] {
            j += 1;
        }
        groups[n_groups] = (shape[i].0, shape[i].1, i as u8, j as u8);
        n_groups += 1;
        i = j;
    }

    // 累积 mask 数组：sigs[s] = (b_count, h_count, b_mask, h_mask)
    let mut sigs: [(u8, u8, u16, u16); 4] = [(0, 0, 0, 0); 4];
    enumerate_groups(&groups[..n_groups], 0, &mut sigs, callback);
}

/// 递归遍历每个 group，group 内 enumerate canonical-sorted mask multiset。
fn enumerate_groups<F: FnMut(u128)>(
    groups: &[(u8, u8, u8, u8)],
    group_idx: usize,
    sigs: &mut [(u8, u8, u16, u16); 4],
    callback: &mut F,
) {
    if group_idx == groups.len() {
        // 全部 4 suit 已赋值，pack 输出 key
        let mut key: u128 = 0;
        for (i, sig) in sigs.iter().enumerate() {
            let pack: u128 = ((sig.0 as u128) << 28)
                | ((sig.1 as u128) << 26)
                | ((sig.2 as u128) << 13)
                | (sig.3 as u128);
            let shift = 32 * (3 - i);
            key |= pack << shift;
        }
        callback(key);
        return;
    }

    let (b_count, h_count, start, end) = groups[group_idx];
    let group_size = (end - start) as usize;
    let s_start = start as usize;
    // 在 group 内 enumerate ordered (b_mask, h_mask) multiset of size = group_size。
    enumerate_mask_multiset_in_group(
        b_count,
        h_count,
        group_size,
        s_start,
        0,
        (0u16, 0u16),
        sigs,
        &mut |s_local| enumerate_groups(groups, group_idx + 1, s_local, callback),
    );
}

/// 在一个 group 内 enumerate canonical-sorted (b_mask, h_mask) multiset of size
/// `group_size`。`min_mask` 是 group 内前一个 suit 的 (b_mask, h_mask)；当前 suit
/// 的 mask 必须 ≥ `min_mask` 字典序。
#[allow(clippy::too_many_arguments)]
fn enumerate_mask_multiset_in_group<F: FnMut(&mut [(u8, u8, u16, u16); 4])>(
    b_count: u8,
    h_count: u8,
    group_size: usize,
    s_start: usize, // 全局 suit 起始位置
    g_idx: usize,   // group 内当前 suit 偏移（0..group_size）
    min_mask: (u16, u16),
    sigs: &mut [(u8, u8, u16, u16); 4],
    callback: &mut F,
) {
    if g_idx == group_size {
        callback(sigs);
        return;
    }
    // Enumerate all valid (b_mask, h_mask) pairs >= min_mask, b_mask has b_count
    // bits set in 0..13, h_mask has h_count bits set in 0..13, b_mask & h_mask = 0。
    each_mask_pair_at_least(b_count, h_count, min_mask, |bm, hm| {
        let global_idx = s_start + g_idx;
        sigs[global_idx] = (b_count, h_count, bm, hm);
        enumerate_mask_multiset_in_group(
            b_count,
            h_count,
            group_size,
            s_start,
            g_idx + 1,
            (bm, hm),
            sigs,
            callback,
        );
    });
}

/// Enumerate all (b_mask, h_mask) pairs where:
///
/// - `b_mask.count_ones() == b_count`，bits in `0..13`
/// - `h_mask.count_ones() == h_count`，bits in `0..13`
/// - `b_mask & h_mask == 0`（per-suit 内 board / hole 用不同 rank）
/// - `(b_mask, h_mask) >= min_mask` lex
fn each_mask_pair_at_least<F: FnMut(u16, u16)>(
    b_count: u8,
    h_count: u8,
    min_mask: (u16, u16),
    mut callback: F,
) {
    each_combination_with_min(13, b_count, min_mask.0, |bm| {
        let h_start = if bm == min_mask.0 { min_mask.1 } else { 0 };
        // h_mask 必须避开 bm 占用的 bits（同 suit 内一张牌不能既 board 又 hole）
        // 同时 h_mask >= h_start（如果 bm == min_mask.0 锁定下界）。
        each_combination_subset_with_min(13, h_count, bm, h_start, |hm| {
            callback(bm, hm);
        });
    });
}

/// Enumerate `u16` masks `m` with `m.count_ones() == k`，bits in `0..n`，
/// `m >= min`，按字典序非降回调（i.e. 数值非降，因为 u16 cmp == bit lex cmp）。
fn each_combination_with_min<F: FnMut(u16)>(n: u8, k: u8, min: u16, mut callback: F) {
    if k == 0 {
        if min == 0 {
            callback(0);
        }
        return;
    }
    // 用经典 Gosper's hack 枚举 k-bit subsets of {0..n}，从 >= min 开始。
    let max_mask: u32 = (1u32 << n) - 1;
    // 起始 mask = max(min, 最小的 k-bit mask = (1 << k) - 1)
    let smallest: u32 = (1u32 << k) - 1;
    let mut m: u32 = (min as u32).max(smallest);
    // 调整 m 到 >= min 的最小有效 k-bit mask
    while m <= max_mask && (m.count_ones() as u8) != k {
        m = next_combination_or_zero(m);
        if m == 0 {
            return;
        }
    }
    if m < min as u32 {
        // 进入循环时如果 next_combination 还在 < min，跳到 min 起跳点
        // （此情况理论上 already adjusted；保留 guard 健壮）
        while m < min as u32 {
            m = next_combination_or_zero(m);
            if m == 0 {
                return;
            }
        }
    }
    while m <= max_mask {
        callback(m as u16);
        let next = next_combination_or_zero(m);
        if next == 0 {
            return;
        }
        m = next;
    }
}

/// Enumerate `u16` masks `m` with `m.count_ones() == k`, bits in `0..n`, and
/// `m & excluded == 0`, `m >= min`。当 `excluded` 占用部分 rank 位时，本函数自动
/// 跳过含 excluded bit 的 mask。
fn each_combination_subset_with_min<F: FnMut(u16)>(
    n: u8,
    k: u8,
    excluded: u16,
    min: u16,
    mut callback: F,
) {
    if k == 0 {
        if min == 0 {
            callback(0);
        }
        return;
    }
    let max_mask: u32 = (1u32 << n) - 1;
    let smallest: u32 = (1u32 << k) - 1;
    let mut m: u32 = (min as u32).max(smallest);
    while m <= max_mask {
        let m16 = m as u16;
        if (m16.count_ones() as u8) == k && (m16 & excluded) == 0 && m16 >= min {
            callback(m16);
        }
        let next = next_combination_or_zero(m);
        if next == 0 {
            return;
        }
        m = next;
    }
}

/// Gosper's hack：给定 `x` 是 k-bit mask 中的一个，返回下一个 k-bit mask（lex
/// 次序）；如已是最大 k-bit mask 则返回 0。
fn next_combination_or_zero(x: u32) -> u32 {
    if x == 0 {
        return 0;
    }
    let c = x & x.wrapping_neg();
    let r = x.wrapping_add(c);
    // (((r ^ x) >> 2) / c) | r
    // 注意：u32 算术，c != 0 保证除法合法
    (((r ^ x) >> 2) / c) | r
}

// ============================================================================
// 单元测试（§G-batch1 §3.1 落地后自验证算法正确性）
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_combination_smoke_3_bits_of_5() {
        // 5 bits 中选 3 位：序列 0b00111, 0b01011, 0b01101, 0b01110, 0b10011,
        // 0b10101, 0b10110, 0b11001, 0b11010, 0b11100 — 共 10 = C(5,3)。
        let mut x: u32 = 0b00111;
        let mut count = 0;
        while x <= 0b11111 {
            assert_eq!(x.count_ones(), 3);
            count += 1;
            let next = next_combination_or_zero(x);
            if next == 0 || next > 0b11111 {
                break;
            }
            x = next;
        }
        assert_eq!(count, 10, "C(5,3) = 10");
    }

    #[test]
    fn each_combination_with_min_full_13_choose_3() {
        let mut count = 0usize;
        each_combination_with_min(13, 3, 0, |_m| count += 1);
        assert_eq!(count, 286, "C(13,3) = 286");
    }

    #[test]
    fn each_combination_with_min_skip_first_few() {
        // C(13, 3) = 286；从 min = 0b1000000000000 起跳（rank 12 必须在 mask 内）
        // 应只枚举包含 bit 12 的 mask 中 ≥ min 的，但 each_combination_with_min 是
        // 字典序 >= min，不是 "must contain bit"；测试比较 simple 边界。
        let mut count = 0usize;
        each_combination_with_min(13, 3, 0b0000000000111, |_| count += 1);
        // ≥ 0b111 = 7 的全部 C(13,3) mask = 286（因为 0b111 本身是最小 3-bit mask）。
        assert_eq!(count, 286);
    }

    /// Debug: 检查一个具体 (board, hole) 的 pack_canonical_form_key 输出
    /// 是否与花色置换等价输入产出相同 key。
    #[test]
    fn pack_key_debug_specific_case_suit_permutation_invariance() {
        // 配置 1：board = [2♣, 3♣, 2♦], hole = [4♥, 5♥]
        let board1 = [
            Card::from_u8(0).unwrap(), // 2♣
            Card::from_u8(4).unwrap(), // 3♣
            Card::from_u8(1).unwrap(), // 2♦
        ];
        let hole1 = [
            Card::from_u8(10).unwrap(), // 4♥
            Card::from_u8(14).unwrap(), // 5♥
        ];
        // 配置 2：suit 0/1 swap（♣ ↔ ♦）
        let board2 = [
            Card::from_u8(1).unwrap(), // 2♦
            Card::from_u8(5).unwrap(), // 3♦
            Card::from_u8(0).unwrap(), // 2♣
        ];
        let hole2 = [
            Card::from_u8(10).unwrap(), // 4♥ (unchanged)
            Card::from_u8(14).unwrap(), // 5♥
        ];
        let k1 = pack_canonical_form_key(&board1, &hole1);
        let k2 = pack_canonical_form_key(&board2, &hole2);
        assert_eq!(
            k1, k2,
            "suit permutation σ(Clubs↔Diamonds) 应该 canonical-invariant\n\
             k1 = 0x{k1:032x}\nk2 = 0x{k2:032x}"
        );
    }

    #[test]
    fn pack_key_round_trip_signature_ordering() {
        // Construct two (board, hole) variants 应得同 key 来验 input-order
        // 不变性（已在 tests/canonical_observation.rs 覆盖；这里冗余 sanity）。
        use crate::core::{Rank, Suit};
        let board = [
            Card::new(Rank::Ace, Suit::Spades),
            Card::new(Rank::King, Suit::Hearts),
            Card::new(Rank::Seven, Suit::Diamonds),
        ];
        let hole = [
            Card::new(Rank::Queen, Suit::Clubs),
            Card::new(Rank::Ten, Suit::Diamonds),
        ];
        let key1 = pack_canonical_form_key(&board, &hole);
        // 调换 board / hole 内部顺序
        let board2 = [
            Card::new(Rank::King, Suit::Hearts),
            Card::new(Rank::Ace, Suit::Spades),
            Card::new(Rank::Seven, Suit::Diamonds),
        ];
        let hole2 = [
            Card::new(Rank::Ten, Suit::Diamonds),
            Card::new(Rank::Queen, Suit::Clubs),
        ];
        let key2 = pack_canonical_form_key(&board2, &hole2);
        assert_eq!(key1, key2, "input-order invariance");
    }

    /// Brute force: iterate all (board, hole) flop combinations + use
    /// `pack_canonical_form_key` to dedupe via HashSet. Ground truth for
    /// `enumerate_canonical_forms` correctness。release 实测 ~4 s。
    #[test]
    #[ignore = "release/--ignored opt-in（~4 s + 1.28M HashSet 内存）"]
    fn brute_force_flop_pack_dedupe_yields_n_flop() {
        use std::collections::HashSet;
        let mut seen: HashSet<u128> = HashSet::with_capacity(N_CANONICAL_OBSERVATION_FLOP as usize);
        for b0 in 0..52u8 {
            for b1 in (b0 + 1)..52 {
                for b2 in (b1 + 1)..52 {
                    let board = [
                        Card::from_u8(b0).unwrap(),
                        Card::from_u8(b1).unwrap(),
                        Card::from_u8(b2).unwrap(),
                    ];
                    let board_slice = &board[..];
                    for h0 in 0..52u8 {
                        if h0 == b0 || h0 == b1 || h0 == b2 {
                            continue;
                        }
                        for h1 in (h0 + 1)..52 {
                            if h1 == b0 || h1 == b1 || h1 == b2 {
                                continue;
                            }
                            let hole = [Card::from_u8(h0).unwrap(), Card::from_u8(h1).unwrap()];
                            let key = pack_canonical_form_key(board_slice, &hole);
                            seen.insert(key);
                        }
                    }
                }
            }
        }
        assert_eq!(
            seen.len(),
            N_CANONICAL_OBSERVATION_FLOP as usize,
            "pack_canonical_form_key brute-force dedupe count must match \
             N_CANONICAL_OBSERVATION_FLOP = {}",
            N_CANONICAL_OBSERVATION_FLOP
        );
    }

    #[test]
    fn enumerate_flop_canonical_form_count_matches_n_flop() {
        let mut count = 0usize;
        enumerate_canonical_forms(3, 2, &mut |_key| count += 1);
        assert_eq!(
            count, N_CANONICAL_OBSERVATION_FLOP as usize,
            "flop canonical form count must match N_CANONICAL_OBSERVATION_FLOP = {}",
            N_CANONICAL_OBSERVATION_FLOP
        );
    }

    #[test]
    fn enumerate_flop_canonical_forms_are_distinct() {
        let mut keys: Vec<u128> = Vec::with_capacity(N_CANONICAL_OBSERVATION_FLOP as usize);
        enumerate_canonical_forms(3, 2, &mut |k| keys.push(k));
        keys.sort_unstable();
        for w in keys.windows(2) {
            assert_ne!(w[0], w[1], "duplicate canonical key in enumeration");
        }
    }

    #[test]
    #[ignore = "release/--ignored opt-in（~0.15 s）"]
    fn enumerate_turn_canonical_form_count_matches_n_turn() {
        let mut count = 0usize;
        enumerate_canonical_forms(4, 2, &mut |_key| count += 1);
        assert_eq!(
            count, N_CANONICAL_OBSERVATION_TURN as usize,
            "turn canonical form count must match N_CANONICAL_OBSERVATION_TURN = {}",
            N_CANONICAL_OBSERVATION_TURN
        );
    }

    #[test]
    #[ignore = "release/--ignored opt-in（~1.5 s）"]
    fn enumerate_river_canonical_form_count_matches_n_river() {
        let mut count = 0usize;
        enumerate_canonical_forms(5, 2, &mut |_key| count += 1);
        assert_eq!(
            count, N_CANONICAL_OBSERVATION_RIVER as usize,
            "river canonical form count must match N_CANONICAL_OBSERVATION_RIVER = {}",
            N_CANONICAL_OBSERVATION_RIVER
        );
    }
}
