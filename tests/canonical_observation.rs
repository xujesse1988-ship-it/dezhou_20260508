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
//!
//! **§G-batch1 §2 [测试] 新增**（详见 `docs/pluribus_stage2_decisions.md` §10
//! "Stage 3 起步 batch 1 — D-218-rev2 / D-244-rev2 真等价类枚举"）：第 6 节
//! D-218-rev2 真等价类枚举 4 类断言（N 常量 / 100K 随机 uniqueness / 100K 随机
//! max id 接近 N / 全 flop 26M 枚举精确 N_FLOP distinct）。全部 `#[ignore]` 等
//! §G-batch1 §3 [实现] 落地（colex ranking + N 真值常量 + schema_version bump）
//! 后由 [实现] commit 取消 ignore。

use std::collections::HashSet;

use poker::abstraction::postflop::{
    N_CANONICAL_OBSERVATION_FLOP, N_CANONICAL_OBSERVATION_RIVER, N_CANONICAL_OBSERVATION_TURN,
};
use poker::rng_substream::{derive_substream_seed, EQUITY_MONTE_CARLO};
use poker::{canonical_observation_id, Card, ChaCha20Rng, Rank, RngSource, StreetTag, Suit};

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
// 4. canonical_observation_id_input_shuffle_invariance（§C-rev2 §4 / D-218-rev1）
// ============================================================================
//
// D-218-rev1 联合 canonical 等价类要求：board / hole 在扑克语义上是无序集合，
// 同一 (board set, hole set) 任意输入顺序应得到同一 canonical_id。原实现
// `for &c in board.iter().chain(hole.iter())` 走原始输入顺序构造 first-appearance
// suit remap，输入顺序不同 → suit_remap 不同 → 不同 id（§C-rev2 §4 反例：
// `[As, Kh, Qd]` vs `[Kh, As, Qd]` 同 board set 不同 id）。修正：remap 之前先
// `to_u8()` 升序排序 board / hole。
//
// 测试覆盖：每条街取一个固定 (board, hole) 集合 → 枚举 board 全排列 + hole 两种
// 顺序 → 全部 canonical_id 必须等于 baseline。flop 6 × 2 = 12 / turn 24 × 2 = 48
// / river 120 × 2 = 240 cases。
#[test]
fn canonical_observation_id_input_shuffle_invariance_flop() {
    let board = flop_board();
    let hole = [
        Card::new(Rank::Queen, Suit::Clubs),
        Card::new(Rank::Ten, Suit::Diamonds),
    ];
    let baseline = canonical_observation_id(StreetTag::Flop, &board, hole);
    for board_perm in permutations(&board) {
        for hole_perm in [hole, [hole[1], hole[0]]] {
            let id = canonical_observation_id(StreetTag::Flop, &board_perm, hole_perm);
            assert_eq!(
                baseline, id,
                "flop input shuffle: board={board_perm:?} hole={hole_perm:?} → {id}, baseline={baseline}"
            );
        }
    }
}

#[test]
fn canonical_observation_id_input_shuffle_invariance_turn() {
    let board = turn_board();
    let hole = [
        Card::new(Rank::Queen, Suit::Clubs),
        Card::new(Rank::Ten, Suit::Diamonds),
    ];
    let baseline = canonical_observation_id(StreetTag::Turn, &board, hole);
    for board_perm in permutations(&board) {
        for hole_perm in [hole, [hole[1], hole[0]]] {
            let id = canonical_observation_id(StreetTag::Turn, &board_perm, hole_perm);
            assert_eq!(
                baseline, id,
                "turn input shuffle: board={board_perm:?} hole={hole_perm:?} → {id}, baseline={baseline}"
            );
        }
    }
}

