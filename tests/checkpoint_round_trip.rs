//! 阶段 3 D1 \[测试\]：Checkpoint round-trip BLAKE3 byte-equal + 5 类
//! `CheckpointError` 错误路径 + byte-flip corruption smoke
//! （D-350 / D-351 / D-352 / D-353 / D-354 / D-356）。
//!
//! 五大测试块（`pluribus_stage3_workflow.md` §步骤 D1 line 231-236 字面对应）：
//!
//! 1. **Round-trip BLAKE3 byte-equal**（D-350）：
//!    - `kuhn_vanilla_cfr_save_at_5_iter_resume_5_more_iter_blake3_equal_to_uninterrupted_10_iter`
//!      （default profile active — 5+5 iter Kuhn release `< 1 ms × 2`；dev profile
//!      仍 `< 100 ms`，与 stage 1/2 active 测试同型）
//!    - `leduc_vanilla_cfr_save_at_1k_iter_resume_1k_more_iter_blake3_equal_to_uninterrupted_2k_iter`
//!      （**release ignored** — 1k+1k Leduc release `~10 s × 2` per D-360 60 s SLO）
//!    - `simplified_nlhe_es_mccfr_save_at_1M_update_resume_1M_more_blake3_equal_to_uninterrupted_2M_update`
//!      （**release ignored** — 1M+1M NLHE release `~100 s × 2` per D-361 + v3
//!      artifact 依赖；artifact 缺失走 pass-with-skip）
//!
//! 2. **5 类 `CheckpointError` 错误路径**（D-351，继承 stage 2 `BucketTableError`
//!    `bucket_table_corruption.rs` 模式）：
//!    - `file_not_found_returns_file_not_found_error`（路径不存在）
//!    - `schema_mismatch_via_byte_flip_at_offset_8`（header 偏移 8 schema_version
//!      bump 1 → 2026 触发 D-350 schema 校验拒绝）
//!    - `trainer_mismatch_kuhn_checkpoint_loaded_as_leduc_*`（多 game 不兼容
//!      D-356 / 跨 trainer_variant 不兼容）
//!    - `bucket_table_mismatch_via_byte_flip_at_offset_60`（NLHE 专属，artifact
//!      可用时跑）
//!    - `corrupted_magic_returns_corrupted` + `corrupted_trailer_blake3_returns_corrupted`
//!      + `corrupted_pad_nonzero_returns_corrupted`（多个 Corrupted 子原因路径）
//!
//! 3. **byte-flip smoke**（D-352 trailer BLAKE3 eager 校验抗 single-bit flip）：
//!    - `random_byte_flip_smoke_1k_iter_0_panic_all_err`（default profile active）
//!    - `random_byte_flip_full_100k_iter_0_panic_all_err`（**release ignored**）
//!
//! 4. **变体 exhaustive match**（D-351 5 类闭门枚举编译期 trip-wire）：
//!    枚举 5 variant 的 `match` 闭门写法，未来增加第 6 类变体会让本测试编译期 fail
//!    （强迫同步追加 case，继承 stage 2 同型 trip-wire）。
//!
//! 5. **`api_signatures.rs` 同 commit 锁**：本 commit 同步追加 `CheckpointError`
//!    5 variant 构造 trip-wire（详见 `tests/api_signatures.rs` stage 3 §训练
//!    错误枚举段尾）。
//!
//! **D1 \[测试\] 角色边界**：本文件不修改 `src/training/`；A1 scaffold + B2 + C2
//! 落地后 `Checkpoint::{save, open}` + `Trainer::{save_checkpoint, load_checkpoint}`
//! 仍 `unimplemented!()`，本文件 active 测试在 scaffold 阶段 panic-fail；D2 \[实现\]
//! 落地后转绿。这是 D1 → D2 工程契约的预期形态（与 B1 → B2 / C1 → C2 同型）。

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use blake3::Hasher;
use poker::training::checkpoint::{Checkpoint, MAGIC, SCHEMA_VERSION};
use poker::training::game::{Game, NodeKind};
use poker::training::kuhn::{KuhnGame, KuhnHistory, KuhnInfoSet};
use poker::training::leduc::{LeducGame, LeducInfoSet, LeducState};
use poker::training::nlhe::{SimplifiedNlheGame, SimplifiedNlheState};
use poker::training::sampling::sample_discrete;
use poker::training::{
    CheckpointError, EsMccfrTrainer, GameVariant, RegretTable, StrategyAccumulator, Trainer,
    TrainerVariant, VanillaCfrTrainer,
};
use poker::{BucketTable, ChaCha20Rng, InfoSetId, RngSource};

// ===========================================================================
// 共享常量 + helper
// ===========================================================================

/// fixed master seed — 跨 round-trip / 重复 / fuzz 三类测试共用；让 BLAKE3 byte-
/// equal 可交叉验证（D-362 / D-347 跨 host 不变量）。
const FIXED_SEED: u64 = 0x44_31_4B_55_48_4E_4C_45; // ASCII "D1KUHNLE"

const KUHN_HALF_ITERS: u64 = 5;
const KUHN_FULL_ITERS: u64 = 10;
const LEDUC_HALF_ITERS: u64 = 1_000;
const LEDUC_FULL_ITERS: u64 = 2_000;
const NLHE_HALF_UPDATES: u64 = 1_000_000;
const NLHE_FULL_UPDATES: u64 = 2_000_000;

