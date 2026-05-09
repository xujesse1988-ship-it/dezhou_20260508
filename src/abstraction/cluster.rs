//! k-means / EMD 聚类 harness（D-230..D-238）。
//!
//! 模块私有 surface（D-254 内部子模块隔离，不在 `lib.rs` 顶层 re-export），
//! 仅由 `tools/train_bucket_table.rs` CLI 引用。允许使用浮点（D-273）。
//!
//! **例外**：[`rng_substream`] 子模块作为 D-228 公开 contract 在 `lib.rs` 顶层
//! re-export，便于 `tests/clustering_determinism.rs` 等 \[测试\] 独立构造
//! sub-stream 验证 byte-equal。
//!
//! A1 阶段聚类内核保持空骨架，B2/C2 \[实现\] 填充 k-means++ 初始化 / EMD 距离 /
//! 空 cluster 切分 / centroid 重编号等实现。

pub mod rng_substream {
    //! D-228 RngSource sub-stream 派生协议（公开 contract）。
    //!
    //! `derive_substream_seed(master_seed, op_id, sub_index) -> u64` 走 SplitMix64
    //! finalizer：
    //!
    //! ```text
    //! let tag = ((op_id as u64) << 32) | (sub_index as u64);
    //! let mut x = master_seed ^ tag;
    //! x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    //! x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    //! x ^ (x >> 31)
    //! ```
    //!
    //! op_id 高 16 位 = 类别，低 16 位 = 街 / 子操作；新增 op_id 必须走 D-228-revM
    //! 流程并 bump `BucketTable.schema_version`（违反会让相同 `(training_seed,
    //! BucketConfig)` 输出不同 BLAKE3 trailer，破坏 D-237 byte-equal 不变量）。
    //!
    //! sub_seed 的标准用法：`ChaCha20Rng::from_seed(sub_seed)`（继承 stage 1
    //! D-028 RNG 实例化），不允许直接 `next_u64()` master 后用其 raw bits 当
    //! sub_seed。

    /// SplitMix64 finalizer-based sub-stream seed derivation（D-228）。
    ///
    /// 入参：`master_seed`（caller 持有的 training-time 主 seed）+
    /// `op_id`（本表内常量之一）+ `sub_index`（caller 在 op_id 命名空间内的
    /// 线性整数：iter / outer-enum-index / split-attempt-index）。
    ///
    /// 输出：64-bit 派生 seed，可直接喂给 `ChaCha20Rng::from_seed`。
    pub fn derive_substream_seed(_master_seed: u64, _op_id: u32, _sub_index: u32) -> u64 {
        unimplemented!("A1 stub; B2 implements per D-228 SplitMix64 finalizer protocol")
    }

    // ===========================================================================
    // op_id 表（D-228）。任何修改必须走 D-228-revM 并 bump
    // BucketTable.schema_version。
    // ===========================================================================

    /// OCHS opponent cluster 暖启动（D-228）。
    pub const OCHS_WARMUP: u32 = 0x0001_0000;

    /// k-means 主聚类 fork（D-228）；街区分以低 16 位标记。
    pub const CLUSTER_MAIN_FLOP: u32 = 0x0002_0001;
    pub const CLUSTER_MAIN_TURN: u32 = 0x0002_0002;
    pub const CLUSTER_MAIN_RIVER: u32 = 0x0002_0003;

    /// k-means++ 初始化采样（D-228）。
    pub const KMEANS_PP_INIT_FLOP: u32 = 0x0003_0001;
    pub const KMEANS_PP_INIT_TURN: u32 = 0x0003_0002;
    pub const KMEANS_PP_INIT_RIVER: u32 = 0x0003_0003;

    /// 空 cluster 切分回退采样（D-236）。
    pub const EMPTY_CLUSTER_SPLIT_FLOP: u32 = 0x0004_0001;
    pub const EMPTY_CLUSTER_SPLIT_TURN: u32 = 0x0004_0002;
    pub const EMPTY_CLUSTER_SPLIT_RIVER: u32 = 0x0004_0003;

    /// `EquityCalculator::equity_vs_hand` preflop Monte Carlo（D-220 / D-220a-rev1）。
    pub const EQUITY_MONTE_CARLO: u32 = 0x0005_0000;

    /// EHS² inner equity Monte Carlo（D-227）；街区分以低 16 位标记。
    pub const EHS2_INNER_EQUITY_FLOP: u32 = 0x0006_0001;
    pub const EHS2_INNER_EQUITY_TURN: u32 = 0x0006_0002;
    pub const EHS2_INNER_EQUITY_RIVER: u32 = 0x0006_0003;

    /// OCHS feature 计算的 inner equity 采样（D-222 / D-228）。
    pub const OCHS_FEATURE_INNER: u32 = 0x0007_0000;
}
