//! 阶段 4 D1 \[测试\]：Checkpoint schema_version=2 round-trip + 跨版本拒绝 +
//! 6-traverser snapshot byte-equal + v2 byte-flip corruption（D-445 / D-449 /
//! API-440 / API-441）。
//!
//! 18 条测试（`pluribus_stage4_workflow.md` §步骤 D1 line 236 字面对应；继承
//! stage 3 `checkpoint_round_trip.rs` D1 模式扩展到 6-traverser × 14-action ×
//! Linear+RM+ trainer variant）：
//!
//! - **Group A — schema v2 round-trip 6-traverser × 14-action**（test 1-3）
//! - **Group B — 跨版本拒绝**（test 4-7）
//! - **Group C — config 字段 mismatch 拒绝**（test 8-10）
//! - **Group D — 5 类 CheckpointError v2 byte-flip + bucket_table_blake3 mismatch**
//!   （test 11-15）
//! - **Group E — 6-traverser regret / strategy serialization + cross-load
//!   equivalence + regret_offset / strategy_offset 计算**（test 16-18）
//!
//! **D1 \[测试\] 角色边界**（继承 stage 1/2/3 同型政策）：本文件 0 改动
//! `src/training/`、`src/error.rs`、`docs/pluribus_stage4_{validation,decisions,
//! api}.md`；如断言落在 \[实现\] 边界错误的产品代码上 → filed issue 移交 D2 \[实现\]，
//! 不在测试内 patch 产品逻辑。
//!
//! **D1 → D2 工程契约**（panic-fail 翻面条件 — D2 \[实现\] 落地后转绿）：
//!
//! 1. `SCHEMA_VERSION` bump 1 → 2 + `HEADER_LEN` bump 108 → 128（C1 \[测试\]
//!    `checkpoint_v2_schema.rs` 已 lock）。
//! 2. `Checkpoint` struct 加 4 字段 `traverser_count: u8` / `linear_weighting_enabled:
//!    bool` / `rm_plus_enabled: bool` / `warmup_complete: bool`（D-449 字面）。
//! 3. `EsMccfrTrainer::save_checkpoint` 在 `config.linear_weighting_enabled &&
//!    config.rm_plus_enabled` 时走 schema_version=2 路径写 v2 header（
//!    `TrainerVariant::EsMccfrLinearRmPlus`）；`EsMccfrTrainer::load_checkpoint`
//!    在 NlheGame6 + schema=2 路径下接受并反序列化 6-traverser RegretTable +
//!    StrategyAccumulator 数组（D-412 字面）。
//! 4. `Checkpoint::open` schema_version 2 dispatch（128-byte header parse +
//!    `regret_offset` / `strategy_offset` u64 LE 字段读取 + 4 个新 u8/bool 字段
//!    读取）；当前 schema=1 路径下 `Checkpoint::open` 把 schema=2 文件直接拒绝
//!    走 `SchemaMismatch { expected=1, got=2 }`（panic-fail 直到 D2 bump
//!    `SCHEMA_VERSION` + 加 schema_version 2 解析分支）。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use blake3::Hasher;
use poker::training::checkpoint::{Checkpoint, MAGIC, TRAILER_LEN};
use poker::training::game::Game;
use poker::training::kuhn::KuhnGame;
use poker::training::nlhe::SimplifiedNlheGame;
use poker::training::nlhe_6max::NlheGame6;
use poker::training::{
    CheckpointError, EsMccfrTrainer, GameVariant, Trainer, TrainerVariant, VanillaCfrTrainer,
};
use poker::{BucketTable, ChaCha20Rng};

// ===========================================================================
// 共享常量
// ===========================================================================

/// stage 4 D1 \[测试\] fixed master seed（ASCII "STG4_D1\0"）。
const FIXED_SEED: u64 = 0x53_54_47_34_5F_44_31_00;

/// stage 4 schema_version 字面（D-449）。当前 `SCHEMA_VERSION = 1`；D2 \[实现\]
/// bump 1 → 2 后本常量 == [`SCHEMA_VERSION`]。
const SCHEMA_V2: u32 = 2;

/// stage 4 HEADER_LEN 字面（D-449）。当前 `HEADER_LEN = 108`；D2 bump 108 → 128
/// 后本常量 == [`HEADER_LEN`]。
const HEADER_V2_LEN: usize = 128;

/// stage 4 v2 binary layout offset 字面（API-440 字面，与 C1 \[测试\]
/// `checkpoint_v2_schema.rs::checkpoint_v2_layout_offsets_match_api_440_spec`
/// 同型）。
const OFFSET_MAGIC: usize = 0;
const OFFSET_SCHEMA_VERSION: usize = 8;
const OFFSET_TRAINER_VARIANT: usize = 12;
const OFFSET_GAME_VARIANT: usize = 13;
const OFFSET_TRAVERSER_COUNT: usize = 14;
const OFFSET_LINEAR_WEIGHTING: usize = 15;
const OFFSET_RM_PLUS: usize = 16;
const OFFSET_WARMUP_COMPLETE: usize = 17;
const OFFSET_PAD_A: usize = 18;
const OFFSET_UPDATE_COUNT: usize = 24;
#[allow(dead_code)]
const OFFSET_RNG_STATE: usize = 32;
const OFFSET_BUCKET_TABLE_BLAKE3: usize = 64;
const OFFSET_REGRET_OFFSET: usize = 96;
const OFFSET_STRATEGY_OFFSET: usize = 104;
const OFFSET_PAD_B: usize = 112;

/// v2 binary layout sanity 锁（API-440 字面，D-449 lock）。
const _: () = assert!(OFFSET_PAD_B + 16 == HEADER_V2_LEN);
const _: () = assert!(OFFSET_TRAVERSER_COUNT == 14);
const _: () = assert!(OFFSET_REGRET_OFFSET == 96);
const _: () = assert!(OFFSET_STRATEGY_OFFSET == 104);

/// stage 4 6-traverser 字面（D-412）。
const N_TRAVERSER_STAGE4: u8 = 6;
/// stage 3 single-traverser 字面（D-321-rev1 / SimplifiedNlheGame）。
const N_TRAVERSER_STAGE3: u8 = 1;

/// v3 production artifact path（D-424 lock）。
const V3_ARTIFACT_PATH: &str = "artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin";
const V3_BODY_BLAKE3_HEX: &str = "67ee555439f2c918698650c05f40a7a5e9e812280ceb87fc3c6590add98650cd";

/// Smoke / round-trip step 数（保 default profile 跑得动）。
const SMOKE_STEPS: u64 = 10;
const SMALL_ROUND_TRIP_STEPS: u64 = 100;

// ===========================================================================
// helper
// ===========================================================================

fn unique_temp_path(label: &str) -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let mut p = std::env::temp_dir();
    p.push(format!("poker_stage4_d1_ckpt_v2_{label}_{pid}_{nanos}.bin"));
    p
}

