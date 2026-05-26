//! F1：bucket table schema 兼容性测试（v4 schema lock-in）。
//!
//! 验收门槛：v4 round-trip 稳定 + (v0 / v1 / v2 / v3 / v5 / u32::MAX) 全部拒绝 +
//! (feature_set_id 0 / 1 / 3) 全部拒绝。
//!
//! 当前 schema = (schema_version=4, feature_set_id=2)，对应 16 维 hist + OCHS feature
//! 集（`docs/bucket_feature_design.md` §2）。v4 与 v3 二进制 layout 相同，仅 lookup
//! 段 canonical id 编号改为 shape-major（`canonical_enum` 2026-05 重写）；v3（旧
//! 编号）/ v2 (9 维 EHS² + OCHS_8) 均已退出，reader 必须 SchemaMismatch 拒绝。

use poker::abstraction::bucket_table::{
    BUCKET_TABLE_DEFAULT_FEATURE_SET_ID, BUCKET_TABLE_FEATURE_SET_2_DIMS, BUCKET_TABLE_HEADER_LEN,
    BUCKET_TABLE_MAGIC, BUCKET_TABLE_SCHEMA_VERSION, BUCKET_TABLE_TRAILER_LEN, PREFLOP_LOOKUP_LEN,
};
use poker::{BucketConfig, BucketTable, BucketTableError};
use std::path::PathBuf;
use std::sync::OnceLock;

// ============================================================================
// 通用 fixture（synthetic_v3_for_tests — 不调 kmeans）
// ============================================================================

const FIXTURE_BUCKET_CONFIG: BucketConfig = BucketConfig {
    flop: 10,
    turn: 10,
    river: 10,
};
const FIXTURE_TRAINING_SEED: u64 = 0xF15C_7EA1_BAAA_5701;

static CACHED_V3_BYTES: OnceLock<Vec<u8>> = OnceLock::new();

fn fixture_v3_bytes() -> &'static [u8] {
    CACHED_V3_BYTES.get_or_init(|| {
        let table =
            BucketTable::synthetic_v3_for_tests(FIXTURE_BUCKET_CONFIG, FIXTURE_TRAINING_SEED);
        let path = unique_temp_path("v3_fixture");
        table
            .write_to_path(&path)
            .expect("write_to_path on fresh synthetic v3 table");
        let bytes = std::fs::read(&path).expect("re-read of written file");
        let _ = std::fs::remove_file(&path);
        bytes
    })
}

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

fn open_must_err(path: &std::path::Path, ctx: &str) -> BucketTableError {
    match BucketTable::open(path) {
        Ok(_) => panic!("expected BucketTable::open to fail: {ctx}"),
        Err(e) => e,
    }
}

/// Mutate `schema_version`（offset 0x08 LE u32）。BLAKE3 trailer 不重算 — schema
/// 检查在 BLAKE3 之前发生，能直接触发 SchemaMismatch。
fn mutate_schema_version(bytes: &mut [u8], new_value: u32) {
    bytes[0x08..0x0C].copy_from_slice(&new_value.to_le_bytes());
}

fn mutate_feature_set_id(bytes: &mut [u8], new_value: u32) {
    bytes[0x0C..0x10].copy_from_slice(&new_value.to_le_bytes());
}

fn write_tmp(bytes: &[u8], label: &str) -> PathBuf {
    let path = unique_temp_path(label);
    std::fs::write(&path, bytes).expect("write tmp fixture");
    path
}

// ============================================================================
// (A) v3 schema 常量锁定
// ============================================================================

#[test]
fn schema_constants_locked_for_v4() {
    assert_eq!(
        BUCKET_TABLE_SCHEMA_VERSION, 4,
        "schema_version 锁定为 4（v4 = v3 layout + shape-major canonical id 编号，\
         `src/abstraction/canonical_enum.rs` 2026-05 重写）"
    );
    assert_eq!(
        BUCKET_TABLE_DEFAULT_FEATURE_SET_ID, 2,
        "feature_set_id 锁定为 2（16 维 hist_8 + OCHS_8 / OCHS_16）"
    );
    assert_eq!(
        BUCKET_TABLE_HEADER_LEN, 0xB8,
        "header 184 bytes（v2 80 字节 + 8 字节 pad + 3 × 32 字节 feature_blake3）"
    );
    assert_eq!(BUCKET_TABLE_TRAILER_LEN, 32, "trailer BLAKE3 32 bytes");
    assert_eq!(
        BUCKET_TABLE_FEATURE_SET_2_DIMS, 16,
        "feature_set_id=2 → 16 dims"
    );
    assert_eq!(PREFLOP_LOOKUP_LEN, 1326, "preflop lookup 1326 hole 组合");
    assert_eq!(&BUCKET_TABLE_MAGIC, b"PLBKT\0\0\0");
}

// ============================================================================
// (B) v3 round-trip 稳定
// ============================================================================

#[test]
fn v3_synthetic_then_open_roundtrip_stable() {
    let bytes = fixture_v3_bytes();
    assert!(
        bytes.len() as u64 >= BUCKET_TABLE_HEADER_LEN + BUCKET_TABLE_TRAILER_LEN,
        "fixture 文件应 ≥ header+trailer"
    );
    let path = write_tmp(bytes, "v3_roundtrip");
    let table = BucketTable::open(&path).expect("v3 round-trip open");
    let _ = std::fs::remove_file(&path);

    assert_eq!(table.schema_version(), 4);
    assert_eq!(table.feature_set_id(), 2);
    assert_eq!(table.training_seed(), FIXTURE_TRAINING_SEED);
    assert_eq!(table.config(), FIXTURE_BUCKET_CONFIG);
    assert_ne!(table.content_hash(), [0u8; 32], "v3 content_hash 应有效");
}

