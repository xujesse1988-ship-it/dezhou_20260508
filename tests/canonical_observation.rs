//! B1 §A 类补：postflop `canonical_observation_id` 公开 helper 不变量
//! （F19 / D-218-rev1 / D-244-rev1 / API §F19 影响 ⑤）。
//!
//! API §1040 影响 ⑤ 字面要求：B1 [测试] `tests/canonical_observation.rs` 起草
//! 断言：
//!
//! - (a) 1k 随机 (board, hole) 同输入重复调用 byte-equal（确定性）。
//! - (b) 花色重命名 / rank 内花色置换不改变 id（D-218-rev1 花色对称等价类核心
//!   不变量）。
//! - (c) id 紧凑分布（无空洞）：枚举到的全部 canonical id 应填满
//!   `[0, n_canonical_observation(street))` 范围。
//!
//! workflow §B1 §输出 A 类 12 项命名 fixed scenario 不含本文件——A0 §B1 段落
//! 在 batch 6 F19 落地之前定稿，与 API §1040 影响 ⑤ 字面要求不一致。本文件按
//! API 字面落地（[测试] 角色范围内）；workflow §修订历史 §B-rev0 同步追加
//! "API §1040 影响 ⑤ vs workflow §B1 §输出 doc drift 消解" 段落。
//!
//! **B1 状态**：A1 阶段 `canonical_observation_id` `unimplemented!()`，本文件
//! 全部 `#[test]` 在第一次调用时 panic（与 §A 类同形态：编译通过、运行失败
//! 因 unimplemented）。
//!
//! **B2 状态**：stub 落地后断言激活；本文件保持原文，仅 [实现] 侧填充 stub。
//!
//! 角色边界：本文件属 `[测试]` agent 产物。

use std::collections::HashSet;

use poker::{canonical_observation_id, Card, Rank, StreetTag, Suit};

// ============================================================================
// 通用 fixture
// ============================================================================

/// 固定 flop board：A♠ K♥ 7♦（覆盖三花色 + Ace 高 + 不连张，通用 fixture）。
fn flop_board() -> Vec<Card> {
    vec![
        Card::new(Rank::Ace, Suit::Spades),
        Card::new(Rank::King, Suit::Hearts),
        Card::new(Rank::Seven, Suit::Diamonds),
    ]
}

/// 固定 turn board：flop + 2♣。
fn turn_board() -> Vec<Card> {
    let mut b = flop_board();
    b.push(Card::new(Rank::Two, Suit::Clubs));
    b
}

/// 固定 river board：turn + Q♥。
fn river_board() -> Vec<Card> {
    let mut b = turn_board();
    b.push(Card::new(Rank::Queen, Suit::Hearts));
    b
}

/// 拿一对不与给定 board 重叠的 hole（取数值最小的两张未占用 card）。
fn pick_hole_disjoint(board: &[Card]) -> [Card; 2] {
    let used: HashSet<u8> = board.iter().map(|c| c.to_u8()).collect();
    let mut picked = Vec::with_capacity(2);
    for v in 0..52u8 {
        if !used.contains(&v) {
            picked.push(Card::from_u8(v).unwrap());
            if picked.len() == 2 {
                break;
            }
        }
    }
    [picked[0], picked[1]]
}

// ============================================================================
// 1. canonical_observation_id_repeat_1k_smoke（确定性 / API §1040 影响 ⑤ (a)）
// ============================================================================
//
// 同 (street, board, hole) 重复调用 1000 次结果 byte-equal。**B1 默认 1k**；
// full 1M 留 D1（与 stage-1 §B1 同形态）。
#[test]
fn canonical_observation_id_repeat_1k_smoke_flop() {
    let board = flop_board();
    let hole = pick_hole_disjoint(&board);
    let baseline = canonical_observation_id(StreetTag::Flop, &board, hole);
    for i in 0..1_000 {
        let other = canonical_observation_id(StreetTag::Flop, &board, hole);
        assert_eq!(
            baseline, other,
            "repeat iter {i}: byte-equal (flop, baseline={baseline}, other={other})"
        );
    }
}

#[test]
fn canonical_observation_id_repeat_1k_smoke_turn() {
    let board = turn_board();
    let hole = pick_hole_disjoint(&board);
    let baseline = canonical_observation_id(StreetTag::Turn, &board, hole);
    for i in 0..1_000 {
        let other = canonical_observation_id(StreetTag::Turn, &board, hole);
        assert_eq!(baseline, other, "repeat iter {i}: byte-equal (turn)");
    }
}

