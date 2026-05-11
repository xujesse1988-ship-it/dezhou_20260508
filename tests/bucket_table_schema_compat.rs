//! F1：bucket table schema 兼容性测试（workflow §F1 第 1 件套）。
//!
//! 验收门槛（workflow §F1 §输出 第 1 行）：
//!
//! > v1 → v2 schema 兼容性（写一个 v1 bucket table，用 v2 代码读取，验证升级
//! > 或拒绝路径）
//!
//! ## 当前实现：v1-only（schema_version = 1）
//!
//! 当前阶段只存在 schema_version = 1（D-240 / `BUCKET_TABLE_SCHEMA_VERSION`）。
//! 「v2 代码读取 v1 文件」 的实际兼容性升级器不在 stage 2 范围内（schema
//! 演化属于 stage 3+ 当真有 v2 时的 D-NNN-revM 工作）。本 F1 文件以三类断言把
//! 「未来 v2 加入」 时必须维持的不变量前置锁死：
//!
//! - **（A）当前 schema_version 常量锁定**：`BUCKET_TABLE_SCHEMA_VERSION = 1`、
//!   `BUCKET_TABLE_DEFAULT_FEATURE_SET_ID = 1`、`BUCKET_TABLE_HEADER_LEN = 80`、
//!   `BUCKET_TABLE_TRAILER_LEN = 32`、`BUCKET_TABLE_FEATURE_SET_1_DIMS = 9`、
//!   `PREFLOP_LOOKUP_LEN = 1326`。任一被静默改动都直接破坏 F1 编译期断言，
//!   提示 stage 3+ schema 演化必须走显式 D-NNN-revM 评审而非数字调整。
//!
//! - **（B）v1 round-trip 稳定**：训练一个 in-memory v1 表 → write_to_path →
//!   open → BLAKE3 trailer 校验通过 + 全部 header 字段同值。这是 「v1 文件
//!   未来被 v2 代码读取」 的最小预备 — open 路径必须在 v1 上稳定，否则 v2
//!   升级器的输入面就漂了。
//!
//! - **（C）伪 v2 / v0 / u32::MAX schema_version 拒绝路径**：把 v1 文件 header
//!   0x08 处的 `schema_version` 字段 mutate 为 2 / 0 / u32::MAX，open 必须返回
//!   `SchemaMismatch { expected: 1, got: X }`（注意 BLAKE3 trailer 一旦改 header
//!   必须重算 — 但 schema 检查在 BLAKE3 之前发生，详见 `bucket_table.rs:294`，
//!   故不重算 trailer 也能触发 SchemaMismatch 而非 Corrupted）。
//!
//! ## F2 视角
//!
//! 如未来 schema_version = 2 加入：
//!
//! 1. 增加 `BUCKET_TABLE_SCHEMA_VERSION_V1: u32 = 1`（保留）+ `_V2: u32 = 2` 常量；
//! 2. open 路径分流：v1 走 「升级到 v2 内存形态」 或显式 `SchemaMismatch` 拒绝；
//! 3. 本文件 (A) 锁定列表追加 v2 常量；(B) v2 train + roundtrip；(C) v3 / 大于
//!    当前最高版本仍拒绝。
//!
//! 角色边界：[测试]，不修改产品代码。攻击 bytes 由直接 mutate write_to_path 输出
//! 的字节缓冲构造，不暴露 `bucket_table.rs` 私有 `mod` 项。

use poker::abstraction::bucket_table::{
    BUCKET_TABLE_DEFAULT_FEATURE_SET_ID, BUCKET_TABLE_FEATURE_SET_1_DIMS, BUCKET_TABLE_HEADER_LEN,
    BUCKET_TABLE_MAGIC, BUCKET_TABLE_SCHEMA_VERSION, BUCKET_TABLE_TRAILER_LEN, PREFLOP_LOOKUP_LEN,
};
use poker::eval::NaiveHandEvaluator;
use poker::{BucketConfig, BucketTable, BucketTableError, HandEvaluator};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

// ============================================================================
// 通用 fixture
// ============================================================================

/// F1 测试 fixture 配置：10/10/10 + cluster_iter=50（与 D1 `tests/abstraction_fuzz.rs`
/// / E1 `benches/baseline.rs::abstraction/bucket_lookup` fixture 同型，~5 s release
/// 训练）。文件大小 ~9 KB，IO 成本可忽略。
const FIXTURE_BUCKET_CONFIG: BucketConfig = BucketConfig {
    flop: 10,
    turn: 10,
    river: 10,
};
const FIXTURE_TRAINING_SEED: u64 = 0xF15C_7EA1_BAAA_5701;
const FIXTURE_CLUSTER_ITER: u32 = 50;

static CACHED_V1_BYTES: OnceLock<Vec<u8>> = OnceLock::new();

