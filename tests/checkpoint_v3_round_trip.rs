//! 阶段 5 B1 \[测试\] — Checkpoint v3 round-trip integration crate（API-596 /
//! D-549 / D-563 字面）。
//!
//! ## 角色边界
//!
//! 本文件属 `[测试]` agent；B1 [测试] 0 改动产品代码。D2 \[实现\] 落地后
//! `Checkpoint::open` 三路径 dispatch + body BLAKE3 self-consistency 转绿。
//!
//! ## D-549 字面 — schema_version 2 → 3 翻面
//!
//! - `HEADER_LEN_V3 = 192` byte（24 byte 新增 + 40 byte 对齐 pad，64 byte
//!   boundary aligned 3 cache line）。
//! - **8 新 header field**（API-551 字面）：trainer_variant / info_set_id_layout_version
//!   / quant_bits / capacity_estimate / pruning_config_* × 4 / resurface_pass_id /
//!   naive_baseline_blake3 / body_blake3。
//! - **6 × 2 sub-region body encoding**：6 traverser × (RegretTable +
//!   StrategyAccumulator) = 12 sub-region；每 region bincode + zstd level=3
//!   compress + 4-byte magic `0xDEADBEEF` 分隔。
//! - **body BLAKE3 self-consistency**（D-563 字面）：写入 header `body_blake3`
//!   字段；load 时校验 mismatch → `CheckpointError::BodyHashMismatch`。
//!
//! ## D-563 字面 — anchor #4 round-trip BLAKE3 self-consistency
//!
//! 同 binary build 写 + 读 + 重写 byte-equal（schema=3 路径内部自洽）。stage 5
//! D-549 schema 2 → 3 翻面**不**要求跨 binary version byte-equal，但同 binary
//! self-consistency 必须保留。
//!
//! ## 测试覆盖
//!
//! 1. `schema_version_v3_constant_is_3` — active sanity（A1 stub 即生效）。
//! 2. `header_len_v3_constant_is_192` — active sanity。
//! 3. `stage3_header_len_constant_is_108` — active sanity。
//! 4. `stage4_header_len_constant_is_128` — active sanity。
//! 5. `checkpoint_header_v3_zero_with_lock_field_literal_values` — active
//!    sanity（A1 stub `CheckpointHeaderV3::zero_with_lock` 已落地真实值）。
//! 6. `ensure_trainer_schema_returns_ok_when_version_matches` — active sanity
//!    （A1 stub 已落地 pure logic）。
//! 7. `ensure_trainer_schema_returns_schema_mismatch_on_unexpected_version` —
//!    active sanity。
//! 8. `trainer_variant_es_mccfr_linear_rm_plus_compact_expected_schema_3` —
//!    active sanity。
//! 9. `checkpoint_v3_save_then_open_byte_equal` — `#[ignore]`，D2 [实现] 落地后转 pass。
//! 10. `checkpoint_v3_dispatch_distinct_from_v1_v2` — `#[ignore]`，D2 [实现] 落地后转 pass。
//! 11. `checkpoint_v3_body_blake3_self_consistency` — `#[ignore]`，D2 [实现] 落地后转 pass。

use poker::error::{CheckpointError, TrainerVariant};
use poker::training::checkpoint::{
    ensure_trainer_schema, CheckpointHeaderV3, HEADER_LEN_V3, MAGIC, SCHEMA_VERSION_V3,
    STAGE3_HEADER_LEN, STAGE4_HEADER_LEN,
};

// ---------------------------------------------------------------------------
// Group A — 常量字面 lock（active；A1 scaffold 已落地）
// ---------------------------------------------------------------------------

/// API-550 字面 — `SCHEMA_VERSION_V3 = 3`。
#[test]
fn schema_version_v3_constant_is_3() {
    assert_eq!(SCHEMA_VERSION_V3, 3, "D-549 字面 schema_version 2 → 3");
}

/// API-550 字面 — `HEADER_LEN_V3 = 192`。
#[test]
fn header_len_v3_constant_is_192() {
    assert_eq!(
        HEADER_LEN_V3, 192,
        "D-549 字面 HEADER_LEN bump 128 → 192（8 新字段 + 对齐 pad）"
    );
}

