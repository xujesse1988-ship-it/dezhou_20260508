//! 阶段 3 F1 \[测试\]：Checkpoint corruption 完整覆盖（D-350 / D-351 / D-352）。
//!
//! 与 `checkpoint_round_trip.rs`（D1 落地）形成互补：D1 已覆盖 5 类
//! `CheckpointError` 的代表性触发路径 + 1k smoke + 100k byte-flip + variant
//! exhaustive match + header constants lock。F1 \[测试\] 在此之上加宽
//! **边界 / 极端值** 覆盖（继承 stage 2 §F1 `bucket_table_corruption.rs` 模式）：
//!
//! 1. **schema_version 极值** — D1 仅测 `0xDEAD_BEEF`，F1 加 `0` / `u32::MAX` /
//!    `SCHEMA_VERSION + 1`（典型 bump）/ `SCHEMA_VERSION - 1`（下游 reader 旧版本）。
//! 2. **trainer_variant / game_variant 越界 tag** — D1 仅测合法 variant 间互翻
//!    （Kuhn↔Leduc / VanillaCfr↔EsMccfr 触发 `TrainerMismatch`），F1 加 unknown
//!    tag（`3` / `0xFF` for trainer；`4` / `0xFF` for game — stage 4 A1 \[实现\]
//!    扩展 TrainerVariant::EsMccfrLinearRmPlus=2 + GameVariant::Nlhe6Max=3 后
//!    未占用 tag 上移一位），走 `Corrupted` dispatch（checkpoint.rs::parse_bytes
//!    line 215-233 字面）。
//! 3. **bucket_table_blake3 mismatch (Kuhn 路径)** — D1 仅在 NLHE 路径触发
//!    `BucketTableMismatch`（且 release/--ignored 依赖 v3 artifact），F1 加 Kuhn
//!    路径：Kuhn 期望 `bucket_table_blake3 = [0; 32]`（D-356 `Game::bucket_table_blake3`
//!    默认方法），翻成非零应触发 `BucketTableMismatch`，default profile 5 ms 跑完，
//!    无外部依赖。
//! 4. **trailer BLAKE3 直接翻 (vs D1 body 翻)** — D1 走 body byte flip 间接触发
//!    trailer mismatch；F1 直接翻 trailer 32 byte 的随机 byte，验证两条路径都走
//!    `Corrupted`。
//! 5. **100k byte-flip Kuhn fixture release sweep** — D1 已落地（`checkpoint_round_trip.rs`
//!    `random_byte_flip_full_100k_iter_0_panic_all_err`），F1 不重复；本文件聚焦
//!    边界值。
//!
//! **F1 \[测试\] 角色边界**：本文件不修改 `src/training/`；如发现 bug 走 F2 \[实现\]
//! 修复（继承 stage 1 / stage 2 §F-rev1 错误前移模式）。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use poker::training::checkpoint::{Checkpoint, SCHEMA_VERSION};
use poker::training::kuhn::KuhnGame;
use poker::training::{CheckpointError, GameVariant, Trainer, TrainerVariant, VanillaCfrTrainer};
use poker::ChaCha20Rng;

// ===========================================================================
// 共享常量（与 checkpoint_round_trip.rs 同型保持 binary layout 同源）
// ===========================================================================

const FIXED_SEED: u64 = 0x46_31_43_4F_52_52_55_50; // ASCII "F1CORRUP"
const KUHN_FIXTURE_ITERS: u64 = 5;

const OFFSET_SCHEMA_VERSION: usize = 8;
const OFFSET_TRAINER_VARIANT: usize = 12;
const OFFSET_GAME_VARIANT: usize = 13;
const OFFSET_BUCKET_TABLE_BLAKE3: usize = 60;
const HEADER_LEN: usize = 108;
const TRAILER_LEN: usize = 32;

// ===========================================================================
// 共享 fixture：训练 5 iter Kuhn → save → 取出 bytes（与 checkpoint_round_trip
// `kuhn_fixture_bytes` 同型，本文件独立缓存避免跨 crate share state）
// ===========================================================================

static CACHED_FIXTURE: OnceLock<Vec<u8>> = OnceLock::new();

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
    std::fs::write(&path, bytes).expect("write tmp checkpoint");
    path
}

fn cleanup(path: &Path) {
    let _ = std::fs::remove_file(path);
}

