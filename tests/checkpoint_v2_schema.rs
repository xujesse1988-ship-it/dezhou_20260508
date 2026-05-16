//! 阶段 4 C1 \[测试\]：Checkpoint v2 schema 128-byte header + 8 个新字段
//! trip-wire（API-440 / API-441 / D-449）。
//!
//! 三组 trip-wire：
//!
//! 1. **128-byte header layout 常量预期值 anchor**（panic-fail until D2）—
//!    [`HEADER_LEN`] 常量当前 = 108（stage 3 D-350 字面），D-449 要求 stage 4
//!    bump 到 128（新增 traverser_count / linear_weighting / rm_plus /
//!    warmup_complete / regret_offset / strategy_offset 6 个字段 + 16-byte pad_b
//!    = +32 bytes）。本 C1 \[测试\] 通过 `assert_eq!(HEADER_LEN, 128)` 走 panic-
//!    fail（当前 = 108，D2 \[实现\] 落地翻面到 128 后通过）。
//!
//! 2. **SCHEMA_VERSION bump 1 → 2 anchor**（panic-fail until D2）—
//!    [`SCHEMA_VERSION`] 当前 = 1（stage 3 字面），D-449 要求 stage 4 升级到 2
//!    （**不向前兼容**，stage 3 trainer 加载 stage 4 checkpoint → SchemaMismatch
//!    拒绝）。本 C1 \[测试\] 通过 `assert_eq!(SCHEMA_VERSION, 2)` 走 panic-fail；
//!    D2 落地翻面后通过。
//!
//! 3. **8 新字段 enum tag round-trip + 字段 layout 字面 sanity**（部分 default
//!    profile active pass，部分 panic-fail until D2）— [`TrainerVariant::
//!    EsMccfrLinearRmPlus`] / [`GameVariant::Nlhe6Max`] tag round-trip（A1
//!    scaffold 已落地 from_u8 dispatch + enum 字面 tag = 2 / 3）+ 8 新字段
//!    layout 预期 offset 常量字面 lock（D2 \[实现\] 起步前 layout 不漂移）。
//!
//! **8 个新字段（API-440 字面）**：
//!
//! | offset | size | field             | value         |
//! |--------|------|-------------------|---------------|
//! | 14     | 1    | traverser_count   | u8 = 1 (stage 3) / 6 (stage 4 NlheGame6) |
//! | 15     | 1    | linear_weighting  | u8 ∈ {0=off, 1=on}                       |
//! | 16     | 1    | rm_plus           | u8 ∈ {0=off, 1=on}                       |
//! | 17     | 1    | warmup_complete   | u8 ∈ {0=in_warmup, 1=complete}           |
//! | 96     | 8    | regret_offset     | u64 — bincode-serialized regret_tables 起始偏移 |
//! | 104    | 8    | strategy_offset   | u64 — bincode-serialized strategy_accumulators 起始偏移 |
//!
//! schema_version u32=2 + trainer_variant u8 stage 3 字面继承 offset 8 + offset 12
//! 不动；本 schema 字段位置变化由 D-449 字面：stage 3 OFFSET_PAD: usize = 14（6-byte
//! pad）→ stage 4 OFFSET_TRAVERSER_COUNT: usize = 14 + 4-byte 新字段 + 2-byte 收缩
//! pad_a。stage 3 OFFSET_REGRET_TABLE_OFFSET = 92 / OFFSET_STRATEGY_SUM_OFFSET =
//! 100 → stage 4 字面 96 / 104（OFFSET_BUCKET_TABLE_BLAKE3 = 60 不变 + 32-byte
//! bucket_table_blake3 让 offset 字段右移 4-byte）。
//!
//! **C1 \[测试\] 角色边界**：本文件 0 改动 `src/training/checkpoint.rs`；A1
//! scaffold 已落地 [`TrainerVariant::EsMccfrLinearRmPlus`] / [`GameVariant::
//! Nlhe6Max`] enum 字面 + `from_u8` dispatch，但 `HEADER_LEN` / `SCHEMA_VERSION`
//! 字面仍 stage 3 值（108 / 1），D2 \[实现\] 落地后 bump 到 128 / 2。Trait impl
//! 桥接 + `Checkpoint::save_v2` / `Checkpoint::open` v2 路径全部 D2 落地，C1 \[
//! 测试\] 仅 lock 字面常量 layout 让 D2 路径不漂移。
//!
//! **C1 → D2 工程契约**：(a) `HEADER_LEN` bump 108 → 128；(b) `SCHEMA_VERSION`
//! bump 1 → 2；(c) 新增 const `OFFSET_TRAVERSER_COUNT = 14` /
//! `OFFSET_LINEAR_WEIGHTING = 15` / `OFFSET_RM_PLUS = 16` /
//! `OFFSET_WARMUP_COMPLETE = 17` / `OFFSET_REGRET_OFFSET = 96` /
//! `OFFSET_STRATEGY_OFFSET = 104`；(d) `Checkpoint::save_v2` /
//! `Checkpoint::open` v2 路径落地（schema_version 1 vs 2 dispatch + bincode
//! body for `[RegretTable<G>; 6]` + `[StrategyAccumulator<G>; 6]`）；
//! (e) `Checkpoint` struct 新增 `traverser_count: u8` /
//! `linear_weighting_enabled: bool` / `rm_plus_enabled: bool` /
//! `warmup_complete: bool` 字段。