/// API-550 字面 — `STAGE3_HEADER_LEN = 108`（stage 3 v1 header；继承 alias）。
#[test]
fn stage3_header_len_constant_is_108() {
    assert_eq!(
        STAGE3_HEADER_LEN, 108,
        "stage 3 D-350 字面 v1 header 108 byte"
    );
}

/// API-550 字面 — `STAGE4_HEADER_LEN = 128`（stage 4 v2 header；继承 alias）。
#[test]
fn stage4_header_len_constant_is_128() {
    assert_eq!(
        STAGE4_HEADER_LEN, 128,
        "stage 4 D-449 字面 v2 header 128 byte"
    );
}

// ---------------------------------------------------------------------------
// Group B — CheckpointHeaderV3 zero_with_lock 字段字面值（API-551 字面）
// ---------------------------------------------------------------------------

/// API-551 字面 — `CheckpointHeaderV3::zero_with_lock` 字段字面值锁。
///
/// **不变量**：`magic` = [`MAGIC`] + `schema_version` = 3 + `trainer_variant` =
/// 3 (EsMccfrLinearRmPlusCompact) + `info_set_id_layout_version` = 1 (stage 2
/// D-218 维持) + `traverser_count` = 6 (stage 4 D-412) + `quant_bits` = 15 (q15)。
#[test]
fn checkpoint_header_v3_zero_with_lock_field_literal_values() {
    let hdr = CheckpointHeaderV3::zero_with_lock();
    assert_eq!(hdr.magic, MAGIC, "magic 应字面 b\"PLCKPT\\0\\0\"");
    assert_eq!(hdr.schema_version, 3, "schema_version 字面 = 3");
    assert_eq!(
        hdr.trainer_variant,
        TrainerVariant::EsMccfrLinearRmPlusCompact as u8,
        "trainer_variant 字面 = 3 (EsMccfrLinearRmPlusCompact)"
    );
    assert_eq!(
        hdr.info_set_id_layout_version, 1,
        "info_set_id_layout_version = 1 (stage 2 D-218 维持)"
    );
    assert_eq!(
        hdr.traverser_count, 6,
        "traverser_count = 6 (stage 4 D-412)"
    );
    assert_eq!(hdr.quant_bits, 15, "quant_bits = 15 (q15 字面 D-511)");
    assert_eq!(hdr.capacity_estimate, 0, "capacity_estimate zero 起步");
    assert_eq!(hdr.update_count, 0);
    assert!(!hdr.warmup_complete);
    assert_eq!(
        hdr.pruning_config_threshold, -300_000_000.0,
        "PruningConfig threshold 字面 D-520"
    );
    assert_eq!(
        hdr.pruning_config_resurface_period, 10_000_000,
        "PruningConfig resurface_period 字面 D-521"
    );
    assert!(
        (hdr.pruning_config_resurface_epsilon - 0.05).abs() < 1e-9,
        "PruningConfig ε 字面 D-521 0.05"
    );
    assert_eq!(
        hdr.pruning_config_resurface_reset, -150_000_000.0,
        "PruningConfig reset 字面 D-521 -150M"
    );
    assert_eq!(hdr.resurface_pass_id, 0);
    assert_eq!(hdr.naive_baseline_blake3, [0u8; 32]);
    assert_eq!(hdr.body_blake3, [0u8; 32]);
}

// ---------------------------------------------------------------------------
// Group C — ensure_trainer_schema preflight（API-553 字面，pure logic 落地）
// ---------------------------------------------------------------------------

/// API-553 — `ensure_trainer_schema(expected, actual)` 在版本匹配时返 `Ok(())`。
#[test]
fn ensure_trainer_schema_returns_ok_when_version_matches() {
    assert!(matches!(
        ensure_trainer_schema(TrainerVariant::VanillaCfr, 1),
        Ok(())
    ));
    assert!(matches!(
        ensure_trainer_schema(TrainerVariant::EsMccfr, 1),
        Ok(())
    ));
    assert!(matches!(
        ensure_trainer_schema(TrainerVariant::EsMccfrLinearRmPlus, 2),
        Ok(())
    ));
    assert!(matches!(
        ensure_trainer_schema(TrainerVariant::EsMccfrLinearRmPlusCompact, 3),
        Ok(())
    ));
}