/// 训练 + write_to_path + 读回字节，缓存到 OnceLock 让每 #[test] 跳过重训练。
fn fixture_v1_bytes() -> &'static [u8] {
    CACHED_V1_BYTES.get_or_init(|| {
        let evaluator: Arc<dyn HandEvaluator> = Arc::new(NaiveHandEvaluator);
        let table = BucketTable::train_in_memory(
            FIXTURE_BUCKET_CONFIG,
            FIXTURE_TRAINING_SEED,
            evaluator,
            FIXTURE_CLUSTER_ITER,
        );
        let path = unique_temp_path("v1_fixture");
        table
            .write_to_path(&path)
            .expect("write_to_path on fresh in-memory table");
        let bytes = std::fs::read(&path).expect("re-read of written file");
        let _ = std::fs::remove_file(&path);
        bytes
    })
}

/// 当前进程私有 tmp 路径（PID + nanos + label，避免 cargo test 并发碰撞）。
fn unique_temp_path(label: &str) -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let mut p = std::env::temp_dir();
    p.push(format!("poker_f1_{label}_{pid}_{nanos}.bin"));
    p
}

/// `BucketTable::open` 必失败 — 由于 `BucketTable` 不实现 Debug，`.expect_err` 不可用，
/// 这里用 match 包到一个返回 Err 的 helper。
fn open_must_err(path: &std::path::Path, ctx: &str) -> BucketTableError {
    match BucketTable::open(path) {
        Ok(_) => panic!("expected BucketTable::open to fail: {ctx}"),
        Err(e) => e,
    }
}

/// Mutate `schema_version`（offset 0x08 LE u32）到指定值。BLAKE3 trailer 故意
/// **不**重算 — schema_version 检查在 BLAKE3 之前发生（src/abstraction/bucket_table.rs:294
/// vs 425+），故能直接触发 SchemaMismatch。
fn mutate_schema_version(bytes: &mut [u8], new_value: u32) {
    let off = 0x08usize;
    let bs = new_value.to_le_bytes();
    bytes[off..off + 4].copy_from_slice(&bs);
}

/// 同理 mutate `feature_set_id`（offset 0x0C LE u32）。
fn mutate_feature_set_id(bytes: &mut [u8], new_value: u32) {
    let off = 0x0Cusize;
    let bs = new_value.to_le_bytes();
    bytes[off..off + 4].copy_from_slice(&bs);
}

/// 把 mutated bytes 写到 tmp path 并返回，便于走 `BucketTable::open` 路径
/// (vs from_bytes — 后者私有未暴露)。
fn write_tmp(bytes: &[u8], label: &str) -> PathBuf {
    let path = unique_temp_path(label);
    std::fs::write(&path, bytes).expect("write tmp fixture");
    path
}

// ============================================================================
// (A) schema 常量锁定（编译期 + 显式断言）
// ============================================================================

#[test]
fn schema_constants_locked_for_v2() {
    // **§G-batch1 §3.2 [实现]**：BUCKET_TABLE_SCHEMA_VERSION 1 → 2（D-244-rev2 §1
    // mandate）。任一改动 → 本 test fail。stage 4+ 添加 v3 时显式追加 `_V2` / `_V3`
    // 常量，而不是直接改 _SCHEMA_VERSION = 3（会让 v2 文件 100% 拒绝，破坏兼容性）。
    assert_eq!(
        BUCKET_TABLE_SCHEMA_VERSION, 2,
        "schema_version 锁定为 2（§G-batch1 §3.2 / D-244-rev2）；v3 必须走 D-NNN-revM 评审"
    );
    assert_eq!(
        BUCKET_TABLE_DEFAULT_FEATURE_SET_ID, 1,
        "feature_set_id 锁定为 1（EHS² + OCHS N=8 = 9 dims）"
    );
    assert_eq!(BUCKET_TABLE_HEADER_LEN, 80, "header 80 bytes（D-244 §⑨）");
    assert_eq!(BUCKET_TABLE_TRAILER_LEN, 32, "trailer BLAKE3 32 bytes");
    assert_eq!(
        BUCKET_TABLE_FEATURE_SET_1_DIMS, 9,
        "feature_set_id=1 → 9 dims"
    );
    assert_eq!(PREFLOP_LOOKUP_LEN, 1326, "preflop lookup 1326 hole 组合");
    assert_eq!(&BUCKET_TABLE_MAGIC, b"PLBKT\0\0\0");
}

// ============================================================================
// (B) v1 round-trip 稳定
// ============================================================================

#[test]
fn v2_train_then_open_roundtrip_stable() {
    let bytes = fixture_v1_bytes();
    assert!(
        bytes.len() as u64 >= BUCKET_TABLE_HEADER_LEN + BUCKET_TABLE_TRAILER_LEN,
        "fixture 文件应 ≥ header+trailer"
    );
    let path = write_tmp(bytes, "v2_roundtrip");
    let table = BucketTable::open(&path).expect("v2 round-trip open");
    let _ = std::fs::remove_file(&path);

    assert_eq!(table.schema_version(), 2);
    assert_eq!(table.feature_set_id(), 1);
    assert_eq!(table.training_seed(), FIXTURE_TRAINING_SEED);
    assert_eq!(table.config(), FIXTURE_BUCKET_CONFIG);
    // BLAKE3 trailer 由 open() 内部 eager 校验过；至此 32-byte content_hash 非全零。
    assert_ne!(
        table.content_hash(),
        [0u8; 32],
        "v2 round-trip content_hash 应有效"
    );
}