fn cleanup(path: &Path) {
    let _ = std::fs::remove_file(path);
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let _ = std::fs::remove_file(PathBuf::from(tmp));
}

fn blake3_hex(bytes: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// 加载 v3 artifact 并返回 `Arc<BucketTable>`；artifact 缺失 / hash 不匹配走
/// pass-with-skip（继承 stage 3 同型 helper 政策）。
fn load_v3_artifact_arc_or_skip() -> Option<Arc<BucketTable>> {
    let path = PathBuf::from(V3_ARTIFACT_PATH);
    if !path.exists() {
        eprintln!("skip: v3 artifact `{V3_ARTIFACT_PATH}` 不存在");
        return None;
    }
    let table = match BucketTable::open(&path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("skip: BucketTable::open 失败：{e:?}");
            return None;
        }
    };
    let body_hex = blake3_hex(&table.content_hash());
    if body_hex != V3_BODY_BLAKE3_HEX {
        eprintln!("skip: artifact body BLAKE3 `{body_hex}` 不匹配 v3");
        return None;
    }
    Some(Arc::new(table))
}

/// 构造一个走 Linear+RM+ 路径的 NlheGame6 EsMccfrTrainer 跑 `n_steps` 后返回。
fn run_nlhe6_linear_rm_plus_trainer(
    table: Arc<BucketTable>,
    n_steps: u64,
    warmup_at: u64,
) -> EsMccfrTrainer<NlheGame6> {
    let game = NlheGame6::new(table).expect("v3 artifact");
    let mut trainer = EsMccfrTrainer::new(game, FIXED_SEED).with_linear_rm_plus(warmup_at);
    let mut rng = ChaCha20Rng::from_seed(FIXED_SEED);
    for i in 0..n_steps {
        trainer
            .step(&mut rng)
            .unwrap_or_else(|e| panic!("NlheGame6 trainer.step #{i} 失败：{e:?}"));
    }
    trainer
}

/// 字节级构造一个最小可用 v2 schema checkpoint 文件（D-449 字面 128-byte header
/// + 空 regret/strategy body + 32-byte trailer BLAKE3）。
///
/// D2 \[实现\] 落地前本函数构造的 buffer 在 `Checkpoint::open` 上必走
/// [`SchemaMismatch { expected = 1, got = 2 }`] 路径；落地后走 schema v2 解析
/// 分支接受。
fn craft_minimal_v2_checkpoint_bytes(
    trainer_tag: u8,
    game_tag: u8,
    traverser_count: u8,
    linear_weighting: u8,
    rm_plus: u8,
    warmup_complete: u8,
    bucket_table_blake3: [u8; 32],
) -> Vec<u8> {
    // 简化 layout：空 body — regret_offset = strategy_offset = HEADER_V2_LEN
    // = 128（body 0 byte）；total = 128 + 0 + 32 = 160 byte。
    let body_end = HEADER_V2_LEN;
    let total_len = body_end + TRAILER_LEN;
    let mut buf = vec![0u8; total_len];

    buf[OFFSET_MAGIC..OFFSET_SCHEMA_VERSION].copy_from_slice(&MAGIC);
    buf[OFFSET_SCHEMA_VERSION..OFFSET_TRAINER_VARIANT].copy_from_slice(&SCHEMA_V2.to_le_bytes());
    buf[OFFSET_TRAINER_VARIANT] = trainer_tag;
    buf[OFFSET_GAME_VARIANT] = game_tag;
    buf[OFFSET_TRAVERSER_COUNT] = traverser_count;
    buf[OFFSET_LINEAR_WEIGHTING] = linear_weighting;
    buf[OFFSET_RM_PLUS] = rm_plus;
    buf[OFFSET_WARMUP_COMPLETE] = warmup_complete;
    // pad_a (18..24) 全 0
    // update_count = 0
    // rng_state = [0; 32]
    buf[OFFSET_BUCKET_TABLE_BLAKE3..OFFSET_REGRET_OFFSET].copy_from_slice(&bucket_table_blake3);
    let regret_offset = HEADER_V2_LEN as u64;
    let strategy_offset = HEADER_V2_LEN as u64;
    buf[OFFSET_REGRET_OFFSET..OFFSET_STRATEGY_OFFSET].copy_from_slice(&regret_offset.to_le_bytes());
    buf[OFFSET_STRATEGY_OFFSET..OFFSET_PAD_B].copy_from_slice(&strategy_offset.to_le_bytes());
    // pad_b (112..128) 全 0

    // trailer BLAKE3 over [0..body_end)
    let mut hasher = Hasher::new();
    hasher.update(&buf[..body_end]);
    let trailer: [u8; 32] = hasher.finalize().into();
    buf[body_end..total_len].copy_from_slice(&trailer);
    buf
}

fn write_tmp(bytes: &[u8], label: &str) -> PathBuf {
    let path = unique_temp_path(label);
    std::fs::write(&path, bytes).expect("write tmp checkpoint");
    path
}

// ===========================================================================
// Group A — schema v2 round-trip 6-traverser × 14-action（test 1-3）
// ===========================================================================

/// Test 1 — D-445 / D-449：schema_version=2 round-trip 6-traverser × 14-action。
///
/// 走 NlheGame6 + Linear+RM+ trainer：跑 small step → save → 读 bytes →
/// 校验 offset 8 处 schema_version 字段 == 2u32 LE（D-449 字面）+ offset 12 处
/// trainer_variant tag == EsMccfrLinearRmPlus(=2)（API-441）+ offset 13 处
/// game_variant tag == Nlhe6Max(=3)（API-411）+ offset 14 处 traverser_count
/// == 6（D-412）。
///
/// **D2 落地前**：`EsMccfrTrainer::save_checkpoint` 字面写 schema_version=1
/// （stage 3 path）+ TrainerVariant::EsMccfr(=1)（非 LinearRmPlus）→ 多条断言
/// fail。D2 落地走 schema_version=2 路径后转绿。
///
/// release/ignored opt-in：依赖 v3 artifact + NlheGame6::new 走 528 MiB
/// BucketTable 加载。
#[test]
#[ignore = "release/--ignored opt-in（v3 artifact 528 MiB 依赖；D2 \\[实现\\] 落地后转绿）"]
fn schema_version_2_round_trip_6_traverser_14_action_layout_check() {
    let Some(table) = load_v3_artifact_arc_or_skip() else {
        return;
    };
    let trainer = run_nlhe6_linear_rm_plus_trainer(Arc::clone(&table), SMOKE_STEPS, 0);
    let path = unique_temp_path("schema_v2_round_trip");
    trainer
        .save_checkpoint(&path)
        .expect("NlheGame6 save_checkpoint @ 10 step 应成功");
    let bytes = std::fs::read(&path).expect("re-read saved checkpoint");
    cleanup(&path);

    assert!(
        bytes.len() >= HEADER_V2_LEN + TRAILER_LEN,
        "v2 checkpoint 文件长度 {} 应 >= header_v2 ({HEADER_V2_LEN}) + trailer ({TRAILER_LEN})",
        bytes.len()
    );

    let schema_field = u32::from_le_bytes(
        bytes[OFFSET_SCHEMA_VERSION..OFFSET_TRAINER_VARIANT]
            .try_into()
            .unwrap(),
    );
    assert_eq!(
        schema_field, SCHEMA_V2,
        "D-449：NlheGame6 + Linear+RM+ save_checkpoint 应写 schema_version=2，实际写 {schema_field}（D2 \\[实现\\] 起步前 stage 3 path 字面 1）"
    );
    assert_eq!(
        bytes[OFFSET_TRAINER_VARIANT],
        TrainerVariant::EsMccfrLinearRmPlus as u8,
        "API-441：trainer_variant tag 应 == EsMccfrLinearRmPlus(2)"
    );
    assert_eq!(
        bytes[OFFSET_GAME_VARIANT],
        GameVariant::Nlhe6Max as u8,
        "API-411：game_variant tag 应 == Nlhe6Max(3)"
    );
    assert_eq!(
        bytes[OFFSET_TRAVERSER_COUNT], N_TRAVERSER_STAGE4,
        "D-412：traverser_count 字面 6（6-traverser alternating）"
    );
}