use poker::error::{GameVariant, TrainerVariant};
use poker::training::checkpoint::{HEADER_LEN, MAGIC, SCHEMA_VERSION, TRAILER_LEN};

// ===========================================================================
// Group A — Trip-wire: stage 4 D-449 字面常量 bump（panic-fail until D2）
// ===========================================================================

/// D-449 字面：stage 4 [`SCHEMA_VERSION`] bump 1 → 2（**不向前兼容**：stage 3
/// trainer 加载 stage 4 checkpoint → SchemaMismatch 拒绝 / stage 4 trainer 加载
/// stage 3 checkpoint → SchemaMismatch 拒绝）。
///
/// **C1 \[测试\] 状态**：当前 `SCHEMA_VERSION = 1`（stage 3 字面），本断言走
/// panic-fail；D2 \[实现\] 落地 bump 到 2 后转绿。
///
/// 翻面顺序：D2 [实现] 起步 commit 1 — bump `SCHEMA_VERSION: u32 = 2` 入
/// `src/training/checkpoint.rs`；本测试转绿。
#[test]
fn checkpoint_schema_version_is_2_until_d2() {
    assert_eq!(
        SCHEMA_VERSION,
        2,
        "D-449：stage 4 SCHEMA_VERSION 应 bump 1 → 2（当前 = {SCHEMA_VERSION}，D2 [实现] 起步落地）"
    );
}

/// D-449 字面：stage 4 [`HEADER_LEN`] bump 108 → 128（+32 bytes 给新增 6 字段 +
/// 16-byte pad_b）。
///
/// **C1 \[测试\] 状态**：当前 `HEADER_LEN = 108`（stage 3 字面），本断言走 panic-
/// fail；D2 落地 bump 后转绿。
///
/// stage 3 layout（96 byte usable + 12 byte 填充 offset_strategy_sum_offset 8 byte +
/// 4-byte alignment pad 不字面写出，HEADER_LEN = 108）→ stage 4 layout（API-440 字面
/// 128 byte header，详 doc 顶部 8-field 表格）。
#[test]
fn checkpoint_header_len_is_128_until_d2() {
    assert_eq!(
        HEADER_LEN, 128,
        "D-449：stage 4 HEADER_LEN 应 bump 108 → 128（当前 = {HEADER_LEN}，D2 [实现] 起步落地）"
    );
}

/// API-440 字面：[`TRAILER_LEN`] = 32（BLAKE3 hash size，stage 3 字面继承不动）。
///
/// 本测试默认 profile active pass — TRAILER_LEN 在 stage 4 字面继承 stage 3 D-352
/// 32-byte trailer BLAKE3 不变；D2 \[实现\] 落地不影响 trailer 长度。
#[test]
fn checkpoint_trailer_len_is_32_stage_3_inherited() {
    assert_eq!(
        TRAILER_LEN, 32,
        "API-440：TRAILER_LEN 字面继承 stage 3 D-352 = 32（BLAKE3 hash size）"
    );
}