#[test]
fn v1_header_magic_at_offset_zero() {
    let bytes = fixture_v1_bytes();
    assert_eq!(&bytes[0..8], &BUCKET_TABLE_MAGIC);
}

#[test]
fn v1_header_schema_version_at_offset_8() {
    let bytes = fixture_v1_bytes();
    let v = u32::from_le_bytes(bytes[0x08..0x0C].try_into().unwrap());
    assert_eq!(v, BUCKET_TABLE_SCHEMA_VERSION);
}

#[test]
fn v1_header_feature_set_id_at_offset_c() {
    let bytes = fixture_v1_bytes();
    let v = u32::from_le_bytes(bytes[0x0C..0x10].try_into().unwrap());
    assert_eq!(v, BUCKET_TABLE_DEFAULT_FEATURE_SET_ID);
}

// ============================================================================
// (C) 伪 v2 / v0 / u32::MAX schema_version → SchemaMismatch
// ============================================================================

#[test]
fn future_v3_schema_version_is_rejected_with_schema_mismatch() {
    let mut bytes = fixture_v1_bytes().to_vec();
    mutate_schema_version(&mut bytes, 3);
    let path = write_tmp(&bytes, "fake_v3");
    let err = open_must_err(&path, "v3 schema_version 必须被拒");
    let _ = std::fs::remove_file(&path);

    match err {
        BucketTableError::SchemaMismatch { expected, got } => {
            assert_eq!(expected, 2, "expected = 当前 schema_version (§G-batch1 §3.2 bumped)");
            assert_eq!(got, 3, "got = 攻击者注入的 v3");
        }
        other => panic!("expected SchemaMismatch {{ expected=2, got=3 }}, got {other:?}"),
    }
}

#[test]
fn pre_v2_schema_version_v1_is_rejected_with_schema_mismatch() {
    // §G-batch1 §3.2 [实现]：v1 → v2 bump 后旧 v1 文件必须拒绝（D-244-rev2 §2）。
    let mut bytes = fixture_v1_bytes().to_vec();
    mutate_schema_version(&mut bytes, 1);
    let path = write_tmp(&bytes, "fake_v1");
    let err = open_must_err(&path, "v1 (legacy D-218-rev1) 文件必须被拒");
    let _ = std::fs::remove_file(&path);

    match err {
        BucketTableError::SchemaMismatch { expected, got } => {
            assert_eq!(expected, 2);
            assert_eq!(got, 1);
        }
        other => panic!("expected SchemaMismatch, got {other:?}"),
    }
}

#[test]
fn pre_v1_schema_version_zero_is_rejected_with_schema_mismatch() {
    let mut bytes = fixture_v1_bytes().to_vec();
    mutate_schema_version(&mut bytes, 0);
    let path = write_tmp(&bytes, "fake_v0");
    let err = open_must_err(&path, "schema_version = 0 必须被拒");
    let _ = std::fs::remove_file(&path);

    match err {
        BucketTableError::SchemaMismatch { expected, got } => {
            assert_eq!(expected, 2);
            assert_eq!(got, 0);
        }
        other => panic!("expected SchemaMismatch, got {other:?}"),
    }
}

#[test]
fn schema_version_u32_max_is_rejected_with_schema_mismatch() {
    let mut bytes = fixture_v1_bytes().to_vec();
    mutate_schema_version(&mut bytes, u32::MAX);
    let path = write_tmp(&bytes, "fake_max");
    let err = open_must_err(&path, "schema_version = u32::MAX 必须被拒");
    let _ = std::fs::remove_file(&path);

    match err {
        BucketTableError::SchemaMismatch { expected, got } => {
            assert_eq!(expected, 2);
            assert_eq!(got, u32::MAX);
        }
        other => panic!("expected SchemaMismatch, got {other:?}"),
    }
}

// ============================================================================
// (C') feature_set_id 漂移走独立 FeatureSetMismatch（不被 SchemaMismatch 吞噬）
// ============================================================================

#[test]
fn future_feature_set_id_2_is_rejected_with_feature_set_mismatch() {
    let mut bytes = fixture_v1_bytes().to_vec();
    mutate_feature_set_id(&mut bytes, 2);
    let path = write_tmp(&bytes, "fake_fs2");
    let err = open_must_err(&path, "feature_set_id = 2 必须被拒");
    let _ = std::fs::remove_file(&path);

    match err {
        BucketTableError::FeatureSetMismatch { expected, got } => {
            assert_eq!(expected, 1);
            assert_eq!(got, 2);
        }
        other => panic!("expected FeatureSetMismatch, got {other:?}"),
    }
}