#[test]
fn v3_header_magic_at_offset_zero() {
    let bytes = fixture_v3_bytes();
    assert_eq!(&bytes[0..8], &BUCKET_TABLE_MAGIC);
}

#[test]
fn v3_header_schema_version_at_offset_8() {
    let bytes = fixture_v3_bytes();
    let v = u32::from_le_bytes(bytes[0x08..0x0C].try_into().unwrap());
    assert_eq!(v, BUCKET_TABLE_SCHEMA_VERSION);
}

#[test]
fn v3_header_feature_set_id_at_offset_c() {
    let bytes = fixture_v3_bytes();
    let v = u32::from_le_bytes(bytes[0x0C..0x10].try_into().unwrap());
    assert_eq!(v, BUCKET_TABLE_DEFAULT_FEATURE_SET_ID);
}

#[test]
fn v3_header_n_dims_at_offset_28() {
    let bytes = fixture_v3_bytes();
    assert_eq!(bytes[0x28], BUCKET_TABLE_FEATURE_SET_2_DIMS);
}

#[test]
fn v3_header_pad_0x50_to_0x58_is_zero() {
    let bytes = fixture_v3_bytes();
    for (off, b) in bytes.iter().enumerate().take(0x58).skip(0x50) {
        assert_eq!(*b, 0, "header pad at 0x{off:02X} must be zero");
    }
}

#[test]
fn v3_header_feature_blake3_fields_present() {
    // synthetic_v3_for_tests 写入 deterministic blake3，全部 32 字节非全零。
    let bytes = fixture_v3_bytes();
    for (label, off) in [
        ("feature_flop_blake3", 0x58usize),
        ("feature_turn_blake3", 0x78),
        ("feature_river_blake3", 0x98),
    ] {
        let slice = &bytes[off..off + 32];
        assert!(
            !slice.iter().all(|&b| b == 0),
            "{label} at offset 0x{off:02X} should not be all zeros"
        );
    }
}

// ============================================================================
// (C) schema_version 漂移 → SchemaMismatch
// ============================================================================

fn assert_schema_mismatch(new_version: u32, label: &str) {
    let mut bytes = fixture_v3_bytes().to_vec();
    mutate_schema_version(&mut bytes, new_version);
    let path = write_tmp(&bytes, label);
    let err = open_must_err(&path, &format!("schema_version={new_version} must Err"));
    let _ = std::fs::remove_file(&path);
    match err {
        BucketTableError::SchemaMismatch { expected, got } => {
            assert_eq!(expected, BUCKET_TABLE_SCHEMA_VERSION);
            assert_eq!(got, new_version);
        }
        other => panic!("expected SchemaMismatch, got {other:?}"),
    }
}

#[test]
fn future_v5_schema_version_is_rejected() {
    assert_schema_mismatch(5, "fake_v5");
}

/// v3 = 旧 canonical id 编号方案（2026-05 前），二进制 layout 与 v4 相同但 lookup
/// 行语义不兼容；reader 必须按 schema_version 拒绝，避免静默读错 bucket。
#[test]
fn pre_v4_schema_version_v3_is_rejected() {
    assert_schema_mismatch(3, "fake_v3");
}

#[test]
fn pre_v3_schema_version_v2_is_rejected() {
    assert_schema_mismatch(2, "fake_v2");
}

#[test]
fn pre_v3_schema_version_v1_is_rejected() {
    assert_schema_mismatch(1, "fake_v1");
}

#[test]
fn pre_v3_schema_version_zero_is_rejected() {
    assert_schema_mismatch(0, "fake_v0");
}

#[test]
fn schema_version_u32_max_is_rejected() {
    assert_schema_mismatch(u32::MAX, "fake_max");
}

// ============================================================================
// (C') feature_set_id 漂移 → FeatureSetMismatch
// ============================================================================

fn assert_feature_set_mismatch(new_fsid: u32, label: &str) {
    let mut bytes = fixture_v3_bytes().to_vec();
    mutate_feature_set_id(&mut bytes, new_fsid);
    let path = write_tmp(&bytes, label);
    let err = open_must_err(&path, &format!("feature_set_id={new_fsid} must Err"));
    let _ = std::fs::remove_file(&path);
    match err {
        BucketTableError::FeatureSetMismatch { expected, got } => {
            assert_eq!(expected, BUCKET_TABLE_DEFAULT_FEATURE_SET_ID);
            assert_eq!(got, new_fsid);
        }
        other => panic!("expected FeatureSetMismatch, got {other:?}"),
    }
}

#[test]
fn feature_set_id_0_is_rejected() {
    assert_feature_set_mismatch(0, "fs0");
}

#[test]
fn legacy_feature_set_id_1_9dim_is_rejected() {
    // 9 维 EHS² + OCHS_8 (feature_set_id=1) 已退出。
    assert_feature_set_mismatch(1, "fs1");
}

#[test]
fn future_feature_set_id_3_is_rejected() {
    assert_feature_set_mismatch(3, "fs3");
}