/// API-440 字面：[`MAGIC`] = `b"PLCKPT\0\0"`（stage 3 字面继承不动；D-449 字面
/// 不变让跨 schema_version 1 / 2 文件 dispatch 通过 magic + schema_version 组合
/// 识别）。
#[test]
fn checkpoint_magic_is_plckpt_pad_stage_3_inherited() {
    assert_eq!(
        MAGIC, *b"PLCKPT\0\0",
        "API-440：MAGIC 字面继承 stage 3 D-350 = b\"PLCKPT\\0\\0\"（D-449 不变）"
    );
}

// ===========================================================================
// Group B — 8 新字段 enum tag round-trip（default profile active pass，A1
// scaffold 已落地 TrainerVariant + GameVariant 字面 tag 与 from_u8 dispatch）
// ===========================================================================

/// API-441 字面：[`TrainerVariant::EsMccfrLinearRmPlus`] tag = 2（stage 3
/// VanillaCfr=0 / EsMccfr=1 → stage 4 新增 = 2）。
///
/// **A1 scaffold 状态**：已落地 enum variant + from_u8 dispatch，本测试 default
/// profile active pass。trip-wire 让 D2 \[实现\] 起步前 enum 字面 tag 漂移立即
/// 暴露（影响 Checkpoint header offset 12 字面写入）。
///
/// **§stage5-rev0 2026-05-16**：stage 5 A1 commit 4d67e24 把
/// `TrainerVariant::EsMccfrLinearRmPlusCompact = 3` + `from_u8(3) -> Some(...)`
/// 加入。原 stage 4 anchor 内 `from_u8(3) == None` 字面断言不再成立；本 commit
/// `#[ignore]` 走 §stage5-rev0 carve-out，沿用 §D2-revM (i) dispatch carve-out
/// 同型模式。stage 5 D1 \[测试\] 起步前 re-author 为完整 4-variant cardinality
/// anchor（已经 ship 在 `tests/checkpoint_v3_round_trip.rs::
/// trainer_variant_es_mccfr_linear_rm_plus_compact_expected_schema_3`）。
#[test]
#[ignore = "§stage5-rev0 — stage 5 A1 commit 加 TrainerVariant tag = 3 (EsMccfrLinearRmPlusCompact)；\
            原 stage 4 字面 `from_u8(3) == None` 断言失效，由 \
            tests/checkpoint_v3_round_trip.rs 4-variant cardinality anchor 承接。\
            详 `docs/pluribus_stage5_workflow.md` §修订历史。"]
fn trainer_variant_es_mccfr_linear_rm_plus_tag_is_2() {
    assert_eq!(
        TrainerVariant::EsMccfrLinearRmPlus as u8,
        2,
        "API-441：TrainerVariant::EsMccfrLinearRmPlus tag = 2（stage 4 新增 3rd variant）"
    );
    // round-trip 字面（D-449 字面：schema_version=2 文件读 tag=2 应 → EsMccfrLinearRmPlus）
    assert_eq!(
        TrainerVariant::from_u8(2),
        Some(TrainerVariant::EsMccfrLinearRmPlus),
        "API-441：TrainerVariant::from_u8(2) round-trip 到 EsMccfrLinearRmPlus"
    );
    // 越界拒绝
    assert_eq!(
        TrainerVariant::from_u8(3),
        None,
        "API-441：TrainerVariant::from_u8(3) 越界应返 None"
    );
    // stage 3 既有 tag 字面继承不退化
    assert_eq!(
        TrainerVariant::from_u8(0),
        Some(TrainerVariant::VanillaCfr),
        "stage 3：VanillaCfr tag = 0 不退化"
    );
    assert_eq!(
        TrainerVariant::from_u8(1),
        Some(TrainerVariant::EsMccfr),
        "stage 3：EsMccfr tag = 1 不退化"
    );
}

