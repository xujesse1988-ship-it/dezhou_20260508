#![no_main]
//! D1：abstraction smoke fuzz target（stage-2 workflow §D1 §输出 第 1 条）。
//!
//! 输入：任意 byte stream。前 1 字节解释为街选择器（Flop/Turn/River）；后续字节
//! 用作 deck 抽样的"randomness 源"——Fisher-Yates 部分洗牌从 0..52 抽
//! `board_len + 2` 张不重复 Card。
//!
//! 验证（与 §D1 §输出 line 373 字面 "1M random (board, hole) → bucket id
//! determinism + in-range" 对齐）：
//!
//! 1. **determinism**：同 (street, board, hole) 输入下，`canonical_observation_id`
//!    重复调用 byte-equal；`BucketTable::lookup(street, id)` 重复调用 byte-equal。
//! 2. **input-shuffle invariance**：board / hole 输入顺序任意置换后，
//!    `canonical_observation_id` 输出不变（§C-rev2 §4 不变量；
//!    `tests/canonical_observation.rs` regression guard 同型，扩到 1M fuzz 规模）。
//! 3. **in-range**：`canonical_observation_id` 输出 `< n_canonical_observation(street)`；
//!    `lookup` 返回 `Some(b)` 且 `b < table.bucket_count(street)`。
//! 4. **no-panic**：上述路径任一 panic / unwrap None / arithmetic overflow 是产品
//!    代码 bug。
//!
//! BucketTable fixture：进程启动时一次性 `train_in_memory(10/10/10, 0xC1C0DE,
//! NaiveHandEvaluator, 50 iter)`（与 `tests/clustering_determinism.rs::
//! BUCKET_BASELINE_CONFIG` 同形态，release ~5 s/seed）。`OnceLock` 缓存避免每个
//! libFuzzer 输入触发重训练。
//!
//! 角色边界：本 target 属 [测试] agent；任何 panic / invariant 违反由 cargo fuzz
//! 写 crash artifact，由 D2 [实现] agent 修产品代码（`§D1 §出口` 字面 "暴露
//! 1-3 个 corner case bug — 列入 issue 移交 D2"）。

use libfuzzer_sys::fuzz_target;
use poker::{canonical_observation_id, BucketConfig, BucketTable, Card, HandEvaluator, StreetTag};
use std::sync::{Arc, OnceLock};

const FUZZ_BUCKET_CONFIG: BucketConfig = BucketConfig {
    flop: 10,
    turn: 10,
    river: 10,
};
const FUZZ_TRAINING_SEED: u64 = 0xC1C0_DEAB_5712_0001;
const FUZZ_CLUSTER_ITER: u32 = 50;

static FUZZ_TABLE: OnceLock<Arc<BucketTable>> = OnceLock::new();

fn fuzz_table() -> Arc<BucketTable> {
    FUZZ_TABLE
        .get_or_init(|| {
            // NaiveHandEvaluator 通过 poker::eval 公开。fuzz crate 走 `poker = { path = ".." }`
            // 依赖；以下间接构造路径与 tests/bucket_quality.rs::cached_trained_table 同形态。
            let evaluator: Arc<dyn HandEvaluator> = Arc::new(poker::eval::NaiveHandEvaluator);
            Arc::new(BucketTable::train_in_memory(
                FUZZ_BUCKET_CONFIG,
                FUZZ_TRAINING_SEED,
                evaluator,
                FUZZ_CLUSTER_ITER,
            ))
        })
        .clone()
}

fuzz_target!(|data: &[u8]| {
    if data.len() < 16 {
        return;
    }
    let street = match data[0] % 3 {
        0 => StreetTag::Flop,
        1 => StreetTag::Turn,
        _ => StreetTag::River,
    };
    let board_len = match street {
        StreetTag::Flop => 3,
        StreetTag::Turn => 4,
        StreetTag::River => 5,
        StreetTag::Preflop => unreachable!(),
    };

    // Fisher-Yates 部分洗牌：byte stream 当 randomness。需要 board_len + 2 张牌
    // （≤ 7），最多消费 7 字节。
    let mut deck: [u8; 52] = std::array::from_fn(|i| i as u8);
    let need = board_len + 2;
    for i in 0..need {
        // data 至少 1 + 7 = 8 字节，前面已 reject < 16；这里 i < 7 无越界。
        let byte = data[1 + i] as usize;
        let pick = i + (byte % (52 - i));
        deck.swap(i, pick);
    }
    let board: Vec<Card> = deck[..board_len]
        .iter()
        .map(|&u| Card::from_u8(u).expect("0..52 valid"))
        .collect();
    let hole: [Card; 2] = [
        Card::from_u8(deck[board_len]).expect("0..52 valid"),
        Card::from_u8(deck[board_len + 1]).expect("0..52 valid"),
    ];

    let table = fuzz_table();
    let bucket_count = table.bucket_count(street);
    let n_canonical = match street {
        StreetTag::Flop => poker::abstraction::postflop::N_CANONICAL_OBSERVATION_FLOP,
        StreetTag::Turn => poker::abstraction::postflop::N_CANONICAL_OBSERVATION_TURN,
        StreetTag::River => poker::abstraction::postflop::N_CANONICAL_OBSERVATION_RIVER,
        StreetTag::Preflop => unreachable!(),
    };

    // (1) determinism：重复调用 byte-equal
    let id1 = canonical_observation_id(street, &board, hole);
    let id2 = canonical_observation_id(street, &board, hole);
    assert_eq!(id1, id2, "canonical_observation_id 重复调用不一致");
    assert!(
        id1 < n_canonical,
        "canonical id {} 越界 (street={:?}, n={})",
        id1,
        street,
        n_canonical
    );

    // (2) input-shuffle invariance：board / hole 输入顺序置换不影响 canonical id
    let mut shuffled_board = board.clone();
    // 用第 9..16 字节做置换 byte 源（已断言 data.len() >= 16）
    let shuffle_byte = data[8 + (board_len % 8)] as usize;
    let n = shuffled_board.len();
    if n >= 2 {
        shuffled_board.swap(0, shuffle_byte % n);
    }
    let shuffled_hole: [Card; 2] = [hole[1], hole[0]];
    let id_shuf = canonical_observation_id(street, &shuffled_board, shuffled_hole);
    assert_eq!(
        id1, id_shuf,
        "canonical_observation_id input-shuffle 不变性破坏 (§C-rev2 §4)"
    );

    // (3) in-range：lookup 返回 Some 且 bucket_id < bucket_count
    let b1 = table
        .lookup(street, id1)
        .expect("trained 路径 lookup 必有解");
    let b2 = table.lookup(street, id1).expect("repeat lookup 必有解");
    assert_eq!(b1, b2, "BucketTable::lookup 重复调用不一致");
    assert!(
        b1 < bucket_count,
        "bucket_id {} 越界 (street={:?}, count={})",
        b1,
        street,
        bucket_count
    );
});