/// Test 2 — D-449：schema v2 round-trip update_count + warmup_complete + 4 个
/// stage 4 新字段 read-back via Checkpoint::open。
///
/// 走 NlheGame6 + Linear+RM+ trainer warmup_at = 5 → 跑 SMALL_ROUND_TRIP_STEPS
/// step (cross 5 边界) → save → `Checkpoint::open` → 验证 schema_version /
/// trainer_variant / game_variant / update_count 在 v2 路径 read-back 一致。
///
/// **D2 落地前**：`Checkpoint::open` 见 schema=2 走 SchemaMismatch（current
/// SCHEMA_VERSION=1），panic-fail；D2 bump SCHEMA_VERSION 后转绿。
#[test]
#[ignore = "release/--ignored opt-in（v3 artifact 依赖；D2 \\[实现\\] 落地后转绿）"]
fn schema_v2_round_trip_open_reads_back_consistent_fields() {
    let Some(table) = load_v3_artifact_arc_or_skip() else {
        return;
    };
    let trainer = run_nlhe6_linear_rm_plus_trainer(Arc::clone(&table), SMALL_ROUND_TRIP_STEPS, 5);
    let path = unique_temp_path("v2_open_reads_back");
    trainer.save_checkpoint(&path).expect("save");
    let ckpt = Checkpoint::open(&path).unwrap_or_else(|e| {
        panic!(
            "D-449：NlheGame6 + Linear+RM+ checkpoint Checkpoint::open 应成功（D2 \\[实现\\] 落地后），实际 Err({e:?})"
        )
    });
    cleanup(&path);

    assert_eq!(
        ckpt.schema_version, SCHEMA_V2,
        "D-449：read-back schema_version 应 == 2"
    );
    assert_eq!(
        ckpt.trainer_variant,
        TrainerVariant::EsMccfrLinearRmPlus,
        "API-441：read-back trainer_variant"
    );
    assert_eq!(
        ckpt.game_variant,
        GameVariant::Nlhe6Max,
        "API-411：read-back game_variant"
    );
    assert_eq!(
        ckpt.update_count, SMALL_ROUND_TRIP_STEPS,
        "update_count 应 == {SMALL_ROUND_TRIP_STEPS}"
    );
}

/// Test 3 — D-449：HU 退化路径 `NlheGame6::new_hu` 配 `EsMccfrTrainer::new` 跑
/// 1M update × 3 BLAKE3 byte-equal stage 3 `SimplifiedNlheGame` anchor（D-416
/// 字面继承）。
///
/// 由 C2 \[实现\] commit 上 single-table 路径与 stage 3 路径数值等价 → 本测试
/// **不依赖 D2 \[实现\]**（save_checkpoint 走 stage 3 path 即可）；但实际 1M
/// update × 3 BLAKE3 anchor 在 D2 \[实现\] 落地 v2 schema 后转绿（schema_version=
/// 1 vs 2 路径在 HU 退化下 byte-equal 维持）。
///
/// release/ignored opt-in：1M × 3 NLHE update ~ 30 min vultr + v3 artifact 528 MiB。
#[test]
#[ignore = "release/--ignored opt-in（1M update × 3 NLHE ~ 30 min；v3 artifact 依赖；D2 \\[实现\\] 落地后通过）"]
fn hu_degenerate_1m_update_x_3_blake3_byte_equal_stage3_simplified_nlhe_anchor() {
    let Some(table) = load_v3_artifact_arc_or_skip() else {
        return;
    };
    let mut hashes: Vec<[u8; 32]> = Vec::with_capacity(3);
    for run in 0..3 {
        let game = NlheGame6::new_hu(Arc::clone(&table)).expect("HU 退化 NlheGame6::new_hu");
        let mut trainer = EsMccfrTrainer::new(game, FIXED_SEED);
        let mut rng = ChaCha20Rng::from_seed(FIXED_SEED);
        for _ in 0..1_000_000u64 {
            trainer.step(&mut rng).expect("HU step");
        }
        let path = unique_temp_path(&format!("hu_1m_run_{run}"));
        trainer.save_checkpoint(&path).expect("HU save_checkpoint");
        let bytes = std::fs::read(&path).expect("re-read");
        cleanup(&path);
        let trailer_start = bytes.len() - TRAILER_LEN;
        let trailer: [u8; 32] = bytes[trailer_start..].try_into().unwrap();
        hashes.push(trailer);
    }
    assert_eq!(
        hashes[0], hashes[1],
        "D-416：HU 退化 NlheGame6 1M update × 3 BLAKE3 byte-equal（run 0 vs run 1）"
    );
    assert_eq!(
        hashes[1], hashes[2],
        "D-416：HU 退化 NlheGame6 1M update × 3 BLAKE3 byte-equal（run 1 vs run 2）"
    );
    eprintln!(
        "HU degenerate 1M × 3 BLAKE3 byte-equal ✓ trailer = {}",
        blake3_hex(&hashes[0])
    );
}

// ===========================================================================
// Group B — 跨版本拒绝（test 4-7）
// ===========================================================================