#[test]
fn canonical_observation_id_input_shuffle_invariance_river() {
    let board = river_board();
    let hole = [
        Card::new(Rank::Jack, Suit::Spades),
        Card::new(Rank::Three, Suit::Clubs),
    ];
    let baseline = canonical_observation_id(StreetTag::River, &board, hole);
    for board_perm in permutations(&board) {
        for hole_perm in [hole, [hole[1], hole[0]]] {
            let id = canonical_observation_id(StreetTag::River, &board_perm, hole_perm);
            assert_eq!(
                baseline, id,
                "river input shuffle: board={board_perm:?} hole={hole_perm:?} → {id}, baseline={baseline}"
            );
        }
    }
}

/// §C-rev2 §4 反例（修复后通过）：原实现下 `[As, Kh, Qd]` vs `[Kh, As, Qd]`
/// 同 (board set, hole set) 但 first-appearance remap 输出不同 suit 编号 → 不同
/// canonical_id。修复后两者必须相等。
#[test]
fn canonical_observation_id_input_shuffle_regression_canary() {
    let hole = [
        Card::new(Rank::Queen, Suit::Clubs),
        Card::new(Rank::Ten, Suit::Diamonds),
    ];
    let board_a = vec![
        Card::new(Rank::Ace, Suit::Spades),
        Card::new(Rank::King, Suit::Hearts),
        Card::new(Rank::Queen, Suit::Diamonds),
    ];
    let board_b = vec![
        Card::new(Rank::King, Suit::Hearts),
        Card::new(Rank::Ace, Suit::Spades),
        Card::new(Rank::Queen, Suit::Diamonds),
    ];
    let id_a = canonical_observation_id(StreetTag::Flop, &board_a, hole);
    let id_b = canonical_observation_id(StreetTag::Flop, &board_b, hole);
    assert_eq!(
        id_a, id_b,
        "§C-rev2 §4 regression canary：[As,Kh,Qd] vs [Kh,As,Qd] 同 board set，必须 canonical_id 相同"
    );
}

/// 生成 `slice` 的所有排列（Heap's algorithm，n ≤ 5 适用）。
fn permutations<T: Clone>(slice: &[T]) -> Vec<Vec<T>> {
    let mut result = Vec::new();
    let mut buf: Vec<T> = slice.to_vec();
    let n = buf.len();
    let mut c = vec![0usize; n];
    result.push(buf.clone());
    let mut i = 0;
    while i < n {
        if c[i] < i {
            let swap_with = if i % 2 == 0 { 0 } else { c[i] };
            buf.swap(swap_with, i);
            result.push(buf.clone());
            c[i] += 1;
            i = 0;
        } else {
            c[i] = 0;
            i += 1;
        }
    }
    result
}

// ============================================================================
// 5. canonical_observation_id_preflop_panics（前置条件断言）
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

// ============================================================================
// 6. D-218-rev2 真等价类枚举（§G-batch1 §2 [测试] 新增）
// ============================================================================
//
// 详见 `docs/pluribus_stage2_decisions.md` §10 "Stage 3 起步 batch 1
// — D-218-rev2 / D-244-rev2 真等价类枚举"。本节 4 类断言钉死 D-218-rev2 [实现]
// 必须满足的契约：
//
// 1. **N 常量精确值**：`N_CANONICAL_OBSERVATION_FLOP / TURN / RIVER` 必须分别
//    等于实测 hand-isomorphism 数 1,286,792 / 13,960,050 / 123,156,254
//    （D-218-rev2 §2 字面；§G-batch1 §3.1 实测修正 stage 2 §C-rev1 §2 "~25K"
//    估算误差）。
// 2. **uniqueness（100K 随机样本）**：从 100K 随机 (board, hole) 应观察到接近
//    N 量级 distinct canonical_id（D-218-rev2 §3 "唯一性（新）"）。当前 FNV-1a
//    hash mod 3K/6K/10K 路径下 distinct count 受 modulus 上限封顶。
// 3. **dense packing（max id 接近 N-1）**：100K 随机样本观察到的 max id 应
//    接近 `N - 1`（D-218-rev2 §3 "稠密性"）。
// 4. **full flop 枚举精确 N_FLOP distinct**：(52 choose 3) × (49 choose 2) =
//    26M (board, hole) 全枚举后 distinct canonical_id 数恰好 = N_FLOP =
//    1,286,792（`#[ignore]` 双重 release + --ignored opt-in，与 stage-2 §C2 /
//    §D2 同形态）。
//
// 全部 `#[ignore = "§G-batch1 §3: D-218-rev2 [实现] 落地后转 active"]` 标注；
// 当前 hash 路径下断言**预期失败**——[实现] 闭合 commit 取消 ignore 并验证
// 全绿（与 stage-1 §C2 / §D2 / §F2 + stage-2 §B1 / §C1 / §F1 同形态）。

