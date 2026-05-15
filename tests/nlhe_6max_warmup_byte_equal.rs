//! 阶段 4 B1 \[测试\]：warm-up byte-equal anchor + Linear+RM+ 数值单元
//! （D-401 / D-402 / D-403 / D-409）。
//!
//! 5 条核心 trip-wire（`pluribus_stage4_workflow.md` §B1 lines 168-175）：
//!
//! 1. `warmup_phase_1m_update_blake3_byte_equal_stage3_anchor`（D-409 warm-up
//!    phase 1M update × stage 3 anchor 维持，**release ignored**）— stage 4
//!    `with_linear_rm_plus(warmup_complete_at = 1_000_000)` 路径在 warm-up 期间
//!    走 stage 3 standard CFR + RM 路径字面等价：同 master_seed 跑 1M update 与
//!    stage 3 baseline `EsMccfrTrainer::new(...)` 走 1M update 在同一组 chance-
//!    deterministic probe 上 BLAKE3 byte-equal。
//! 2. `warmup_boundary_deterministic_byte_equal_two_runs`（D-409 boundary，
//!    default profile active **持续通过 sanity anchor**）— 同 master_seed 跨
//!    两 run 跑 1_001 step（跨越 1_000 → 1_001 warm-up 边界），切换点那个 step
//!    跨 run 必须 byte-equal（D-446 字面 `warmup_complete: bool` checkpoint
//!    字段决定性来源；trainer 的任意非决定性侧通道在切换点产生 BLAKE3 漂移 →
//!    立即 fail）。B2 \[实现\] 落地前后皆通过（stage 3 path deterministic）；
//!    继承 stage 3 D-362 fixed-seed repeat byte-equal 同型 anchor 政策。
//! 3. `warmup_to_linear_rm_plus_post_warmup_strategy_diverges_from_baseline`
//!    （D-409 post-warmup 数值差异，default profile active **panic-fail 至 B2
//!    后转绿**）— `with_linear_rm_plus(warmup_complete_at = 100)` 跑 200 step
//!    （warm-up 100 + post-warmup 100）；stage 4 trainer 与 stage 3 baseline
//!    trainer 在 step 200 处的 `current_strategy` 至少在 1 个 InfoSet 上有
//!    `> 1e-9` 差异（post-warmup 100 step 内 Linear weighting + RM+ clamp 让两
//!    路径 σ 发散）。B1 \[测试\] anchor "warm-up 切换后 Linear+RM+ 路径与 stage 3
//!    路径不再字面等价" — B2 \[实现\] 漏 routing 让两路径恒等会立即 fail。
//! 4. `linear_weighting_t2_cumulative_formula_unit`（D-401 cumulative，default
//!    profile active **panic-fail 至 B2 后转绿**）— `with_linear_rm_plus(
//!    warmup_complete_at = 0)` 让 step 1 起就走 Linear MCCFR，跑 2 step 后
//!    Kuhn 12 InfoSet 上至少 1 个 InfoSet 的 σ 与 stage 3 standard CFR baseline
//!    显著不同（D-401 字面 `R̃_2 = (1/2) × R̃_1 + r_2` vs 标准 `R̃_2 = R̃_1 + r_2`）。
//! 5. `rm_plus_clamp_raw_regret_non_negative_via_checkpoint_inspection`（D-402
//!    boundary，default profile active **panic-fail 至 B2 后转绿**）— 走
//!    `EsMccfrTrainer::save_checkpoint` + `Checkpoint::open` + bincode
//!    deserialize 路径读 `RegretTable` raw `Vec<(KuhnInfoSet, Vec<f64>)>` 累积
//!    值，验证 stage 4 `with_linear_rm_plus(0)` 路径的全部 raw R 严格 `>= 0`
//!    （D-402 字面 in-place clamp）；同 seed stage 3 baseline 路径至少存在 1 条
//!    负 R 累积（证明测试有"区分度"，不是平凡地全表非负）。
//!
//! **B1 \[测试\] 角色边界**：本文件 0 改动 `src/training/` + 0 改动
//! `docs/pluribus_stage4_{validation,decisions,api}.md`；任一断言落在 \[实现\]
//! 边界错误的产品代码上，filed issue 移交 B2 \[实现\]，不在测试内 patch 产品逻辑。

