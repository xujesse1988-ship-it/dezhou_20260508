//! B1 §D 类 + C1 §输出 line 313：Clustering determinism harness 骨架。
//!
//! 覆盖 `pluribus_stage2_workflow.md` §B1 §输出 D 类清单 + §C1 §输出 line 313：
//!
//! - 同 seed clustering 重复 → bucket table 字节比对 stub（§B1）
//! - 跨线程 bucket id 一致 stub（§B1）
//! - **跨架构 32-seed bucket id baseline regression guard**（§C1 §输出 line 313；
//!   与阶段 1 `cross_arch_hash` 同形态）
//! - D-228 RngSource sub-stream 派生协议公开 contract（独立验证）
//!
//! 本文件按 §B1 §出口 line 250 "harness 能跑出占位结果或断言失败，流程不
//! panic"——核心 clustering 重复 / 跨线程 / 跨架构 baseline 断言 `#[ignore]`
//! （依赖 BucketTable 训练 CLI 完整路径，C2 才接），但 D-228 sub-stream 派生
//! 协议测试在 stub 落地前无法 byte-equal 比对（`derive_substream_seed` 是
//! unimplemented），同样 `#[ignore]`。
//!
//! D-228 op_id 命名常量是 const 路径（A1 已落地具体数值），不依赖 stub；其值
//! 域 / 命名空间断言走非 ignored 路径，验证 D-228 公开 contract 字面值。
//!
//! 全套 1M repeat / BLAKE3 byte-equal 完整断言留 D1。本文件骨架建立 harness
//! 入口与命名空间，B2 / C2 / D1 / D2 [实现] 与 [测试] 在此基础上扩展。
//!
//! 角色边界：本文件属 `[测试]` agent 产物（B1 / C1）。

use std::sync::Arc;

use poker::eval::NaiveHandEvaluator;
use poker::rng_substream::{
    self, derive_substream_seed, CLUSTER_MAIN_FLOP, CLUSTER_MAIN_RIVER, CLUSTER_MAIN_TURN,
    EHS2_INNER_EQUITY_FLOP, EHS2_INNER_EQUITY_RIVER, EHS2_INNER_EQUITY_TURN,
    EMPTY_CLUSTER_SPLIT_FLOP, EMPTY_CLUSTER_SPLIT_RIVER, EMPTY_CLUSTER_SPLIT_TURN,
    EQUITY_MONTE_CARLO, KMEANS_PP_INIT_FLOP, KMEANS_PP_INIT_RIVER, KMEANS_PP_INIT_TURN,
    OCHS_FEATURE_INNER, OCHS_WARMUP,
};
use poker::{canonical_observation_id, BucketConfig, BucketTable, Card, HandEvaluator, StreetTag};