/// Test 4 — D-449：跨版本 schema 1 → 2 不兼容。stage 3 schema_version=1
/// checkpoint 加载到 stage 4 NlheGame6 trainer → SchemaMismatch 拒绝。
///
/// 走 byte-craft + `Checkpoint::open`：构造一个 stage 3 schema=1 + Kuhn fixture
/// （schema=1 + VanillaCfr + Kuhn）→ 在当前 SCHEMA_VERSION=2（D2 落地后）路径
/// 上 `Checkpoint::open` 应返 `SchemaMismatch { expected=2, got=1 }`。
///
/// **D2 落地前**：`SCHEMA_VERSION=1`，`Checkpoint::open` 见 schema=1 直接通过，
/// 不返 SchemaMismatch → panic-fail。D2 bump 1→2 后转绿。
#[test]
#[ignore = "§D2-revM 2026-05-15（stage 4 D2 \\[实现\\] dispatch carve-out）：用户授权 Option A — Checkpoint::open 走 v1/v2 dispatch（接受两个 schema 版本）让 stage 3 既有 corruption / round-trip / warmup 测试套件全部 byte-equal 维持。本 test 期望 schema=1 文件被 Checkpoint::open 严格拒绝，但 dispatch 路径下 schema=1 走 v1 parse 合法 → 与 stage 3 兼容性政策不可同时满足。stage 3 ↔ stage 4 跨版本拒绝改由 Trainer::load_checkpoint 内置 `ensure_trainer_schema` preflight 落地（VanillaCfr/EsMccfr expected=1 / EsMccfrLinearRmPlus expected=2）；test 5 (`stage4_byte_crafted_schema_v2_file_rejected_by_stage3_kuhn_trainer_dispatch`) 字面继续覆盖该 trainer-level dispatch。本 test 留待 §D2-revM 后续 re-author（如走 Trainer::load_checkpoint dispatch 路径 / 或 byte-craft schema > 2 unsupported file）。详 `docs/pluribus_stage4_workflow.md` §D2 修订历史。"]
fn stage3_schema_v1_kuhn_checkpoint_rejected_by_stage4_with_schema_mismatch() {
    // 走 stage 3 Kuhn trainer 真实 save 写 schema=1 文件（artifact-free）。
    let mut trainer = VanillaCfrTrainer::new(KuhnGame, FIXED_SEED);
    let mut rng = ChaCha20Rng::from_seed(FIXED_SEED);
    for _ in 0..5u64 {
        trainer.step(&mut rng).expect("Kuhn step");
    }
    let path = unique_temp_path("schema_v1_kuhn");
    trainer.save_checkpoint(&path).expect("Kuhn save");
    let result = Checkpoint::open(&path);
    cleanup(&path);

    let err = result.expect_err(
        "D-449：当 stage 4 SCHEMA_VERSION=2 时，schema=1 文件 Checkpoint::open 应返 Err",
    );
    match err {
        CheckpointError::SchemaMismatch { expected, got } => {
            assert_eq!(expected, SCHEMA_V2, "D-449：stage 4 expected schema_version=2");
            assert_eq!(got, 1, "stage 3 文件 schema_version=1");
        }
        other => panic!(
            "D-449：期望 SchemaMismatch {{ expected=2, got=1 }}，实际 {other:?}（D2 \\[实现\\] 起步前 SCHEMA_VERSION 仍为 1 让 Checkpoint::open 通过 → panic-fail）"
        ),
    }
}

/// Test 5 — D-449：跨版本 schema 2 → 1 不兼容。stage 4 byte-crafted schema=2
/// 文件加载到 stage 3 path → SchemaMismatch。
///
/// 由于 stage 3 `Checkpoint::open` 期望 schema=1（与 SCHEMA_VERSION 字面），
/// 构造 v2 文件后调 `Checkpoint::open` 应在 D2 落地后（SCHEMA_VERSION=2）反而通过 —
/// 该测试构造的语义在 stage 4 下意指 "stage 3 binary 读 stage 4 文件" 的拒绝路径。
/// D1 \[测试\] 用 byte-flip schema=1 → 2 模拟 "stage 4 文件" 让 `Checkpoint::open`
/// 在 stage 3 SCHEMA_VERSION=1 处拒绝（panic-fail：当前路径下 schema=2 byte-flip
/// 后由 `parse_bytes` 走 trailer BLAKE3 fail → Corrupted 而非 SchemaMismatch）。
///
/// **D2 落地后**：SCHEMA_VERSION=2，本测试构造的 schema=2 文件 byte-craft 走
/// 合法 trailer BLAKE3 路径 → 该字节序列在 stage 3 binary 读时（独立测试 binary）
/// 拒绝；但本进程 SCHEMA_VERSION=2 让 v2 文件合法 → 我们 byte-flip 到 v3 (schema=3)
/// 模拟 "未来 stage 5 文件" 验证 stage 4 拒绝路径。
///
/// 简化形态：构造 schema=2 + bucket_table_blake3=[0;32]（Kuhn 字面 game=Kuhn）
/// 文件 → `Checkpoint::open` 路径上：D2 落地前 SchemaMismatch；D2 落地后走 trainer
/// (EsMccfrLinearRmPlus=2, Nlhe6Max=3) tag round-trip 路径通过 → 但 Kuhn 路径
/// （trainer=0, game=0）schema=2 字面冲突（v2 文件不允许 Kuhn）走 Corrupted /
/// TrainerMismatch（具体由 D2 落地策略决定）。本测试断言落 5 类已知 variant 任一。
#[test]
fn stage4_byte_crafted_schema_v2_file_rejected_by_stage3_kuhn_trainer_dispatch() {
    let bytes = craft_minimal_v2_checkpoint_bytes(
        TrainerVariant::VanillaCfr as u8,
        GameVariant::Kuhn as u8,
        N_TRAVERSER_STAGE3,
        0,
        0,
        0,
        [0u8; 32],
    );
    let path = write_tmp(&bytes, "schema_v2_kuhn_crafted");

    // 通过 stage 3 Kuhn trainer load_checkpoint 走 preflight + parse_bytes 双
    // 路径。stage 3 SCHEMA_VERSION=1 + stage 4 文件 schema=2 → preflight 见
    // schema≠1 通过；parse_bytes 见 schema=2 ≠ SCHEMA_VERSION=1 → SchemaMismatch。
    let result =
        <VanillaCfrTrainer<KuhnGame> as Trainer<KuhnGame>>::load_checkpoint(&path, KuhnGame);
    cleanup(&path);

    let err = match result {
        Ok(_) => {
            panic!("D-449：stage 4 byte-crafted schema=2 文件加载到 stage 3 Kuhn trainer 应 Err")
        }
        Err(e) => e,
    };
    match &err {
        CheckpointError::SchemaMismatch { expected, got } => {
            assert_eq!(*got, SCHEMA_V2, "D-449：got = stage 4 schema=2");
            assert_eq!(*expected, 1, "stage 3 expected = 1");
        }
        CheckpointError::TrainerMismatch { .. }
        | CheckpointError::Corrupted { .. }
        | CheckpointError::FileNotFound { .. }
        | CheckpointError::BucketTableMismatch { .. } => {
            panic!(
                "D-449：期望 SchemaMismatch {{ expected=1, got=2 }}，实际 {err:?}\
                （D2 \\[实现\\] 落地后 SCHEMA_VERSION=2 让本测试 byte-craft 的 schema=2 \
                 文件通过 schema 校验，本测试形态需 D2-后续 carve-out 重新设计）"
            );
        }
    }
}

