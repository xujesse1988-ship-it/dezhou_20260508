//! Postflop bucket abstraction（API §2）。
//!
//! `PostflopBucketAbstraction` + `canonical_observation_id` helper
//! （D-216-rev1 / D-218-rev1 / D-244-rev1）。

use crate::core::Card;
use crate::rules::state::GameState;

use crate::abstraction::bucket_table::{BucketConfig, BucketTable};
use crate::abstraction::info::{InfoAbstraction, InfoSetId, StreetTag};
use crate::abstraction::map::pack_info_set_id;
use crate::abstraction::preflop::{
    compute_betting_state, compute_position_bucket, compute_stack_bucket, compute_street_tag,
};

/// postflop 联合 (board, hole) canonical observation id ∈
/// 0..n_canonical_observation(street)（花色对称等价类）。
/// `BucketTable::lookup(street, _)` postflop 入参由本函数计算。
///
/// 仅对 `StreetTag::{Flop, Turn, River}` 有效（`board.len() ∈ {3, 4, 5}`）；
/// `StreetTag::Preflop` 调用 panic（caller 应改用 `canonical_hole_id`）。
///
/// **B2 实现策略**：first-appearance suit remap → sorted (board / hole) →
/// FNV-1a 32-bit fold → mod `2_000_000` 安全上界（D-244-rev1 BT-008-rev1
/// flop 保守上界）。算法满足：
///
/// - **确定性**：纯函数，无 RNG / 全局状态。
/// - **花色对称不变性**：全局花色置换 σ 应用到 (board, hole) 后 first-appearance
///   remap 输出同一 canonical 序列；FNV-1a fold 输入相同 → 输出相同 id。
/// - **范围**：mod 2_000_000 后 id < 2_000_000，落在 D-244-rev1 BT-008-rev1
///   保守上界内。
///
/// **B2 stub 状态**：本算法不保证不同等价类映射到不同 id（hash 碰撞），但
/// `BucketTable::lookup` 在 B2 stub 路径下总返回 `Some(0)`，碰撞不影响
/// `info_abs_postflop_bucket_id_in_range` 等 in-range smoke 断言。完整等价类
/// 枚举留 C2 \[实现\]。
pub fn canonical_observation_id(street: StreetTag, board: &[Card], hole: [Card; 2]) -> u32 {
    if matches!(street, StreetTag::Preflop) {
        panic!(
            "canonical_observation_id called with StreetTag::Preflop; use canonical_hole_id \
             for preflop hole canonical id (API §2 / D-218-rev1)"
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
        "canonical_observation_id: board length mismatch for {street:?}: expected {expected_board_len}, got {}",
        board.len()
    );

    // First-appearance suit remap across (board || hole), preserving original
    // input order so that any global suit permutation σ applied to all cards
    // yields the same canonical relabeling sequence (D-218-rev1 花色对称等价类).
    let mut suit_remap: [u8; 4] = [u8::MAX; 4];
    let mut next_suit: u8 = 0;
    for &c in board.iter().chain(hole.iter()) {
        let s = c.suit() as u8;
        if suit_remap[s as usize] == u8::MAX {
            suit_remap[s as usize] = next_suit;
            next_suit += 1;
        }
    }
    let to_canonical = |c: Card| -> u8 {
        let canon_suit = suit_remap[c.suit() as usize];
        c.rank() as u8 * 4 + canon_suit
    };

    // Sort each of (board, hole) canonically so the input order doesn't affect
    // id (boards / holes are unordered sets in poker semantics).
    let mut board_canon: [u8; 5] = [0; 5];
    for (i, c) in board.iter().enumerate() {
        board_canon[i] = to_canonical(*c);
    }
    board_canon[..board.len()].sort_unstable();
    let mut hole_canon: [u8; 2] = [to_canonical(hole[0]), to_canonical(hole[1])];
    hole_canon.sort_unstable();

    // FNV-1a 32-bit fold, mod 2_000_000 (BT-008-rev1 flop conservative upper bound).
    let mut id: u32 = 2_166_136_261;
    let prime: u32 = 16_777_619;
    for &c in &board_canon[..board.len()] {
        id = id.wrapping_mul(prime) ^ u32::from(c);
    }
    for &c in &hole_canon {
        id = id.wrapping_mul(prime) ^ u32::from(c);
    }
    id % 2_000_000
}

/// mmap-backed postflop bucket abstraction（D-213 / D-214 / D-216 / D-218-rev1 /
/// D-219）。
pub struct PostflopBucketAbstraction {
    table: BucketTable,
}

impl PostflopBucketAbstraction {
    /// 从 mmap-loaded `BucketTable` 构造。
    pub fn new(table: BucketTable) -> PostflopBucketAbstraction {
        PostflopBucketAbstraction { table }
    }

    /// 仅对 flop / turn / river 街生效；preflop 应走 `PreflopLossless169`。
    /// 内部走 `canonical_observation_id(street, board, hole)` →
    /// `BucketTable::lookup`。
    pub fn bucket_id(&self, state: &GameState, hole: [Card; 2]) -> u32 {
        let street_tag = compute_street_tag(state.street());
        if matches!(street_tag, StreetTag::Preflop) {
            panic!(
                "PostflopBucketAbstraction::bucket_id called on Preflop; caller should use \
                 PreflopLossless169 for preflop"
            );
        }
        let observation_id = canonical_observation_id(street_tag, state.board(), hole);
        self.table
            .lookup(street_tag, observation_id)
            .expect("BucketTable::lookup returned None on in-range observation_id (B2 stub bug)")
    }

    pub fn config(&self) -> BucketConfig {
        self.table.config()
    }
}

impl InfoAbstraction for PostflopBucketAbstraction {
    /// postflop 路径：`(street, board, hole) → bucket_id`（mmap），与 preflop key
    /// 字段（position / stack / betting_state / street_tag）合并到 `InfoSetId`
    /// （D-215 统一 64-bit 编码；`bucket_id` 由 `BucketTable::lookup` 命中得到）。
    fn map(&self, state: &GameState, hole: [Card; 2]) -> InfoSetId {
        let actor_seat = state
            .current_player()
            .expect("InfoAbstraction::map called on terminal state (IA-006-rev1)");
        let bucket_id = self.bucket_id(state, hole);
        let position_bucket = compute_position_bucket(state, actor_seat);
        let stack_bucket = compute_stack_bucket(state, actor_seat);
        let betting_state = compute_betting_state(state);
        let street_tag = compute_street_tag(state.street());
        pack_info_set_id(
            bucket_id,
            position_bucket,
            stack_bucket,
            betting_state,
            street_tag,
        )
    }
}
