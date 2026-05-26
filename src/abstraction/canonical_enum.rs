//! D-218-rev2 真等价类枚举（§G-batch1 §3.1 \[实现\]）。
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
//! 3. **Direct combinatorial rank（2026-05 重写，shape-major）**：canonical id 由
//!    组合数学公式 O(1) 直接算出，不再建 per-street `Vec<u128>` 整表、不再 binary
//!    search 整表。编号方案：
//!
//!    - **shape 偏移**：4 个 canonical suit 的 (b_count, h_count) 序列当作 shape，
//!      按 shape 字典序排列；id 落在「所有字典序更小 shape 的容量之和」（`shape_offset`）
//!      起的连续区间。合法 shape 总数极少（每街 < 几百），用一张几百字节的
//!      [`OnceLock`] 表存 `(shape_key, offset, size)`。
//!    - **shape 内排名**：同一 (b_count, h_count) 的连续 suit 组成 group；每个
//!      group 内的 (b_mask, h_mask) 多重集用组合数系（colex / combinadic）rank
//!      编号，各 group 间按混合进制（group 0 最高位）拼成 shape 内稠密 rank。
//!
//!    id = `shape_offset + shape 内 rank`，是 `[0, N)` 上的双射。
//!
//!    **注意**：本方案与 2026-05 之前的「整表 sort + binary search」产出的 id
//!    **不同**——旧方案按 packed u128 数值序，会把不同 shape 交错排列（早 slot 的
//!    mask 位先于晚 slot 的 count 位参与比较）。任何用旧 id 训练的 bucket 表语义
//!    失效；`bucket_table` 的 `schema_version` 已 bump 以令 reader 拒绝旧 artifact。
//!
//! 4. **Enumeration（仅供微型 shape 表 + 自校验测试）**：递归枚举每个 canonical
//!    shape → 每个 shape 内 enumerate canonical multiset of (b_mask, h_mask)
//!    pairs。详见 `enumerate_canonical_forms`（本模块 private 递归实现，非 pub
//!    API）。现仅用于建微型 shape 表（取 shape 列表 + 用公式算各 shape 容量）和
//!    校验 N；运行时 id 计算不再走它。
//!
//! # 内存预算
//!
//! 不再常驻 per-street `Vec<u128>` 整表（旧方案 river ~1.97 GB / build ~3 min）。
//! 现在每街只 lazy 建一张 `(shape_key, offset, size)` 微型表（每街 < 几百条目，
//! < 10 KB），build < 1 ms。[`canonical_observation_id`] / [`nth_canonical_form`]
//! 都是 O(1) 公式 + 一次微型表 binary search。
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
//! - **唯一性**：两个互不等价的 (board, hole) 一定映射到不同 id。由组合数系 rank
//!   的双射性严格保证（每个 canonical 等价类 ↔ 唯一 (shape, 各 group 多重集 rank)
//!   ↔ 唯一 id）。
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
// canonical signatures + key packing
// ============================================================================

/// 计算 (board, hole) 的 canonical sorted suit signatures：4 个
/// `(b_count, h_count, b_mask, h_mask)` 按字典序升序排列。同一花色对称等价类下
/// 返回完全相同的数组（花色重标 + 输入顺序都被吸收，因为先按 suit 聚合 + sort）。
fn canonical_sigs(board: &[Card], hole: &[Card; 2]) -> [(u8, u8, u16, u16); 4] {
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
    sigs
}