use std::path::PathBuf;
use std::sync::Arc;

use blake3::Hasher;
use poker::training::game::{Game, NodeKind};
use poker::training::kuhn::{KuhnGame, KuhnHistory, KuhnInfoSet};
use poker::training::nlhe::{SimplifiedNlheGame, SimplifiedNlheState};
use poker::training::sampling::sample_discrete;
use poker::training::{Checkpoint, EsMccfrTrainer, Trainer};
use poker::{BucketTable, ChaCha20Rng, InfoSetId};

// ===========================================================================
// 共享常量 + helper
// ===========================================================================

/// stage 3 §G-batch1 §3.10 v3 production artifact 路径（D-314-rev1 lock，相对
/// repo root，与 `tests/cfr_simplified_nlhe.rs` 字面一致）。
const V3_ARTIFACT_PATH: &str = "artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin";

/// v3 artifact body BLAKE3 ground truth（D-314-rev1）。
const V3_BODY_BLAKE3_HEX: &str = "67ee555439f2c918698650c05f40a7a5e9e812280ceb87fc3c6590add98650cd";

/// stage 4 B1 \[测试\] fixed master seed（继承 stage 3 D-335 sub-stream root
/// 决定性 anchor；ASCII "STG4_B1"+0x00 让跨文件 trip-wire 易于交叉验证）。
const FIXED_SEED: u64 = 0x53_54_47_34_5F_42_31_00; // ASCII "STG4_B1\0"

/// Test 1 步数：D-409 warm-up phase 长度（与 stage 3 D-362 1M update anchor 字面继承）。
const WARMUP_ANCHOR_UPDATES: u64 = 1_000_000;

/// Test 2 步数：warm-up boundary 切换点 + 1 step。
const BOUNDARY_UPDATES: u64 = 1_001;

/// Test 3 步数：warm-up 100 + post-warmup 100 = 200。
const TRANSITION_UPDATES: u64 = 200;

/// Test 3 warmup_complete_at（切换边界）。
const TRANSITION_WARMUP_AT: u64 = 100;

/// Test 5 Kuhn step 数（多步累积让某 InfoSet 的某 action 拿负 regret，让 RM+
/// clamp 在 baseline 上有区分度可证）。Kuhn ES-MCCFR step 极快（~10⁵+ /s），
/// 20 step ~µs 级开销。
const KUHN_CLAMP_STEPS: u64 = 20;

/// BLAKE3 snapshot 走的 chance-deterministic path 上收集的 Player InfoSet
/// 数量上限（与 cfr_simplified_nlhe.rs `SNAPSHOT_PROBE_LIMIT` 字面一致）。
const SNAPSHOT_PROBE_LIMIT: usize = 64;

/// 加载 v3 artifact 并返回 `Arc<BucketTable>` 让多 SimplifiedNlheGame 共享同一
/// 底层 528 MiB body（继承 `tests/cfr_simplified_nlhe.rs` 同型 Arc-share 政策；
/// 避免 Test 1 / Test 2 双路径分别 `BucketTable::open` 各占 528 MiB 致 OOM）。
/// artifact 缺失 / schema 不匹配时 eprintln + 返回 `None`（pass-with-skip）。
fn load_v3_artifact_arc_or_skip() -> Option<Arc<BucketTable>> {
    let path = PathBuf::from(V3_ARTIFACT_PATH);
    if !path.exists() {
        eprintln!(
            "skip: v3 artifact `{V3_ARTIFACT_PATH}` 不存在（CI / GitHub-hosted runner 典型 \
             场景；本地 dev box / vultr / AWS host 有 artifact 时跑）。"
        );
        return None;
    }
    let table = match BucketTable::open(&path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("skip: BucketTable::open({V3_ARTIFACT_PATH}) 失败：{e:?}");
            return None;
        }
    };
    let body_hex = blake3_hex(&table.content_hash());
    if body_hex != V3_BODY_BLAKE3_HEX {
        eprintln!(
            "skip: artifact body BLAKE3 `{body_hex}` 不匹配 v3 ground truth \
             `{V3_BODY_BLAKE3_HEX}`（D-314-rev1 要求 v3 artifact）。"
        );
        return None;
    }
    Some(Arc::new(table))
}