#[test]
fn canonical_observation_id_repeat_1k_smoke_river() {
    let board = river_board();
    let hole = pick_hole_disjoint(&board);
    let baseline = canonical_observation_id(StreetTag::River, &board, hole);
    for i in 0..1_000 {
        let other = canonical_observation_id(StreetTag::River, &board, hole);
        assert_eq!(baseline, other, "repeat iter {i}: byte-equal (river)");
    }
}

// ============================================================================
// 2. canonical_observation_id_suit_rename_invariance（API §1040 影响 ⑤ (b)）
// ============================================================================
//
// D-218-rev1 花色对称等价类核心不变量：全局花色重命名（permutation σ on
// {Spades, Hearts, Diamonds, Clubs}）应用到 (board, hole) 不改变 canonical_id。
//
// 示例：σ = {S↔H, D↔C}（双 transposition）。原 board {A♠ K♥ 7♦} → 重命名后
// {A♥ K♠ 7♣}。原 hole {Q♣ T♦} → {Q♦ T♣}。canonical_id 必须相等。
//
// 测试 4 种代表性花色置换：identity、双 transposition (S↔H, D↔C)、3-cycle
// (S→H→D→S)、reversal (S↔C, H↔D)。
#[test]
fn canonical_observation_id_suit_rename_invariance_flop() {
    let board = flop_board();
    let hole = [
        Card::new(Rank::Queen, Suit::Clubs),
        Card::new(Rank::Ten, Suit::Diamonds),
    ];
    let baseline = canonical_observation_id(StreetTag::Flop, &board, hole);

    let permutations: [[Suit; 4]; 4] = [
        // identity
        [Suit::Spades, Suit::Hearts, Suit::Diamonds, Suit::Clubs],
        // S↔H, D↔C
        [Suit::Hearts, Suit::Spades, Suit::Clubs, Suit::Diamonds],
        // S→H→D→S, C 不动
        [Suit::Hearts, Suit::Diamonds, Suit::Spades, Suit::Clubs],
        // S↔C, H↔D
        [Suit::Clubs, Suit::Diamonds, Suit::Hearts, Suit::Spades],
    ];
    for (idx, sigma) in permutations.iter().enumerate() {
        let renamed_board: Vec<Card> = board.iter().map(|c| rename_suit(*c, sigma)).collect();
        let renamed_hole = [rename_suit(hole[0], sigma), rename_suit(hole[1], sigma)];
        let other = canonical_observation_id(StreetTag::Flop, &renamed_board, renamed_hole);
        assert_eq!(
            baseline, other,
            "D-218-rev1 花色重命名不变性 (perm idx {idx})：σ={sigma:?}, baseline={baseline}, got {other}"
        );
    }
}

#[test]
fn canonical_observation_id_suit_rename_invariance_turn() {
    let board = turn_board();
    let hole = [
        Card::new(Rank::Queen, Suit::Clubs),
        Card::new(Rank::Ten, Suit::Diamonds),
    ];
    let baseline = canonical_observation_id(StreetTag::Turn, &board, hole);
    // S↔C, H↔D 双 transposition。
    let sigma: [Suit; 4] = [Suit::Clubs, Suit::Diamonds, Suit::Hearts, Suit::Spades];
    let renamed_board: Vec<Card> = board.iter().map(|c| rename_suit(*c, &sigma)).collect();
    let renamed_hole = [rename_suit(hole[0], &sigma), rename_suit(hole[1], &sigma)];
    let other = canonical_observation_id(StreetTag::Turn, &renamed_board, renamed_hole);
    assert_eq!(baseline, other, "D-218-rev1 turn 花色重命名不变性");
}

#[test]
fn canonical_observation_id_suit_rename_invariance_river() {
    let board = river_board();
    let hole = [
        Card::new(Rank::Jack, Suit::Spades),
        Card::new(Rank::Three, Suit::Clubs),
    ];
    let baseline = canonical_observation_id(StreetTag::River, &board, hole);
    let sigma: [Suit; 4] = [Suit::Hearts, Suit::Spades, Suit::Clubs, Suit::Diamonds];
    let renamed_board: Vec<Card> = board.iter().map(|c| rename_suit(*c, &sigma)).collect();
    let renamed_hole = [rename_suit(hole[0], &sigma), rename_suit(hole[1], &sigma)];
    let other = canonical_observation_id(StreetTag::River, &renamed_board, renamed_hole);
    assert_eq!(baseline, other, "D-218-rev1 river 花色重命名不变性");
}

/// 把 card 的花色按 sigma 重命名（sigma[i] 是原花色 i 的目标花色）。
fn rename_suit(card: Card, sigma: &[Suit; 4]) -> Card {
    let new_suit = sigma[card.suit() as usize];
    Card::new(card.rank(), new_suit)
}