/// 把 canonical sorted sigs 打包成 `u128` key（仅 test ground-truth 用；运行时
/// id 计算走 direct combinatorial rank，不再依赖整表 sort）。
///
/// Layout（MSB → LSB，4 suits × 32-bit）：slot 0 在 `bit 127:96` … slot 3 在
/// `bit 31:0`。每 suit 32-bit 高位优先排 `[unused(1) | b_count(3) | h_count(2) |
/// b_mask(13) | h_mask(13)]`，所以 `u128` 数值序 == canonical tuple 字典序。
#[cfg(test)]
fn pack_sigs(sigs: &[(u8, u8, u16, u16); 4]) -> u128 {
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

#[cfg(test)]
fn pack_canonical_form_key(board: &[Card], hole: &[Card; 2]) -> u128 {
    pack_sigs(&canonical_sigs(board, hole))
}

// ============================================================================
// 组合数学底层原语（direct combinatorial rank）
// ============================================================================

/// 组合数 C(n, k)。n / k 在本模块用途内都很小（n ≤ ~4300，k ≤ 4），增量乘法精确
/// 无溢出（中间值 << u64::MAX）。`k > n` 返回 0。
fn choose(n: u64, k: u64) -> u64 {
    if k > n {
        return 0;
    }
    let k = k.min(n - k);
    let mut result: u64 = 1;
    let mut i: u64 = 1;
    while i <= k {
        // C(n, i) = C(n, i-1) * (n - i + 1) / i，按此顺序每步整除精确。
        result = result * (n - k + i) / i;
        i += 1;
    }
    result
}

/// 一个集合（用 set bit 位置表示）的 colexicographic rank：取 set bit 位置升序
/// `p_0 < p_1 < ...`，rank = Σ_i C(p_i, i+1)。该值随 mask 整数值单调递增，等于
/// Gosper 升序枚举里的下标。本重载吃 `u16` mask（位置 ≤ 15）。
fn colex_rank(mask: u16) -> u64 {
    let mut rank: u64 = 0;
    let mut m = mask;
    let mut i: u64 = 1;
    while m != 0 {
        let p = m.trailing_zeros() as u64;
        rank += choose(p, i);
        m &= m - 1;
        i += 1;
    }
    rank
}

/// `colex_rank` 的逆：在 `n`-bit 空间里恢复有 `k` 个 set bit、rank 为 `rank` 的
/// mask（组合数系 greedy unranking）。`k == 0` 返回 0。
fn colex_unrank(mut rank: u64, k: u32, n: u32) -> u16 {
    let mut mask: u16 = 0;
    let mut i = k;
    while i >= 1 {
        // 找最大 cand（< n）使 C(cand, i) <= rank：从 i-1（C(i-1,i)=0）往上爬。
        let mut cand = i - 1;
        while cand + 1 < n && choose((cand + 1) as u64, i as u64) <= rank {
            cand += 1;
        }
        rank -= choose(cand as u64, i as u64);
        mask |= 1u16 << cand;
        i -= 1;
    }
    mask
}

/// 把 `h_mask`（与 `b_mask` 不相交）压缩到 `b_mask` 空出的 13 - popcount(b_mask)
/// 个 rank 位中——每个 set bit 的新位置 = 原位置减去其下方被 `b_mask` 占用的位数。
fn compress_mask(h_mask: u16, b_mask: u16) -> u16 {
    let mut out: u16 = 0;
    let mut m = h_mask;
    while m != 0 {
        let p = m.trailing_zeros();
        let below = b_mask & ((1u16 << p) - 1);
        let cp = p - below.count_ones();
        out |= 1u16 << cp;
        m &= m - 1;
    }
    out
}

/// `compress_mask` 的逆：把压缩位 `mapped_h` 在 `b_mask` 的空位上展开回 13-bit
/// rank 空间。
fn expand_mask(mapped_h: u16, b_mask: u16) -> u16 {
    let mut out: u16 = 0;
    let mut comp_idx: u32 = 0;
    for p in 0..13u16 {
        if (b_mask >> p) & 1 == 1 {
            continue; // 被 board 占用，跳过
        }
        if (mapped_h >> comp_idx) & 1 == 1 {
            out |= 1u16 << p;
        }
        comp_idx += 1;
    }
    out
}

/// 固定 (b_count, h_count) 下合法 (b_mask, h_mask) 对的总数 =
/// C(13, b_count) · C(13 - b_count, h_count)。
fn mask_pair_count(bc: u8, hc: u8) -> u64 {
    choose(13, bc as u64) * choose(13 - bc as u64, hc as u64)
}

/// 把一个 (b_mask, h_mask) 对映射成稠密整数 ∈ `[0, mask_pair_count(bc, hc))`。
/// 与 `each_mask_pair_at_least(.., min=0)` 的枚举顺序一致（b_mask colex 升序，
/// 内层 h_mask 在剩余位上 colex 升序）。
fn mask_pair_rank(bc: u8, hc: u8, b_mask: u16, h_mask: u16) -> u64 {
    let b_rank = colex_rank(b_mask);
    let mapped_h = compress_mask(h_mask, b_mask);
    let h_rank = colex_rank(mapped_h);
    b_rank * choose(13 - bc as u64, hc as u64) + h_rank
}

/// `mask_pair_rank` 的逆。
fn mask_pair_unrank(bc: u8, hc: u8, rank: u64) -> (u16, u16) {
    let h_space = choose(13 - bc as u64, hc as u64);
    let b_rank = rank / h_space;
    let h_rank = rank % h_space;
    let b_mask = colex_unrank(b_rank, bc as u32, 13);
    let mapped_h = colex_unrank(h_rank, hc as u32, 13 - bc as u32);
    let h_mask = expand_mask(mapped_h, b_mask);
    (b_mask, h_mask)
}

/// 一段非降序列 `ranks`（各 ∈ `[0, m)`）的稠密 rank ∈ `[0, C(m + k - 1, k))`。
/// 用 `c_i = ranks[i] + i` 把非降序列变成严格升序的 k-子集（值域 `[0, m+k-1)`），
/// 再取其 colex rank = Σ_i C(c_i, i+1)。`ranks` 必须非降。
fn multiset_rank(ranks: &[u64], _m: u64) -> u64 {
    let mut rank: u64 = 0;
    for (i, &r) in ranks.iter().enumerate() {
        let c = r + i as u64;
        rank += choose(c, i as u64 + 1);
    }
    rank
}

/// `multiset_rank` 的逆：给定 rank / 序列长度 `k` / 值域上界 `m`，恢复非降序列。
fn multiset_unrank(rank: u64, k: u32, m: u64) -> [u64; 4] {
    let mut cs = [0u64; 4]; // c_i（1-based 系数存到 cs[i-1]）
    let n = m + k as u64 - 1; // 严格升序子集的值域上界（exclusive）
    let mut rem = rank;
    let mut i = k;
    while i >= 1 {
        // 上界：i==k 用 n，否则用更高位系数（保证严格递减）。
        let upper = if i == k { n } else { cs[i as usize] };
        let mut cand = (i - 1) as u64;
        while cand + 1 < upper && choose(cand + 1, i as u64) <= rem {
            cand += 1;
        }
        rem -= choose(cand, i as u64);
        cs[(i - 1) as usize] = cand;
        i -= 1;
    }
    // r_i = c_i - i
    let mut out = [0u64; 4];
    for j in 0..k as usize {
        out[j] = cs[j] - j as u64;
    }
    out
}

// ============================================================================
// 微型 shape 偏移表（shape_key → 起始 offset + 容量）
// ============================================================================

/// 把 shape（4 个 (b_count, h_count)）打包成 u32 key：每 slot 5 bit
/// `[b_count(3) | h_count(2)]`，slot 0 在高位。u32 数值序 == shape 字典序。
fn pack_shape_key(shape: &[(u8, u8); 4]) -> u32 {
    let mut key: u32 = 0;
    for (i, &(bc, hc)) in shape.iter().enumerate() {
        let pack: u32 = ((bc as u32) << 2) | (hc as u32);
        let shift = 5 * (3 - i as u32);
        key |= pack << shift;
    }
    key
}

fn unpack_shape_key(key: u32) -> [(u8, u8); 4] {
    let mut shape = [(0u8, 0u8); 4];
    for (i, slot) in shape.iter_mut().enumerate() {
        let shift = 5 * (3 - i as u32);
        let chunk = (key >> shift) & 0x1F;
        *slot = (((chunk >> 2) & 0x7) as u8, (chunk & 0x3) as u8);
    }
    shape
}

/// 把 shape 切成 group（连续相同 (b_count, h_count) 的 suit 段）：
/// 返回 `[(b_count, h_count, start, end_exclusive); 4]` + group 数。
fn group_runs(shape: &[(u8, u8); 4]) -> ([(u8, u8, u8, u8); 4], usize) {
    let mut groups = [(0u8, 0u8, 0u8, 0u8); 4];
    let mut n = 0usize;
    let mut i = 0usize;
    while i < 4 {
        let mut j = i + 1;
        while j < 4 && shape[j] == shape[i] {
            j += 1;
        }
        groups[n] = (shape[i].0, shape[i].1, i as u8, j as u8);
        n += 1;
        i = j;
    }
    (groups, n)
}

/// 一个 shape 的容量 = Π_group C(mask_pair_count + group_size - 1, group_size)。
fn shape_size(shape: &[(u8, u8); 4]) -> u64 {
    let (groups, n) = group_runs(shape);
    let mut size: u64 = 1;
    for &(bc, hc, start, end) in &groups[..n] {
        let k = (end - start) as u64;
        size *= choose(mask_pair_count(bc, hc) + k - 1, k);
    }
    size
}

static FLOP_SHAPES: OnceLock<Vec<(u32, u32, u32)>> = OnceLock::new();
static TURN_SHAPES: OnceLock<Vec<(u32, u32, u32)>> = OnceLock::new();
static RIVER_SHAPES: OnceLock<Vec<(u32, u32, u32)>> = OnceLock::new();

/// 该街的微型 shape 表，元素 `(shape_key, shape_offset, shape_size)` 按 shape_key
/// 升序（= 字典序），offset 为前缀和。lazy build，几百条目 / build < 1 ms。
fn shape_table(street: StreetTag) -> &'static [(u32, u32, u32)] {
    match street {
        StreetTag::Preflop => {
            panic!("canonical_enum::shape_table called on Preflop; use canonical_hole_id")
        }
        StreetTag::Flop => {
            FLOP_SHAPES.get_or_init(|| build_shape_table(3, 2, N_CANONICAL_OBSERVATION_FLOP))
        }
        StreetTag::Turn => {
            TURN_SHAPES.get_or_init(|| build_shape_table(4, 2, N_CANONICAL_OBSERVATION_TURN))
        }
        StreetTag::River => {
            RIVER_SHAPES.get_or_init(|| build_shape_table(5, 2, N_CANONICAL_OBSERVATION_RIVER))
        }
    }
    .as_slice()
}