/// API-553 — `ensure_trainer_schema(expected, actual)` 在版本不匹配时返
/// `CheckpointError::SchemaMismatch`。
#[test]
fn ensure_trainer_schema_returns_schema_mismatch_on_unexpected_version() {
    // stage 4 trainer 不接受 v1 文件 → SchemaMismatch
    let err = ensure_trainer_schema(TrainerVariant::EsMccfrLinearRmPlus, 1).unwrap_err();
    match err {
        CheckpointError::SchemaMismatch { expected, got } => {
            assert_eq!(expected, 2, "EsMccfrLinearRmPlus 期望 schema=2");
            assert_eq!(got, 1);
        }
        other => panic!("期望 SchemaMismatch，实际 {other:?}"),
    }
    // stage 5 trainer 不接受 v2 文件 → SchemaMismatch
    let err = ensure_trainer_schema(TrainerVariant::EsMccfrLinearRmPlusCompact, 2).unwrap_err();
    match err {
        CheckpointError::SchemaMismatch { expected, got } => {
            assert_eq!(expected, 3, "EsMccfrLinearRmPlusCompact 期望 schema=3");
            assert_eq!(got, 2);
        }
        other => panic!("期望 SchemaMismatch，实际 {other:?}"),
    }
}

/// API-540 — `TrainerVariant::EsMccfrLinearRmPlusCompact.expected_schema_version() = 3`。
#[test]
fn trainer_variant_es_mccfr_linear_rm_plus_compact_expected_schema_3() {
    assert_eq!(
        TrainerVariant::EsMccfrLinearRmPlusCompact.expected_schema_version(),
        3
    );
    // stage 4 trainer 仍走 schema=2，stage 3 trainer 仍走 schema=1。
    assert_eq!(
        TrainerVariant::EsMccfrLinearRmPlus.expected_schema_version(),
        2
    );
    assert_eq!(TrainerVariant::EsMccfr.expected_schema_version(), 1);
    assert_eq!(TrainerVariant::VanillaCfr.expected_schema_version(), 1);
}

// ---------------------------------------------------------------------------
// Group D — D2 [实现] 落地后转 pass 的 round-trip 测试（B1 stub `#[ignore]`）
// ---------------------------------------------------------------------------

/// D-563 字面 anchor #4 — 同 binary build 写 → 读 → 重写 byte-equal
/// （schema=3 路径内部自洽）。
///
/// **B1 [测试] 状态**：D2 \[实现\] 未落地 `Checkpoint::save_schema_v3` /
/// `Checkpoint::parse_bytes_v3` 路径；本测试 `#[ignore]` opt-in。D2 落地后
/// 移除 `#[ignore]` 转 pass。
#[test]
#[ignore = "B1 scaffold; D2 [实现] 落地 `Checkpoint::save_schema_v3` + `parse_bytes_v3` 后转 pass"]
fn checkpoint_v3_save_then_open_byte_equal() {
    // D2 [实现] 起步前 stub — 本测试 panic-fail 设计：
    // 1. 构造 6 traverser × `RegretTableCompact` + `StrategyAccumulatorCompact` 任意值。
    // 2. 走 `Checkpoint::save_schema_v3(path, &header, &regret_tables, &strategy_accums)`
    //    → 192-byte header + 12 sub-region body + trailer。
    // 3. 走 `Checkpoint::open(path)` → schema=3 dispatch → 重建 `Checkpoint` struct。
    // 4. 重新 `save_schema_v3` → 与第一次写出 byte-equal。
    panic!(
        "D-563 字面 anchor #4 round-trip byte-equal — D2 [实现] 起步前 stub。\
         路径：Checkpoint::save_schema_v3 + Checkpoint::parse_bytes_v3 由 D2 [实现] 落地。"
    );
}