/// API-411 字面：[`GameVariant::Nlhe6Max`] tag = 3（stage 3 Kuhn=0 / Leduc=1 /
/// SimplifiedNlhe=2 → stage 4 新增 = 3）。
///
/// **A1 scaffold 状态**：已落地 enum variant + from_u8 dispatch，default profile
/// active pass。让 D2 \[实现\] 起步前 enum 字面 tag 漂移立即暴露（影响 Checkpoint
/// header offset 13 字面写入）。
#[test]
fn game_variant_nlhe6max_tag_is_3() {
    assert_eq!(
        GameVariant::Nlhe6Max as u8,
        3,
        "API-411：GameVariant::Nlhe6Max tag = 3（stage 4 新增 4th variant）"
    );
    assert_eq!(
        GameVariant::from_u8(3),
        Some(GameVariant::Nlhe6Max),
        "API-411：GameVariant::from_u8(3) round-trip 到 Nlhe6Max"
    );
    // 越界拒绝
    assert_eq!(
        GameVariant::from_u8(4),
        None,
        "API-411：GameVariant::from_u8(4) 越界应返 None"
    );
    // stage 3 既有 tag 字面继承不退化
    assert_eq!(GameVariant::from_u8(0), Some(GameVariant::Kuhn));
    assert_eq!(GameVariant::from_u8(1), Some(GameVariant::Leduc));
    assert_eq!(GameVariant::from_u8(2), Some(GameVariant::SimplifiedNlhe));
}

// ===========================================================================
// Group C — 8 新字段 layout 字面 sanity（C1 钉死 D-449 字面 offset 表 + size 表
// 让 D2 [实现] 起步前不漂移）
// ===========================================================================

/// API-440 字面 layout 字面 sanity：8 新字段总 byte size = 32（与 HEADER_LEN
/// bump 108 → 128 的 +32 byte 一致；详 doc 顶部 8-field 表格）。
///
/// 字段 size 合计：
/// - traverser_count: 1 byte
/// - linear_weighting: 1 byte
/// - rm_plus: 1 byte
/// - warmup_complete: 1 byte
/// - regret_offset: 8 byte
/// - strategy_offset: 8 byte
/// - schema_version 升级（u32 不变）: 0 byte
/// - trainer_variant 字面（u8 不变）: 0 byte
///
/// 上述 6 字段合计 = 4 + 16 = 20 byte；HEADER_LEN +32 byte 额外 12 byte 来自
/// stage 3 OFFSET_REGRET_TABLE_OFFSET = 92 / OFFSET_STRATEGY_SUM_OFFSET = 100 →
/// stage 4 字面 96 / 104（+4 byte 让 8-byte alignment 满足 u64 offset 字段）+
/// 8 byte pad_b 给 reserved 区域。本测试纯字面常量 sanity，D2 \[实现\] 起步前
/// 字面表 lock。
#[test]
fn checkpoint_header_field_size_addendum_32_bytes() {
    // 6 个新字段大小（API-440 字面）
    let traverser_count_size: usize = 1;
    let linear_weighting_size: usize = 1;
    let rm_plus_size: usize = 1;
    let warmup_complete_size: usize = 1;
    let regret_offset_size: usize = 8;
    let strategy_offset_size: usize = 8;
    let new_fields_total = traverser_count_size
        + linear_weighting_size
        + rm_plus_size
        + warmup_complete_size
        + regret_offset_size
        + strategy_offset_size;
    assert_eq!(new_fields_total, 20, "API-440：6 个新字段总 byte size = 20");

    // §D2-revM 2026-05-15（stage 4 D2 \[实现\] 落地，C1 测试 +12 算术误差订正）：
    // stage 3 → stage 4 HEADER_LEN bump 108 → 128 = +20 byte。`new_fields_total`
    // 统计 6 项含 regret_offset / strategy_offset（这两项 v1 已存在 8 byte，
    // 在 v2 仅 byte offset 平移）→ 该 20 已包含 v1 → v2 总增量；不再加额外 +12
    // pad 修正。pad_a (6 byte) 在 v1 已存在；新增 pad_b 16 byte 与 4 个 stage 4
    // u8 共同贡献 20 byte 总增量（4 + 16 = 20，与 new_fields_total 同值）。详
    // `docs/pluribus_stage4_workflow.md` §D2 修订历史。
    let stage_3_header_len: usize = 108;
    let stage_4_header_len: usize = 128;
    assert_eq!(
        stage_4_header_len - stage_3_header_len,
        new_fields_total,
        "D-449：HEADER_LEN bump = 20 byte = 4 个新 u8 + 16-byte pad_b（与 new_fields_total 等价）"
    );

    // 当前 HEADER_LEN 应当 == stage_4 expected（D2 落地后），不等则 panic-fail
    assert_eq!(
        HEADER_LEN,
        stage_4_header_len,
        "D-449：当前 HEADER_LEN = {HEADER_LEN}，stage 4 字面 = {stage_4_header_len}（D2 落地后 bump）"
    );
}