fn build_shape_table(board_size: u8, hole_size: u8, expected_n: u32) -> Vec<(u32, u32, u32)> {
    let mut shapes: Vec<(u32, u64)> = Vec::new();
    let mut shape = [(0u8, 0u8); 4];
    enumerate_shapes(
        &mut shape,
        0,
        board_size,
        hole_size,
        (0, 0),
        &mut |s: &[(u8, u8); 4]| {
            shapes.push((pack_shape_key(s), shape_size(s)));
        },
    );
    shapes.sort_unstable_by_key(|x| x.0);
    debug_assert!(
        shapes.windows(2).all(|w| w[0].0 < w[1].0),
        "canonical_enum: 重复 shape_key（enumerate_shapes 产出重复 shape）"
    );
    let mut out: Vec<(u32, u32, u32)> = Vec::with_capacity(shapes.len());
    let mut offset: u64 = 0;
    for (key, size) in shapes {
        out.push((key, offset as u32, size as u32));
        offset += size;
    }
    assert_eq!(
        offset, expected_n as u64,
        "canonical_enum: shape 表容量之和 {offset} != N {expected_n}"
    );
    out
}

// ============================================================================
// canonical_observation_id：direct combinatorial rank（shape-major）
// ============================================================================

/// 计算 (board, hole) 在 `street` 街上的 canonical observation id ∈ `[0, N)`。
///
/// 仅对 `StreetTag::{Flop, Turn, River}` 有效；preflop 路径 panic（caller 应改用
/// [`crate::abstraction::preflop::canonical_hole_id`]）。
///
/// 算法（shape-major direct rank）：canonical sorted sigs → shape → 从微型 shape
/// 表查 `shape_offset` → 逐 group 算多重集 rank、按混合进制拼成 shape 内 rank →
/// `shape_offset + shape 内 rank`。全程 O(1) 公式 + 一次微型表 binary search。
///
/// **注意**：本 id 编号与 2026-05 之前的「整表 sort + binary search」方案 **不同**
/// （旧方案按 packed u128 数值序）。详见模块头 §算法 step 3。
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

    let sigs = canonical_sigs(board, &hole);
    let shape: [(u8, u8); 4] = [
        (sigs[0].0, sigs[0].1),
        (sigs[1].0, sigs[1].1),
        (sigs[2].0, sigs[2].1),
        (sigs[3].0, sigs[3].1),
    ];
    let table = shape_table(street);
    let shape_key = pack_shape_key(&shape);
    let idx = table
        .binary_search_by_key(&shape_key, |e| e.0)
        .unwrap_or_else(|_| {
            panic!(
                "canonical_observation_id: shape_key 0x{shape_key:05x} not in shape table \
                 (street {street:?}); enumeration bug"
            )
        });
    let offset = table[idx].1 as u64;

    // shape 内 rank：逐 group 多重集 rank，group 0 最高位混合进制。
    let (groups, ng) = group_runs(&shape);
    let mut local: u64 = 0;
    for &(bc, hc, start, end) in &groups[..ng] {
        let k = (end - start) as usize;
        let m = mask_pair_count(bc, hc);
        let mut ranks = [0u64; 4];
        for (j, slot) in (start as usize..end as usize).enumerate() {
            ranks[j] = mask_pair_rank(bc, hc, sigs[slot].2, sigs[slot].3);
        }
        debug_assert!(
            ranks[..k].windows(2).all(|w| w[0] <= w[1]),
            "canonical_observation_id: group mask ranks 非降假设被破坏"
        );
        let g_size = choose(m + k as u64 - 1, k as u64);
        let g_rank = multiset_rank(&ranks[..k], m);
        local = local * g_size + g_rank;
    }

    let id = offset + local;
    debug_assert!(
        id < n_canonical_observation(street) as u64,
        "canonical_observation_id: id {id} >= N for {street:?}"
    );
    id as u32
}