fn kuhn_fixture_bytes() -> &'static [u8] {
    CACHED_FIXTURE.get_or_init(|| {
        let mut trainer = VanillaCfrTrainer::new(KuhnGame, FIXED_SEED);
        let mut rng = ChaCha20Rng::from_seed(FIXED_SEED);
        for _ in 0..KUHN_FIXTURE_ITERS {
            trainer.step(&mut rng).expect("kuhn step");
        }
        let path = unique_temp_path("kuhn_corrupt_fixture");
        trainer
            .save_checkpoint(&path)
            .expect("save_checkpoint to produce reference bytes");
        let bytes = std::fs::read(&path).expect("re-read of written checkpoint file");
        cleanup(&path);
        bytes
    })
}

fn assert_one_of_five_known_variants(err: &CheckpointError) {
    match err {
        CheckpointError::FileNotFound { .. }
        | CheckpointError::SchemaMismatch { .. }
        | CheckpointError::TrainerMismatch { .. }
        | CheckpointError::BucketTableMismatch { .. }
        | CheckpointError::Corrupted { .. } => {}
    }
}

fn open_must_err(path: &Path, ctx: &str) -> CheckpointError {
    match Checkpoint::open(path) {
        Ok(_) => panic!("expected Checkpoint::open to fail: {ctx}"),
        Err(e) => {
            assert_one_of_five_known_variants(&e);
            e
        }
    }
}

// ===========================================================================
// 1. schema_version 极值（D-350 / D-351 SchemaMismatch）
// ===========================================================================

fn write_schema(bytes: &mut [u8], val: u32) {
    bytes[OFFSET_SCHEMA_VERSION..OFFSET_SCHEMA_VERSION + 4].copy_from_slice(&val.to_le_bytes());
}

#[test]
fn schema_version_zero_returns_schema_mismatch() {
    let mut bytes = kuhn_fixture_bytes().to_vec();
    write_schema(&mut bytes, 0);
    let path = write_tmp(&bytes, "schema_0");
    let err = open_must_err(&path, "schema_version = 0 必须 Err");
    cleanup(&path);
    match err {
        CheckpointError::SchemaMismatch { expected, got } => {
            assert_eq!(expected, SCHEMA_VERSION);
            assert_eq!(got, 0);
        }
        other => panic!("expected SchemaMismatch (zero), got {other:?}"),
    }
}

#[test]
fn schema_version_u32_max_returns_schema_mismatch() {
    let mut bytes = kuhn_fixture_bytes().to_vec();
    write_schema(&mut bytes, u32::MAX);
    let path = write_tmp(&bytes, "schema_max");
    let err = open_must_err(&path, "schema_version = u32::MAX 必须 Err");
    cleanup(&path);
    match err {
        CheckpointError::SchemaMismatch { expected, got } => {
            assert_eq!(expected, SCHEMA_VERSION);
            assert_eq!(got, u32::MAX);
        }
        other => panic!("expected SchemaMismatch (u32::MAX), got {other:?}"),
    }
}

#[test]
fn schema_version_bump_plus_one_returns_schema_mismatch() {
    let mut bytes = kuhn_fixture_bytes().to_vec();
    write_schema(&mut bytes, SCHEMA_VERSION + 1);
    let path = write_tmp(&bytes, "schema_bumped");
    let err = open_must_err(&path, "schema_version = SCHEMA_VERSION+1 必须 Err");
    cleanup(&path);
    match err {
        CheckpointError::SchemaMismatch { expected, got } => {
            assert_eq!(expected, SCHEMA_VERSION);
            assert_eq!(got, SCHEMA_VERSION + 1);
        }
        other => panic!("expected SchemaMismatch (bumped), got {other:?}"),
    }
}

#[test]
fn schema_version_downgrade_returns_schema_mismatch() {
    // SCHEMA_VERSION 起步 = 1，downgrade 到 0；与 schema_version_zero 等价但
    // 文案显式表达 reader-on-newer-writer-on-older 的语义边界（D-350 schema
    // 字段 monotonically increasing 政策：旧 reader 不读新 writer，新 reader
    // 不读旧 writer）。
    if SCHEMA_VERSION == 0 {
        return; // 不可达；防止 assert_eq! panic 错位。
    }
    let mut bytes = kuhn_fixture_bytes().to_vec();
    write_schema(&mut bytes, SCHEMA_VERSION - 1);
    let path = write_tmp(&bytes, "schema_downgrade");
    let err = open_must_err(&path, "schema_version 下移 必须 Err");
    cleanup(&path);
    match err {
        CheckpointError::SchemaMismatch { expected, got } => {
            assert_eq!(expected, SCHEMA_VERSION);
            assert_eq!(got, SCHEMA_VERSION - 1);
        }
        other => panic!("expected SchemaMismatch (downgrade), got {other:?}"),
    }
}

// ===========================================================================
// 2. trainer_variant / game_variant 未知 tag → Corrupted
// ===========================================================================