/// API-440 字面 layout sanity：stage 4 字段 offset 字面（在 D2 \[实现\] 起步前
/// 锁定，新字段不漂移）。
///
/// 本测试构造**预期 layout offsets**为 const 字面，让 D2 起步前 layout 漂移立
/// 即在 cargo test 暴露。D2 落地 src/training/checkpoint.rs 新增的常量应当与本
/// 测试字面一致。
#[test]
fn checkpoint_v2_layout_offsets_match_api_440_spec() {
    // API-440 字面 stage 4 layout（doc 顶部表格）
    let expected_offset_magic: usize = 0;
    let expected_offset_schema_version: usize = 8;
    let expected_offset_trainer_variant: usize = 12;
    let expected_offset_game_variant: usize = 13;
    let expected_offset_traverser_count: usize = 14;
    let expected_offset_linear_weighting: usize = 15;
    let expected_offset_rm_plus: usize = 16;
    let expected_offset_warmup_complete: usize = 17;
    let expected_offset_pad_a: usize = 18;
    let expected_offset_update_count: usize = 24;
    let expected_offset_rng_state: usize = 32;
    let expected_offset_bucket_table_blake3: usize = 64;
    let expected_offset_regret_offset: usize = 96;
    let expected_offset_strategy_offset: usize = 104;
    let expected_offset_pad_b: usize = 112;
    let expected_header_end: usize = 128;

    // 字段长度字面（doc 顶部表格）
    assert_eq!(expected_offset_schema_version - expected_offset_magic, 8);
    assert_eq!(
        expected_offset_trainer_variant - expected_offset_schema_version,
        4
    );
    assert_eq!(
        expected_offset_game_variant - expected_offset_trainer_variant,
        1
    );
    assert_eq!(
        expected_offset_traverser_count - expected_offset_game_variant,
        1
    );
    assert_eq!(
        expected_offset_linear_weighting - expected_offset_traverser_count,
        1
    );
    assert_eq!(
        expected_offset_rm_plus - expected_offset_linear_weighting,
        1
    );
    assert_eq!(expected_offset_warmup_complete - expected_offset_rm_plus, 1);
    assert_eq!(expected_offset_pad_a - expected_offset_warmup_complete, 1);
    assert_eq!(
        expected_offset_update_count - expected_offset_pad_a,
        6,
        "pad_a 6-byte 让 update_count 落在 8-byte alignment offset 24"
    );
    assert_eq!(
        expected_offset_rng_state - expected_offset_update_count,
        8,
        "update_count = u64 = 8 byte"
    );
    assert_eq!(
        expected_offset_bucket_table_blake3 - expected_offset_rng_state,
        32,
        "rng_state = ChaCha20 32-byte state"
    );
    assert_eq!(
        expected_offset_regret_offset - expected_offset_bucket_table_blake3,
        32,
        "bucket_table_blake3 = 32-byte BLAKE3 hash"
    );
    assert_eq!(
        expected_offset_strategy_offset - expected_offset_regret_offset,
        8,
        "regret_offset = u64 = 8 byte"
    );
    assert_eq!(
        expected_offset_pad_b - expected_offset_strategy_offset,
        8,
        "strategy_offset = u64 = 8 byte"
    );
    assert_eq!(
        expected_header_end - expected_offset_pad_b,
        16,
        "pad_b 16-byte reserved 区域"
    );
    assert_eq!(expected_header_end, 128, "stage 4 HEADER_LEN = 128");

    // 当前 src/training/checkpoint.rs HEADER_LEN 应当 == 128（D2 落地后）。
    // 当前 = 108 → panic-fail。
    assert_eq!(
        HEADER_LEN, expected_header_end,
        "D-449：src/training/checkpoint.rs HEADER_LEN 应 == 128 字面（D2 [实现] 起步落地）"
    );
}