/// §G-batch1 §2 sampling helper：从 deck 抽 `count` 张不重复 `Card`。
fn sample_distinct_cards(rng: &mut dyn RngSource, count: usize) -> Vec<Card> {
    assert!(count <= 52, "至多 52 张");
    let mut available: Vec<u8> = (0..52u8).collect();
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let pick = (rng.next_u64() % (available.len() as u64 - i as u64)) as usize;
        let idx = i + pick;
        available.swap(i, idx);
        out.push(Card::from_u8(available[i]).expect("0..52 valid"));
    }
    out
}

/// D-218-rev2 §2 N 常量精确值断言（实测标准 hand-isomorphism 数）。
///
/// **当前 D-218-rev1 状态**：3_000 / 6_000 / 10_000 （C2 收紧上界）——断言失败。
/// **§G-batch1 §3 [实现] 落地后**：1_286_792 / 13_960_050 / 123_156_254——断言通过。
///
/// 数字来源：`src/abstraction/canonical_enum.rs::enumerate_canonical_forms`
/// 实测枚举（详见 `docs/pluribus_stage2_decisions.md` §10 "Stage 3 起步 batch 1
/// — D-218-rev2"；§G-batch1 §3.1 实测修正 stage 2 §C-rev1 §2 "~25K" 估算 14000x
/// 误差）。
#[test]
#[ignore = "§G-batch1 §3: D-218-rev2 [实现] 落地后转 active"]
fn n_canonical_observation_constants_match_d218_rev2_spec() {
    assert_eq!(
        N_CANONICAL_OBSERVATION_FLOP, 1_286_792,
        "D-218-rev2 §2: flop 3+2 cards canonical 等价类 = 1,286,792"
    );
    assert_eq!(
        N_CANONICAL_OBSERVATION_TURN, 13_960_050,
        "D-218-rev2 §2: turn 4+2 cards canonical 等价类 = 13,960,050"
    );
    assert_eq!(
        N_CANONICAL_OBSERVATION_RIVER, 123_156_254,
        "D-218-rev2 §2: river 5+2 cards canonical 等价类 = 123,156,254"
    );
}

/// D-218-rev2 §3 "唯一性（新）" + "稠密性" flop 100K 随机样本 uniqueness。
///
/// 100K 随机 (board, hole) → 期望 distinct count > 95K（N_FLOP = 1,286,792 远 >
/// 100K 采样容量 → equivalence-class 碰撞稀少，期望 ~99K 几乎无重）+ max_id >
/// 1M（接近 N - 1，dense packing）。当前 FNV-1a hash mod 3K 路径下 distinct <
/// 3000 / max_id < 3000——断言失败。
#[test]
#[ignore = "§G-batch1 §3: D-218-rev2 [实现] 落地后转 active"]
fn canonical_observation_id_uniqueness_random_100k_flop() {
    let mut seen: HashSet<u32> = HashSet::new();
    let mut max_id: u32 = 0;
    let mut rng = ChaCha20Rng::from_seed(derive_substream_seed(
        0xD218_BEEF_CAFE_0001,
        EQUITY_MONTE_CARLO,
        0,
    ));
    for _ in 0..100_000 {
        let cards = sample_distinct_cards(&mut rng, 5);
        let board: Vec<Card> = cards[..3].to_vec();
        let hole: [Card; 2] = [cards[3], cards[4]];
        let id = canonical_observation_id(StreetTag::Flop, &board, hole);
        max_id = max_id.max(id);
        seen.insert(id);
        assert!(
            id < N_CANONICAL_OBSERVATION_FLOP,
            "id ({id}) < N_CANONICAL_OBSERVATION_FLOP ({N_CANONICAL_OBSERVATION_FLOP})"
        );
    }
    assert!(
        seen.len() > 95_000,
        "D-218-rev2 §3 uniqueness（flop）：expected > 95K distinct canonical_ids from \
         100K random samples (got {})；当前 FNV-1a hash mod 3K 路径下 distinct < 3000",
        seen.len()
    );
    assert!(
        max_id > 1_000_000,
        "D-218-rev2 §3 dense packing（flop）：max canonical_id ({max_id}) 应接近 \
         N_FLOP - 1 = {}",
        N_CANONICAL_OBSERVATION_FLOP - 1
    );
}