/// D-350 binary layout offset 锁（与 `pluribus_stage3_api.md` §5 binary schema 表对齐）。
const OFFSET_MAGIC: usize = 0;
const OFFSET_SCHEMA_VERSION: usize = 8;
const OFFSET_TRAINER_VARIANT: usize = 12;
const OFFSET_GAME_VARIANT: usize = 13;
const OFFSET_PAD: usize = 14;
const OFFSET_UPDATE_COUNT: usize = 20;
const OFFSET_RNG_STATE: usize = 28;
const OFFSET_BUCKET_TABLE_BLAKE3: usize = 60;
const OFFSET_REGRET_TABLE_OFFSET: usize = 92;
const OFFSET_STRATEGY_SUM_OFFSET: usize = 100;
const HEADER_LEN: usize = 108;
const TRAILER_LEN: usize = 32;

/// fuzz / 多变体场景共享：byte-flip 不变量约束（D-352 BLAKE3 抗 single-bit flip）。
const FLIP_SMOKE_ITER: usize = 1_000;
const FLIP_FULL_ITER: usize = 100_000;

/// v3 production artifact path（D-314-rev1 lock；NLHE round-trip + BucketTableMismatch 依赖）。
const V3_ARTIFACT_PATH: &str = "artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin";

/// v3 artifact body BLAKE3 ground truth（D-314-rev1 / CLAUDE.md "当前 artifact 基线"）。
const V3_BODY_BLAKE3_HEX: &str = "67ee555439f2c918698650c05f40a7a5e9e812280ceb87fc3c6590add98650cd";

fn unique_temp_path(label: &str) -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let mut p = std::env::temp_dir();
    p.push(format!("poker_d1_ckpt_{label}_{pid}_{nanos}.bin"));
    p
}

fn write_tmp(bytes: &[u8], label: &str) -> PathBuf {
    let path = unique_temp_path(label);
    std::fs::write(&path, bytes).expect("write tmp checkpoint");
    path
}

fn cleanup(path: &Path) {
    let _ = std::fs::remove_file(path);
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp"); // D-353 atomic rename 中间产物（如残留）
    let _ = std::fs::remove_file(PathBuf::from(tmp));
}

/// 跑 `iters` 次 Kuhn Vanilla CFR step；fixed seed 由 [`VanillaCfrTrainer::new`]
/// 内部走 D-335 SplitMix64 派生（同 `cfr_kuhn.rs::train_kuhn_full`）。
fn train_kuhn(master_seed: u64, iters: u64) -> VanillaCfrTrainer<KuhnGame> {
    let mut trainer = VanillaCfrTrainer::new(KuhnGame, master_seed);
    let mut rng = ChaCha20Rng::from_seed(master_seed);
    for _ in 0..iters {
        trainer
            .step(&mut rng)
            .expect("Kuhn Vanilla CFR step 期望成功（D-330 容差仅 warn 不 panic）");
    }
    trainer
}

fn train_leduc(master_seed: u64, iters: u64) -> VanillaCfrTrainer<LeducGame> {
    let mut trainer = VanillaCfrTrainer::new(LeducGame, master_seed);
    let mut rng = ChaCha20Rng::from_seed(master_seed);
    for _ in 0..iters {
        trainer
            .step(&mut rng)
            .expect("Leduc Vanilla CFR step 期望成功");
    }
    trainer
}

/// 加载 v3 artifact 并构造 `SimplifiedNlheGame`；artifact 缺失 / hash 不匹配 →
/// `None`（pass-with-skip 路径，与 `cfr_simplified_nlhe.rs::load_v3_artifact_or_skip`
/// 同型）。
fn load_v3_artifact_or_skip() -> Option<Arc<BucketTable>> {
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
        eprintln!(
            "skip: artifact body BLAKE3 `{body_hex}` 不匹配 v3 ground truth \
             `{V3_BODY_BLAKE3_HEX}`"
        );
        return None;
    }
    Some(Arc::new(table))
}