// ============================================================================
// 1. D-228 op_id 命名空间分类（独立常量断言，不依赖 stub）
// ============================================================================
//
// D-228 字面：op_id 高 16 位 = 类别（OCHS / cluster / kmeans++ / split /
// equity / EHS² / OCHS_feature），低 16 位 = 街 / 子操作。本测试断言全 15 个
// 命名常量的位编码与 D-228 字面值一致。任一漂移立即 fail。
#[test]
fn d228_op_id_namespace_layout() {
    // 类别码（高 16 位）。
    assert_eq!(
        OCHS_WARMUP & 0xFFFF_0000,
        0x0001_0000,
        "D-228: OCHS 类 0x0001"
    );
    assert_eq!(
        CLUSTER_MAIN_FLOP & 0xFFFF_0000,
        0x0002_0000,
        "D-228: cluster main 类 0x0002"
    );
    assert_eq!(
        KMEANS_PP_INIT_FLOP & 0xFFFF_0000,
        0x0003_0000,
        "D-228: kmeans++ init 类 0x0003"
    );
    assert_eq!(
        EMPTY_CLUSTER_SPLIT_FLOP & 0xFFFF_0000,
        0x0004_0000,
        "D-228: empty cluster split 类 0x0004"
    );
    assert_eq!(
        EQUITY_MONTE_CARLO & 0xFFFF_0000,
        0x0005_0000,
        "D-228: equity Monte Carlo 类 0x0005"
    );
    assert_eq!(
        EHS2_INNER_EQUITY_FLOP & 0xFFFF_0000,
        0x0006_0000,
        "D-228: EHS² inner equity 类 0x0006"
    );
    assert_eq!(
        OCHS_FEATURE_INNER & 0xFFFF_0000,
        0x0007_0000,
        "D-228: OCHS feature inner 类 0x0007"
    );

    // 街区分（低 16 位）。
    assert_eq!(CLUSTER_MAIN_FLOP & 0xFFFF, 0x0001, "D-228: flop = 0x0001");
    assert_eq!(CLUSTER_MAIN_TURN & 0xFFFF, 0x0002, "D-228: turn = 0x0002");
    assert_eq!(CLUSTER_MAIN_RIVER & 0xFFFF, 0x0003, "D-228: river = 0x0003");
    assert_eq!(KMEANS_PP_INIT_FLOP & 0xFFFF, 0x0001);
    assert_eq!(KMEANS_PP_INIT_TURN & 0xFFFF, 0x0002);
    assert_eq!(KMEANS_PP_INIT_RIVER & 0xFFFF, 0x0003);
    assert_eq!(EMPTY_CLUSTER_SPLIT_FLOP & 0xFFFF, 0x0001);
    assert_eq!(EMPTY_CLUSTER_SPLIT_TURN & 0xFFFF, 0x0002);
    assert_eq!(EMPTY_CLUSTER_SPLIT_RIVER & 0xFFFF, 0x0003);
    assert_eq!(EHS2_INNER_EQUITY_FLOP & 0xFFFF, 0x0001);
    assert_eq!(EHS2_INNER_EQUITY_TURN & 0xFFFF, 0x0002);
    assert_eq!(EHS2_INNER_EQUITY_RIVER & 0xFFFF, 0x0003);

    // 街无关 op_id 低 16 位 = 0。
    assert_eq!(OCHS_WARMUP & 0xFFFF, 0x0000);
    assert_eq!(EQUITY_MONTE_CARLO & 0xFFFF, 0x0000);
    assert_eq!(OCHS_FEATURE_INNER & 0xFFFF, 0x0000);
}

// ============================================================================
// 2. D-228 op_id 全局唯一（无重复）
// ============================================================================
//
// 15 个命名常量必须互不相等。任一重复触发 sub-stream seed 碰撞，破坏 D-237
// byte-equal 不变量。
#[test]
fn d228_op_id_globally_unique() {
    let all_op_ids: [u32; 15] = [
        OCHS_WARMUP,
        CLUSTER_MAIN_FLOP,
        CLUSTER_MAIN_TURN,
        CLUSTER_MAIN_RIVER,
        KMEANS_PP_INIT_FLOP,
        KMEANS_PP_INIT_TURN,
        KMEANS_PP_INIT_RIVER,
        EMPTY_CLUSTER_SPLIT_FLOP,
        EMPTY_CLUSTER_SPLIT_TURN,
        EMPTY_CLUSTER_SPLIT_RIVER,
        EQUITY_MONTE_CARLO,
        EHS2_INNER_EQUITY_FLOP,
        EHS2_INNER_EQUITY_TURN,
        EHS2_INNER_EQUITY_RIVER,
        OCHS_FEATURE_INNER,
    ];
    for i in 0..all_op_ids.len() {
        for j in (i + 1)..all_op_ids.len() {
            assert_ne!(
                all_op_ids[i], all_op_ids[j],
                "D-228: op_id #{i} ({:#010x}) 与 op_id #{j} ({:#010x}) 重复",
                all_op_ids[i], all_op_ids[j]
            );
        }
    }
}

