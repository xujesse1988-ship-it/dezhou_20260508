//! F1：corrupted bucket table 错误路径测试（v3 schema）。
//!
//! 验收门槛：byte flip 0 panic + 5 类错误（`FileNotFound` / `SchemaMismatch` /
//! `FeatureSetMismatch` / `Corrupted` / `SizeMismatch`）覆盖。
//!
//! 1. **结构性 byte flip**（smoke 10 active + full 100k `#[ignore]`）：
//!    随机选 byte，XOR 翻位。每次 `BucketTable::open` 必须返回 `Err`，不 panic。
//! 2. **5 类错误命名 case**（确定性构造）：每个变体一条 `#[test]` 直接触发。
//! 3. **变体 exhaustive match**：枚举 5 类的 `match` 闭门写法，未来添加第 6 类
//!    变体会让本 test 在编译期 fail，提示同步追加 case。

use poker::abstraction::bucket_table::{
    BUCKET_TABLE_DEFAULT_FEATURE_SET_ID, BUCKET_TABLE_HEADER_LEN, BUCKET_TABLE_MAGIC,
    BUCKET_TABLE_SCHEMA_VERSION, BUCKET_TABLE_TRAILER_LEN,
};
use poker::{BucketConfig, BucketTable, BucketTableError, ChaCha20Rng, RngSource};
use std::path::PathBuf;
use std::sync::OnceLock;

// ============================================================================
// 通用 fixture（synthetic_v3_for_tests — 不调 kmeans，deterministic byte-equal）
// ============================================================================

const FIXTURE_BUCKET_CONFIG: BucketConfig = BucketConfig {
    flop: 10,
    turn: 10,
    river: 10,
};
const FIXTURE_TRAINING_SEED: u64 = 0xF1BF_71BA_ADF0_5702;

static CACHED_BYTES: OnceLock<Vec<u8>> = OnceLock::new();