// ============================================================================
// nth_canonical_form：canonical id → 具体 (board, hole) 代表（D-218-rev2 §3）
// ============================================================================

/// 反函数：给定 `street` 与 canonical id ∈ `[0, N)`，返回该 canonical 等价类
/// 的一个具体 (board, hole) representative。round-trip 与 [`canonical_observation_id`]
/// 等价（debug_assert）。
///
/// 适用 `StreetTag::{Flop, Turn, River}`；preflop / id ≥ N panic。
///
/// **representative 选择规则**：把 canonical suit slot 0/1/2/3（pack key 中按
/// (b_count, h_count, b_mask, h_mask) 字典序排序后的位置）按位映射到真实 suit
/// 0/1/2/3——同一等价类内多个真实 (board, hole) 都映射到同一 canonical id，
/// 本函数选取 canonical slot 与 real suit 一一对齐的那个 representative。
///
/// **用途**（§G-batch1 §3.4 \[实现\] dual-phase production training）：phase 2 100%
/// canonical 覆盖路径——为每个 canonical id 解码出一张实际 (board, hole)，计算
/// features，分配到最近 centroid，让 `BucketTable::lookup_table` 不再依赖 Knuth
/// hash fallback。
pub fn nth_canonical_form(street: StreetTag, id: u32) -> (Vec<Card>, [Card; 2]) {
    if matches!(street, StreetTag::Preflop) {
        panic!(
            "nth_canonical_form called with StreetTag::Preflop; preflop canonical id is hole-only \
             via canonical_hole_id (D-218-rev2 §2)"
        );
    }
    let table = shape_table(street);
    let total: u64 = table.last().map(|e| e.1 as u64 + e.2 as u64).unwrap_or(0);
    assert!(
        (id as u64) < total,
        "nth_canonical_form: id {id} >= N_canonical_observation = {total} for street {street:?}"
    );
    let board_size: usize = match street {
        StreetTag::Flop => 3,
        StreetTag::Turn => 4,
        StreetTag::River => 5,
        StreetTag::Preflop => unreachable!(),
    };

    // 1. 定位 shape：offset 单调递增（按 shape_key 排序的前缀和），找最后一个
    //    offset <= id 的条目。
    let pos = table.partition_point(|e| (e.1 as u64) <= id as u64);
    debug_assert!(pos >= 1, "nth_canonical_form: 第一个 shape offset 必为 0");
    let (shape_key, offset, _size) = table[pos - 1];
    let shape = unpack_shape_key(shape_key);
    let (groups, ng) = group_runs(&shape);

    // 2. 每个 group 的容量（混合进制 radix），group 0 最高位。
    let mut sizes = [1u64; 4];
    for (g, &(bc, hc, start, end)) in groups[..ng].iter().enumerate() {
        let k = (end - start) as u64;
        sizes[g] = choose(mask_pair_count(bc, hc) + k - 1, k);
    }

    // 3. 逐 group 反解多重集 rank → 每个 suit 的 (b_mask, h_mask)。
    let mut local = id as u64 - offset as u64;
    let mut sigs: [(u8, u8, u16, u16); 4] = [(0, 0, 0, 0); 4];
    for (g, &(bc, hc, start, end)) in groups[..ng].iter().enumerate() {
        let k = (end - start) as usize;
        let m = mask_pair_count(bc, hc);
        let weight: u64 = sizes[g + 1..ng].iter().product();
        let g_rank = local / weight;
        local %= weight;
        let rs = multiset_unrank(g_rank, k as u32, m);
        for (j, slot) in (start as usize..end as usize).enumerate() {
            let (bm, hm) = mask_pair_unrank(bc, hc, rs[j]);
            sigs[slot] = (bc, hc, bm, hm);
        }
    }

    // 4. canonical slot 0..3 直接映射到真实 suit 0..3，重建 (board, hole)。
    let mut board: Vec<Card> = Vec::with_capacity(board_size);
    let mut hole_buf: Vec<Card> = Vec::with_capacity(2);
    for (slot, &(_bc, _hc, b_mask, h_mask)) in sigs.iter().enumerate() {
        let suit_u8: u8 = slot as u8;
        for rank in 0u8..13 {
            if (b_mask >> rank) & 1 == 1 {
                board.push(Card::from_u8(rank * 4 + suit_u8).expect("rank<13 + suit<4 → card<52"));
            }
            if (h_mask >> rank) & 1 == 1 {
                hole_buf
                    .push(Card::from_u8(rank * 4 + suit_u8).expect("rank<13 + suit<4 → card<52"));
            }
        }
    }

    assert_eq!(
        board.len(),
        board_size,
        "nth_canonical_form: board enumeration produced {} cards, expected {board_size}",
        board.len()
    );
    assert_eq!(
        hole_buf.len(),
        2,
        "nth_canonical_form: hole enumeration produced {} cards, expected 2",
        hole_buf.len()
    );
    let hole: [Card; 2] = [hole_buf[0], hole_buf[1]];

    debug_assert_eq!(
        canonical_observation_id(street, &board, hole),
        id,
        "nth_canonical_form: round-trip mismatch for id {id} street {street:?}"
    );

    (board, hole)
}