fn blake3_hex(bytes: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

// ===========================================================================
// BLAKE3 snapshot helpers — 一份 trainer → 一份 32 byte digest（与 cfr_kuhn /
// cfr_leduc / cfr_simplified_nlhe 各自 snapshot 函数同型；本测试不复用同型
// helper 是因为 round-trip 比较两个 trainer 必须共享同样的 enumerate 顺序与
// 编码格式，跨文件耦合度反而抬升）。
// ===========================================================================

fn kuhn_enumerate_info_sets() -> Vec<KuhnInfoSet> {
    let mut out = Vec::with_capacity(12);
    for actor in 0u8..2 {
        let histories: [KuhnHistory; 2] = if actor == 0 {
            [KuhnHistory::Empty, KuhnHistory::CheckBet]
        } else {
            [KuhnHistory::Check, KuhnHistory::Bet]
        };
        for &history in &histories {
            for private_card in [11u8, 12, 13] {
                out.push(KuhnInfoSet {
                    actor,
                    private_card,
                    history,
                });
            }
        }
    }
    out
}

fn kuhn_blake3_snapshot(trainer: &VanillaCfrTrainer<KuhnGame>) -> [u8; 32] {
    let mut hasher = Hasher::new();
    hasher.update(&trainer.update_count().to_le_bytes());
    for info in kuhn_enumerate_info_sets() {
        let avg = trainer.average_strategy(&info);
        let cur = trainer.current_strategy(&info);
        hasher.update(&(avg.len() as u32).to_le_bytes());
        for &p in &avg {
            hasher.update(&p.to_le_bytes());
        }
        hasher.update(&(cur.len() as u32).to_le_bytes());
        for &p in &cur {
            hasher.update(&p.to_le_bytes());
        }
    }
    hasher.finalize().into()
}

fn leduc_blake3_snapshot(trainer: &VanillaCfrTrainer<LeducGame>) -> [u8; 32] {
    let mut hasher = Hasher::new();
    hasher.update(&trainer.update_count().to_le_bytes());
    let mut visited: std::collections::HashSet<LeducInfoSet> = std::collections::HashSet::new();
    let mut rng = ChaCha20Rng::from_seed(0xCAFE_F00D_DEAD_BEEF);
    let root = LeducGame.root(&mut rng);
    leduc_collect_dfs(&root, trainer, &mut visited, &mut hasher, &mut rng);
    hasher.finalize().into()
}

fn leduc_collect_dfs(
    state: &LeducState,
    trainer: &VanillaCfrTrainer<LeducGame>,
    visited: &mut std::collections::HashSet<LeducInfoSet>,
    hasher: &mut Hasher,
    rng: &mut dyn RngSource,
) {
    match LeducGame::current(state) {
        NodeKind::Terminal => {}
        NodeKind::Chance => {
            let dist = LeducGame::chance_distribution(state);
            for (action, _prob) in dist {
                let next_state = LeducGame::next(state.clone(), action, rng);
                leduc_collect_dfs(&next_state, trainer, visited, hasher, rng);
            }
        }
        NodeKind::Player(actor) => {
            let info = LeducGame::info_set(state, actor);
            if visited.insert(info.clone()) {
                let avg = trainer.average_strategy(&info);
                hasher.update(&[info.actor]);
                hasher.update(&[info.private_card]);
                hasher.update(&[info.public_card.unwrap_or(0xFF)]);
                hasher.update(&[info.street as u8]);
                hasher.update(&(info.history.len() as u32).to_le_bytes());
                for a in &info.history {
                    hasher.update(&[*a as u8]);
                }
                hasher.update(&(avg.len() as u32).to_le_bytes());
                for &p in &avg {
                    hasher.update(&p.to_le_bytes());
                }
            }
            let actions = LeducGame::legal_actions(state);
            for action in actions {
                let next_state = LeducGame::next(state.clone(), action, rng);
                leduc_collect_dfs(&next_state, trainer, visited, hasher, rng);
            }
        }
    }
}

/// NLHE snapshot — walk deterministic chance-path 收 InfoSet 序列 + hash
/// avg_strategy（同 `cfr_simplified_nlhe.rs::blake3_avg_strategy_snapshot`
/// 输入端结构，重复以让本文件 self-contained）。
fn nlhe_blake3_snapshot(
    trainer: &EsMccfrTrainer<SimplifiedNlheGame>,
    probes: &[InfoSetId],
) -> [u8; 32] {
    let mut hasher = Hasher::new();
    hasher.update(&trainer.update_count().to_le_bytes());
    hasher.update(&(probes.len() as u64).to_le_bytes());
    for info in probes {
        let strategy = trainer.average_strategy(info);
        hasher.update(&info.raw().to_le_bytes());
        hasher.update(&(strategy.len() as u32).to_le_bytes());
        for &p in &strategy {
            hasher.update(&p.to_le_bytes());
        }
    }
    hasher.finalize().into()
}

fn nlhe_collect_probes(game: &SimplifiedNlheGame) -> Vec<InfoSetId> {
    const PROBE_LIMIT: usize = 64;
    let mut rng = ChaCha20Rng::from_seed(FIXED_SEED.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    let mut state: SimplifiedNlheState = game.root(&mut rng);
    let mut probes = Vec::with_capacity(PROBE_LIMIT);
    for _ in 0..PROBE_LIMIT {
        match SimplifiedNlheGame::current(&state) {
            NodeKind::Terminal => break,
            NodeKind::Chance => {
                let dist = SimplifiedNlheGame::chance_distribution(&state);
                let action = sample_discrete(&dist, &mut rng);
                state = SimplifiedNlheGame::next(state, action, &mut rng);
            }
            NodeKind::Player(actor) => {
                let info = SimplifiedNlheGame::info_set(&state, actor);
                probes.push(info);
                let actions = SimplifiedNlheGame::legal_actions(&state);
                if actions.is_empty() {
                    break;
                }
                state = SimplifiedNlheGame::next(state, actions[0], &mut rng);
            }
        }
    }
    probes
}

// ===========================================================================
// 1. Round-trip BLAKE3 byte-equal（D-350）
// ===========================================================================

/// D-350 Kuhn round-trip：5 iter save → load → 5 more iter，BLAKE3 snapshot 与
/// 不中断 10 iter 完全 byte-equal。
///
/// default profile active：5+5 iter Kuhn release `< 1 ms × 2`；dev profile `<
/// 100 ms`，与 stage 1/2 default-active 测试同型。
#[test]
fn kuhn_vanilla_cfr_save_at_5_iter_resume_5_more_iter_blake3_equal_to_uninterrupted_10_iter() {
    let path = unique_temp_path("kuhn_round_trip");

    // (1) 训练 5 iter → save
    let trainer_first = train_kuhn(FIXED_SEED, KUHN_HALF_ITERS);
    trainer_first
        .save_checkpoint(&path)
        .expect("kuhn save_checkpoint @ 5 iter 期望成功（D-353 atomic rename）");
    assert!(
        path.exists(),
        "save_checkpoint 后文件应存在（atomic rename 终点）"
    );

    // (2) load → 继续 5 iter
    let mut loaded = VanillaCfrTrainer::<KuhnGame>::load_checkpoint(&path, KuhnGame)
        .expect("kuhn load_checkpoint 期望成功（D-352 trailer BLAKE3 + D-350 schema 校验）");
    assert_eq!(
        loaded.update_count(),
        KUHN_HALF_ITERS,
        "load 后 update_count 应等于 save 时的 iter 数"
    );
    let mut rng = ChaCha20Rng::from_seed(FIXED_SEED);
    // 由于 Vanilla CFR full-tree 确定性枚举，外部 rng 不参与 cfv / regret 更新
    // （D-300 详解）；load 后传入 fresh ChaCha20Rng 不破 byte-equal。
    for _ in 0..KUHN_HALF_ITERS {
        loaded.step(&mut rng).expect("kuhn resume step 期望成功");
    }
    assert_eq!(loaded.update_count(), KUHN_FULL_ITERS);
    let resumed_hash = kuhn_blake3_snapshot(&loaded);

    // (3) 不中断 10 iter 对照
    let uninterrupted = train_kuhn(FIXED_SEED, KUHN_FULL_ITERS);
    let uninterrupted_hash = kuhn_blake3_snapshot(&uninterrupted);

    cleanup(&path);
    assert_eq!(
        resumed_hash, uninterrupted_hash,
        "Kuhn round-trip BLAKE3 不一致：D-350 round-trip 不变量被破坏\n  \
         resumed   = {resumed_hash:x?}\n  \
         continuous = {uninterrupted_hash:x?}"
    );
}

/// D-350 Leduc round-trip：1k iter save → load → 1k more iter，与不中断 2k iter
/// byte-equal。
///
/// release ignored：1k+1k Leduc release `~10 s × 2` per D-360 60 s SLO；
/// dev profile `~5 min`，故 release/--ignored opt-in。
#[test]
#[ignore = "release/--ignored opt-in（1k + 1k Leduc Vanilla CFR ~ 20 s release；D2 \\[实现\\] 落地后通过）"]
fn leduc_vanilla_cfr_save_at_1k_iter_resume_1k_more_iter_blake3_equal_to_uninterrupted_2k_iter() {
    let path = unique_temp_path("leduc_round_trip");

    let trainer_first = train_leduc(FIXED_SEED, LEDUC_HALF_ITERS);
    trainer_first
        .save_checkpoint(&path)
        .expect("leduc save_checkpoint @ 1k iter 期望成功");

    let mut loaded = VanillaCfrTrainer::<LeducGame>::load_checkpoint(&path, LeducGame)
        .expect("leduc load_checkpoint 期望成功");
    assert_eq!(loaded.update_count(), LEDUC_HALF_ITERS);
    let mut rng = ChaCha20Rng::from_seed(FIXED_SEED);
    for _ in 0..LEDUC_HALF_ITERS {
        loaded.step(&mut rng).expect("leduc resume step 期望成功");
    }
    assert_eq!(loaded.update_count(), LEDUC_FULL_ITERS);
    let resumed_hash = leduc_blake3_snapshot(&loaded);

    let uninterrupted = train_leduc(FIXED_SEED, LEDUC_FULL_ITERS);
    let uninterrupted_hash = leduc_blake3_snapshot(&uninterrupted);

    cleanup(&path);
    assert_eq!(
        resumed_hash, uninterrupted_hash,
        "Leduc round-trip BLAKE3 不一致：D-350 round-trip 不变量被破坏"
    );
}

/// D-350 简化 NLHE round-trip：1M update save → load → 1M more update，与不中断
/// 2M update byte-equal。
///
/// release ignored：1M+1M NLHE release `~100 s × 2` per D-361 单线程 10K update/s
/// SLO；artifact 缺失 / 不匹配 v3 → pass-with-skip（同 cfr_simplified_nlhe.rs
/// 模式）。
#[test]
#[ignore = "release/--ignored opt-in（1M + 1M simplified NLHE ES-MCCFR ~ 200 s release + v3 artifact 依赖；D2 \\[实现\\] 落地后通过）"]
fn simplified_nlhe_es_mccfr_save_at_1m_update_resume_1m_more_blake3_equal_to_uninterrupted_2m_update(
) {
    let Some(table) = load_v3_artifact_or_skip() else {
        return;
    };
    let path = unique_temp_path("nlhe_round_trip");

    let game1 = SimplifiedNlheGame::new(Arc::clone(&table)).expect("v3 artifact");
    let probes1 = nlhe_collect_probes(&game1);
    let mut trainer_first = EsMccfrTrainer::new(game1, FIXED_SEED);
    let mut rng = ChaCha20Rng::from_seed(FIXED_SEED);
    for _ in 0..NLHE_HALF_UPDATES {
        trainer_first.step(&mut rng).expect("NLHE step 期望成功");
    }
    trainer_first.save_checkpoint(&path).expect(
        "nlhe save_checkpoint @ 1M update 期望成功（D-353 atomic + D-356 bucket_table_blake3）",
    );

    let game2 = SimplifiedNlheGame::new(Arc::clone(&table)).expect("v3 artifact (reload)");
    let mut loaded = EsMccfrTrainer::<SimplifiedNlheGame>::load_checkpoint(&path, game2).expect(
        "nlhe load_checkpoint 期望成功（D-352 trailer BLAKE3 + D-356 bucket_table_blake3 匹配）",
    );
    assert_eq!(loaded.update_count(), NLHE_HALF_UPDATES);
    let mut rng2 = ChaCha20Rng::from_seed(FIXED_SEED);
    for _ in 0..NLHE_HALF_UPDATES {
        loaded.step(&mut rng2).expect("NLHE resume step 期望成功");
    }
    assert_eq!(loaded.update_count(), NLHE_FULL_UPDATES);
    let resumed_hash = nlhe_blake3_snapshot(&loaded, &probes1);

    let game3 = SimplifiedNlheGame::new(Arc::clone(&table)).expect("v3 artifact (uninterrupted)");
    let probes3 = nlhe_collect_probes(&game3);
    assert_eq!(
        probes1, probes3,
        "snapshot probe 序列应跨 trainer 构造 byte-equal（chance-deterministic + first-action）"
    );
    let mut uninterrupted = EsMccfrTrainer::new(game3, FIXED_SEED);
    let mut rng3 = ChaCha20Rng::from_seed(FIXED_SEED);
    for _ in 0..NLHE_FULL_UPDATES {
        uninterrupted
            .step(&mut rng3)
            .expect("NLHE uninterrupted step 期望成功");
    }
    let uninterrupted_hash = nlhe_blake3_snapshot(&uninterrupted, &probes3);

    cleanup(&path);
    assert_eq!(
        resumed_hash, uninterrupted_hash,
        "NLHE round-trip BLAKE3 不一致：D-350 round-trip 不变量被破坏"
    );
}

// ===========================================================================
// 2. 共享 fixture：通过 train + save 产生一份 reference checkpoint bytes
// （继承 stage 2 `bucket_table_corruption.rs::fixture_bytes()` 模式）
// ===========================================================================

static CACHED_KUHN_BYTES: OnceLock<Vec<u8>> = OnceLock::new();
static CACHED_NLHE_BYTES: OnceLock<Option<Vec<u8>>> = OnceLock::new();

fn kuhn_fixture_bytes() -> &'static [u8] {
    CACHED_KUHN_BYTES.get_or_init(|| {
        let trainer = train_kuhn(FIXED_SEED, KUHN_HALF_ITERS);
        let path = unique_temp_path("kuhn_fixture");
        trainer
            .save_checkpoint(&path)
            .expect("save_checkpoint to produce reference bytes");
        let bytes = std::fs::read(&path).expect("re-read of written checkpoint file");
        cleanup(&path);
        bytes
    })
}

fn nlhe_fixture_bytes() -> Option<&'static [u8]> {
    let cached = CACHED_NLHE_BYTES.get_or_init(|| {
        let table = load_v3_artifact_or_skip()?;
        let game = SimplifiedNlheGame::new(table).ok()?;
        // 1K update fixture（足以触发非空 regret_table_body + 真实 bucket_table_blake3）。
        let mut trainer = EsMccfrTrainer::new(game, FIXED_SEED);
        let mut rng = ChaCha20Rng::from_seed(FIXED_SEED);
        for _ in 0..1_000 {
            trainer.step(&mut rng).ok()?;
        }
        let path = unique_temp_path("nlhe_fixture");
        if trainer.save_checkpoint(&path).is_err() {
            return None;
        }
        let bytes = std::fs::read(&path).ok();
        cleanup(&path);
        bytes
    });
    cached.as_deref()
}