// ===========================================================================
// Group D — 跨 schema_version 不兼容 sanity（D-449 字面：stage 3 ↔ stage 4
// SchemaMismatch 拒绝路径；C1 \[测试\] panic-fail 当前 SCHEMA_VERSION=1 路径，
// D2 落地走 bump 后 SchemaMismatch dispatch 转绿）
// ===========================================================================

/// D-449 字面：stage 3 schema_version=1 ↔ stage 4 schema_version=2 跨版本不兼容
/// （SchemaMismatch 拒绝）。
///
/// **C1 \[测试\] 状态**：当前 `SCHEMA_VERSION = 1`（stage 3），本测试构造 expected
/// schema_version=2 字面让 D2 \[实现\] 起步前 schema 字面漂移立即在 cargo test
/// 暴露。
///
/// D1 \[测试\] 会扩展真正的 SchemaMismatch round-trip 测试（写 schema_version=1
/// 的 buffer 让 stage 4 trainer 加载触发 `SchemaMismatch { expected: 2, got: 1 }`）；
/// C1 仅 lock 字面常量。
#[test]
fn checkpoint_schema_version_mismatch_dispatch_anchor_d2() {
    let stage_3_schema: u32 = 1;
    let stage_4_schema: u32 = 2;
    assert_ne!(
        stage_3_schema, stage_4_schema,
        "D-449：stage 3 (=1) 与 stage 4 (=2) schema_version 字面不一致（不向前兼容）"
    );

    // 当前 SCHEMA_VERSION 应当 == stage_4 expected（D2 落地后）
    assert_eq!(
        SCHEMA_VERSION, stage_4_schema,
        "D-449：当前 SCHEMA_VERSION = {SCHEMA_VERSION}，stage 4 字面 = {stage_4_schema}（D2 [实现] 起步落地）"
    );
}

/// API-440 字面 sanity：stage 3 + stage 4 各自字面 6 个 GameVariant + 3 个
/// TrainerVariant tag 组合（stage 3 = 3 game × 2 trainer / stage 4 新增 1 game + 1
/// trainer = 4 game × 3 trainer = 12 combo，跨 schema_version dispatch 通过 magic
/// + schema_version + trainer_variant + game_variant 4 字段联合识别）。
///
/// 本测试纯字面 enum cardinality sanity（A1 scaffold 已落地），D2 \[实现\] 起步
/// 前 enum 变体增减立即在 cargo test 暴露。
///
/// **§stage5-rev0 2026-05-16**：stage 5 A1 commit 4d67e24 把
/// `TrainerVariant::EsMccfrLinearRmPlusCompact = 3` 加入；原 stage 4 cardinality
/// 字面（`from_u8(3) == None`）失效；本 commit `#[ignore]` 走 §stage5-rev0
/// carve-out 沿用 §D2-revM (i) 模式。stage 5 D1 \[测试\] 起步前 re-author 为
/// 完整 4 trainer × 4 game cardinality anchor。
#[test]
#[ignore = "§stage5-rev0 — stage 5 A1 commit 加 TrainerVariant tag = 3 (EsMccfrLinearRmPlusCompact)；\
            原 stage 4 cardinality 字面 from_u8(3) == None 失效。\
            详 `docs/pluribus_stage5_workflow.md` §修订历史。"]
fn checkpoint_variant_cardinality_anchor() {
    // 4 个 GameVariant（A1 scaffold 字面，D-411 字面 stage 4 4th variant 锁定）
    assert!(GameVariant::from_u8(0).is_some());
    assert!(GameVariant::from_u8(1).is_some());
    assert!(GameVariant::from_u8(2).is_some());
    assert!(GameVariant::from_u8(3).is_some());
    assert!(GameVariant::from_u8(4).is_none());

    // 3 个 TrainerVariant（A1 scaffold 字面，D-449 字面 stage 4 3rd variant 锁定）
    assert!(TrainerVariant::from_u8(0).is_some());
    assert!(TrainerVariant::from_u8(1).is_some());
    assert!(TrainerVariant::from_u8(2).is_some());
    assert!(TrainerVariant::from_u8(3).is_none());

    // stage 4 (trainer, game) 主组合（D-449 字面）：
    // (EsMccfrLinearRmPlus, Nlhe6Max) 走 schema_version=2 主路径
    let combo = (TrainerVariant::EsMccfrLinearRmPlus, GameVariant::Nlhe6Max);
    assert_eq!(combo.0 as u8, 2);
    assert_eq!(combo.1 as u8, 3);
}