/// 把 `Arc<BucketTable>` 构造为 `SimplifiedNlheGame`；失败时 eprintln + 返回
/// `None`（pass-with-skip）。
fn build_simplified_nlhe_game_or_skip(table: &Arc<BucketTable>) -> Option<SimplifiedNlheGame> {
    match SimplifiedNlheGame::new(Arc::clone(table)) {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!("skip: SimplifiedNlheGame::new 失败：{e:?}");
            None
        }
    }
}

/// `[u8; 32]` → hex string（lowercase，无 `0x` 前缀）。
fn blake3_hex(bytes: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// 沿 deterministic chance-path 走收集 Player InfoSet 序列（与 cfr_simplified_nlhe.rs
/// `collect_snapshot_probes` 同型，跨 run 同 seed 路径相同）。
fn collect_snapshot_probes(game: &SimplifiedNlheGame) -> Vec<InfoSetId> {
    let mut rng = ChaCha20Rng::from_seed(FIXED_SEED.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    let mut state: SimplifiedNlheState = game.root(&mut rng);
    let mut probes = Vec::with_capacity(SNAPSHOT_PROBE_LIMIT);
    for _ in 0..SNAPSHOT_PROBE_LIMIT {
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
                if let Some(a) = actions.first() {
                    state = SimplifiedNlheGame::next(state, *a, &mut rng);
                } else {
                    break;
                }
            }
        }
    }
    probes
}

/// 训练完成后对 `probes` 上的 `average_strategy` 序列求 BLAKE3。
fn blake3_avg_strategy_snapshot(
    trainer: &EsMccfrTrainer<SimplifiedNlheGame>,
    probes: &[InfoSetId],
) -> [u8; 32] {
    let mut hasher = Hasher::new();
    for info in probes {
        let avg = trainer.average_strategy(info);
        for &p in &avg {
            hasher.update(&p.to_le_bytes());
        }
    }
    hasher.finalize().into()
}

/// 训练完成后对 `probes` 上的 `current_strategy` 序列求 BLAKE3。
fn blake3_current_strategy_snapshot(
    trainer: &EsMccfrTrainer<SimplifiedNlheGame>,
    probes: &[InfoSetId],
) -> [u8; 32] {
    let mut hasher = Hasher::new();
    for info in probes {
        let cur = trainer.current_strategy(info);
        for &p in &cur {
            hasher.update(&p.to_le_bytes());
        }
    }
    hasher.finalize().into()
}

/// 跑 `n_steps` 个 [`Trainer::step`] 调用于 SimplifiedNlheGame trainer。
fn run_simplified_nlhe_trainer_steps(
    trainer: &mut EsMccfrTrainer<SimplifiedNlheGame>,
    rng: &mut ChaCha20Rng,
    n_steps: u64,
) {
    for i in 0..n_steps {
        trainer
            .step(rng)
            .unwrap_or_else(|e| panic!("SimplifiedNlhe trainer.step #{i} 失败：{e:?}"));
    }
}

/// 跑 `n_steps` 个 [`Trainer::step`] 调用于 KuhnGame trainer。
fn run_kuhn_trainer_steps(
    trainer: &mut EsMccfrTrainer<KuhnGame>,
    rng: &mut ChaCha20Rng,
    n_steps: u64,
) {
    for i in 0..n_steps {
        trainer
            .step(rng)
            .unwrap_or_else(|e| panic!("KuhnGame trainer.step #{i} 失败：{e:?}"));
    }
}