/// D-549 / API-552 字面 — `Checkpoint::open` 三路径 dispatch（v1 / v2 / v3）
/// 互不冲突；v3 文件 magic + schema_version 字段与 v1 / v2 区分。
#[test]
#[ignore = "B1 scaffold; D2 [实现] 落地 `Checkpoint::parse_bytes` schema=3 dispatch 后转 pass"]
fn checkpoint_v3_dispatch_distinct_from_v1_v2() {
    // D2 [实现] 落地后协议：
    // - schema_version = 1 → parse_bytes_v1 (HEADER_LEN_V1 = 108)
    // - schema_version = 2 → parse_bytes_v2 (HEADER_LEN = 128)
    // - schema_version = 3 → parse_bytes_v3 (HEADER_LEN_V3 = 192)
    // - 其它 → CheckpointError::SchemaMismatch { expected: 3, got: ... }
    panic!(
        "D-549 三路径 dispatch — D2 [实现] 起步前 stub。\
         路径：Checkpoint::parse_bytes 在 schema_version = 3 时调 parse_bytes_v3。"
    );
}

/// D-563 字面 — body BLAKE3 self-consistency（写 header `body_blake3` 字段；
/// load 时校验 mismatch → `CheckpointError::BodyHashMismatch`）。
#[test]
#[ignore = "B1 scaffold; D2 [实现] 落地 body BLAKE3 self-consistency 后转 pass"]
fn checkpoint_v3_body_blake3_self_consistency() {
    // D2 [实现] 落地后协议：
    // 1. save_schema_v3 写 12 sub-region body → 计算 body BLAKE3 → 写 header
    //    body_blake3 字段。
    // 2. parse_bytes_v3 读 header body_blake3 + 重算 12 sub-region body BLAKE3。
    // 3. mismatch → CheckpointError::Corrupted / 新 variant BodyHashMismatch。
    panic!(
        "D-563 body BLAKE3 self-consistency — D2 [实现] 起步前 stub。\
         路径：Checkpoint::parse_bytes_v3 计算 body BLAKE3 + 校验 header.body_blake3 字段。"
    );
}

// ---------------------------------------------------------------------------
// Group E — 跨 binary version 拒绝（stage 4 既有 first usable checkpoint 加载
// 路径不退化 — §D2-revM dispatch carve-out 继承模式）
// ---------------------------------------------------------------------------

/// §D2-revM dispatch carve-out 模式（继承 stage 4 §D2-revM (i)）—
/// stage 4 schema=2 trainer 读 stage 5 schema=3 文件应**拒绝** SchemaMismatch
/// （`ensure_trainer_schema` preflight）。
#[test]
#[ignore = "B1 scaffold; D2 [实现] 落地 `ensure_trainer_schema` 在 `Trainer::load_checkpoint` \
            preflight 路径接入后转 pass"]
fn stage4_trainer_rejects_stage5_schema_v3_checkpoint() {
    // D2 [实现] 落地后：
    //   Trainer<NlheGame6>::load_checkpoint 内部走 ensure_trainer_schema(self.variant, file.schema)
    //   stage 4 trainer (EsMccfrLinearRmPlus) + schema=3 文件 → SchemaMismatch 拒绝。
    panic!(
        "§D2-revM dispatch carve-out 模式 — D2 [实现] 起步前 stub。\
         路径：EsMccfrTrainer<NlheGame6>::load_checkpoint 内 ensure_trainer_schema preflight \
         在 (variant=EsMccfrLinearRmPlus, schema=3) 时返 SchemaMismatch。"
    );
}

/// §D2-revM dispatch carve-out — stage 5 trainer 读 stage 4 schema=2 文件应
/// **拒绝** SchemaMismatch（不接受向后兼容；D-549 字面**不向前兼容**）。
#[test]
#[ignore = "B1 scaffold; D2 [实现] 落地后转 pass"]
fn stage5_compact_trainer_rejects_stage4_schema_v2_checkpoint() {
    panic!(
        "§D2-revM dispatch carve-out — D2 [实现] 起步前 stub。\
         路径：EsMccfrLinearRmPlusCompactTrainer::load_checkpoint 内 ensure_trainer_schema \
         在 (variant=EsMccfrLinearRmPlusCompact, schema=2) 时返 SchemaMismatch。"
    );
}
