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

/// 每条街联合 (board, hole) canonical observation id 的上界。
///
/// **§G-batch1 §3.2 [实现]**：从 C2 收紧的 FNV-1a hash mod 上界（flop=3K / turn=6K
/// / river=10K）切换到 D-218-rev2 §2 实测真等价类数。三常量改为 `pub use` re-export
/// 自 `crate::abstraction::canonical_enum`，让既有 caller（`tests/canonical_observation.rs`
/// / `bucket_table.rs` lookup_table 分配）通过本路径无缝切换到真值。
///
/// 切换前 C2 口径（FNV-1a + mod）：3K + 6K + 10K = 19K × 4 bytes ≈ 76 KB
/// lookup_table；hash 碰撞下 ~100% bucket 覆盖（D-244-rev1 工程取舍）。
///
/// §G-batch1 §3.2 后口径（真等价类）：1,286,792 + 13,960,050 + 123,156,254 =
/// 138,403,096 entries × 4 bytes ≈ 528 MB lookup_table；每 canonical 观察唯一
/// obs_id，无碰撞（详见 D-218-rev2 §3 "唯一性（新）"）。Artifact 量级飞涨到
/// ~528 MB（D-218-rev2 §5），分发走 GitHub Release + BLAKE3（D-218-rev2 §10）。
pub use crate::abstraction::canonical_enum::{
    N_CANONICAL_OBSERVATION_FLOP, N_CANONICAL_OBSERVATION_RIVER, N_CANONICAL_OBSERVATION_TURN,
};

/// postflop 联合 (board, hole) canonical observation id ∈
/// `0..n_canonical_observation(street)`（花色对称等价类）。
/// `BucketTable::lookup(street, _)` postflop 入参由本函数计算。
///
/// 仅对 `StreetTag::{Flop, Turn, River}` 有效（`board.len() ∈ {3, 4, 5}`）；
/// `StreetTag::Preflop` 调用 panic（caller 应改用 `canonical_hole_id`）。
///
/// **§G-batch1 §3.2 [实现]**：实现 forward 到
/// [`crate::abstraction::canonical_enum::canonical_observation_id`]——
/// D-218-rev2 真等价类枚举（Waugh 2013-style hand isomorphism + colex ranking 3
/// 街全枚举）。公开签名 byte-equal 不变；返回值数值切换。
///
/// **历史 D-218-rev1 / C2 设计（FNV-1a hash mod 上界）**：sort (board, hole) by
/// raw card index → first-appearance suit remap → FNV-1a 32-bit fold → mod
/// `N_CANONICAL_OBSERVATION_<street>`（C2 收紧到 3K/6K/10K）。hash 碰撞让多个互
/// 不等价 (board, hole) 共享 obs_id → 共享 bucket id → bucket 内 EHS std_dev 由
/// 碰撞跨度决定而非 k-means clustering 质量决定。
///
/// **§G-batch1 §3.2 后口径**：调用 [`canonical_enum`](crate::abstraction::canonical_enum)，
/// 返回 dense `[0, N)` 上的 canonical id（N = 1,286,792 / 13,960,050 / 123,156,254
/// per street）。两个互不等价 (board, hole) 一定映射到不同 id（D-218-rev2 §3
/// "唯一性（新）"）。算法满足同一组不变量（确定性 / input-order invariance /
/// 花色对称 / partition 区分），由 canonical_enum 严格保证而非 hash 近似。
pub fn canonical_observation_id(street: StreetTag, board: &[Card], hole: [Card; 2]) -> u32 {
    crate::abstraction::canonical_enum::canonical_observation_id(street, board, hole)
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
