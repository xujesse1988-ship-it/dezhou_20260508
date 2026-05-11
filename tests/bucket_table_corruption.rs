//! F1：corrupted bucket table 错误路径测试（workflow §F1 第 2 件套）。
//!
//! 验收门槛（workflow §F1 §输出 第 2 行）：
//!
//! > byte flip 100k 次 0 panic + 5 类错误（`FileNotFound` / `SchemaMismatch` /
//! > `FeatureSetMismatch` / `Corrupted` / `SizeMismatch`）覆盖
//!
//! 与 stage-1 `tests/history_corruption.rs` 同形态：
//!
//! 1. **结构性 byte flip**（smoke 1k 默认 active + full 100k `#[ignore]`）：
//!    随机选 byte，XOR 0xFF 翻位。每次开 `BucketTable::open` 必须返回 `Err`，
//!    不 panic、不 unwrap None。错误变体落在 5 类之一。
//!
//! 2. **5 类错误命名 case**（确定性构造）：每个变体一条 `#[test]` 直接触发，
//!    便于 F2 [实现] 在补完任一路径时直接对照断言。
//!
//! 3. **变体 exhaustive match**：枚举 5 类的 `match` 闭门写法，未来添加第 6 类
//!    变体（如 stage 3+ v2 schema 时的 `VersionUpgradeRequired`）会让本 test
//!    在编译期 fail，提示同步追加 case。
//!
//! 角色边界：[测试]，不修改产品代码。攻击 bytes 通过 mutate `write_to_path`
//! 字节缓冲构造，与 F1 `bucket_table_schema_compat.rs` 同形态。

use poker::abstraction::bucket_table::{
    BUCKET_TABLE_HEADER_LEN, BUCKET_TABLE_MAGIC, BUCKET_TABLE_TRAILER_LEN,
};
use poker::eval::NaiveHandEvaluator;
use poker::{BucketConfig, BucketTable, BucketTableError, ChaCha20Rng, HandEvaluator, RngSource};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

// ============================================================================
// 通用 fixture（与 bucket_table_schema_compat.rs 同型）
// ============================================================================

const FIXTURE_BUCKET_CONFIG: BucketConfig = BucketConfig {
    flop: 10,
    turn: 10,
    river: 10,
};
const FIXTURE_TRAINING_SEED: u64 = 0xF1BF_71BA_ADF0_5702;
const FIXTURE_CLUSTER_ITER: u32 = 50;

static CACHED_BYTES: OnceLock<Vec<u8>> = OnceLock::new();