/// 5 类错误 exhaustive match — 添加第 6 类变体必须显式同步本函数（编译期 trip-wire）。
fn assert_one_of_five_known_variants(err: &CheckpointError) {
    match err {
        CheckpointError::FileNotFound { .. }
        | CheckpointError::SchemaMismatch { .. }
        | CheckpointError::TrainerMismatch { .. }
        | CheckpointError::BucketTableMismatch { .. }
        | CheckpointError::Corrupted { .. } => { /* known 5 variants */ }
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
// 3. 5 类 CheckpointError 错误路径
// ===========================================================================

// --- (1) FileNotFound ---

#[test]
fn file_not_found_returns_file_not_found_error() {
    let bogus = unique_temp_path("does_not_exist");
    assert!(!bogus.exists(), "fixture sanity: path must not pre-exist");
    let err = open_must_err(&bogus, "open on nonexistent path 必须 Err");
    match err {
        CheckpointError::FileNotFound { path } => {
            assert_eq!(path, bogus, "FileNotFound.path 应回填实际尝试的路径");
        }
        other => panic!("expected FileNotFound, got {other:?}"),
    }
}

// --- (2) SchemaMismatch ---

#[test]
fn schema_mismatch_via_byte_flip_at_offset_8() {
    let mut bytes = kuhn_fixture_bytes().to_vec();
    // 把 schema_version (offset 8 LE u32) 改成 0xDEAD_BEEF（远高于 SCHEMA_VERSION = 1）
    let bs = 0xDEAD_BEEFu32.to_le_bytes();
    bytes[OFFSET_SCHEMA_VERSION..OFFSET_SCHEMA_VERSION + 4].copy_from_slice(&bs);
    let path = write_tmp(&bytes, "schema_mismatch");
    let err = open_must_err(&path, "schema_version 0xDEADBEEF 必须 Err");
    cleanup(&path);

    match err {
        CheckpointError::SchemaMismatch { expected, got } => {
            assert_eq!(expected, SCHEMA_VERSION, "expected 应是当前 SCHEMA_VERSION");
            assert_eq!(got, 0xDEAD_BEEF, "got 应回填 byte-flip 后的 raw 值");
        }
        other => panic!("expected SchemaMismatch, got {other:?}"),
    }
}

// --- (3) TrainerMismatch — game_variant byte flip ---

#[test]
fn trainer_mismatch_kuhn_checkpoint_game_variant_flipped_to_leduc() {
    // 把 Kuhn checkpoint 的 game_variant byte (offset 13) 改成 Leduc，期望 Trainer::
    // load_checkpoint 拒绝（多 game 不兼容 D-356 / D-351 TrainerMismatch）。
    let mut bytes = kuhn_fixture_bytes().to_vec();
    bytes[OFFSET_GAME_VARIANT] = GameVariant::Leduc as u8;
    let path = write_tmp(&bytes, "trainer_mismatch_game");
    let err = match VanillaCfrTrainer::<KuhnGame>::load_checkpoint(&path, KuhnGame) {
        Ok(_) => panic!("Kuhn trainer 加载 game_variant=Leduc 必须 Err"),
        Err(e) => e,
    };
    cleanup(&path);

    match err {
        CheckpointError::TrainerMismatch { expected, got } => {
            assert_eq!(
                expected,
                (TrainerVariant::VanillaCfr, GameVariant::Kuhn),
                "expected = Kuhn trainer 自报变体"
            );
            assert_eq!(
                got,
                (TrainerVariant::VanillaCfr, GameVariant::Leduc),
                "got = checkpoint header 字段（trainer_variant 不变 + game_variant 翻成 Leduc）"
            );
        }
        other => panic!("expected TrainerMismatch (game_variant), got {other:?}"),
    }
}

// --- (3b) TrainerMismatch — trainer_variant byte flip ---

#[test]
fn trainer_mismatch_kuhn_checkpoint_trainer_variant_flipped_to_es_mccfr() {
    let mut bytes = kuhn_fixture_bytes().to_vec();
    bytes[OFFSET_TRAINER_VARIANT] = TrainerVariant::EsMccfr as u8;
    let path = write_tmp(&bytes, "trainer_mismatch_trainer");
    let err = match VanillaCfrTrainer::<KuhnGame>::load_checkpoint(&path, KuhnGame) {
        Ok(_) => panic!("VanillaCfrTrainer 加载 trainer_variant=ESMccfr 必须 Err"),
        Err(e) => e,
    };
    cleanup(&path);

    match err {
        CheckpointError::TrainerMismatch { expected, got } => {
            assert_eq!(expected, (TrainerVariant::VanillaCfr, GameVariant::Kuhn));
            assert_eq!(got, (TrainerVariant::EsMccfr, GameVariant::Kuhn));
        }
        other => panic!("expected TrainerMismatch (trainer_variant), got {other:?}"),
    }
}

// --- (4) BucketTableMismatch — 仅 NLHE 路径触发 ---

#[test]
#[ignore = "release/--ignored opt-in（需 v3 artifact 528 MiB；D2 \\[实现\\] 落地后通过）"]
fn bucket_table_mismatch_via_byte_flip_at_offset_60() {
    let Some(table) = load_v3_artifact_or_skip() else {
        return;
    };
    let Some(reference_bytes) = nlhe_fixture_bytes() else {
        eprintln!("skip: nlhe_fixture_bytes 不可用");
        return;
    };

    // 把 bucket_table_blake3 (offset 60..92) 全部翻成 0xAA，让 BLAKE3 mismatch。
    let mut bytes = reference_bytes.to_vec();
    for b in &mut bytes[OFFSET_BUCKET_TABLE_BLAKE3..OFFSET_BUCKET_TABLE_BLAKE3 + 32] {
        *b = 0xAA;
    }
    let path = write_tmp(&bytes, "bucket_table_mismatch");
    let game = SimplifiedNlheGame::new(table).expect("v3 artifact");
    let err = match EsMccfrTrainer::<SimplifiedNlheGame>::load_checkpoint(&path, game) {
        Ok(_) => panic!("bucket_table_blake3 0xAA×32 必须 Err"),
        Err(e) => e,
    };
    cleanup(&path);

    match err {
        CheckpointError::BucketTableMismatch { expected, got } => {
            assert_eq!(
                got, [0xAA; 32],
                "got 应回填 byte-flip 后的 raw bucket_table_blake3"
            );
            assert_ne!(
                expected, [0; 32],
                "expected 应是当前 BucketTable.content_hash 非零"
            );
        }
        other => panic!("expected BucketTableMismatch, got {other:?}"),
    }
}

// --- (5) Corrupted — magic ---

#[test]
fn corrupted_magic_returns_corrupted() {
    let mut bytes = kuhn_fixture_bytes().to_vec();
    // 把 magic 区全部翻成 0xFF（远不是 b"PLCKPT\0\0"）
    for b in &mut bytes[OFFSET_MAGIC..OFFSET_MAGIC + 8] {
        *b = 0xFF;
    }
    let path = write_tmp(&bytes, "bad_magic");
    let err = open_must_err(&path, "magic 0xFF×8 必须 Err");
    cleanup(&path);

    match err {
        CheckpointError::Corrupted { offset, reason } => {
            assert_eq!(offset, 0, "magic 校验 offset = 0");
            assert!(
                reason.to_lowercase().contains("magic"),
                "Corrupted.reason 应提及 'magic'，实际：{reason}"
            );
        }
        other => panic!("expected Corrupted(magic), got {other:?}"),
    }
}

// --- (5b) Corrupted — pad 非零 ---

#[test]
fn corrupted_pad_nonzero_returns_corrupted() {
    let mut bytes = kuhn_fixture_bytes().to_vec();
    // pad 区 offset 14..20 必须为 0；置 0xAA 触发 Corrupted
    bytes[OFFSET_PAD] = 0xAA;
    let path = write_tmp(&bytes, "pad_nonzero");
    let err = open_must_err(&path, "pad 非零必须 Err");
    cleanup(&path);

    match err {
        CheckpointError::Corrupted { offset, reason } => {
            // offset 字段允许指向 pad 起点（14）或字符级偏移（14）；不严格锁定具体值，
            // 只要落在 [OFFSET_PAD, OFFSET_UPDATE_COUNT) 区间内即可，让 D2 [实现] 保留
            // sub-reason 文案自由度。
            assert!(
                (OFFSET_PAD as u64..OFFSET_UPDATE_COUNT as u64).contains(&offset),
                "Corrupted.offset = {offset} 应落在 pad 区 [{OFFSET_PAD}, {OFFSET_UPDATE_COUNT})"
            );
            assert!(
                reason.to_lowercase().contains("pad"),
                "Corrupted.reason 应提及 'pad'，实际：{reason}"
            );
        }
        other => panic!("expected Corrupted(pad), got {other:?}"),
    }
}

// --- (5c) Corrupted — trailer BLAKE3 mismatch（body byte flip） ---

#[test]
fn corrupted_trailer_blake3_returns_corrupted_via_body_byte_flip() {
    let mut bytes = kuhn_fixture_bytes().to_vec();
    let len = bytes.len();
    assert!(
        len > HEADER_LEN + TRAILER_LEN,
        "Kuhn fixture 应至少包含 header + body + trailer（实际 {len} byte）"
    );
    // 翻 body 中段一个 byte，BLAKE3 trailer 校验失败
    let mid = (HEADER_LEN + len - TRAILER_LEN) / 2;
    bytes[mid] ^= 0xFF;
    let path = write_tmp(&bytes, "trailer_blake3_mismatch");
    let err = open_must_err(
        &path,
        "body byte flip 触发 trailer BLAKE3 mismatch 必须 Err",
    );
    cleanup(&path);

    match err {
        CheckpointError::Corrupted { offset, reason } => {
            // D-352 eager BLAKE3 校验：reason 应包含 "blake3" / "trailer" / "hash" 之一；
            // 不严格锁定文案，留 D2 [实现] 自由度（与 stage 2
            // `corrupted_blake3_trailer_mismatch_returns_corrupted` 同型）。
            let r = reason.to_lowercase();
            assert!(
                r.contains("blake3")
                    || r.contains("trailer")
                    || r.contains("hash")
                    || r.contains("body"),
                "Corrupted.reason 应提及 BLAKE3/trailer/hash/body，实际：{reason}"
            );
            assert!(
                (HEADER_LEN as u64) <= offset,
                "trailer BLAKE3 mismatch 的 offset {offset} 应 ≥ header_len {HEADER_LEN}"
            );
        }
        other => panic!("expected Corrupted(blake3/body), got {other:?}"),
    }
}

// --- (5d) Corrupted — file 过短 ---

#[test]
fn corrupted_file_too_short_returns_corrupted() {
    let bytes = &kuhn_fixture_bytes()[..HEADER_LEN]; // 只剩 header，少 trailer + body
    let path = write_tmp(bytes, "truncated_header_only");
    let err = open_must_err(&path, "文件截到 header 必须 Err");
    cleanup(&path);

    // 截断后可能落 Corrupted 或 SchemaMismatch（取决于 D2 [实现] 校验顺序）；
    // D-351 5 类内任意一类合规（多数实现会落到 Corrupted size 子原因，少数实现先
    // 校验 schema_version 还能读到正确 SCHEMA_VERSION 而落 Corrupted "missing
    // trailer"，本测试不锁定具体 sub-variant，仅保证落在已知 5 类）。
    assert_one_of_five_known_variants(&err);
}

#[test]
fn corrupted_empty_file_returns_corrupted() {
    let path = write_tmp(&[], "empty_file");
    let err = open_must_err(&path, "空文件必须 Err");
    cleanup(&path);
    assert_one_of_five_known_variants(&err);
}

// --- (5e) Corrupted — offset 表越界 ---

#[test]
fn corrupted_regret_table_offset_out_of_range_returns_corrupted() {
    let mut bytes = kuhn_fixture_bytes().to_vec();
    // regret_table_offset 改成 0（小于 HEADER_LEN = 108），触发越界
    let bs = 0u64.to_le_bytes();
    bytes[OFFSET_REGRET_TABLE_OFFSET..OFFSET_REGRET_TABLE_OFFSET + 8].copy_from_slice(&bs);
    let path = write_tmp(&bytes, "regret_offset_zero");
    let err = open_must_err(&path, "regret_table_offset = 0 必须 Err");
    cleanup(&path);
    // 走 Corrupted（offset 越界）路径；若 D2 [实现] 先校验 trailer BLAKE3，会先在
    // BLAKE3 mismatch 路径返回 — 这种情况下 reason 不一定提到 "offset"，因此
    // 仅断言落在已知 5 类。
    assert_one_of_five_known_variants(&err);
}

// ===========================================================================
// 4. byte-flip smoke（D-352 BLAKE3 抗 single-bit flip）
// ===========================================================================

fn run_random_byte_flip(iter_count: usize, seed: u64, label: &str) {
    let base = kuhn_fixture_bytes().to_vec();
    let len = base.len();
    let mut rng = ChaCha20Rng::from_seed(seed);
    let mut err_count = 0u64;
    let mut ok_count = 0u64;

    for i in 0..iter_count {
        let mut bytes = base.clone();
        let pos = (rng.next_u64() as usize) % len;
        let mask = ((rng.next_u64() & 0xFE) | 1) as u8; // mask ∈ {1, 3, 5, ..., 255}
        bytes[pos] ^= mask;

        let path = unique_temp_path(&format!("flip_{label}_{i}_{pos}"));
        std::fs::write(&path, &bytes).expect("write byte-flipped fixture");
        let result = Checkpoint::open(&path);
        cleanup(&path);

        match result {
            Ok(_) => {
                // 极少数情况下 byte-flip 命中 pad 之外恰好"等价"位置 — 不可能因
                // mask != 0；BLAKE3 抗碰撞期望 single-flip 100% 检测。出现 Ok 累
                // 加 counter 但不立即 fail，end 一次性断言。
                ok_count += 1;
            }
            Err(e) => {
                assert_one_of_five_known_variants(&e);
                err_count += 1;
            }
        }
    }

    eprintln!("byte_flip_{label}: iter={iter_count}, err={err_count}, ok={ok_count}");
    assert_eq!(
        ok_count, 0,
        "{label}：byte-flip {ok_count} 次 open 成功 — D-352 BLAKE3 抗 single-bit flip 不变量破坏"
    );
    assert_eq!(err_count as usize, iter_count);
}

#[test]
fn random_byte_flip_smoke_1k_iter_0_panic_all_err() {
    run_random_byte_flip(FLIP_SMOKE_ITER, 0xD1F1_5302_5302_5302, "smoke_1k");
}

#[test]
#[ignore = "release/--ignored opt-in（100k byte-flip × Checkpoint::open ~30 s release；D2 \\[实现\\] 落地后通过）"]
fn random_byte_flip_full_100k_iter_0_panic_all_err() {
    run_random_byte_flip(FLIP_FULL_ITER, 0xD1FF_F100_F100_F100, "full_100k");
}

// ===========================================================================
// 5. 变体 exhaustive match（D-351 5 类闭门枚举编译期 trip-wire）
// ===========================================================================

#[test]
fn checkpoint_error_5_variants_exhaustive_match_lock() {
    // 构造 5 个变体的 minimum sample，让 match 闭门枚举编译期 trip-wire 触发。
    let samples: [CheckpointError; 5] = [
        CheckpointError::FileNotFound {
            path: PathBuf::from("/nonexistent"),
        },
        CheckpointError::SchemaMismatch {
            expected: SCHEMA_VERSION,
            got: 0xDEAD_BEEF,
        },
        CheckpointError::TrainerMismatch {
            expected: (TrainerVariant::VanillaCfr, GameVariant::Kuhn),
            got: (TrainerVariant::EsMccfr, GameVariant::Leduc),
        },
        CheckpointError::BucketTableMismatch {
            expected: [0xAA; 32],
            got: [0xBB; 32],
        },
        CheckpointError::Corrupted {
            offset: 42,
            reason: "test".to_string(),
        },
    ];
    for s in &samples {
        assert_one_of_five_known_variants(s);
    }
    // sanity：Debug fmt 全部不 panic（D-351 5 variant 均 thiserror::Error 实现）
    for s in &samples {
        let _ = format!("{s}");
        let _ = format!("{s:?}");
    }
}

// ===========================================================================
// 6. header 常量字面值锁（D-350 binary layout 头号不变量）
// ===========================================================================

#[test]
fn d350_header_constants_lock() {
    assert_eq!(MAGIC, *b"PLCKPT\0\0", "D-350 magic 字面字节序列锁");
    assert_eq!(SCHEMA_VERSION, 1, "D-350 SCHEMA_VERSION 起步值 = 1");
}

#[test]
fn d350_binary_layout_offsets_lock() {
    // 与 `pluribus_stage3_api.md` §5 binary schema 表对齐；offset 漂移立即在
    // 本测试 fail。
    assert_eq!(OFFSET_MAGIC, 0);
    assert_eq!(OFFSET_SCHEMA_VERSION, 8);
    assert_eq!(OFFSET_TRAINER_VARIANT, 12);
    assert_eq!(OFFSET_GAME_VARIANT, 13);
    assert_eq!(OFFSET_PAD, 14);
    assert_eq!(OFFSET_UPDATE_COUNT, 20);
    assert_eq!(OFFSET_RNG_STATE, 28);
    assert_eq!(OFFSET_BUCKET_TABLE_BLAKE3, 60);
    assert_eq!(OFFSET_REGRET_TABLE_OFFSET, 92);
    assert_eq!(OFFSET_STRATEGY_SUM_OFFSET, 100);
    assert_eq!(HEADER_LEN, 108);
    assert_eq!(TRAILER_LEN, 32);
}

// ===========================================================================
// dead_code 抑制 import helper（同 cfr_kuhn / cfr_leduc / cfr_simplified_nlhe 模式）
// ===========================================================================

#[allow(dead_code)]
fn _import_check(
    _r: RegretTable<KuhnInfoSet>,
    _s: StrategyAccumulator<KuhnInfoSet>,
    _c: Checkpoint,
) {
}