/// Test 6 — D-449：traverser_count=1 → 6 不兼容。stage 4 byte-crafted
/// schema=2 + traverser_count=1（stage 3 字面）→ stage 4 NlheGame6 trainer
/// (期望 traverser_count=6) 加载应触发拒绝。
///
/// **D2 落地前**：`NlheGame6` 构造依赖 v3 artifact + `EsMccfrTrainer::load_checkpoint`
/// preflight 不校验 traverser_count（stage 3 字段不存在）→ 当前 byte-craft 文件
/// `Checkpoint::open` 直接返 SchemaMismatch（schema=2 ≠ SCHEMA_VERSION=1）→
/// panic-fail 在断言 "traverser_count mismatch" 上。D2 落地 v2 trainer_variant
/// (EsMccfrLinearRmPlus, Nlhe6Max) + traverser_count 校验后转绿（具体新 variant
/// 由 D2 \[实现\] 定义）。
///
/// release/ignored：依赖 v3 artifact 构造 NlheGame6 trainer。
#[test]
#[ignore = "release/--ignored opt-in（v3 artifact 依赖；D2 \\[实现\\] 落地后转绿）"]
fn traverser_count_1_vs_6_mismatch_rejected_by_nlhe6_trainer() {
    let Some(table) = load_v3_artifact_arc_or_skip() else {
        return;
    };
    let game = NlheGame6::new(Arc::clone(&table)).expect("NlheGame6::new");
    let expected_blake3 = <NlheGame6 as Game>::bucket_table_blake3(&game);

    // byte-craft：traverser_count=1（stage 3 字面），其他字段 stage 4 NlheGame6
    // 合法（trainer=EsMccfrLinearRmPlus, game=Nlhe6Max, schema=2, blake3 匹配）。
    let bytes = craft_minimal_v2_checkpoint_bytes(
        TrainerVariant::EsMccfrLinearRmPlus as u8,
        GameVariant::Nlhe6Max as u8,
        N_TRAVERSER_STAGE3, // traverser_count=1 字面冲突
        1,
        1,
        0,
        expected_blake3,
    );
    let path = write_tmp(&bytes, "traverser_count_mismatch");

    let result = <EsMccfrTrainer<NlheGame6> as Trainer<NlheGame6>>::load_checkpoint(&path, game);
    cleanup(&path);

    let err = match result {
        Ok(_) => {
            panic!("D-449：traverser_count=1 vs NlheGame6 期望=6 不兼容应触发 load_checkpoint Err")
        }
        Err(e) => e,
    };
    // D2 \[实现\] 落地后预期走 Corrupted（reason 提及 traverser_count）或
    // 新增 variant；具体由 D2 决定，本断言只锁 5 类 known variant 任一。
    match err {
        CheckpointError::SchemaMismatch { .. }
        | CheckpointError::TrainerMismatch { .. }
        | CheckpointError::Corrupted { .. }
        | CheckpointError::FileNotFound { .. }
        | CheckpointError::BucketTableMismatch { .. } => {
            // ok — 拒绝路径，D2 落地后字面细化
        }
    }
}

/// Test 7 — D-449：schema 2 文件被 stage 4 NlheGame6 trainer 接受（正向路径）。
///
/// byte-craft 一个 valid v2 文件（trainer=EsMccfrLinearRmPlus, game=Nlhe6Max,
/// traverser_count=6, blake3 匹配 v3 artifact）→ stage 4 trainer
/// `load_checkpoint` 应成功。
///
/// **D2 落地前**：`SCHEMA_VERSION=1` + stage 3 path `EsMccfrTrainer::load_checkpoint`
/// 走 preflight + parse_bytes；preflight 见 schema=2 ≠ 1 通过；parse_bytes
/// 见 schema=2 ≠ SCHEMA_VERSION=1 → SchemaMismatch → load_checkpoint 返
/// Err（本测试 expect Ok → panic-fail）。D2 落地后转绿。
///
/// release/ignored：v3 artifact 依赖。
#[test]
#[ignore = "release/--ignored opt-in（v3 artifact 依赖 + 空 body byte-craft；D2 \\[实现\\] 落地后转绿）"]
fn schema_v2_byte_crafted_valid_file_accepted_by_stage4_nlhe6_trainer() {
    let Some(table) = load_v3_artifact_arc_or_skip() else {
        return;
    };
    let game = NlheGame6::new(Arc::clone(&table)).expect("NlheGame6::new");
    let expected_blake3 = <NlheGame6 as Game>::bucket_table_blake3(&game);
    let bytes = craft_minimal_v2_checkpoint_bytes(
        TrainerVariant::EsMccfrLinearRmPlus as u8,
        GameVariant::Nlhe6Max as u8,
        N_TRAVERSER_STAGE4,
        1,
        1,
        1,
        expected_blake3,
    );
    let path = write_tmp(&bytes, "schema_v2_valid");

    let result = <EsMccfrTrainer<NlheGame6> as Trainer<NlheGame6>>::load_checkpoint(&path, game);
    cleanup(&path);
    let _trainer = result.unwrap_or_else(|e| {
        panic!(
            "D-449：valid v2 file（schema=2 + EsMccfrLinearRmPlus + Nlhe6Max + traverser_count=6 \
            + blake3 匹配）应被 NlheGame6 trainer 加载成功（D2 \\[实现\\] 落地后），实际 Err({e:?})"
        )
    });
}

// ===========================================================================
// Group C — config 字段 mismatch 拒绝（test 8-10）
// ===========================================================================

/// Test 8 — D-449：linear_weighting_enabled mismatch 拒绝。
///
/// byte-craft v2 文件 linear_weighting=0（stage 3 path） + trainer=EsMccfrLinearRmPlus
/// → 字段冲突；NlheGame6 trainer load_checkpoint 应拒绝（具体 variant 由 D2 决定）。
#[test]
#[ignore = "release/--ignored opt-in（v3 artifact 依赖；D2 \\[实现\\] 落地后转绿）"]
fn linear_weighting_enabled_mismatch_rejected() {
    let Some(table) = load_v3_artifact_arc_or_skip() else {
        return;
    };
    let game = NlheGame6::new(Arc::clone(&table)).expect("NlheGame6::new");
    let expected_blake3 = <NlheGame6 as Game>::bucket_table_blake3(&game);
    // linear_weighting=0 但 trainer_variant=EsMccfrLinearRmPlus → D-449
    // 字面冲突（trainer variant 要求 linear_weighting=1）。
    let bytes = craft_minimal_v2_checkpoint_bytes(
        TrainerVariant::EsMccfrLinearRmPlus as u8,
        GameVariant::Nlhe6Max as u8,
        N_TRAVERSER_STAGE4,
        0, // linear_weighting=0 冲突
        1,
        1,
        expected_blake3,
    );
    let path = write_tmp(&bytes, "linear_weighting_mismatch");
    let result = <EsMccfrTrainer<NlheGame6> as Trainer<NlheGame6>>::load_checkpoint(&path, game);
    cleanup(&path);
    match result {
        Ok(_) => panic!(
            "D-449：linear_weighting=0 + EsMccfrLinearRmPlus trainer 字段冲突应拒绝（D2 \\[实现\\] 起步前 SCHEMA_VERSION=1 让本断言 panic-fail）"
        ),
        Err(_) => { /* 5 类 variant 任一 ok */ }
    }
}