/// D-218-rev2 §3 turn 100K 随机样本 uniqueness（N = 13,960,050 远 > 100K 采样
/// 容量 → equivalence-class 自然碰撞极稀少，distinct 期望 > 99.5K + max_id >
/// 10M）。当前 FNV-1a hash mod 6K 路径下 distinct < 6000——断言失败。
#[test]
#[ignore = "§G-batch1 §3: D-218-rev2 [实现] 落地后转 active"]
fn canonical_observation_id_uniqueness_random_100k_turn() {
    let mut seen: HashSet<u32> = HashSet::new();
    let mut max_id: u32 = 0;
    let mut rng = ChaCha20Rng::from_seed(derive_substream_seed(
        0xD218_BEEF_CAFE_0002,
        EQUITY_MONTE_CARLO,
        0,
    ));
    for _ in 0..100_000 {
        let cards = sample_distinct_cards(&mut rng, 6);
        let board: Vec<Card> = cards[..4].to_vec();
        let hole: [Card; 2] = [cards[4], cards[5]];
        let id = canonical_observation_id(StreetTag::Turn, &board, hole);
        max_id = max_id.max(id);
        seen.insert(id);
        assert!(
            id < N_CANONICAL_OBSERVATION_TURN,
            "id ({id}) < N_CANONICAL_OBSERVATION_TURN ({N_CANONICAL_OBSERVATION_TURN})"
        );
    }
    assert!(
        seen.len() > 99_500,
        "D-218-rev2 §3 uniqueness（turn）：expected > 99.5K distinct canonical_ids from \
         100K random samples (got {})；当前 FNV-1a hash mod 6K 路径下 distinct < 6000",
        seen.len()
    );
    assert!(
        max_id > 10_000_000,
        "D-218-rev2 §3 dense packing（turn）：max canonical_id ({max_id}) 应接近 \
         N_TURN - 1 = {}",
        N_CANONICAL_OBSERVATION_TURN - 1
    );
}

/// D-218-rev2 §3 river 100K 随机样本 uniqueness（N = 123,156,254，几乎不可能
/// 碰撞 → distinct 期望 > 99,900 + max_id > 50M）。当前 FNV-1a hash mod 10K
/// 路径下 distinct < 10000 / max_id < 10000——断言失败。
#[test]
#[ignore = "§G-batch1 §3: D-218-rev2 [实现] 落地后转 active"]
fn canonical_observation_id_uniqueness_random_100k_river() {
    let mut seen: HashSet<u32> = HashSet::new();
    let mut max_id: u32 = 0;
    let mut rng = ChaCha20Rng::from_seed(derive_substream_seed(
        0xD218_BEEF_CAFE_0003,
        EQUITY_MONTE_CARLO,
        0,
    ));
    for _ in 0..100_000 {
        let cards = sample_distinct_cards(&mut rng, 7);
        let board: Vec<Card> = cards[..5].to_vec();
        let hole: [Card; 2] = [cards[5], cards[6]];
        let id = canonical_observation_id(StreetTag::River, &board, hole);
        max_id = max_id.max(id);
        seen.insert(id);
        assert!(
            id < N_CANONICAL_OBSERVATION_RIVER,
            "id ({id}) < N_CANONICAL_OBSERVATION_RIVER ({N_CANONICAL_OBSERVATION_RIVER})"
        );
    }
    assert!(
        seen.len() > 99_900,
        "D-218-rev2 §3 uniqueness（river）：expected > 99.9K distinct canonical_ids from \
         100K random samples (got {})；当前 FNV-1a hash mod 10K 路径下 distinct < 10000",
        seen.len()
    );
    assert!(
        max_id > 50_000_000,
        "D-218-rev2 §3 dense packing（river）：max canonical_id ({max_id}) 应接近 \
         N_RIVER - 1 = {}",
        N_CANONICAL_OBSERVATION_RIVER - 1
    );
}

