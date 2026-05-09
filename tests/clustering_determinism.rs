//! B1 §D 类：Clustering determinism harness 骨架。
//!
//! 覆盖 `pluribus_stage2_workflow.md` §B1 §输出 D 类清单：
//!
//! - 同 seed clustering 重复 → bucket table 字节比对 stub
//! - 跨线程 bucket id 一致 stub
//! - D-228 RngSource sub-stream 派生协议公开 contract（独立验证）
//!
//! 本文件按 §B1 §出口 line 250 "harness 能跑出占位结果或断言失败，流程不
//! panic"——核心 clustering 重复 / 跨线程断言 `#[ignore]`（依赖 BucketTable
//! 训练 CLI 完整路径，C2 才接），但 D-228 sub-stream 派生协议测试在 stub
//! 落地前无法 byte-equal 比对（`derive_substream_seed` 是 unimplemented），
//! 同样 `#[ignore]`。
//!
//! D-228 op_id 命名常量是 const 路径（A1 已落地具体数值），不依赖 stub；其值
//! 域 / 命名空间断言走非 ignored 路径，验证 D-228 公开 contract 字面值。
//!
//! 全套 1M repeat / BLAKE3 byte-equal 完整断言留 D1。本文件骨架建立 harness
//! 入口与命名空间，B2 / C2 / D1 / D2 [实现] 与 [测试] 在此基础上扩展。
//!
//! 角色边界：本文件属 `[测试]` agent 产物。

use poker::rng_substream::{
    self, derive_substream_seed, CLUSTER_MAIN_FLOP, CLUSTER_MAIN_RIVER, CLUSTER_MAIN_TURN,
    EHS2_INNER_EQUITY_FLOP, EHS2_INNER_EQUITY_RIVER, EHS2_INNER_EQUITY_TURN,
    EMPTY_CLUSTER_SPLIT_FLOP, EMPTY_CLUSTER_SPLIT_RIVER, EMPTY_CLUSTER_SPLIT_TURN,
    EQUITY_MONTE_CARLO, KMEANS_PP_INIT_FLOP, KMEANS_PP_INIT_RIVER, KMEANS_PP_INIT_TURN,
    OCHS_FEATURE_INNER, OCHS_WARMUP,
};

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
#[ignore = "B2: derive_substream_seed unimplemented; 落地后取消 ignore"]
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
#[ignore = "B2: derive_substream_seed unimplemented; 落地后取消 ignore"]
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
// 5. Clustering BLAKE3 byte-equal harness 占位（C2 / D1 接入完整）
// ============================================================================
//
// 验证 §B1 line 238 "同 seed clustering 重复 → bucket table 字节比对 stub"。
// 完整 10 次 clustering 重复需要 `tools/train_bucket_table.rs` CLI（C2 落地）+
// 默认 500/500/500 配置（D-213）+ 同 master_seed 多次跑出 BLAKE3 byte-equal。
//
// **B1 状态**：CLI / mmap 写入路径在 A1 全部 `unimplemented!()`，本骨架仅
// 落地 harness 入口与命名空间；完整断言留 C2/D1。
#[test]
#[ignore = "C2/D1: clustering CLI + bucket_table 写入路径未落地；harness 占位"]
fn clustering_repeat_blake3_byte_equal_skeleton() {
    // C2 [实现] 落地后展开为：
    //
    // ```ignore
    // let cfg = BucketConfig::default_500_500_500();
    // let master_seed = 0xCAFE_BABE;
    // let path1 = tempdir().join("bt1.bin");
    // let path2 = tempdir().join("bt2.bin");
    // run_train_cli(master_seed, cfg, &path1)?;
    // run_train_cli(master_seed, cfg, &path2)?;
    // let bt1 = BucketTable::open(&path1)?;
    // let bt2 = BucketTable::open(&path2)?;
    // assert_eq!(bt1.content_hash(), bt2.content_hash(),
    //   "D-237: 同 seed BLAKE3 byte-equal");
    // ```
    panic!("C2/D1 placeholder：clustering CLI 落地后取消 ignore");
}

// ============================================================================
// 6. 跨线程 bucket id 一致 harness 占位（C2 / D1）
// ============================================================================
//
// 验证 §B1 line 239 "跨线程 bucket id 一致 stub"。完整覆盖：单线程串行
// vs 4 线程并行 vs 8 线程并行 → 同输入下 bucket_id 全部 byte-equal。完整
// 1M 手 / 多线程比对留 D1（与 stage-1 §D1 同形态）。
#[test]
#[ignore = "C2/D1: 多线程 BucketTable lookup 路径未落地；harness 占位"]
fn cross_thread_bucket_id_consistency_skeleton() {
    // C2 / D1 落地后展开为：
    //
    // ```ignore
    // let bt = BucketTable::open(/* artifact */)?;
    // let inputs: Vec<(StreetTag, u32)> = generate_random_observations(1_000_000);
    // let single = inputs.iter().map(|(s, id)| bt.lookup(*s, *id)).collect::<Vec<_>>();
    // let multi = parallel_map_4_threads(&inputs, |x| bt.lookup(x.0, x.1));
    // assert_eq!(single, multi, "D-238 / IA-004: 跨线程 byte-equal");
    // ```
    panic!("C2/D1 placeholder：多线程 lookup harness 落地后取消 ignore");
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