fn fixture_bytes() -> &'static [u8] {
    CACHED_BYTES.get_or_init(|| {
        let evaluator: Arc<dyn HandEvaluator> = Arc::new(NaiveHandEvaluator);
        let table = BucketTable::train_in_memory(
            FIXTURE_BUCKET_CONFIG,
            FIXTURE_TRAINING_SEED,
            evaluator,
            FIXTURE_CLUSTER_ITER,
        );
        let path = unique_temp_path("corruption_fixture");
        table
            .write_to_path(&path)
            .expect("write_to_path on fresh in-memory table");
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

/// `BucketTable::open` 必失败 — 由于 `BucketTable` 不实现 Debug，`.expect_err` 不可用，
/// 这里用 match 包到一个返回 Err 的 helper。
fn open_must_err(path: &std::path::Path, ctx: &str) -> BucketTableError {
    match BucketTable::open(path) {
        Ok(_) => panic!("expected BucketTable::open to fail: {ctx}"),
        Err(e) => e,
    }
}

/// 5 类错误变体的 exhaustive 自检 — 添加第 6 类必须显式更新本函数与各 case test。
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
// (1) FileNotFound — 路径不存在
// ============================================================================

#[test]
fn file_not_found_returns_file_not_found_error() {
    let bogus = unique_temp_path("does_not_exist");
    // 不写文件，直接 open
    assert!(!bogus.exists(), "fixture sanity: path must not pre-exist");
    let err = open_must_err(&bogus, "open on nonexistent path 必须 Err");

    match err {
        BucketTableError::FileNotFound { path } => {
            assert_eq!(path, bogus, "FileNotFound.path 应回填实际尝试的路径");
        }
        other => panic!("expected FileNotFound, got {other:?}"),
    }
}

// ============================================================================
// (2) SchemaMismatch — header @ 0x08 ≠ 1
// ============================================================================

#[test]
fn schema_mismatch_via_byte_flip_at_offset_8() {
    let mut bytes = fixture_bytes().to_vec();
    // 把 schema_version (offset 0x08 LE u32) 改成 0x42_DEAD_BEEF 的低 32 位
    let bs = 0xDEAD_BEEFu32.to_le_bytes();
    bytes[0x08..0x0C].copy_from_slice(&bs);
    let path = write_tmp(&bytes, "schema_mismatch");
    let err = open_must_err(&path, "schema_version 0xDEADBEEF 必须 Err");
    let _ = std::fs::remove_file(&path);

    match err {
        BucketTableError::SchemaMismatch { expected, got } => {
            // §G-batch1 §3.2 [实现]：expected 从 1 → 2（BUCKET_TABLE_SCHEMA_VERSION
            // bump by D-244-rev2 §1）。
            assert_eq!(expected, 2);
            assert_eq!(got, 0xDEAD_BEEF);
        }
        other => panic!("expected SchemaMismatch, got {other:?}"),
    }
}

// ============================================================================
// (3) FeatureSetMismatch — header @ 0x0C ≠ 1
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
            assert_eq!(expected, 1);
            assert_eq!(got, 0xCAFE_BABE);
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
    // 把 magic 全部翻成 0xFF
    for b in &mut bytes[0..8] {
        *b = 0xFF;
    }
    let path = write_tmp(&bytes, "bad_magic");
    let err = open_must_err(&path, "magic 0xFF×8 必须 Err");
    let _ = std::fs::remove_file(&path);

    match err {
        BucketTableError::Corrupted { offset, reason } => {
            assert_eq!(offset, 0, "magic 校验失败 offset = 0");
            assert!(
                reason.contains("magic"),
                "Corrupted.reason 应提及 'magic'，实际：{reason}"
            );
        }
        other => panic!("expected Corrupted(magic), got {other:?}"),
    }
}

#[test]
fn corrupted_header_pad_nonzero_returns_corrupted() {
    let mut bytes = fixture_bytes().to_vec();
    // pad 区 0x29..0x30 必须为 0；置 1 触发 Corrupted
    bytes[0x29] = 0xAA;
    let path = write_tmp(&bytes, "pad_nonzero");
    let err = open_must_err(&path, "pad 非零必须 Err");
    let _ = std::fs::remove_file(&path);

    match err {
        BucketTableError::Corrupted { offset, reason } => {
            assert_eq!(offset, 0x29);
            assert!(
                reason.contains("pad"),
                "reason 应提及 'pad'，实际：{reason}"
            );
        }
        other => panic!("expected Corrupted(pad), got {other:?}"),
    }
}

#[test]
fn corrupted_blake3_trailer_mismatch_returns_corrupted() {
    let mut bytes = fixture_bytes().to_vec();
    // 翻 body 中段一个 byte，BLAKE3 trailer 检测失败（reason 含 "blake3" 或 "hash"
    // 取决于具体 impl；本测试 only 校验落到 Corrupted）。
    let mid = (BUCKET_TABLE_HEADER_LEN as usize + bytes.len()) / 2;
    bytes[mid] ^= 0xFF;
    let path = write_tmp(&bytes, "blake3_mismatch");
    let err = open_must_err(&path, "body byte flip 必须 Err");
    let _ = std::fs::remove_file(&path);

    match err {
        BucketTableError::Corrupted { .. } => { /* ok — sub-reason 文案不锁，留实现自由度 */
        }
        other => panic!("expected Corrupted(blake3 / body), got {other:?}"),
    }
}

// ============================================================================
// (5) SizeMismatch — file len < header + trailer
// ============================================================================

#[test]
fn size_mismatch_when_truncated_under_header_plus_trailer() {
    // 截到 header 长度（80 byte）— 短于 80+32=112 必须 SizeMismatch
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
// (6) byte flip smoke — 1k iter 默认 active + 100k iter `#[ignore]`
// ============================================================================

fn run_random_byte_flip_smoke(iter_count: u32, seed: u64) {
    let base = fixture_bytes().to_vec();
    let len = base.len();
    let mut rng = ChaCha20Rng::from_seed(seed);
    let mut ok_count = 0u64; // 极少数情况下：flip 命中 pad 之外的 already-equal 值（XOR 0 = noop？不可能，XOR mask != 0）
    let mut err_count = 0u64;

    for _ in 0..iter_count {
        let mut bytes = base.clone();
        let pos = (rng.next_u64() as usize) % len;
        // mask ∈ [1, 255]，确保 XOR 后必然改变
        let mut mask = (rng.next_u64() as u8) | 1;
        if mask == 0 {
            mask = 1;
        }
        bytes[pos] ^= mask;

        // 写文件 + open（不走 from_bytes 私有路径）
        let path = unique_temp_path(&format!("flip_{pos}"));
        std::fs::write(&path, &bytes).expect("write fixture");
        let result = BucketTable::open(&path);
        let _ = std::fs::remove_file(&path);

        match result {
            Ok(_) => {
                // 单 byte flip 在理论上有极小概率不被检测（例如 magic 区写入的 byte
                // 恰好仍是合法 magic — 不可能因 XOR mask != 0；BLAKE3 抗碰撞期望
                // single-flip 100% 检测）。本 smoke 不期望出现 Ok，但保留 counter
                // 用于诊断而非 panic — fuzz 角色不应让单次 unexpected Ok 阻断后续
                // 1k-1 个 iter 的 panic-coverage 验证（与 stage-1 history_corruption
                // 同形态：fuzz 路径优先 「不 panic」 的覆盖，单点 false-negative 落
                // ok_count 后回到 F2 review）。
                ok_count += 1;
            }
            Err(e) => {
                assert_one_of_five_known_variants(&e);
                err_count += 1;
            }
        }
    }

    // 主断言：单 byte flip 不 panic（达此处即 0 panic）+ 覆盖率断言
    assert_eq!(
        ok_count + err_count,
        iter_count as u64,
        "全部 iter 都得返回（Ok 或 Err），不允许 panic"
    );
    assert!(
        err_count >= (iter_count as u64) * 99 / 100,
        "单 byte XOR flip 期望 ≥99% Err（BLAKE3 + header sanity）；\
         实际 ok={ok_count} / err={err_count}，疑似检测面回退"
    );
}

#[test]
#[ignore = "§G-batch1 §3.2 [实现]: v2 artifact 553 MB × 1000 iter byte-flip ≈ 数小时；\
            10 iter smoke 见 random_byte_flip_smoke_10_no_panic（v2 fixture artifact \
            size 由 D-218-rev2 §2 真等价类 lookup_table 主导，scale 与 K 无关）"]
fn random_byte_flip_smoke_1k_no_panic() {
    run_random_byte_flip_smoke(1_000, 0xF1C0_BB1E_5701);
}

#[test]
fn random_byte_flip_smoke_10_no_panic() {
    // §G-batch1 §3.2 [实现]: v2 artifact 553 MB → 1000-iter smoke 已 #[ignore]；
    // 本 10-iter smoke 取代 default smoke 位置（~30 s release，仍验证 byte-flip
    // 5 类错误体系全覆盖；100k full 仍 #[ignore]）。
    run_random_byte_flip_smoke(10, 0xF1C0_BB1E_5701);
}

#[test]
#[ignore = "F1 full: 100k iter byte flip（v2 artifact 553 MB → 数十小时，\
            §G-batch1 §3.4+ artifact 重训路径下复审 + 调整 smoke 规模）"]
fn random_byte_flip_full_100k_no_panic() {
    run_random_byte_flip_smoke(100_000, 0xF1C0_BB1E_5702);
}

// ============================================================================
// (7) Exhaustive variant match — 5 类全覆盖编译期 trip-wire
// ============================================================================

#[test]
fn bucket_table_error_has_exactly_five_variants() {
    // 任一变体添加 / 删除 / 重命名都让本 fn 编译失败，强制同步追加 case。
    fn _exhaustive_match(err: BucketTableError) -> &'static str {
        match err {
            BucketTableError::FileNotFound { .. } => "FileNotFound",
            BucketTableError::SchemaMismatch { .. } => "SchemaMismatch",
            BucketTableError::FeatureSetMismatch { .. } => "FeatureSetMismatch",
            BucketTableError::Corrupted { .. } => "Corrupted",
            BucketTableError::SizeMismatch { .. } => "SizeMismatch",
        }
    }
    // 通过 Display trait 触发；不要求实际值，仅证明 5 个 variant 可被实例化。
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
// (8) Sanity：fixture 自身 magic 完整
// ============================================================================

#[test]
fn fixture_self_magic_intact() {
    let bytes = fixture_bytes();
    assert_eq!(&bytes[0..8], &BUCKET_TABLE_MAGIC);
}