// ============================================================================
// 3. D-228 derive_substream_seed 公式正确性（B2 stub-driven，#[ignore]）
// ============================================================================
//
// D-228 字面 SplitMix64 finalizer：
//
// ```text
// let tag = ((op_id as u64) << 32) | (sub_index as u64);
// let mut x = master_seed ^ tag;
// x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
// x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
// x ^ (x >> 31)
// ```
//
// 本测试用独立公式实现作为 ground truth，与 `derive_substream_seed` 输出
// byte-equal 比对。
//
// **B1 状态**：A1 阶段 `derive_substream_seed` `unimplemented!()`，本测试
// `#[ignore]`；B2 [实现] 落地 SplitMix64 后取消 ignore。
#[test]
fn d228_derive_substream_seed_splitmix64_byte_equal() {
    fn closed_form_splitmix64(master_seed: u64, op_id: u32, sub_index: u32) -> u64 {
        let tag = ((op_id as u64) << 32) | (sub_index as u64);
        let mut x = master_seed ^ tag;
        x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
        x ^ (x >> 31)
    }

    // 4 个具有代表性的输入组合。
    let cases: [(u64, u32, u32); 4] = [
        (0xA0B1_C2D3_E4F5_0617, EQUITY_MONTE_CARLO, 0),
        (0xDEAD_BEEF_CAFE_BABE, KMEANS_PP_INIT_FLOP, 42),
        (0, OCHS_WARMUP, u32::MAX),
        (u64::MAX, EHS2_INNER_EQUITY_RIVER, 1234),
    ];

    for (master, op_id, sub_index) in cases {
        let expected = closed_form_splitmix64(master, op_id, sub_index);
        let got = derive_substream_seed(master, op_id, sub_index);
        assert_eq!(
            got, expected,
            "D-228 SplitMix64 finalizer：master={master:#018x} op={op_id:#010x} \
             idx={sub_index} → expected {expected:#018x}, got {got:#018x}"
        );
    }
}

// ============================================================================
// 4. D-228 sub-stream seed 区分性（B2-driven，#[ignore]）
// ============================================================================
//
// 不同 (master_seed, op_id, sub_index) 必须派生不同 sub_seed（否则 sub-stream
// 碰撞，破坏 D-237 byte-equal）。本测试验证 32 组随机输入 → 32 个唯一 sub_seed。
#[test]
fn d228_derive_substream_seed_distinctness_smoke() {
    let mut seeds = Vec::with_capacity(32);
    for i in 0..32u32 {
        let s = derive_substream_seed(0xA0B1_C2D3_E4F5_0617, EQUITY_MONTE_CARLO, i);
        seeds.push(s);
    }
    // 32 个 sub_seed 互不相等。
    for i in 0..seeds.len() {
        for j in (i + 1)..seeds.len() {
            assert_ne!(
                seeds[i], seeds[j],
                "D-228 sub-stream seed 区分性：sub_index={i} vs {j} 派生相同 seed {seeds:#?}"
            );
        }
    }
}