/// Test 9 — D-449：rm_plus_enabled mismatch 拒绝（对称 Test 8）。
#[test]
#[ignore = "release/--ignored opt-in（v3 artifact 依赖；D2 \\[实现\\] 落地后转绿）"]
fn rm_plus_enabled_mismatch_rejected() {
    let Some(table) = load_v3_artifact_arc_or_skip() else {
        return;
    };
    let game = NlheGame6::new(Arc::clone(&table)).expect("NlheGame6::new");
    let expected_blake3 = <NlheGame6 as Game>::bucket_table_blake3(&game);
    let bytes = craft_minimal_v2_checkpoint_bytes(
        TrainerVariant::EsMccfrLinearRmPlus as u8,
        GameVariant::Nlhe6Max as u8,
        N_TRAVERSER_STAGE4,
        1,
        0, // rm_plus=0 冲突
        1,
        expected_blake3,
    );
    let path = write_tmp(&bytes, "rm_plus_mismatch");
    let result = <EsMccfrTrainer<NlheGame6> as Trainer<NlheGame6>>::load_checkpoint(&path, game);
    cleanup(&path);
    if result.is_ok() {
        panic!("D-449：rm_plus=0 + EsMccfrLinearRmPlus trainer 字段冲突应拒绝");
    }
}

/// Test 10 — D-449：warmup_complete=0 → 1 边界恢复（save 时 warmup 未完成 →
/// load 后 trainer state 一致，可继续走 stage 3 path step；下一 step 跨边界
/// 转 stage 4 path）。
///
/// **D2 落地前**：v2 schema 不可用 → save 写 stage 3 schema=1 文件 → load 不读
/// warmup_complete 字段 → 断言 `loaded.config.warmup_complete_at == 1`（D2
/// 落地后 from-header 字段）panic-fail。
#[test]
#[ignore = "release/--ignored opt-in（v3 artifact 依赖；D2 \\[实现\\] 落地后转绿）"]
fn warmup_complete_zero_to_one_boundary_recovery() {
    let Some(table) = load_v3_artifact_arc_or_skip() else {
        return;
    };
    // warmup_at=1 让前 1 step 走 stage 3 path，第 2 step 起切 Linear+RM+；save 在
    // step 1 边界（update_count=1，warmup_complete=1，恰好刚切换完成）。
    let game = NlheGame6::new(Arc::clone(&table)).expect("NlheGame6::new");
    let mut trainer = EsMccfrTrainer::new(game, FIXED_SEED).with_linear_rm_plus(1);
    let mut rng = ChaCha20Rng::from_seed(FIXED_SEED);
    trainer.step(&mut rng).expect("step 1");
    let path = unique_temp_path("warmup_boundary");
    trainer
        .save_checkpoint(&path)
        .expect("save @ warmup boundary");
    let ckpt = Checkpoint::open(&path).unwrap_or_else(|e| {
        panic!("Checkpoint::open 应成功（D2 \\[实现\\] 落地后），实际 Err({e:?})")
    });
    cleanup(&path);
    assert_eq!(
        ckpt.update_count, 1,
        "warmup 边界 save → load update_count = 1"
    );
    assert_eq!(
        ckpt.schema_version, SCHEMA_V2,
        "D-449：warmup boundary save 应写 schema_version=2"
    );
}

// ===========================================================================
// Group D — 5 类 CheckpointError v2 byte-flip + bucket_table_blake3 mismatch
// （test 11-15）
// ===========================================================================

/// Test 11 — D-352：v2 trailer BLAKE3 corruption (body byte flip)。
///
/// 构造 minimal v2 checkpoint → 翻 body 中段 1 byte → trailer BLAKE3 mismatch
/// → `Checkpoint::open` 返 `Corrupted`。
///
/// **D2 落地前**：`Checkpoint::open` 在 schema=2 处直接 SchemaMismatch（先于
/// trailer BLAKE3 校验）→ 期望 Corrupted panic-fail。D2 落地后转绿。
#[test]
fn v2_body_byte_flip_returns_corrupted_after_trailer_blake3_check() {
    let mut bytes = craft_minimal_v2_checkpoint_bytes(
        TrainerVariant::EsMccfrLinearRmPlus as u8,
        GameVariant::Nlhe6Max as u8,
        N_TRAVERSER_STAGE4,
        1,
        1,
        1,
        [0xCDu8; 32],
    );
    // 翻一个 trailer-外的 byte（header 内 OFFSET_UPDATE_COUNT 区域）让 trailer
    // BLAKE3 mismatch；不直接翻 schema_version / magic / trainer_variant 避免
    // 走更早的 dispatch 路径。
    bytes[OFFSET_UPDATE_COUNT] ^= 0xFF;
    let path = write_tmp(&bytes, "v2_body_flip");
    let result = Checkpoint::open(&path);
    cleanup(&path);
    let err = result.expect_err("v2 body byte-flip 必须 Err");
    match err {
        CheckpointError::Corrupted { reason, .. } => {
            let r = reason.to_lowercase();
            assert!(
                r.contains("blake3") || r.contains("trailer") || r.contains("hash") || r.contains("body"),
                "D-352：reason 应提及 BLAKE3/trailer/hash/body，实际：{reason}"
            );
        }
        CheckpointError::SchemaMismatch { .. } => panic!(
            "D-352：期望 Corrupted（trailer BLAKE3 mismatch），实际 SchemaMismatch — D2 \\[实现\\] 落地前 SCHEMA_VERSION=1 让 schema=2 文件先走 SchemaMismatch dispatch；D2 落地后转绿"
        ),
        other => panic!("D-352：期望 Corrupted(trailer BLAKE3)，实际 {other:?}"),
    }
}