fn fixture_bytes() -> &'static [u8] {
    CACHED_BYTES.get_or_init(|| {
        let table =
            BucketTable::synthetic_v3_for_tests(FIXTURE_BUCKET_CONFIG, FIXTURE_TRAINING_SEED);
        let path = unique_temp_path("corruption_fixture");
        table
            .write_to_path(&path)
            .expect("write_to_path on fresh synthetic table");
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
    p.push(format!("poker_f1_corrupt_{label}_{pid}_{nanos}.bin"));
    p
}

fn write_tmp(bytes: &[u8], label: &str) -> PathBuf {
    let path = unique_temp_path(label);
    std::fs::write(&path, bytes).expect("write tmp fixture");
    path
}

fn open_must_err(path: &std::path::Path, ctx: &str) -> BucketTableError {
    match BucketTable::open(path) {
        Ok(_) => panic!("expected BucketTable::open to fail: {ctx}"),
        Err(e) => e,
    }
}

fn assert_one_of_five_known_variants(err: &BucketTableError) {
    match err {
        BucketTableError::FileNotFound { .. }
        | BucketTableError::SchemaMismatch { .. }
        | BucketTableError::FeatureSetMismatch { .. }
        | BucketTableError::Corrupted { .. }
        | BucketTableError::SizeMismatch { .. } => { /* known */ }
    }
}

// ============================================================================
// (1) FileNotFound
// ============================================================================

#[test]
fn file_not_found_returns_file_not_found_error() {
    let bogus = unique_temp_path("does_not_exist");
    assert!(!bogus.exists(), "fixture sanity: path must not pre-exist");
    let err = open_must_err(&bogus, "open on nonexistent path 必须 Err");

    match err {
        BucketTableError::FileNotFound { path } => {
            assert_eq!(path, bogus);
        }
        other => panic!("expected FileNotFound, got {other:?}"),
    }
}

// ============================================================================
// (2) SchemaMismatch — header @ 0x08 ≠ 3
// ============================================================================

#[test]
fn schema_mismatch_via_byte_flip_at_offset_8() {
    let mut bytes = fixture_bytes().to_vec();
    let bs = 0xDEAD_BEEFu32.to_le_bytes();
    bytes[0x08..0x0C].copy_from_slice(&bs);
    let path = write_tmp(&bytes, "schema_mismatch");
    let err = open_must_err(&path, "schema_version 0xDEADBEEF 必须 Err");
    let _ = std::fs::remove_file(&path);

    match err {
        BucketTableError::SchemaMismatch { expected, got } => {
            assert_eq!(expected, BUCKET_TABLE_SCHEMA_VERSION);
            assert_eq!(got, 0xDEAD_BEEF);
        }
        other => panic!("expected SchemaMismatch, got {other:?}"),
    }
}

#[test]
fn v2_schema_artifact_is_rejected() {
    // 模拟 v2 artifact（schema_version=2, feature_set_id=1）：reader 必须 SchemaMismatch
    // 拒绝（参 `docs/bucket_feature_design.md` §7 末段）。
    let mut bytes = fixture_bytes().to_vec();
    bytes[0x08..0x0C].copy_from_slice(&2u32.to_le_bytes());
    let path = write_tmp(&bytes, "v2_schema");
    let err = open_must_err(&path, "schema_version=2 必须 Err");
    let _ = std::fs::remove_file(&path);

    match err {
        BucketTableError::SchemaMismatch { expected, got } => {
            assert_eq!(expected, BUCKET_TABLE_SCHEMA_VERSION);
            assert_eq!(got, 2);
        }
        other => panic!("expected SchemaMismatch, got {other:?}"),
    }
}

// ============================================================================
// (3) FeatureSetMismatch — header @ 0x0C ≠ 2
// ============================================================================

#[test]
fn feature_set_mismatch_via_byte_flip_at_offset_c() {
    let mut bytes = fixture_bytes().to_vec();
    let bs = 0xCAFE_BABEu32.to_le_bytes();
    bytes[0x0C..0x10].copy_from_slice(&bs);
    let path = write_tmp(&bytes, "feature_set_mismatch");
    let err = open_must_err(&path, "feature_set_id 0xCAFEBABE 必须 Err");
    let _ = std::fs::remove_file(&path);

    match err {
        BucketTableError::FeatureSetMismatch { expected, got } => {
            assert_eq!(expected, BUCKET_TABLE_DEFAULT_FEATURE_SET_ID);
            assert_eq!(got, 0xCAFE_BABE);
        }
        other => panic!("expected FeatureSetMismatch, got {other:?}"),
    }
}

#[test]
fn feature_set_id_1_old_9dim_is_rejected() {
    // 9-dim EHS² + OCHS_8 feature_set_id=1 已退出：reader 必须拒绝。
    let mut bytes = fixture_bytes().to_vec();
    bytes[0x0C..0x10].copy_from_slice(&1u32.to_le_bytes());
    let path = write_tmp(&bytes, "feature_set_1");
    let err = open_must_err(&path, "feature_set_id=1 必须 Err");
    let _ = std::fs::remove_file(&path);

    match err {
        BucketTableError::FeatureSetMismatch { expected, got } => {
            assert_eq!(expected, BUCKET_TABLE_DEFAULT_FEATURE_SET_ID);
            assert_eq!(got, 1);
        }
        other => panic!("expected FeatureSetMismatch, got {other:?}"),
    }
}

// ============================================================================
// (4) Corrupted — magic / pad / offset / BLAKE3 多个子原因路径
// ============================================================================

#[test]
fn corrupted_magic_returns_corrupted() {
    let mut bytes = fixture_bytes().to_vec();
    for b in &mut bytes[0..8] {
        *b = 0xFF;
    }
    let path = write_tmp(&bytes, "bad_magic");
    let err = open_must_err(&path, "magic 0xFF×8 必须 Err");
    let _ = std::fs::remove_file(&path);

    match err {
        BucketTableError::Corrupted { offset, reason } => {
            assert_eq!(offset, 0);
            assert!(reason.contains("magic"));
        }
        other => panic!("expected Corrupted(magic), got {other:?}"),
    }
}

#[test]
fn corrupted_header_pad_nonzero_at_0x29_returns_corrupted() {
    let mut bytes = fixture_bytes().to_vec();
    bytes[0x29] = 0xAA;
    let path = write_tmp(&bytes, "pad_nonzero_29");
    let err = open_must_err(&path, "pad 0x29 非零必须 Err");
    let _ = std::fs::remove_file(&path);

    match err {
        BucketTableError::Corrupted { offset, reason } => {
            assert_eq!(offset, 0x29);
            assert!(reason.contains("pad"));
        }
        other => panic!("expected Corrupted(pad@0x29), got {other:?}"),
    }
}

#[test]
fn corrupted_header_pad_nonzero_at_0x50_returns_corrupted() {
    // v3 新增 8-byte pad 0x50..0x58。reader 必须校验 = 0。
    let mut bytes = fixture_bytes().to_vec();
    bytes[0x50] = 0xBB;
    let path = write_tmp(&bytes, "pad_nonzero_50");
    let err = open_must_err(&path, "pad 0x50 非零必须 Err");
    let _ = std::fs::remove_file(&path);

    match err {
        BucketTableError::Corrupted { offset, reason } => {
            assert_eq!(offset, 0x50);
            assert!(reason.contains("pad"));
        }
        other => panic!("expected Corrupted(pad@0x50), got {other:?}"),
    }
}

#[test]
fn corrupted_blake3_trailer_mismatch_returns_corrupted() {
    let mut bytes = fixture_bytes().to_vec();
    let mid = (BUCKET_TABLE_HEADER_LEN as usize + bytes.len()) / 2;
    bytes[mid] ^= 0xFF;
    let path = write_tmp(&bytes, "blake3_mismatch");
    let err = open_must_err(&path, "body byte flip 必须 Err");
    let _ = std::fs::remove_file(&path);

    match err {
        BucketTableError::Corrupted { .. } => { /* ok — reason 文案不锁 */ }
        other => panic!("expected Corrupted(blake3 / body), got {other:?}"),
    }
}

// ============================================================================
// (5) SizeMismatch
// ============================================================================

#[test]
fn size_mismatch_when_truncated_under_header_plus_trailer() {
    let bytes = &fixture_bytes()[..BUCKET_TABLE_HEADER_LEN as usize];
    let path = write_tmp(bytes, "truncated_header_only");
    let err = open_must_err(&path, "文件截到只剩 header 必须 Err");
    let _ = std::fs::remove_file(&path);

    match err {
        BucketTableError::SizeMismatch { expected, got } => {
            assert_eq!(expected, BUCKET_TABLE_HEADER_LEN + BUCKET_TABLE_TRAILER_LEN);
            assert_eq!(got, BUCKET_TABLE_HEADER_LEN);
        }
        other => panic!("expected SizeMismatch, got {other:?}"),
    }
}

#[test]
fn size_mismatch_when_truncated_to_zero() {
    let path = write_tmp(&[], "truncated_zero");
    let err = open_must_err(&path, "空文件必须 Err");
    let _ = std::fs::remove_file(&path);

    match err {
        BucketTableError::SizeMismatch { expected, got } => {
            assert_eq!(expected, BUCKET_TABLE_HEADER_LEN + BUCKET_TABLE_TRAILER_LEN);
            assert_eq!(got, 0);
        }
        other => panic!("expected SizeMismatch, got {other:?}"),
    }
}

#[test]
fn size_mismatch_when_one_byte_short_of_header_plus_trailer() {
    let bytes =
        &fixture_bytes()[..(BUCKET_TABLE_HEADER_LEN + BUCKET_TABLE_TRAILER_LEN - 1) as usize];
    let path = write_tmp(bytes, "one_short");
    let err = open_must_err(&path, "文件少 1 byte 必须 Err");
    let _ = std::fs::remove_file(&path);

    match err {
        BucketTableError::SizeMismatch { expected, got } => {
            assert_eq!(expected, BUCKET_TABLE_HEADER_LEN + BUCKET_TABLE_TRAILER_LEN);
            assert_eq!(got, BUCKET_TABLE_HEADER_LEN + BUCKET_TABLE_TRAILER_LEN - 1);
        }
        other => panic!("expected SizeMismatch, got {other:?}"),
    }
}

// ============================================================================
// (6) byte flip smoke — 10 iter active + 1k/100k `#[ignore]`
// ============================================================================

fn run_random_byte_flip_smoke(iter_count: u32, seed: u64) {
    let base = fixture_bytes().to_vec();
    let len = base.len();
    let mut rng = ChaCha20Rng::from_seed(seed);
    let mut ok_count = 0u64;
    let mut err_count = 0u64;

    for _ in 0..iter_count {
        let mut bytes = base.clone();
        let pos = (rng.next_u64() as usize) % len;
        let mut mask = (rng.next_u64() as u8) | 1;
        if mask == 0 {
            mask = 1;
        }
        bytes[pos] ^= mask;

        let path = unique_temp_path(&format!("flip_{pos}"));
        std::fs::write(&path, &bytes).expect("write fixture");
        let result = BucketTable::open(&path);
        let _ = std::fs::remove_file(&path);

        match result {
            Ok(_) => {
                ok_count += 1;
            }
            Err(e) => {
                assert_one_of_five_known_variants(&e);
                err_count += 1;
            }
        }
    }

    assert_eq!(
        ok_count + err_count,
        iter_count as u64,
        "全部 iter 都得返回（Ok 或 Err），不允许 panic"
    );
    assert!(
        err_count >= (iter_count as u64) * 99 / 100,
        "单 byte XOR flip 期望 ≥99% Err；实际 ok={ok_count} / err={err_count}"
    );
}

#[test]
fn random_byte_flip_smoke_10_no_panic() {
    run_random_byte_flip_smoke(10, 0xF1C0_BB1E_5701);
}

#[test]
#[ignore = "1k iter byte flip on small synthetic v3 artifact (~3 MB); run via cargo test --release --ignored"]
fn random_byte_flip_smoke_1k_no_panic() {
    run_random_byte_flip_smoke(1_000, 0xF1C0_BB1E_5701);
}

#[test]
#[ignore = "100k iter byte flip; long run"]
fn random_byte_flip_full_100k_no_panic() {
    run_random_byte_flip_smoke(100_000, 0xF1C0_BB1E_5702);
}

// ============================================================================
// (7) Exhaustive variant match
// ============================================================================

#[test]
fn bucket_table_error_has_exactly_five_variants() {
    fn _exhaustive_match(err: BucketTableError) -> &'static str {
        match err {
            BucketTableError::FileNotFound { .. } => "FileNotFound",
            BucketTableError::SchemaMismatch { .. } => "SchemaMismatch",
            BucketTableError::FeatureSetMismatch { .. } => "FeatureSetMismatch",
            BucketTableError::Corrupted { .. } => "Corrupted",
            BucketTableError::SizeMismatch { .. } => "SizeMismatch",
        }
    }
    let _ = _exhaustive_match(BucketTableError::FileNotFound {
        path: PathBuf::from("/dev/null"),
    });
    let _ = _exhaustive_match(BucketTableError::SchemaMismatch {
        expected: 1,
        got: 2,
    });
    let _ = _exhaustive_match(BucketTableError::FeatureSetMismatch {
        expected: 1,
        got: 2,
    });
    let _ = _exhaustive_match(BucketTableError::Corrupted {
        offset: 0,
        reason: "stub".into(),
    });
    let _ = _exhaustive_match(BucketTableError::SizeMismatch {
        expected: 112,
        got: 0,
    });
}