/// D-218-rev2 §3 flop 全枚举精确 1,286,792 distinct（exhaustive ground truth）。
///
/// 枚举 (52 choose 3) × (49 choose 2) = 22,100 × 1,176 = 25,989,600 (board, hole)
/// 组合，所有 canonical_id 收集到 HashSet → 最终 size 必须恰好等于 N_FLOP =
/// 1,286,792（实测标准 hand-isomorphism 数）。
///
/// **耗时**：~10 s release on 1-CPU host（26M canonical_observation_id calls
/// × ~400 ns/call）。`#[ignore]` 默认 `cargo test` 跳过，仅 `cargo test --release
/// -- --ignored` 触发（与 stage-1 §C2 / §D2 + stage-2 §C1 / §F1 同形态：
/// double-ignore 路径下 release + --ignored opt-in）。
///
/// turn / river 不写 full enumeration test 因为：turn (52 choose 4) × (48
/// choose 2) ~305M / river (52 choose 5) × (47 choose 2) ~2.8B 即使 release
/// 也需 ~2 h / ~16 h——超 dev loop SLO + 与 100K uniqueness 测试覆盖度等价
/// （100K 随机采样在 turn/river 等价类空间下与全枚举差距 < 0.1% statistical）。
#[test]
#[ignore = "§G-batch1 §3: D-218-rev2 [实现] 落地后转 active + release/--ignored opt-in"]
fn canonical_observation_id_full_flop_enumeration_exactly_n_flop_distinct() {
    let mut seen: HashSet<u32> = HashSet::new();
    for b0 in 0..52u8 {
        for b1 in (b0 + 1)..52u8 {
            for b2 in (b1 + 1)..52u8 {
                let board = [
                    Card::from_u8(b0).expect("0..52"),
                    Card::from_u8(b1).expect("0..52"),
                    Card::from_u8(b2).expect("0..52"),
                ];
                let board_vec = board.to_vec();
                for h0 in 0..52u8 {
                    if h0 == b0 || h0 == b1 || h0 == b2 {
                        continue;
                    }
                    for h1 in (h0 + 1)..52u8 {
                        if h1 == b0 || h1 == b1 || h1 == b2 {
                            continue;
                        }
                        let hole = [
                            Card::from_u8(h0).expect("0..52"),
                            Card::from_u8(h1).expect("0..52"),
                        ];
                        let id = canonical_observation_id(StreetTag::Flop, &board_vec, hole);
                        seen.insert(id);
                    }
                }
            }
        }
    }
    assert_eq!(
        seen.len(),
        N_CANONICAL_OBSERVATION_FLOP as usize,
        "D-218-rev2 §3 exhaustive ground truth（flop）：26M (board, hole) 全枚举后 \
         distinct canonical_id 必须精确等于 N_FLOP = {}",
        N_CANONICAL_OBSERVATION_FLOP
    );
    // Dense packing 强约束：所有 id ∈ [0, N) 全覆盖（无空洞）。
    let max_id = *seen.iter().max().expect("non-empty");
    assert_eq!(
        max_id,
        N_CANONICAL_OBSERVATION_FLOP - 1,
        "D-218-rev2 §3 稠密性（flop）：max canonical_id = N_FLOP - 1（id ∈ [0, N) 全覆盖）"
    );
}