/// Test 12 — D-350：v2 magic corruption → Corrupted(magic)。
///
/// **D2 落地前**：与 Test 11 同型，schema=2 文件先走 SchemaMismatch → panic-fail。
#[test]
fn v2_magic_byte_flip_returns_corrupted_magic() {
    let mut bytes = craft_minimal_v2_checkpoint_bytes(
        TrainerVariant::EsMccfrLinearRmPlus as u8,
        GameVariant::Nlhe6Max as u8,
        N_TRAVERSER_STAGE4,
        1,
        1,
        1,
        [0u8; 32],
    );
    // 翻 magic 字面，让 magic 校验在最前面 fail
    for b in &mut bytes[OFFSET_MAGIC..OFFSET_SCHEMA_VERSION] {
        *b = 0xFF;
    }
    // 翻 magic 后 trailer 也必须重新计算（让 magic 错误能在 BLAKE3 trailer 校验
    // 之前命中 — D2 落地后 magic 校验在 trailer 之前；D2 落地前同样在
    // parse_bytes 顺序 1 处校验）。
    let body_end = HEADER_V2_LEN;
    let mut hasher = Hasher::new();
    hasher.update(&bytes[..body_end]);
    let trailer: [u8; 32] = hasher.finalize().into();
    bytes[body_end..body_end + TRAILER_LEN].copy_from_slice(&trailer);

    let path = write_tmp(&bytes, "v2_magic_flip");
    let result = Checkpoint::open(&path);
    cleanup(&path);
    let err = result.expect_err("v2 magic flip 必须 Err");
    match err {
        CheckpointError::Corrupted { reason, .. } => {
            assert!(
                reason.to_lowercase().contains("magic"),
                "D-350：reason 应提及 'magic'，实际：{reason}"
            );
        }
        other => panic!("期望 Corrupted(magic)，实际 {other:?}"),
    }
}

/// Test 13 — D-352：v2 trailer 直接翻（vs Test 11 body 翻）。
#[test]
fn v2_trailer_direct_flip_returns_corrupted() {
    let mut bytes = craft_minimal_v2_checkpoint_bytes(
        TrainerVariant::EsMccfrLinearRmPlus as u8,
        GameVariant::Nlhe6Max as u8,
        N_TRAVERSER_STAGE4,
        1,
        1,
        1,
        [0u8; 32],
    );
    let len = bytes.len();
    // 翻 trailer 中段 1 byte（位于 [len-TRAILER_LEN, len)）
    bytes[len - TRAILER_LEN + 5] ^= 0xA5;
    let path = write_tmp(&bytes, "v2_trailer_flip");
    let result = Checkpoint::open(&path);
    cleanup(&path);
    let err = result.expect_err("v2 trailer flip 必须 Err");
    match err {
        CheckpointError::Corrupted { .. } => { /* ok */ }
        CheckpointError::SchemaMismatch { .. } => panic!(
            "D-352：期望 Corrupted（trailer flip），实际 SchemaMismatch — D2 \\[实现\\] 落地后转绿"
        ),
        other => panic!("期望 Corrupted(trailer)，实际 {other:?}"),
    }
}

/// Test 14 — D-356：v2 bucket_table_blake3 mismatch。
///
/// byte-craft v2 + bucket_table_blake3 = [0xAA; 32]；NlheGame6 trainer 期望
/// blake3 = v3 anchor (`67ee5554...`) → BucketTableMismatch。
#[test]
#[ignore = "release/--ignored opt-in（v3 artifact 依赖；D2 \\[实现\\] 落地后转绿）"]
fn v2_bucket_table_blake3_mismatch_rejected() {
    let Some(table) = load_v3_artifact_arc_or_skip() else {
        return;
    };
    let game = NlheGame6::new(Arc::clone(&table)).expect("NlheGame6::new");
    let bytes = craft_minimal_v2_checkpoint_bytes(
        TrainerVariant::EsMccfrLinearRmPlus as u8,
        GameVariant::Nlhe6Max as u8,
        N_TRAVERSER_STAGE4,
        1,
        1,
        1,
        [0xAAu8; 32], // 故意错误 blake3
    );
    let path = write_tmp(&bytes, "v2_bucket_blake3_mismatch");
    let result = <EsMccfrTrainer<NlheGame6> as Trainer<NlheGame6>>::load_checkpoint(&path, game);
    cleanup(&path);
    let err = match result {
        Ok(_) => panic!("v2 bucket_table_blake3 mismatch 必须 Err"),
        Err(e) => e,
    };
    match err {
        CheckpointError::BucketTableMismatch { expected, got } => {
            assert_eq!(got, [0xAAu8; 32], "got 应回填 byte-flip 后的 raw blake3");
            assert_ne!(expected, [0u8; 32], "expected 应是 v3 anchor 非零");
        }
        CheckpointError::SchemaMismatch { .. } => {
            panic!("期望 BucketTableMismatch，实际 SchemaMismatch — D2 \\[实现\\] 落地后转绿")
        }
        other => panic!("期望 BucketTableMismatch，实际 {other:?}"),
    }
}

/// Test 15 — D-353：v2 atomic write tempfile 路径不残留 `.tmp` 中间产物 sanity。
///
/// 走 `save_checkpoint` 落到目标 path → `<path>.tmp` 与 sibling 残留必须不存在
/// （`tempfile::persist` 成功后 `.tmp` 已 rename 走）。
///
/// **D2 落地前**：stage 3 save_checkpoint 路径已落地 atomic rename（D-353），
/// 本测试在 stage 3 path 路径上默认 active pass；D2 落地 v2 schema 后 atomic
/// rename 路径同型继承，本测试不退化。
#[test]
fn v2_atomic_write_no_temp_residue_sanity() {
    let mut trainer = VanillaCfrTrainer::new(KuhnGame, FIXED_SEED);
    let mut rng = ChaCha20Rng::from_seed(FIXED_SEED);
    for _ in 0..5u64 {
        trainer.step(&mut rng).expect("Kuhn step");
    }
    let path = unique_temp_path("atomic_no_residue");
    trainer.save_checkpoint(&path).expect("save");
    assert!(path.exists(), "save_checkpoint 后目标路径应存在");
    let mut tmp_path = path.as_os_str().to_owned();
    tmp_path.push(".tmp");
    let tmp_path = PathBuf::from(tmp_path);
    assert!(
        !tmp_path.exists(),
        "D-353：tempfile persist 成功后 `.tmp` sibling 不应残留"
    );
    cleanup(&path);
}

// ===========================================================================
// Group E — 6-traverser regret / strategy serialization + cross-load
// equivalence + regret_offset / strategy_offset 计算（test 16-18）
// ===========================================================================

