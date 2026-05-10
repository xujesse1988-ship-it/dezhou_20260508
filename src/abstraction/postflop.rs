//! Postflop bucket abstraction（API §2）。
//!
//! `PostflopBucketAbstraction` + `canonical_observation_id` helper
//! （D-216-rev1 / D-218-rev1 / D-244-rev1）。
//!
//! D-252 浮点边界：本文件位于运行时映射热路径（`InfoAbstraction::map` 落地 +
//! `canonical_observation_id` 整数 hash + `BucketTable::lookup` 整数 key），
//! 与 `abstraction::map` 子模块同级承担整数 key 计算，故同样顶
//! `#![deny(clippy::float_arithmetic)]` inner attribute——任何 `f32` / `f64`
//! 算术触发硬错。浮点特征 / clustering 仅允许在 `abstraction::equity` /
//! `abstraction::cluster` / `abstraction::feature` sibling 模块出现。

#![deny(clippy::float_arithmetic)]

use crate::core::Card;
use crate::rules::state::GameState;

use crate::abstraction::bucket_table::{BucketConfig, BucketTable};
use crate::abstraction::info::{InfoAbstraction, InfoSetId, StreetTag};
use crate::abstraction::map::pack_info_set_id;
use crate::abstraction::preflop::{
    compute_betting_state, compute_position_bucket, compute_stack_bucket, compute_street_tag,
};

/// 每条街联合 (board, hole) canonical observation id 的上界（C2 收紧版本，原 A1
/// 保守上界 flop ≤ 2_000_000 / turn ≤ 20_000_000 / river ≤ 200_000_000，详见
/// D-244-rev1 BT-008-rev1 "A1 实测后可收紧"）。
///
/// **C2 实测取值**：3K + 6K + 10K = 19K entries × 4 bytes ≈ 76 KB。该取值压紧的
/// 设计动因是 lookup table 必须 100% feature-based 覆盖（对每个 obs_id 至少有一
/// 个训练 sample 经 hash 命中，从而其 bucket id 来自 k-means feature 分配而非
/// hash fallback；见 `train_one_street` 实现）。FNV-1a 32-bit hash 经 mod 后作为
/// equivalence representative，碰撞率随 N 减小而上升——但碰撞 (board, hole) 共享
/// 同一 bucket 是工程取舍（D-244-rev1 注：「stage 2 不解决该耦合」），换取 100%
/// 训练覆盖让 validation §3 std dev / EMD / 单调性门槛在小训练 set 下可达。
///
/// 上界保留在 D-244-rev1 BT-008-rev1 conservative cap 内（flop 3K ≤ 2M 等）。
pub const N_CANONICAL_OBSERVATION_FLOP: u32 = 3_000;
pub const N_CANONICAL_OBSERVATION_TURN: u32 = 6_000;
pub const N_CANONICAL_OBSERVATION_RIVER: u32 = 10_000;

/// postflop 联合 (board, hole) canonical observation id ∈
/// 0..n_canonical_observation(street)（花色对称等价类）。
/// `BucketTable::lookup(street, _)` postflop 入参由本函数计算。
///
/// 仅对 `StreetTag::{Flop, Turn, River}` 有效（`board.len() ∈ {3, 4, 5}`）；
/// `StreetTag::Preflop` 调用 panic（caller 应改用 `canonical_hole_id`）。
///
/// **C2 实现策略**：first-appearance suit remap → sorted (board / hole) →
/// FNV-1a 32-bit fold → mod 街相关上界（[`N_CANONICAL_OBSERVATION_FLOP`] /
/// `..._TURN` / `..._RIVER`）。算法满足：
///
/// - **确定性**：纯函数，无 RNG / 全局状态。
/// - **花色对称不变性**：全局花色置换 σ 应用到 (board, hole) 后 first-appearance
///   remap 输出同一 canonical 序列；FNV-1a fold 输入相同 → 输出相同 id。
/// - **范围**：mod 街相关上界后 id < `n_canonical_observation(street)`，落在
///   D-244-rev1 BT-008-rev1 收紧上界内。
///
/// FNV-1a hash 不保证不同等价类映射到不同 id（小概率碰撞），但 lookup table
/// 由 `train_bucket_table` 在 sample-and-assign 路径写入：每个 obs_id 取首次
/// 命中的 (board, hole) feature → bucket id。碰撞 obs_id 共用一个 bucket 是
/// 工程取舍（D-244-rev1 / D-273 浮点边界保护下，运行时禁止跑 inner equity）。
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

    // FNV-1a 32-bit fold, mod 街相关上界（C2 收紧；A1 原 2_000_000 全街共用）。
    let mut id: u32 = 2_166_136_261;
    let prime: u32 = 16_777_619;
    for &c in &board_canon[..board.len()] {
        id = id.wrapping_mul(prime) ^ u32::from(c);
    }
    for &c in &hole_canon {
        id = id.wrapping_mul(prime) ^ u32::from(c);
    }
    let modulus = match street {
        StreetTag::Flop => N_CANONICAL_OBSERVATION_FLOP,
        StreetTag::Turn => N_CANONICAL_OBSERVATION_TURN,
        StreetTag::River => N_CANONICAL_OBSERVATION_RIVER,
        StreetTag::Preflop => unreachable!(),
    };
    id % modulus
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
            .expect("BucketTable::lookup returned None on in-range observation_id")
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