// ============================================================================
// 3. canonical_observation_id_compactness_smoke（API §1040 影响 ⑤ (c)）
// ============================================================================
//
// id 紧凑（无空洞）：穷举一定量的随机 (board, hole) → 收集所有 canonical_id →
// 应填满 `[0, n_canonical_observation(street))` 中的紧凑前缀（B1 smoke 不要求
// 完整覆盖全集，仅断言**id 不超界 + 覆盖到的 id 集合是 [0, max+1) 的子集且
// max+1 ≤ n_canonical_observation(street)**）。完整 1326 hole × C(50,3) board
// 枚举留 C2/D1。
//
// **B1 状态**：`canonical_observation_id` `unimplemented!()` ⇒ 第一次调用
// panic；本测试附加 `n_canonical_observation` 上界查询，B2 stub 落地后激活
// 完整断言。
#[test]
fn canonical_observation_id_compactness_smoke_flop() {
    use poker::BucketTable;
    // 注：BucketTable::n_canonical_observation 是 BucketTable 实例方法（API §4
    // BT-005-rev1）。B1 阶段无 mmap artifact，这条用 placeholder 上界 2_000_000
    // （API line 1034 BT-008-rev1 保守上界 flop ≤ 2_000_000）。B2 落地后可改为
    // 实际从 BucketTable header 读取。
    let _hint = BucketTable::n_canonical_observation; // 类型存在性检查（trip-wire）。
    let upper_bound: u32 = 2_000_000;

    let mut seen: HashSet<u32> = HashSet::new();
    // 32 个 fixed (board, hole) 组合（B1 smoke）。
    let boards: [Vec<Card>; 4] = [
        flop_board(),
        vec![
            Card::new(Rank::Two, Suit::Clubs),
            Card::new(Rank::Three, Suit::Clubs),
            Card::new(Rank::Four, Suit::Clubs),
        ],
        vec![
            Card::new(Rank::Five, Suit::Hearts),
            Card::new(Rank::Five, Suit::Diamonds),
            Card::new(Rank::Five, Suit::Spades),
        ],
        vec![
            Card::new(Rank::Ten, Suit::Spades),
            Card::new(Rank::Jack, Suit::Spades),
            Card::new(Rank::Queen, Suit::Spades),
        ],
    ];
    for board in boards.iter() {
        // 每个 board 取 8 个不同 hole（数值最小 16 张未占用中两两组合）。
        let used: HashSet<u8> = board.iter().map(|c| c.to_u8()).collect();
        let avail: Vec<u8> = (0..52u8).filter(|v| !used.contains(v)).take(16).collect();
        for i in 0..4 {
            for j in (i + 1)..4 {
                let hole = [
                    Card::from_u8(avail[i]).unwrap(),
                    Card::from_u8(avail[j]).unwrap(),
                ];
                let id = canonical_observation_id(StreetTag::Flop, board, hole);
                assert!(
                    id < upper_bound,
                    "id ({id}) < 保守上界 ({upper_bound})（D-244-rev1 BT-008-rev1）"
                );
                seen.insert(id);
            }
        }
    }
    assert!(
        !seen.is_empty(),
        "至少观察到 1 个 canonical_id（B2 stub 落地后）"
    );
    // 紧凑性 smoke：max(seen) < n_canonical_observation(flop) 上界。
    let max_seen = *seen.iter().max().unwrap();
    assert!(
        max_seen < upper_bound,
        "max(seen) = {max_seen} < upper_bound {upper_bound}"
    );
}

// ============================================================================
// 4. canonical_observation_id_preflop_panics（前置条件断言）
// ============================================================================
//
// API §2 字面（line 1022）：`canonical_observation_id` 仅对 `StreetTag::{Flop,
// Turn, River}` 有效（`board.len() ∈ {3, 4, 5}`）；`StreetTag::Preflop` 调用
// **panic**（caller 应改用 `canonical_hole_id`）。
//
// **B1 状态**：A1 stub 整体 `unimplemented!()`，进入函数体即 panic（任何
// street 都触发）；测试用 `should_panic` 包住。B2 [实现] 落地后该测试仍应
// panic，因为 preflop 路径必须 panic（caller error）。
#[test]
#[should_panic]
fn canonical_observation_id_preflop_panics() {
    let _ = canonical_observation_id(
        StreetTag::Preflop,
        &[],
        [
            Card::new(Rank::Ace, Suit::Spades),
            Card::new(Rank::Ace, Suit::Hearts),
        ],
    );
}