// ============================================================================
// 5. Clustering BLAKE3 byte-equal（C2 实测：D-237 byte-equal 不变量）
// ============================================================================
//
// 验证 §B1 line 238 + D-237：同 (BucketConfig, training_seed, cluster_iter) 输入
// 重复 train_in_memory 必须输出 BLAKE3 byte-equal bucket table。
//
// **C-rev1 batch 2 carve-out**：active 路径用 10/10/10 + 50 iter（与本文件
// `BUCKET_BASELINE_CONFIG` 同形态，release ≈ 5 s / debug ≈ 30 s），保证默认
// `cargo test` dev loop 不被阻塞 10 min；50/50/50 + 200 iter 完整版另设
// `_full` 子测试 `#[ignore]`（D1 + CI release 路径触发，与 stage-1 perf_slo /
// fuzz / cross_arch_baseline 同形态）。byte-equal 是二元属性，小配置同样验证
// D-237 不变量。
fn run_clustering_repeat_blake3_byte_equal(cfg: BucketConfig, cluster_iter: u32) {
    let master_seed = 0xC2_BE71_BD75_710E;
    let evaluator: Arc<dyn HandEvaluator> = Arc::new(NaiveHandEvaluator);
    let bt1 = BucketTable::train_in_memory(cfg, master_seed, Arc::clone(&evaluator), cluster_iter);
    let bt2 = BucketTable::train_in_memory(cfg, master_seed, Arc::clone(&evaluator), cluster_iter);
    assert_eq!(
        bt1.content_hash(),
        bt2.content_hash(),
        "D-237 / clustering byte-equal：同 (cfg, seed, iter) 重复训练 BLAKE3 必须相等"
    );
    // 校验 lookup 路径上 1k 输入命中相同 bucket id。
    use poker::rng_substream::{derive_substream_seed, EQUITY_MONTE_CARLO};
    use poker::ChaCha20Rng;
    use poker::RngSource;
    let mut rng = ChaCha20Rng::from_seed(derive_substream_seed(master_seed, EQUITY_MONTE_CARLO, 0));
    for _ in 0..1000 {
        // sample one random (board, hole) on each street and compare bt1.lookup vs bt2.lookup
        for street in [StreetTag::Flop, StreetTag::Turn, StreetTag::River] {
            let board_len = match street {
                StreetTag::Flop => 3,
                StreetTag::Turn => 4,
                StreetTag::River => 5,
                _ => unreachable!(),
            };
            let mut deck: [u8; 52] = [0; 52];
            for (i, slot) in deck.iter_mut().enumerate() {
                *slot = i as u8;
            }
            for i in 0..(board_len + 2) {
                let j = i + (rng.next_u64() % ((52 - i) as u64)) as usize;
                deck.swap(i, j);
            }
            let board: Vec<Card> = (0..board_len)
                .map(|i| Card::from_u8(deck[i]).expect("0..52"))
                .collect();
            let hole = [
                Card::from_u8(deck[board_len]).expect("0..52"),
                Card::from_u8(deck[board_len + 1]).expect("0..52"),
            ];
            let obs_id = canonical_observation_id(street, &board, hole);
            let b1 = bt1.lookup(street, obs_id);
            let b2 = bt2.lookup(street, obs_id);
            assert_eq!(b1, b2, "lookup 应 byte-equal across two trainings");
        }
    }
}

#[test]
fn clustering_repeat_blake3_byte_equal() {
    run_clustering_repeat_blake3_byte_equal(
        BucketConfig {
            flop: 10,
            turn: 10,
            river: 10,
        },
        50,
    );
}

#[test]
#[ignore = "D1: 50/50/50 + 200 iter 完整版（release ~30 s / debug 数分钟）；CI release + --ignored opt-in"]
fn clustering_repeat_blake3_byte_equal_full() {
    run_clustering_repeat_blake3_byte_equal(
        BucketConfig {
            flop: 50,
            turn: 50,
            river: 50,
        },
        200,
    );
}