// ===========================================================================
// Group E — Checkpoint struct 字段扩展 trip-wire（D2 落地前本测试 panic-fail；
// D2 落地后 Checkpoint struct 新增 traverser_count / linear_weighting_enabled /
// rm_plus_enabled / warmup_complete 字段后转绿）
// ===========================================================================

/// API-441 字面：stage 4 Checkpoint struct 新增 4 个字段 — `traverser_count: u8`
/// / `linear_weighting_enabled: bool` / `rm_plus_enabled: bool` /
/// `warmup_complete: bool`。
///
/// **C1 \[测试\] 状态**：当前 `Checkpoint` struct 仅有 stage 3 字面 7 字段
/// （schema_version / trainer_variant / game_variant / update_count / rng_state /
/// bucket_table_blake3 / regret_table_bytes + strategy_sum_bytes）；本测试通过
/// 字面 4 个新字段 expected 值（C2/D2 落地后字面构造 Checkpoint 实例）让 D2
/// \[实现\] 起步前字段集合漂移立即在 cargo test 暴露。
///
/// 当前形态：构造 stage 3 Checkpoint 字面 + 4 个新字段 expected 字面 sanity；D2
/// 落地后翻面成 `Checkpoint { ..., traverser_count: 6, ... }` 构造路径。本测试
/// **不**直接构造 Checkpoint 实例（pub 字段集合在 D2 落地后变化）— 仅锁字面常量。
#[test]
fn checkpoint_stage_4_new_fields_expected_values() {
    // stage 4 NlheGame6 路径上 4 个新字段 expected 字面值（D-449 字面）：
    let expected_traverser_count: u8 = 6;
    let expected_linear_weighting_enabled: bool = true;
    let expected_rm_plus_enabled: bool = true;
    // warmup_complete: stage 4 训练起步 = false（前 1M update 走 stage 3 standard CFR +
    // RM 路径 byte-equal anchor 维持），跨 1M update 边界后 = true。
    let expected_warmup_complete_initial: bool = false;
    let expected_warmup_complete_post_warmup: bool = true;

    // 字面 sanity（D-449 字面 6 traverser / Linear MCCFR / RM+ / warm-up 1M update
    // edge-detection 路径）
    assert_eq!(
        expected_traverser_count, 6,
        "D-412：6-traverser alternating（每 trainer 6 套独立 RegretTable）"
    );
    assert!(
        expected_linear_weighting_enabled,
        "D-401：stage 4 主路径 Linear discounting enabled"
    );
    assert!(
        expected_rm_plus_enabled,
        "D-402：stage 4 主路径 RM+ in-place clamp enabled"
    );
    assert!(
        !expected_warmup_complete_initial,
        "D-409：warmup 阶段（前 1M update）warmup_complete = false"
    );
    assert!(
        expected_warmup_complete_post_warmup,
        "D-409：warmup 完成（1M update 边界后）warmup_complete = true"
    );
}

/// D-449 字面 sanity：stage 3 单 traverser 路径 traverser_count = 1（不破 stage 3
/// 字面 checkpoint 兼容性；schema_version=1 文件 traverser_count 字段不存在但
/// stage 4 trainer 加载 stage 3 文件应当走 SchemaMismatch 拒绝路径，不走
/// traverser_count 字段读路径）。
#[test]
fn checkpoint_stage_3_traverser_count_is_1_in_schema_v1_path() {
    let stage_3_traverser_count: u8 = 1;
    assert_eq!(
        stage_3_traverser_count, 1,
        "stage 3 SimplifiedNlheGame / Kuhn / Leduc 单 traverser 路径"
    );
    // stage 4 NlheGame6 路径 = 6
    let stage_4_traverser_count: u8 = 6;
    assert_eq!(
        stage_4_traverser_count, 6,
        "stage 4 NlheGame6 6-traverser 路径"
    );
    assert_ne!(
        stage_3_traverser_count, stage_4_traverser_count,
        "D-449：stage 3 (=1) 与 stage 4 (=6) traverser_count 字面不一致 → SchemaMismatch 拒绝"
    );
}