// ============================================================================
// 枚举（喂微型 shape 表 + 自校验测试；不再常驻整表）
// ============================================================================

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
#[cfg(test)]
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
#[cfg(test)]
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
#[cfg(test)]
fn enumerate_groups<F: FnMut(u128)>(
    groups: &[(u8, u8, u8, u8)],
    group_idx: usize,
    sigs: &mut [(u8, u8, u16, u16); 4],
    callback: &mut F,
) {
    if group_idx == groups.len() {
        // 全部 4 suit 已赋值，pack 输出 key
        callback(pack_sigs(sigs));
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
#[cfg(test)]
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
#[cfg(test)]
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
#[cfg(test)]
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
#[cfg(test)]
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
#[cfg(test)]
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

    // ------------------------------------------------------------------------
    // direct combinatorial rank 原语自校验（2026-05 shape-major 重写）
    // ------------------------------------------------------------------------

    #[test]
    fn choose_basic_values() {
        assert_eq!(choose(13, 0), 1);
        assert_eq!(choose(13, 1), 13);
        assert_eq!(choose(13, 2), 78);
        assert_eq!(choose(13, 3), 286);
        assert_eq!(choose(5, 3), 10);
        assert_eq!(choose(3, 5), 0);
        assert_eq!(choose(0, 0), 1);
    }

    #[test]
    fn colex_rank_matches_gosper_index_and_round_trips() {
        for k in 0u32..=5 {
            let total = choose(13, k as u64);
            // colex_rank == 升序枚举下标；unrank 反解一致。
            let mut expect = 0u64;
            let mut prev: Option<u16> = None;
            each_combination_with_min(13, k as u8, 0, |m| {
                assert_eq!(colex_rank(m), expect, "colex_rank == enum index (k={k})");
                assert_eq!(colex_unrank(expect, k, 13), m, "colex_unrank (k={k})");
                if let Some(p) = prev {
                    assert!(m > p, "Gosper 升序");
                }
                prev = Some(m);
                expect += 1;
            });
            assert_eq!(expect, total, "C(13,{k}) 枚举数");
        }
    }

    #[test]
    fn mask_pair_rank_round_trips_exhaustively() {
        for (bc, hc) in [
            (0u8, 0u8),
            (0, 1),
            (0, 2),
            (1, 0),
            (1, 1),
            (1, 2),
            (2, 0),
            (2, 1),
            (2, 2),
            (3, 2),
            (5, 0),
        ] {
            let m = mask_pair_count(bc, hc);
            for r in 0..m {
                let (bm, hm) = mask_pair_unrank(bc, hc, r);
                assert_eq!(bm.count_ones() as u8, bc, "b_count ({bc},{hc}) r={r}");
                assert_eq!(hm.count_ones() as u8, hc, "h_count ({bc},{hc}) r={r}");
                assert_eq!(bm & hm, 0, "board/hole disjoint ({bc},{hc}) r={r}");
                assert!(bm < (1 << 13) && hm < (1 << 13));
                assert_eq!(
                    mask_pair_rank(bc, hc, bm, hm),
                    r,
                    "mask_pair round-trip ({bc},{hc}) r={r}"
                );
            }
        }
    }

    #[test]
    fn multiset_rank_round_trips() {
        for m in [1u64, 2, 3, 13, 78] {
            for k in 1u32..=3 {
                let total = choose(m + k as u64 - 1, k as u64);
                for r in 0..total {
                    let rs = multiset_unrank(r, k, m);
                    for j in 0..k as usize {
                        assert!(rs[j] < m, "value in range m={m} k={k} r={r}");
                        if j > 0 {
                            assert!(rs[j] >= rs[j - 1], "non-decreasing m={m} k={k} r={r}");
                        }
                    }
                    assert_eq!(
                        multiset_rank(&rs[..k as usize], m),
                        r,
                        "multiset round-trip m={m} k={k} r={r}"
                    );
                }
            }
        }
    }

    #[test]
    fn shape_table_flop_sums_to_n() {
        let t = shape_table(StreetTag::Flop);
        let total: u64 = t.last().map(|e| e.1 as u64 + e.2 as u64).unwrap();
        assert_eq!(total, N_CANONICAL_OBSERVATION_FLOP as u64);
        for w in t.windows(2) {
            assert!(w[0].0 < w[1].0, "shape_key 严格升序");
            assert!(w[0].1 < w[1].1, "offset 严格升序");
        }
    }

    #[test]
    fn shape_key_pack_unpack_round_trip() {
        for &shape in &[
            [(0u8, 0u8), (0, 1), (1, 0), (2, 1)],
            [(0, 2), (1, 0), (1, 0), (1, 0)],
            [(0, 0), (0, 0), (0, 0), (5, 2)],
        ] {
            assert_eq!(unpack_shape_key(pack_shape_key(&shape)), shape);
        }
    }

    /// 独立 ground truth：用 `enumerate_canonical_forms`（与 id 公式无关的代码路径）
    /// 枚举全部 flop canonical 等价类，decode 出 representative (board, hole)，
    /// 经新 `canonical_observation_id` 算 id，断言这些 id 恰好填满 `[0, N)` 无重无漏
    /// ——即新 shape-major 编号是双射。
    #[test]
    #[ignore = "release/--ignored opt-in（flop 1.28M 枚举）"]
    fn flop_new_id_is_bijection_via_enumeration() {
        let n = N_CANONICAL_OBSERVATION_FLOP as usize;
        let mut seen = vec![false; n];
        enumerate_canonical_forms(3, 2, &mut |key| {
            let mut board: Vec<Card> = Vec::with_capacity(3);
            let mut hole: Vec<Card> = Vec::with_capacity(2);
            for slot in 0u32..4 {
                let shift = 32 * (3 - slot);
                let chunk = ((key >> shift) & 0xFFFF_FFFF) as u32;
                let b_mask = ((chunk >> 13) & 0x1FFF) as u16;
                let h_mask = (chunk & 0x1FFF) as u16;
                for rank in 0u8..13 {
                    if (b_mask >> rank) & 1 == 1 {
                        board.push(Card::from_u8(rank * 4 + slot as u8).unwrap());
                    }
                    if (h_mask >> rank) & 1 == 1 {
                        hole.push(Card::from_u8(rank * 4 + slot as u8).unwrap());
                    }
                }
            }
            assert_eq!(board.len(), 3);
            assert_eq!(hole.len(), 2);
            let id = canonical_observation_id(StreetTag::Flop, &board, [hole[0], hole[1]]);
            assert!((id as usize) < n, "id {id} 越界");
            assert!(!seen[id as usize], "重复 id {id}");
            seen[id as usize] = true;
        });
        assert!(seen.iter().all(|&b| b), "存在未覆盖 id → 非双射");
    }
}