// ============================================================================
// 6. 跨线程 bucket id 一致（C2 实测：D-238 / IA-004 byte-equal across threads）
// ============================================================================
//
// 验证 §B1 line 239：BucketTable::lookup 是只读纯函数（&self），多线程并发
// 调用必须返回 byte-equal 结果。1M 手 fuzz 留 D1 跑（`--ignored` opt-in）；
// 默认 active 跑 1k 手 4 线程 sanity。
//
// **C-rev1 batch 2 carve-out**：active 路径同 §5 用 10/10/10 + 50 iter（与
// `BUCKET_BASELINE_CONFIG` 同形态，release ≈ 5 s）。完整 50/50/50 + 200 iter
// 版本另设 `_full` 子测试 `#[ignore]`（D1 / CI release opt-in）。lookup 多线程
// safety 与 bucket_count 解耦，小配置同样验证 IA-004 不变量。
fn run_cross_thread_bucket_id_consistency(cfg: BucketConfig, cluster_iter: u32) {
    use std::sync::Arc as ArcStd;
    use std::thread;

    let evaluator: ArcStd<dyn HandEvaluator> = ArcStd::new(NaiveHandEvaluator);
    let bt = ArcStd::new(BucketTable::train_in_memory(
        cfg,
        0xC27B_BD75_710E,
        evaluator,
        cluster_iter,
    ));

    // 生成 1k 个随机 (street, board, hole) 输入。
    use poker::rng_substream::{derive_substream_seed, EQUITY_MONTE_CARLO};
    use poker::ChaCha20Rng;
    use poker::RngSource;
    let mut rng = ChaCha20Rng::from_seed(derive_substream_seed(
        0xC27B_BD75_710E,
        EQUITY_MONTE_CARLO,
        0,
    ));
    let mut inputs: Vec<(StreetTag, Vec<Card>, [Card; 2])> = Vec::with_capacity(1000);
    for _ in 0..1000 {
        let street = match rng.next_u64() % 3 {
            0 => StreetTag::Flop,
            1 => StreetTag::Turn,
            _ => StreetTag::River,
        };
        let board_len = match street {
            StreetTag::Flop => 3,
            StreetTag::Turn => 4,
            StreetTag::River => 5,
            _ => unreachable!(),
        };
        let mut deck: [u8; 52] = [0; 52];
        for (i, slot) in deck.iter_mut().enumerate() {
            *slot = i as u8;
        }
        for i in 0..(board_len + 2) {
            let j = i + (rng.next_u64() % ((52 - i) as u64)) as usize;
            deck.swap(i, j);
        }
        let board: Vec<Card> = (0..board_len)
            .map(|i| Card::from_u8(deck[i]).expect("0..52"))
            .collect();
        let hole = [
            Card::from_u8(deck[board_len]).expect("0..52"),
            Card::from_u8(deck[board_len + 1]).expect("0..52"),
        ];
        inputs.push((street, board, hole));
    }

    // 单线程 baseline。
    let single: Vec<Option<u32>> = inputs
        .iter()
        .map(|(s, board, hole)| {
            let id = canonical_observation_id(*s, board, *hole);
            bt.lookup(*s, id)
        })
        .collect();

    // 4 线程并发对比。
    let inputs_arc = ArcStd::new(inputs);
    let mut handles = Vec::new();
    for _t in 0..4 {
        let bt_c = ArcStd::clone(&bt);
        let inputs_c = ArcStd::clone(&inputs_arc);
        handles.push(thread::spawn(move || {
            inputs_c
                .iter()
                .map(|(s, board, hole)| {
                    let id = canonical_observation_id(*s, board, *hole);
                    bt_c.lookup(*s, id)
                })
                .collect::<Vec<_>>()
        }));
    }
    for h in handles {
        let parallel = h.join().expect("thread joined");
        assert_eq!(
            single, parallel,
            "D-238 / IA-004：跨线程 lookup 必须 byte-equal"
        );
    }
}

#[test]
fn cross_thread_bucket_id_consistency_smoke() {
    run_cross_thread_bucket_id_consistency(
        BucketConfig {
            flop: 10,
            turn: 10,
            river: 10,
        },
        50,
    );
}

#[test]
#[ignore = "D1: 50/50/50 + 200 iter 完整版（release ~30 s / debug 数分钟）；CI release + --ignored opt-in"]
fn cross_thread_bucket_id_consistency_full() {
    run_cross_thread_bucket_id_consistency(
        BucketConfig {
            flop: 50,
            turn: 50,
            river: 50,
        },
        200,
    );
}