#[test]
fn trainer_variant_unknown_tag_3_returns_corrupted() {
    // TrainerVariant 已扩到 0 (VanillaCfr) / 1 (EsMccfr) / 2 (EsMccfrLinearRmPlus
    // stage 4 API-441)；3 是 stage 4 A1 [实现] 后下一个未占用 tag，必走 Corrupted
    // dispatch（checkpoint.rs::parse_bytes 字面）。
    let mut bytes = kuhn_fixture_bytes().to_vec();
    bytes[OFFSET_TRAINER_VARIANT] = 3;
    let path = write_tmp(&bytes, "trainer_tag_3");
    let err = open_must_err(&path, "trainer_variant = 3 必须 Err");
    cleanup(&path);
    match err {
        CheckpointError::Corrupted { offset, reason } => {
            assert_eq!(
                offset, OFFSET_TRAINER_VARIANT as u64,
                "trainer_variant 越界 offset 应指向 byte 12"
            );
            assert!(
                reason.to_lowercase().contains("trainer_variant"),
                "reason 应提及 'trainer_variant'，实际：{reason}"
            );
        }
        other => panic!("expected Corrupted(trainer_variant tag), got {other:?}"),
    }
}

#[test]
fn trainer_variant_unknown_tag_0xff_returns_corrupted() {
    let mut bytes = kuhn_fixture_bytes().to_vec();
    bytes[OFFSET_TRAINER_VARIANT] = 0xFF;
    let path = write_tmp(&bytes, "trainer_tag_ff");
    let err = open_must_err(&path, "trainer_variant = 0xFF 必须 Err");
    cleanup(&path);
    match err {
        CheckpointError::Corrupted { offset, .. } => {
            assert_eq!(offset, OFFSET_TRAINER_VARIANT as u64);
        }
        other => panic!("expected Corrupted(trainer_variant 0xFF), got {other:?}"),
    }
}

#[test]
fn game_variant_unknown_tag_4_returns_corrupted() {
    // GameVariant 已扩到 0 (Kuhn) / 1 (Leduc) / 2 (SimplifiedNlhe) / 3 (Nlhe6Max
    // stage 4 API-411)；4 是 stage 4 A1 [实现] 后下一个未占用 tag，必走 Corrupted。
    let mut bytes = kuhn_fixture_bytes().to_vec();
    bytes[OFFSET_GAME_VARIANT] = 4;
    let path = write_tmp(&bytes, "game_tag_4");
    let err = open_must_err(&path, "game_variant = 4 必须 Err");
    cleanup(&path);
    match err {
        CheckpointError::Corrupted { offset, reason } => {
            assert_eq!(
                offset, OFFSET_GAME_VARIANT as u64,
                "game_variant 越界 offset 应指向 byte 13"
            );
            assert!(
                reason.to_lowercase().contains("game_variant"),
                "reason 应提及 'game_variant'，实际：{reason}"
            );
        }
        other => panic!("expected Corrupted(game_variant tag), got {other:?}"),
    }
}

#[test]
fn game_variant_unknown_tag_0xff_returns_corrupted() {
    let mut bytes = kuhn_fixture_bytes().to_vec();
    bytes[OFFSET_GAME_VARIANT] = 0xFF;
    let path = write_tmp(&bytes, "game_tag_ff");
    let err = open_must_err(&path, "game_variant = 0xFF 必须 Err");
    cleanup(&path);
    match err {
        CheckpointError::Corrupted { offset, .. } => {
            assert_eq!(offset, OFFSET_GAME_VARIANT as u64);
        }
        other => panic!("expected Corrupted(game_variant 0xFF), got {other:?}"),
    }
}

// ===========================================================================
// 3. bucket_table_blake3 mismatch (Kuhn 路径)
// ===========================================================================