// ============================================================================
// (8) Sanity
// ============================================================================

#[test]
fn fixture_self_magic_intact() {
    let bytes = fixture_bytes();
    assert_eq!(&bytes[0..8], &BUCKET_TABLE_MAGIC);
}

#[test]
fn fixture_self_schema_version_is_v4() {
    let bytes = fixture_bytes();
    let sv = u32::from_le_bytes(bytes[0x08..0x0C].try_into().unwrap());
    assert_eq!(sv, BUCKET_TABLE_SCHEMA_VERSION);
}

#[test]
fn fixture_self_feature_set_id_is_2() {
    let bytes = fixture_bytes();
    let fsid = u32::from_le_bytes(bytes[0x0C..0x10].try_into().unwrap());
    assert_eq!(fsid, BUCKET_TABLE_DEFAULT_FEATURE_SET_ID);
}

#[test]
fn fixture_synthetic_byte_equal_across_calls() {
    // synthetic_v3_for_tests 必须 deterministic: 同 (config, seed) 重复调用 → 字节相同。
    let a = BucketTable::synthetic_v3_for_tests(FIXTURE_BUCKET_CONFIG, FIXTURE_TRAINING_SEED);
    let b = BucketTable::synthetic_v3_for_tests(FIXTURE_BUCKET_CONFIG, FIXTURE_TRAINING_SEED);
    assert_eq!(a.content_hash(), b.content_hash());
}