// ============================================================================
// 7. 跨架构 32-seed bucket id baseline regression guard 占位（C1 §输出 line 313；
//    C2 / D1 接入完整）
// ============================================================================
//
// 验证 `pluribus_stage2_workflow.md` §C1 §输出 line 313 字面：
//   "跨架构 32-seed bucket id baseline regression guard（与阶段 1
//    `cross_arch_hash` 同形态）"
// + `pluribus_stage2_validation.md` §6 字面：
//   "32-seed bucket id baseline 强制；1M 手 bucket id 跨架构 byte-identical 是
//    aspirational，不是阶段 2 出口门槛"
//
// stage-1 `cross_arch_hash` 模板（参考 `tests/cross_arch_hash.rs::ARCH_BASELINE_SEEDS`）：
//
// 1. 选定 32 个固定 seed（覆盖 0 / 小 / 大 / 边界 / 魔数）。
// 2. 每个 seed → train default 500/500/500 bucket table（C2 `tools/train_bucket_table.rs`
//    CLI）→ 对每条街取若干固定 (board, hole) probe → `BucketTable::lookup` 收到
//    bucket id 序列 → BLAKE3 fold。
// 3. 在 `tests/data/bucket-table-arch-hashes-<os>-<arch>.txt` 维护 baseline 文件：
//    每行 `seed=<dec> hash=<hex>`。
// 4. 跨架构 (linux-x86_64 vs darwin-aarch64) baseline 文件 byte-equal 比对（D-052
//    aspirational 在 32-seed 样本上是强制门槛）。
//
// **C1 状态**：B2 stub `lookup` 全部返回 `Some(0)`、`BucketTable::open` /
// `train_bucket_table.rs` CLI 在 A1 阶段全部 `unimplemented!()`，本测试在
// stub 路径下无法生成有意义的 32-seed bucket id 序列（全 0 → BLAKE3 退化为
// 常量），baseline 文件也无法捕获。`#[ignore]` 留 C2 [实现] / D1 [测试]：
//
// - C2 commit 落地 `train_bucket_table.rs` + `BucketTable::open` 真实 mmap 路径
//   后，把当前 host 的 32-seed 输出 capture 到
//   `tests/data/bucket-table-arch-hashes-linux-x86_64.txt`（与 stage-1
//   `arch-hashes-linux-x86_64.txt` 同目录）；取消本 #[ignore]；同时引入
//   `bucket_table_arch_hash_capture_only` capture-only 入口（与 stage-1
//   `cross_arch_hash_capture_only` 同形态）。
// - D1 [测试] 把跨架构 cross-pair guard（linux ↔ darwin baseline byte-equal）
//   纳入夜间 fuzz（与 stage-1 `cross_arch_baselines_byte_equal_when_both_present`
//   同形态）；详见 §D1 §输出 `tests/clustering_cross_host.rs`。
/// 32 个 baseline seed（与 stage-1 `tests/cross_arch_hash.rs::ARCH_BASELINE_SEEDS`
/// byte-equal 复用，覆盖 0 / 小 / 大 / 边界 / 魔数）。
const BUCKET_TABLE_BASELINE_SEEDS: [u64; 32] = [
    0,
    1,
    2,
    3,
    7,
    13,
    42,
    100,
    255,
    256,
    1023,
    1024,
    65535,
    65536,
    1_000_000,
    0xCAFE_BABE,
    0xDEAD_BEEF,
    0xFEED_FACE,
    0xC1_E1AA,
    0xC1_DA_7A,
    0xC1_F00D,
    0xC001_CAFE,
    0xFFFF_FFFF,
    1u64 << 32,
    1u64 << 48,
    (1u64 << 63) - 1,
    1u64 << 63,
    u64::MAX - 1,
    u64::MAX,
    0xA5A5_A5A5_A5A5_A5A5,
    0x5A5A_5A5A_5A5A_5A5A,
    0x1234_5678_9ABC_DEF0,
];

/// C2 baseline 训练配置：使用 10/10/10 + 50 iter（最小可训练规模，每 seed
/// 总 32 seeds × 3 街 ≈ 1500 candidate 训练；release ~10s/seed = 5 min total）。
/// D1 [测试] 把跨架构 cross-pair guard 引入夜间 fuzz，可使用 50/50/50 + 200 iter
/// 的更精细配置。
const BUCKET_BASELINE_CONFIG: BucketConfig = BucketConfig {
    flop: 10,
    turn: 10,
    river: 10,
};
const BUCKET_BASELINE_CLUSTER_ITER: u32 = 50;

fn capture_bucket_table_baseline() -> String {
    let evaluator: Arc<dyn HandEvaluator> = Arc::new(NaiveHandEvaluator);
    let mut lines = String::new();
    for seed in BUCKET_TABLE_BASELINE_SEEDS {
        let bt = BucketTable::train_in_memory(
            BUCKET_BASELINE_CONFIG,
            seed,
            Arc::clone(&evaluator),
            BUCKET_BASELINE_CLUSTER_ITER,
        );
        let hash = bt.content_hash();
        let hex: String = hash.iter().map(|b| format!("{:02x}", b)).collect();
        lines.push_str(&format!("seed={} hash={}\n", seed, hex));
    }
    lines
}

fn bucket_baseline_path() -> Option<std::path::PathBuf> {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let os = if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        return None;
    };
    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        return None;
    };
    Some(
        std::path::PathBuf::from(manifest)
            .join("tests")
            .join("data")
            .join(format!("bucket-table-arch-hashes-{}-{}.txt", os, arch)),
    )
}