#[test]
fn kuhn_bucket_table_blake3_nonzero_returns_bucket_table_mismatch() {
    // KuhnGame::bucket_table_blake3() 默认返回 [0; 32]（D-356 `Game::bucket_table_blake3`
    // default impl）；checkpoint header offset 60..92 在写出时填 0；翻成非零字节
    // 让 `Trainer::load_checkpoint::preflight_trainer` 走 `BucketTableMismatch`
    // dispatch（checkpoint.rs::preflight_trainer line 401-409 字面）。
    //
    // 与 `checkpoint_round_trip.rs::bucket_table_mismatch_via_byte_flip_at_offset_60`
    // 互补：那个 release/--ignored 依赖 v3 artifact，本测试 default profile 5 ms
    // 跑完无外部依赖。
    let mut bytes = kuhn_fixture_bytes().to_vec();
    for b in &mut bytes[OFFSET_BUCKET_TABLE_BLAKE3..OFFSET_BUCKET_TABLE_BLAKE3 + 32] {
        *b = 0xCC;
    }
    let path = write_tmp(&bytes, "kuhn_bucket_blake3");
    let err = match VanillaCfrTrainer::<KuhnGame>::load_checkpoint(&path, KuhnGame) {
        Ok(_) => panic!("Kuhn load_checkpoint with bucket_table_blake3 = 0xCC×32 必须 Err"),
        Err(e) => e,
    };
    cleanup(&path);

    match err {
        CheckpointError::BucketTableMismatch { expected, got } => {
            assert_eq!(
                expected, [0u8; 32],
                "Kuhn expected bucket_table_blake3 = [0; 32]"
            );
            assert_eq!(got, [0xCC; 32], "got 应回填 byte-flip 后的 raw bytes");
        }
        other => panic!("expected BucketTableMismatch (Kuhn), got {other:?}"),
    }
}

// ===========================================================================
// 4. trailer BLAKE3 直接翻（vs D1 body byte flip 间接触发）
// ===========================================================================

#[test]
fn trailer_direct_byte_flip_returns_corrupted() {
    let mut bytes = kuhn_fixture_bytes().to_vec();
    let len = bytes.len();
    assert!(
        len > HEADER_LEN + TRAILER_LEN,
        "Kuhn fixture 应至少包含 header + body + trailer（实际 {len} byte）"
    );
    // 翻 trailer 中段一个 byte（`len - TRAILER_LEN/2`）让 trailer != actual_blake3
    // 且翻位不在 header / body — 与 D1 `body_byte_flip` 互补两条等价错误路径。
    let trailer_mid = len - TRAILER_LEN / 2;
    bytes[trailer_mid] ^= 0xFF;
    let path = write_tmp(&bytes, "trailer_direct_flip");
    let err = open_must_err(&path, "trailer 直接翻位必须 Err");
    cleanup(&path);

    match err {
        CheckpointError::Corrupted { offset, reason } => {
            let r = reason.to_lowercase();
            assert!(
                r.contains("blake3")
                    || r.contains("trailer")
                    || r.contains("hash")
                    || r.contains("body"),
                "Corrupted.reason 应提及 BLAKE3/trailer/hash/body，实际：{reason}"
            );
            // D-352 eager BLAKE3 校验在 trailer_start 偏移返回；offset 应 ≥ HEADER_LEN
            assert!(
                offset >= HEADER_LEN as u64,
                "trailer flip offset {offset} 应 ≥ header_len {HEADER_LEN}"
            );
        }
        other => panic!("expected Corrupted(trailer direct), got {other:?}"),
    }
}

#[test]
fn trailer_truncated_returns_corrupted() {
    // 截断 trailer 一半 byte，让 file size < header + trailer 检测路径触发
    // `Corrupted` "file too short"（checkpoint.rs::parse_bytes line 181-188 字面）
    // 或 trailer BLAKE3 mismatch（取决于 D2 [实现] 校验顺序，落任一已知 5 类即可）。
    let base = kuhn_fixture_bytes();
    let truncated = &base[..base.len() - TRAILER_LEN / 2];
    let path = write_tmp(truncated, "trailer_truncated");
    let err = open_must_err(&path, "trailer 截断必须 Err");
    cleanup(&path);
    assert_one_of_five_known_variants(&err);
}

// ===========================================================================
// 5. CheckpointError 5 variant `from`/`Display`/`Debug` 表面 sanity
// ===========================================================================

#[test]
fn checkpoint_error_display_and_debug_no_panic() {
    let samples: [CheckpointError; 5] = [
        CheckpointError::FileNotFound {
            path: PathBuf::from("/nonexistent/path/to/file.bin"),
        },
        CheckpointError::SchemaMismatch {
            expected: SCHEMA_VERSION,
            got: 0,
        },
        CheckpointError::TrainerMismatch {
            expected: (TrainerVariant::VanillaCfr, GameVariant::Kuhn),
            got: (TrainerVariant::EsMccfr, GameVariant::SimplifiedNlhe),
        },
        CheckpointError::BucketTableMismatch {
            expected: [0u8; 32],
            got: [0xFFu8; 32],
        },
        CheckpointError::Corrupted {
            offset: 0,
            reason: "smoke".to_string(),
        },
    ];
    for s in &samples {
        let display = format!("{s}");
        let debug = format!("{s:?}");
        assert!(
            !display.is_empty(),
            "Display 输出非空 (thiserror Error trait 实现)"
        );
        assert!(!debug.is_empty(), "Debug 输出非空（derive Debug）");
    }
}