/// Test 16 — D-412 / D-445：6-traverser regret_table BLAKE3 byte-equal across
/// repeated saves of same trainer state（fixed seed determinism）。
///
/// 走 NlheGame6 + Linear+RM+ trainer → save 两次 → 文件 BLAKE3 trailer 应一致
/// （D-327 sorted-by-Debug encoding + D-353 atomic rename 决定性）。
///
/// **D2 落地前**：单 RegretTable 路径 save 两次本就 byte-equal（stage 3 既有
/// 不变量）；本测试在 D2 落地 6-traverser RegretTable 数组 + bincode 序列化后
/// 仍 byte-equal — 实际 panic-fail trigger 在 schema_version=2 字面（Test 1）。
/// 本 test 通过 default-active 实际验证 6-traverser save → save byte-equal
/// determinism 不被 D2 改动破坏。
#[test]
#[ignore = "release/--ignored opt-in（v3 artifact 依赖；C2 commit single-table 路径已 byte-equal，D2 落地 6-table 后维持）"]
fn six_traverser_regret_table_save_save_blake3_byte_equal() {
    let Some(table) = load_v3_artifact_arc_or_skip() else {
        return;
    };
    let trainer = run_nlhe6_linear_rm_plus_trainer(Arc::clone(&table), SMOKE_STEPS, 0);

    let path1 = unique_temp_path("6trav_save1");
    let path2 = unique_temp_path("6trav_save2");
    trainer.save_checkpoint(&path1).expect("save 1");
    trainer.save_checkpoint(&path2).expect("save 2");
    let bytes1 = std::fs::read(&path1).expect("read 1");
    let bytes2 = std::fs::read(&path2).expect("read 2");
    cleanup(&path1);
    cleanup(&path2);

    assert_eq!(
        bytes1.len(),
        bytes2.len(),
        "D-445：同 trainer state 两次 save 文件长度 byte-equal"
    );
    assert_eq!(
        bytes1, bytes2,
        "D-445：同 trainer state 两次 save 文件 byte-equal（D-327 sorted-by-Debug encoding）"
    );

    // 校验 trainer state 是 stage 4 v2 schema（panic-fail until D2）
    let schema = u32::from_le_bytes(
        bytes1[OFFSET_SCHEMA_VERSION..OFFSET_TRAINER_VARIANT]
            .try_into()
            .unwrap(),
    );
    assert_eq!(
        schema, SCHEMA_V2,
        "D-449：6-traverser save 应走 schema_version=2 路径"
    );
}

/// Test 17 — D-445：6-traverser cross-load equivalence（save → load → re-save
/// → 文件 byte-equal）。
///
/// 走 NlheGame6 + Linear+RM+ trainer → save → load → re-save → 两个文件应
/// byte-equal（D-445 字面 round-trip 不变量）。
///
/// **D2 落地前**：stage 3 path 走 schema=1 + 单 RegretTable，本测试 panic-fail
/// 在 schema_version=2 字面断言上；D2 落地后转绿。
#[test]
#[ignore = "release/--ignored opt-in（v3 artifact 依赖；D2 \\[实现\\] 落地后转绿）"]
fn six_traverser_save_load_resave_byte_equal_cross_load_equivalence() {
    let Some(table) = load_v3_artifact_arc_or_skip() else {
        return;
    };
    let trainer = run_nlhe6_linear_rm_plus_trainer(Arc::clone(&table), SMOKE_STEPS, 0);
    let path1 = unique_temp_path("cross_load_1");
    trainer.save_checkpoint(&path1).expect("save 1");

    // load → re-save
    let game = NlheGame6::new(Arc::clone(&table)).expect("NlheGame6::new (reload)");
    let loaded = <EsMccfrTrainer<NlheGame6> as Trainer<NlheGame6>>::load_checkpoint(&path1, game)
        .unwrap_or_else(|e| {
            panic!("D-445：v2 load_checkpoint 应成功（D2 \\[实现\\] 落地后），实际 Err({e:?})")
        });
    let path2 = unique_temp_path("cross_load_2");
    loaded.save_checkpoint(&path2).expect("re-save");

    let bytes1 = std::fs::read(&path1).expect("read 1");
    let bytes2 = std::fs::read(&path2).expect("read 2");
    cleanup(&path1);
    cleanup(&path2);

    assert_eq!(
        bytes1, bytes2,
        "D-445：6-traverser save → load → re-save 文件 byte-equal（cross-load equivalence 不变量）"
    );
    assert_eq!(
        loaded.update_count(),
        SMOKE_STEPS,
        "D-445：load 后 update_count 与 save 时一致"
    );
}

/// Test 18 — API-440：v2 header `regret_offset` (offset 96) + `strategy_offset`
/// (offset 104) 字段值合法性（regret_offset == HEADER_V2_LEN = 128；
/// strategy_offset >= regret_offset + min_regret_body；trailer_start = strategy_offset
/// + strategy_body_len）。
///
/// 走 NlheGame6 + Linear+RM+ trainer save → 读 bytes → 校验 v2 header 字面
/// offset 字段值。
///
/// **D2 落地前**：stage 3 path 写 schema=1 + offset 92/100（不是 v2 offset 96/104）
/// → 本测试 panic-fail 在 `bytes[OFFSET_REGRET_OFFSET..]` 读出非期望值。D2 落地
/// 后转绿。
#[test]
#[ignore = "release/--ignored opt-in（v3 artifact 依赖；D2 \\[实现\\] 落地后转绿）"]
fn v2_regret_offset_and_strategy_offset_field_values_correct() {
    let Some(table) = load_v3_artifact_arc_or_skip() else {
        return;
    };
    let trainer = run_nlhe6_linear_rm_plus_trainer(Arc::clone(&table), SMOKE_STEPS, 0);
    let path = unique_temp_path("v2_offsets");
    trainer.save_checkpoint(&path).expect("save");
    let bytes = std::fs::read(&path).expect("re-read");
    cleanup(&path);

    assert!(
        bytes.len() >= HEADER_V2_LEN + TRAILER_LEN,
        "v2 checkpoint 文件长度 {} 应 >= header_v2 ({HEADER_V2_LEN}) + trailer ({TRAILER_LEN})",
        bytes.len()
    );
    let regret_offset = u64::from_le_bytes(
        bytes[OFFSET_REGRET_OFFSET..OFFSET_STRATEGY_OFFSET]
            .try_into()
            .unwrap(),
    );
    let strategy_offset = u64::from_le_bytes(
        bytes[OFFSET_STRATEGY_OFFSET..OFFSET_PAD_B]
            .try_into()
            .unwrap(),
    );
    let total_len = bytes.len() as u64;
    let trailer_start = total_len - TRAILER_LEN as u64;

    assert_eq!(
        regret_offset, HEADER_V2_LEN as u64,
        "API-440：regret_offset 应 == HEADER_V2_LEN (128)"
    );
    assert!(
        strategy_offset >= regret_offset,
        "API-440：strategy_offset ({strategy_offset}) 应 >= regret_offset ({regret_offset})"
    );
    assert!(
        strategy_offset <= trailer_start,
        "API-440：strategy_offset ({strategy_offset}) 应 <= trailer_start ({trailer_start})"
    );

    // pad_a (18..24) 必须全 0（D-449 字面 layout 字段）
    for (i, &b) in bytes[OFFSET_PAD_A..OFFSET_UPDATE_COUNT].iter().enumerate() {
        assert_eq!(b, 0, "API-440：pad_a byte {i} 应 == 0，实际 0x{b:02x}");
    }
    // pad_b (112..128) 必须全 0
    for (i, &b) in bytes[OFFSET_PAD_B..HEADER_V2_LEN].iter().enumerate() {
        assert_eq!(b, 0, "API-440：pad_b byte {i} 应 == 0，实际 0x{b:02x}");
    }
}

// ===========================================================================
// dead_code 抑制 import helper（同 stage 3 D1 \[测试\] 同型）
// ===========================================================================

#[allow(dead_code)]
fn _import_check(_c: Checkpoint, _g: SimplifiedNlheGame) {}