/// C2 §C-rev0 carve-out 落地 + §D-rev0 batch 1 baseline 缺失分支硬 panic：
/// 32-seed bucket table BLAKE3 baseline regression guard（与 stage-1
/// cross_arch_hash 同形态）。
///
/// 默认 `#[ignore]` —— 训练成本 ~74 min release（§C-rev2 batch 6 carve-out
/// 实测 ~107 min 估算下限；32 seed × 3 街 × 10/10/10 × 50 iter × OCHS real
/// 169-class，§C-rev2 §3 真实化 OCHS 后 ~21x slower per ochs），不适合
/// every-`cargo test` 触发。CI 在 release profile + `--ignored` opt-in 跑一次
/// （与 stage-1 §C2 / §D2 同形态）。capture 入口 `bucket_table_arch_hash_capture_only`
/// 同样 `#[ignore]`，由 `scripts/capture-bucket-table-baseline.sh`（占位，D1
/// 落地）调用。
///
/// **§D-rev0 batch 1**：baseline 文件缺失分支从 `eprintln + return` 升级到
/// `panic!`（issue #3 §出口 step 2 字面）。
#[test]
#[ignore = "D1: 32-seed baseline 训练 ~74 min release；release + --ignored opt-in（与 stage-1 cross_arch_hash 同形态）"]
fn cross_arch_bucket_id_baseline() {
    let actual = capture_bucket_table_baseline();
    let path = match bucket_baseline_path() {
        Some(p) => p,
        None => {
            eprintln!(
                "[bucket-table-arch] no baseline declared for this (os, arch); current capture:\n{}",
                actual
            );
            return;
        }
    };
    let expected = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            // §D-rev0 batch 1 / issue #3 §出口 step 2：baseline 缺失从软 skip
            // 升级为硬 panic。capture 输出在 stdout，便于复现指引。stage-1
            // `cross_arch_hash::cross_arch_hash_matches_baseline` 同形态 panic 政策。
            panic!(
                "[bucket-table-arch] baseline missing at {}: {} — run \
                 `cargo test --release --test clustering_determinism \
                 bucket_table_arch_hash_capture_only -- --ignored --nocapture` to regenerate.\n\
                 Current capture:\n{}",
                path.display(),
                e,
                actual
            );
        }
    };
    if actual.trim() != expected.trim() {
        let mut diff = Vec::new();
        for (i, (a, e)) in actual.lines().zip(expected.lines()).enumerate() {
            if a != e {
                diff.push(format!("line {i}: actual={a:?} expected={e:?}"));
                if diff.len() >= 5 {
                    break;
                }
            }
        }
        panic!(
            "bucket-table baseline drift at {}:\n{}\n",
            path.display(),
            diff.join("\n")
        );
    }
}

/// capture-only 入口：开发者跑 `cargo test --release --test clustering_determinism
/// bucket_table_arch_hash_capture_only -- --ignored --nocapture` 把当前 host 输出
/// dump 到 stdout，重定向写入 baseline 文件。
#[test]
#[ignore = "capture-only entry point — dump 32-seed baseline 到 stdout，由 capture script 重定向"]
fn bucket_table_arch_hash_capture_only() {
    let out = capture_bucket_table_baseline();
    print!("{}", out);
}

// ============================================================================
// 占位：sub-module re-export 可访问（编译期检查）
// ============================================================================
//
// 通过 `poker::rng_substream::derive_substream_seed` + 全 15 个 op_id 常量的
// import 编译验证（详见文件顶部 use 语句），D-253-rev1 顶层 re-export 含
// `pub use abstraction::cluster::rng_substream;` 路径暴露。任一漂移立即在
// `cargo test --no-run` 失败。
#[test]
fn rng_substream_module_path_compiles() {
    // 仅做存在性检查（已通过 use 语句 import 暴露）。
    let _: u32 = rng_substream::OCHS_WARMUP;
    let _: fn(u64, u32, u32) -> u64 = rng_substream::derive_substream_seed;
}