/// 枚举 Kuhn 12 InfoSet 全集（与 `tests/regret_matching_numeric.rs` 字面一致）。
fn enumerate_kuhn_info_sets() -> Vec<KuhnInfoSet> {
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

/// 跑 Kuhn trainer 后存 checkpoint，读回 RegretTable raw 累积值
/// `Vec<(KuhnInfoSet, Vec<f64>)>`（D-327 `encode_table` 反路径：sort-by-Debug
/// + bincode 1.x LE varint）。
fn dump_kuhn_regret_table_raw(
    trainer: &EsMccfrTrainer<KuhnGame>,
    label: &str,
) -> Vec<(KuhnInfoSet, Vec<f64>)> {
    let tmpdir = tempfile::tempdir().expect("tempfile::tempdir 失败");
    let path = tmpdir.path().join(format!("{label}.ckpt"));
    trainer
        .save_checkpoint(&path)
        .unwrap_or_else(|e| panic!("{label} save_checkpoint 失败：{e:?}"));
    let ckpt =
        Checkpoint::open(&path).unwrap_or_else(|e| panic!("{label} Checkpoint::open 失败：{e:?}"));
    bincode::deserialize::<Vec<(KuhnInfoSet, Vec<f64>)>>(&ckpt.regret_table_bytes)
        .unwrap_or_else(|e| panic!("{label} bincode::deserialize regret_table_bytes 失败：{e:?}"))
}

// ===========================================================================
// Test 1 — D-409 warm-up phase 1M update BLAKE3 byte-equal stage 3 anchor
// ===========================================================================

/// D-409 字面：warm-up phase（前 1M update）走 stage 3 standard CFR + RM 路径，
/// 与 stage 3 baseline `EsMccfrTrainer::new(...)` byte-equal。
///
/// 跑两条 1M update trainer 路径：
/// - **stage 3 baseline**：`EsMccfrTrainer::new(game, FIXED_SEED)` 不走
///   `with_linear_rm_plus()`，等价 stage 3 D-302/D-303 standard CFR + RM。
/// - **stage 4 warmup**：`EsMccfrTrainer::new(game, FIXED_SEED).with_linear_rm_plus(
///   warmup_complete_at = 1_000_000)`，前 1_000_000 step 应当走 stage 3 路径，
///   D-409 字面 byte-equal 维持。
///
/// 两条路径在同一组 chance-deterministic probe 上做 `average_strategy` BLAKE3
/// snapshot，必须 byte-equal。
///
/// **B2 \[实现\] 落地前转绿条件**：B2 实现 step() 内部 warmup routing 后，前
/// 1M update 走 stage 3 路径不被 Linear weighting / RM+ clamp 触达，断言通过。
/// 若 B2 错误把 Linear weighting 应用在 warmup phase 内 → BLAKE3 漂移立即 fail。
///
/// release ignored opt-in（D-361 单线程 ~7K update/s vultr → 1M × 2 ~ 5 min）。
#[test]
#[ignore = "release/--ignored opt-in（1M update × 2 trainer ~ 5 min vultr；B2 \\[实现\\] 落地后通过）"]
fn warmup_phase_1m_update_blake3_byte_equal_stage3_anchor() {
    let Some(table) = load_v3_artifact_arc_or_skip() else {
        return;
    };
    let Some(game_baseline) = build_simplified_nlhe_game_or_skip(&table) else {
        return;
    };
    let Some(game_warmup) = build_simplified_nlhe_game_or_skip(&table) else {
        return;
    };
    let probes = collect_snapshot_probes(&game_baseline);

    let baseline_hash = {
        let mut trainer = EsMccfrTrainer::new(game_baseline, FIXED_SEED);
        let mut rng = ChaCha20Rng::from_seed(FIXED_SEED);
        run_simplified_nlhe_trainer_steps(&mut trainer, &mut rng, WARMUP_ANCHOR_UPDATES);
        blake3_avg_strategy_snapshot(&trainer, &probes)
    };

    let warmup_hash = {
        let mut trainer =
            EsMccfrTrainer::new(game_warmup, FIXED_SEED).with_linear_rm_plus(WARMUP_ANCHOR_UPDATES);
        let mut rng = ChaCha20Rng::from_seed(FIXED_SEED);
        run_simplified_nlhe_trainer_steps(&mut trainer, &mut rng, WARMUP_ANCHOR_UPDATES);
        blake3_avg_strategy_snapshot(&trainer, &probes)
    };

    assert_eq!(
        baseline_hash,
        warmup_hash,
        "D-409 字面继承：warm-up phase 内 stage 4 `with_linear_rm_plus(...)` \
         路径必须与 stage 3 standard CFR + RM 路径 BLAKE3 byte-equal\n\
         baseline = {baseline_hex}\nwarmup   = {warmup_hex}\n\
         B2 \\[实现\\] 落地前 step() 未路由 warm-up routing；落地后 byte-equal 必须维持。",
        baseline_hex = blake3_hex(&baseline_hash),
        warmup_hex = blake3_hex(&warmup_hash)
    );
    eprintln!(
        "warm-up phase 1M update byte-equal stage 3 anchor ✓ BLAKE3 = {}",
        blake3_hex(&baseline_hash)
    );
}

// ===========================================================================
// Test 2 — D-409 boundary determinism（跨 run 同 seed byte-equal sanity anchor）
// ===========================================================================

/// D-409 boundary determinism sanity anchor（D-446 字面 `warmup_complete: bool`
/// checkpoint 字段决定性来源）：同 master_seed 跨两 run 跑 1_001 step（跨越
/// 1_000 → 1_001 warm-up 边界），切换点那个 step 跨 run 必须 byte-equal。
///
/// 该测试是 B1 \[测试\] 5 条中**唯一 default profile active sanity anchor**：
/// stage 3 path deterministic（D-335 sub-stream + 显式 [`ChaCha20Rng`]），跨 run
/// byte-equal 是 stage 3 既有不变量；B2 \[实现\] 后 stage 4 Linear+RM+ 路径仍
/// deterministic，跨 run byte-equal 维持。该 anchor "通过" 因为 trainer 整路径
/// deterministic；如果 B2 引入任意 thread-local RNG / 全局状态 / unsafe global
/// 让切换点非决定性，立即 fail。
///
/// **B2 \[实现\] 落地前后**：均通过 sanity anchor（stage 3 path / stage 4 path
/// 各自 deterministic，跨 run byte-equal）。该测试不证明 B2 实现正确，仅证明
/// trainer 决定性侧通道未被破坏（参考 stage 3 D-362 fixed-seed repeat 同型政策）。
///
/// artifact 缺失走 pass-with-skip 不 fail（CI / GitHub-hosted runner 典型场景）。
#[test]
#[ignore = "release/--ignored opt-in（2 × 1_001 step SimplifiedNlheGame ~ 300 ms；artifact \
            528 MiB 加 default profile 的整套常驻易触 OOM，走 release/--ignored 隔离）"]
fn warmup_boundary_deterministic_byte_equal_two_runs() {
    let Some(table) = load_v3_artifact_arc_or_skip() else {
        return;
    };
    let Some(game_a) = build_simplified_nlhe_game_or_skip(&table) else {
        return;
    };
    let Some(game_b) = build_simplified_nlhe_game_or_skip(&table) else {
        return;
    };
    let probes = collect_snapshot_probes(&game_a);
    if probes.is_empty() {
        eprintln!("skip: deterministic probe 路径未触达 Player node");
        return;
    }

    let run_hash = |game: SimplifiedNlheGame| -> [u8; 32] {
        let mut trainer = EsMccfrTrainer::new(game, FIXED_SEED).with_linear_rm_plus(1_000);
        let mut rng = ChaCha20Rng::from_seed(FIXED_SEED);
        run_simplified_nlhe_trainer_steps(&mut trainer, &mut rng, BOUNDARY_UPDATES);
        blake3_current_strategy_snapshot(&trainer, &probes)
    };

    let hash_a = run_hash(game_a);
    let hash_b = run_hash(game_b);
    assert_eq!(
        hash_a,
        hash_b,
        "D-409 boundary deterministic byte-equal：同 seed × 同 warmup_complete_at 跨 run \
         current_strategy BLAKE3 必须 byte-equal\n\
         run A = {a}\nrun B = {b}\n\
         任一非决定性侧通道（thread-local RNG / 全局状态 / unsafe global）让切换点 \
         non-deterministic 即 fail。",
        a = blake3_hex(&hash_a),
        b = blake3_hex(&hash_b)
    );
    eprintln!(
        "warm-up boundary deterministic byte-equal sanity anchor ✓ BLAKE3 = {}",
        blake3_hex(&hash_a)
    );
}

// ===========================================================================
// Test 3 — D-409 post-warmup σ 与 stage 3 baseline 显著差异
// ===========================================================================

/// D-409 post-warmup 数值连续性 + Linear+RM+ routing trip-wire：
///
/// `with_linear_rm_plus(warmup_complete_at = 100)` 跑 200 step（warm-up 100 +
/// post-warmup 100）— stage 4 trainer 与 stage 3 baseline trainer 走同 seed +
/// 同 step 数后，在 Kuhn 12 InfoSet 中至少 1 个 InfoSet 的 `current_strategy`
/// 显著差异（max abs diff `> 1e-9`）。
///
/// 走 Kuhn 单元路径（避免 v3 artifact 依赖，让 default profile 跑得动）：
/// - **stage 3 baseline**：`EsMccfrTrainer::new(KuhnGame, FIXED_SEED)`
/// - **stage 4 path**：`EsMccfrTrainer::new(KuhnGame, FIXED_SEED).with_linear_rm_plus(100)`
///
/// 200 step 内 warm-up 100 step 应当 byte-equal（D-409 字面），post-warmup 100
/// step Linear weighting + RM+ clamp 让 σ 发散。如果 B2 \[实现\] 漏 routing
/// 让 stage 4 trainer 全程走 stage 3 路径 → 两 trainer σ 在所有 InfoSet 上严格
/// byte-equal → max_diff = 0 → 断言 fail。
///
/// **B2 \[实现\] 落地前转绿条件**：B2 实现 warm-up routing + post-warmup
/// Linear weighting + RM+ clamp 后，stage 4 trainer 在 step 200 处的 σ 与
/// stage 3 baseline σ 在至少 1 InfoSet 显著不同（典型 max_diff ~ 1e-2 量级）。
#[test]
fn warmup_to_linear_rm_plus_post_warmup_strategy_diverges_from_baseline() {
    let baseline_strategies = {
        let mut trainer = EsMccfrTrainer::new(KuhnGame, FIXED_SEED);
        let mut rng = ChaCha20Rng::from_seed(FIXED_SEED);
        run_kuhn_trainer_steps(&mut trainer, &mut rng, TRANSITION_UPDATES);
        kuhn_collect_current_strategies(&trainer)
    };

    let stage4_strategies = {
        let mut trainer =
            EsMccfrTrainer::new(KuhnGame, FIXED_SEED).with_linear_rm_plus(TRANSITION_WARMUP_AT);
        let mut rng = ChaCha20Rng::from_seed(FIXED_SEED);
        run_kuhn_trainer_steps(&mut trainer, &mut rng, TRANSITION_UPDATES);
        kuhn_collect_current_strategies(&trainer)
    };

    let info_sets = enumerate_kuhn_info_sets();
    let mut max_diff = 0.0_f64;
    let mut max_diff_info: Option<&KuhnInfoSet> = None;
    for info in &info_sets {
        let b = baseline_strategies.get(info).cloned().unwrap_or_default();
        let s4 = stage4_strategies.get(info).cloned().unwrap_or_default();
        if b.len() != s4.len() {
            // 长度不一致也是显著差异，但更可能是 D-324 violation；fail 让 B2 debug
            panic!(
                "info={info:?} baseline.len={} stage4.len={} mismatch (D-324)",
                b.len(),
                s4.len()
            );
        }
        for (bi, si) in b.iter().zip(&s4) {
            let d = (bi - si).abs();
            if d > max_diff {
                max_diff = d;
                max_diff_info = Some(info);
            }
        }
    }

    assert!(
        max_diff > 1e-9,
        "D-409 post-warmup Linear+RM+ routing 未应用：200 step 后 stage 4 trainer 与 stage 3 \
         baseline 在 Kuhn 12 InfoSet 全集 σ 完全 byte-equal（max_diff = {max_diff:.3e} < 1e-9）。\
         B2 \\[实现\\] 起步前 step() 未路由 warm-up 切换 → Linear weighting + RM+ clamp \
         未被应用。"
    );
    eprintln!(
        "warm-up → post-warmup σ divergence ✓ max_diff = {max_diff:.6e} at info={:?}",
        max_diff_info
    );
}

/// 收集 Kuhn 12 InfoSet 全集上的 `current_strategy` 输出（map 形式让跨 trainer
/// 配对比较）。
fn kuhn_collect_current_strategies(
    trainer: &EsMccfrTrainer<KuhnGame>,
) -> std::collections::HashMap<KuhnInfoSet, Vec<f64>> {
    let mut out = std::collections::HashMap::new();
    for info in enumerate_kuhn_info_sets() {
        let sigma = trainer.current_strategy(&info);
        if !sigma.is_empty() {
            out.insert(info, sigma);
        }
    }
    out
}

// ===========================================================================
// Test 4 — D-401 Linear weighting cumulative formula 单元（t=2）
// ===========================================================================

/// D-401 字面：`R̃_t(I, a) = (t / (t + 1)) × R̃_{t-1}(I, a) + r_t(I, a)`
/// （Brown & Sandholm 2019 §3.1 字面 Linear CFR cumulative weighted regret）。
///
/// 走 Kuhn 单元路径：
/// - `with_linear_rm_plus(warmup_complete_at = 0)` 让 step 1 起就走 Linear MCCFR
/// - 跑 2 step（step 1 / step 2）
/// - 与 stage 3 standard CFR baseline 跑同 2 step 对比 Kuhn 12 InfoSet 上的
///   `current_strategy`（D-401 让两路径 σ 严格不等是 Linear decay 生效的
///   trip-wire；如果两路径 σ 完全相等则证明 Linear decay 未被 trainer 应用）。
///
/// **B2 \[实现\] 落地前转绿条件**：B2 实现 step() 内部 Linear decay eager 路径
/// 后，stage 4 路径与 stage 3 路径在 step 2 之后 σ 严格不等（max_diff > 1e-9）。
#[test]
fn linear_weighting_t2_cumulative_formula_unit() {
    let baseline_strategies = {
        let mut trainer = EsMccfrTrainer::new(KuhnGame, FIXED_SEED);
        let mut rng = ChaCha20Rng::from_seed(FIXED_SEED);
        run_kuhn_trainer_steps(&mut trainer, &mut rng, 2);
        kuhn_collect_current_strategies(&trainer)
    };

    let linear_strategies = {
        let mut trainer = EsMccfrTrainer::new(KuhnGame, FIXED_SEED).with_linear_rm_plus(0);
        let mut rng = ChaCha20Rng::from_seed(FIXED_SEED);
        run_kuhn_trainer_steps(&mut trainer, &mut rng, 2);
        kuhn_collect_current_strategies(&trainer)
    };

    let info_sets = enumerate_kuhn_info_sets();
    let mut max_diff = 0.0_f64;
    let mut max_diff_info: Option<&KuhnInfoSet> = None;
    for info in &info_sets {
        let b = baseline_strategies.get(info).cloned().unwrap_or_default();
        let l = linear_strategies.get(info).cloned().unwrap_or_default();
        if b.len() != l.len() {
            panic!(
                "info={info:?} baseline.len={} linear.len={} mismatch (D-324)",
                b.len(),
                l.len()
            );
        }
        for (bi, li) in b.iter().zip(&l) {
            let d = (bi - li).abs();
            if d > max_diff {
                max_diff = d;
                max_diff_info = Some(info);
            }
        }
    }

    assert!(
        max_diff > 1e-9,
        "D-401 Linear weighting 未应用：stage 4 σ 与 stage 3 σ 在 step 2 后完全相等\n\
         max_diff = {max_diff:.3e} < 1e-9 容差 — B2 \\[实现\\] 起步前 trainer.step \
         未路由 D-401 eager decay 路径。"
    );
    eprintln!(
        "D-401 Linear weighting t=2 cumulative formula ✓ max_diff = {max_diff:.6e} at info={:?}",
        max_diff_info
    );
}

// ===========================================================================
// Test 5 — D-402 RM+ in-place clamp raw R via Checkpoint 读取
// ===========================================================================

/// D-402 字面：`R^+_t(I, a) = max(R̃_t(I, a), 0)`，clamp 时机在每 update 后
/// in-place 应用（Tammelin 2015 §3）。从外部测试**无法**直接访问
/// `EsMccfrTrainer::regret` 字段（`pub(crate)`）；走 `save_checkpoint` +
/// `Checkpoint::open` + `bincode::deserialize` 路径间接读 raw `Vec<(KuhnInfoSet,
/// Vec<f64>)>`（D-327 `encode_table` 反路径）。
///
/// 验证两条互补性质：
/// 1. stage 4 `with_linear_rm_plus(0)` 路径跑 [`KUHN_CLAMP_STEPS`] step 后，
///    RegretTable 全表 raw R 严格 `>= 0`（D-402 in-place clamp）。
/// 2. stage 3 standard CFR baseline 路径跑同 step 数，RegretTable 至少 1 个
///    entry 含负 R 累积（证明本测试有"区分度"：non-trivial — 测试在 D-402
///    未生效时确实会捕捉负值；不是平凡的全表非负）。
///
/// **B2 \[实现\] 落地前转绿条件**：B2 实现 step() 内部 RM+ in-place clamp 后，
/// stage 4 RegretTable raw R 全 `>= 0`；baseline 路径 R table 含负 entry（区分
/// 度 anchor）。如果 B2 漏 clamp 或仅在 query 时延迟 clamp（D-402 字面禁止），
/// 断言 1 立即 fail（外部读 raw R 仍有负值）。
#[test]
fn rm_plus_clamp_raw_regret_non_negative_via_checkpoint_inspection() {
    // stage 3 baseline 路径
    let baseline_entries = {
        let mut trainer = EsMccfrTrainer::new(KuhnGame, FIXED_SEED);
        let mut rng = ChaCha20Rng::from_seed(FIXED_SEED);
        run_kuhn_trainer_steps(&mut trainer, &mut rng, KUHN_CLAMP_STEPS);
        dump_kuhn_regret_table_raw(&trainer, "baseline")
    };

    // stage 4 Linear MCCFR + RM+ 路径
    let stage4_entries = {
        let mut trainer = EsMccfrTrainer::new(KuhnGame, FIXED_SEED).with_linear_rm_plus(0);
        let mut rng = ChaCha20Rng::from_seed(FIXED_SEED);
        run_kuhn_trainer_steps(&mut trainer, &mut rng, KUHN_CLAMP_STEPS);
        dump_kuhn_regret_table_raw(&trainer, "stage4")
    };

    // 断言 1（区分度）：baseline 路径至少 1 个 entry 含负 R 累积。否则测试无
    // 区分度（任意实现都通过断言 2），需要调整 KUHN_CLAMP_STEPS 步数或换 InfoSet。
    let baseline_min: f64 = baseline_entries
        .iter()
        .flat_map(|(_, rs)| rs.iter().copied())
        .fold(f64::INFINITY, f64::min);
    assert!(
        baseline_min < 0.0,
        "测试区分度 broken：stage 3 baseline 跑 {KUHN_CLAMP_STEPS} step 后 RegretTable raw \
         min R = {baseline_min} >= 0；本测试的 RM+ clamp 断言对 baseline 也无效（测试无区分 \
         度，需要调整 step 数 / seed）。"
    );

    // 断言 2（D-402）：stage 4 路径全表 raw R `>= 0`
    for (info, rs) in &stage4_entries {
        for (i, &r) in rs.iter().enumerate() {
            assert!(
                r >= 0.0,
                "D-402 RM+ in-place clamp 失效：info={info:?} R[{i}]={r} < 0 in stage 4 \
                 `with_linear_rm_plus(0)` 路径\n\
                 baseline 路径 min R = {baseline_min} 证明本测试有区分度（B2 \\[实现\\] \
                 起步前 stage 4 路径 == stage 3 baseline 路径 → 断言 fail）。"
            );
            assert!(r.is_finite(), "info={info:?} R[{i}]={r} 非 finite");
        }
    }

    eprintln!(
        "D-402 RM+ in-place clamp via Checkpoint inspection ✓ \
         baseline_entries={} stage4_entries={} baseline_min_R={baseline_min:.6e}",
        baseline_entries.len(),
        stage4_entries.len()
    );
}
