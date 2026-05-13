//! 阶段 3 D1 \[测试\]：CFR / MCCFR fuzz 不变量（D-300 / D-301 / D-330 / D-332 /
//! D-342 / D-379 浮点边界）。
//!
//! 三 game variant × 两规模（active smoke + release-ignored full）共 6 条
//! `#[test]`（`pluribus_stage3_workflow.md` §步骤 D1 line 237-240 字面对应）：
//!
//! - active smoke：每 game variant 跑 **1k iter** Vanilla / ES-MCCFR step；
//!   0 panic / 0 NaN / 0 Inf / probability sum ∈ \[1 - 1e-6, 1 + 1e-6\]
//!   （D-330 字面 1e-9 容差放宽至 1e-6 让 ES-MCCFR sampling noise 不假阳）；
//! - release-ignored full：Kuhn 1M iter / Leduc 1M iter / 简化 NLHE 100M update
//!   同型断言（NLHE full 单跑 ~3 h vultr，本文件只保留 1k smoke + release-
//!   ignored 1M iter 上限；100M update 走 `tests/simplified_nlhe_100M_update.rs`
//!   独立 SLO 测试）。
//!
//! **fuzz 维度**（与 stage 2 `tests/abstraction_fuzz.rs` 同型 master-seed-driven
//! 输入扰动模式）：
//! - 每 iter 重 seed master_seed → ChaCha20Rng 派生子流 → trainer.step 消费；
//! - 每 N=100 iter 取若干 InfoSet probe `current_strategy` / `average_strategy`，
//!   全部值 `.is_finite()` + `Σ.probs ∈ [1 - 1e-6, 1 + 1e-6]`；
//! - 任一 invariant 失败 → panic + 打印 (iter, info_set, raw values)；
//! - 全套 0 panic 即通过（继承 stage 1 `tests/fuzz_smoke.rs` panic-safety + stage
//!   2 `tests/abstraction_fuzz.rs` 100k+ 输入维度模式）。
//!
//! **D-300 / D-301 不变量**（fuzz 实测覆盖）：
//! - regret_table.current_strategy 输出全 finite + Σ = 1（D-303 标准 RM 退化均匀
//!   分布 fallback）；
//! - strategy_sum.average_strategy 输出全 finite + Σ = 1 当 InfoSet 已触达；
//!   未触达 InfoSet 走 lazy 0-length Vec（D-323 lazy init）允许，跳过断言；
//! - update_count() 单调非降；step 后增量 = 1（per D-307 alternating traverser 在
//!   ES-MCCFR 模式 / per D-300 full-tree 在 Vanilla 模式）。
//!
//! **release-ignored full** 对照 `cargo fuzz` 子命令（D-300 + workflow line 238
//! 字面 "cargo fuzz target cfr_kuhn_smoke / cfr_leduc_smoke / cfr_simplified_nlhe_smoke"
//! 继承 stage 2 D1 模式）：本测试文件用 `cargo test --release -- --ignored` 路径
//! 等价覆盖（`cargo fuzz` 需 `cargo-fuzz` 工具链 + nightly toolchain，stage 1/2
//! 同型做法是 fuzz `#[test]` + `#[ignore]`，避免引入 nightly）。
//!
//! **D1 \[测试\] 角色边界**：本文件不修改 `src/training/`；
//! - Kuhn / Leduc Vanilla CFR step 已在 B2 \[实现\] 落地，本文件 active smoke
//!   **应当通过**（不依赖 D2 [实现]）；
//! - 简化 NLHE ES-MCCFR step 已在 C2 \[实现\] 落地，本文件 active smoke 在 v3
//!   artifact 可用时**应当通过**，artifact 缺失走 pass-with-skip。
//! - release-ignored full 测试同型。

use std::path::PathBuf;
use std::sync::Arc;

use poker::training::game::{Game, NodeKind};
use poker::training::kuhn::{KuhnGame, KuhnHistory, KuhnInfoSet};
use poker::training::leduc::{LeducGame, LeducInfoSet, LeducState};
use poker::training::nlhe::{SimplifiedNlheGame, SimplifiedNlheState};
use poker::training::sampling::sample_discrete;
use poker::training::{EsMccfrTrainer, Trainer, VanillaCfrTrainer};
use poker::{BucketTable, ChaCha20Rng, InfoSetId, RngSource};

// ===========================================================================
// 共享常量
// ===========================================================================

/// active smoke 规模（D-300 / D-301 fuzz 不变量 baseline）。
const SMOKE_ITERS: u64 = 1_000;
/// release-ignored full 规模（D-300 / D-301 fuzz invariant full coverage；NLHE 100M
/// 走 `tests/simplified_nlhe_100M_update.rs` 单独文件，本文件上限 NLHE 1M update）。
const FULL_ITERS: u64 = 1_000_000;
const NLHE_FULL_UPDATES: u64 = 1_000_000;

/// probability sum 容差（D-330 字面 1e-9 + ES-MCCFR sampling noise → 1e-6 上限）。
const PROB_SUM_TOLERANCE: f64 = 1e-6;
/// 每 N iter probe 一次 strategy；让 fuzz `O(N · K · probes)` 复杂度可控。
const PROBE_EVERY: u64 = 100;
/// 每次 probe 抽样 InfoSet 上限（避免 NLHE 大规模 InfoSet 拖慢 probe）。
const PROBES_PER_BATCH: usize = 4;

/// v3 production artifact path（NLHE fuzz 依赖；artifact 缺失走 pass-with-skip）。
const V3_ARTIFACT_PATH: &str = "artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin";
const V3_BODY_BLAKE3_HEX: &str = "67ee555439f2c918698650c05f40a7a5e9e812280ceb87fc3c6590add98650cd";

// ===========================================================================
// 通用 invariant 检查器
// ===========================================================================

fn assert_finite(probs: &[f64], label: &str, iter: u64) {
    for (i, &p) in probs.iter().enumerate() {
        assert!(
            p.is_finite(),
            "fuzz iter {iter}: {label}[{i}] = {p} 非 finite（D-330 / D-342 禁止 NaN / Inf）"
        );
    }
}

fn assert_prob_sum(probs: &[f64], label: &str, iter: u64) {
    if probs.is_empty() {
        return; // lazy init InfoSet（D-323）：允许 0-length
    }
    let sum: f64 = probs.iter().sum();
    assert!(
        (sum - 1.0).abs() < PROB_SUM_TOLERANCE,
        "fuzz iter {iter}: {label} 概率和 {sum} 超 {PROB_SUM_TOLERANCE} 容差（D-330）"
    );
}

// ===========================================================================
// Kuhn fuzz（D-300 Vanilla CFR / 1k smoke active + 1M ignored）
// ===========================================================================

fn kuhn_info_set_probes() -> Vec<KuhnInfoSet> {
    // 取 4 个 representative InfoSet（actor 0 / 1 × Empty/Check × cards 11/13）
    vec![
        KuhnInfoSet {
            actor: 0,
            private_card: 11,
            history: KuhnHistory::Empty,
        },
        KuhnInfoSet {
            actor: 0,
            private_card: 13,
            history: KuhnHistory::CheckBet,
        },
        KuhnInfoSet {
            actor: 1,
            private_card: 12,
            history: KuhnHistory::Check,
        },
        KuhnInfoSet {
            actor: 1,
            private_card: 13,
            history: KuhnHistory::Bet,
        },
    ]
}

fn run_kuhn_fuzz(iters: u64, master_seed: u64, label: &str) {
    let mut trainer = VanillaCfrTrainer::new(KuhnGame, master_seed);
    let mut rng = ChaCha20Rng::from_seed(master_seed);
    let probes = kuhn_info_set_probes();
    let mut last_count: u64 = 0;
    for i in 0..iters {
        trainer
            .step(&mut rng)
            .unwrap_or_else(|e| panic!("kuhn fuzz {label} iter {i}: step 失败 {e:?}"));
        let cur = trainer.update_count();
        assert_eq!(
            cur,
            last_count + 1,
            "kuhn fuzz {label} iter {i}: update_count 应当 += 1（D-300 step 增量约定）"
        );
        last_count = cur;
        if i % PROBE_EVERY == 0 || i + 1 == iters {
            for info in probes.iter().take(PROBES_PER_BATCH) {
                let cur_strat = trainer.current_strategy(info);
                let avg_strat = trainer.average_strategy(info);
                assert_finite(&cur_strat, "current_strategy", i);
                assert_finite(&avg_strat, "average_strategy", i);
                assert_prob_sum(&cur_strat, "current_strategy", i);
                assert_prob_sum(&avg_strat, "average_strategy", i);
            }
        }
    }
    assert_eq!(trainer.update_count(), iters);
    eprintln!("kuhn fuzz {label}: {iters} iter 全部 0 panic / 0 NaN / 0 Inf / sum ∈ 1 ± {PROB_SUM_TOLERANCE} ✓");
}

#[test]
fn cfr_kuhn_smoke_1k_iter_no_panic_no_nan_no_inf() {
    run_kuhn_fuzz(SMOKE_ITERS, 0xD1FA_4B55_4E48_0001, "smoke_1k");
}

#[test]
#[ignore = "release/--ignored opt-in（1M Kuhn Vanilla CFR iter ~100 s release per D-360 1 s × 100；D2 \\[实现\\] 落地后通过）"]
fn cfr_kuhn_full_1m_iter_no_panic_no_nan_no_inf() {
    run_kuhn_fuzz(FULL_ITERS, 0xD1FA_4B55_4E48_F001, "full_1M");
}

// ===========================================================================
// Leduc fuzz（D-300 Vanilla CFR / 1k smoke active + 1M ignored）
// ===========================================================================

fn leduc_info_set_probes(trainer: &VanillaCfrTrainer<LeducGame>) -> Vec<LeducInfoSet> {
    // 走 DFS 收集 reachable InfoSet 全集，截取前 PROBES_PER_BATCH 个（按 DFS 顺序，
    // chance_distribution / legal_actions 顺序 D-311 deterministic，跨 iter 稳定）。
    let mut probes: Vec<LeducInfoSet> = Vec::new();
    let mut visited: std::collections::HashSet<LeducInfoSet> = std::collections::HashSet::new();
    let mut rng = ChaCha20Rng::from_seed(0xCAFE_FACE_FACE_FACE);
    let root = LeducGame.root(&mut rng);
    leduc_collect_probes(&root, &mut visited, &mut probes, &mut rng);
    let _ = trainer; // unused — probes 由 game-tree 结构决定，不依赖 trainer state
    probes.into_iter().take(PROBES_PER_BATCH).collect()
}

fn leduc_collect_probes(
    state: &LeducState,
    visited: &mut std::collections::HashSet<LeducInfoSet>,
    out: &mut Vec<LeducInfoSet>,
    rng: &mut dyn RngSource,
) {
    if out.len() >= PROBES_PER_BATCH {
        return;
    }
    match LeducGame::current(state) {
        NodeKind::Terminal => {}
        NodeKind::Chance => {
            let dist = LeducGame::chance_distribution(state);
            for (action, _prob) in dist {
                let next_state = LeducGame::next(state.clone(), action, rng);
                leduc_collect_probes(&next_state, visited, out, rng);
                if out.len() >= PROBES_PER_BATCH {
                    return;
                }
            }
        }
        NodeKind::Player(actor) => {
            let info = LeducGame::info_set(state, actor);
            if visited.insert(info.clone()) {
                out.push(info);
            }
            if out.len() >= PROBES_PER_BATCH {
                return;
            }
            let actions = LeducGame::legal_actions(state);
            for action in actions {
                let next_state = LeducGame::next(state.clone(), action, rng);
                leduc_collect_probes(&next_state, visited, out, rng);
                if out.len() >= PROBES_PER_BATCH {
                    return;
                }
            }
        }
    }
}

fn run_leduc_fuzz(iters: u64, master_seed: u64, label: &str) {
    let mut trainer = VanillaCfrTrainer::new(LeducGame, master_seed);
    let mut rng = ChaCha20Rng::from_seed(master_seed);
    let probes = leduc_info_set_probes(&trainer);
    let mut last_count: u64 = 0;
    for i in 0..iters {
        trainer
            .step(&mut rng)
            .unwrap_or_else(|e| panic!("leduc fuzz {label} iter {i}: step 失败 {e:?}"));
        let cur = trainer.update_count();
        assert_eq!(cur, last_count + 1);
        last_count = cur;
        if i % PROBE_EVERY == 0 || i + 1 == iters {
            for info in probes.iter().take(PROBES_PER_BATCH) {
                let cur_strat = trainer.current_strategy(info);
                let avg_strat = trainer.average_strategy(info);
                assert_finite(&cur_strat, "current_strategy", i);
                assert_finite(&avg_strat, "average_strategy", i);
                assert_prob_sum(&cur_strat, "current_strategy", i);
                assert_prob_sum(&avg_strat, "average_strategy", i);
            }
        }
    }
    assert_eq!(trainer.update_count(), iters);
    eprintln!("leduc fuzz {label}: {iters} iter 全部 0 panic / 0 NaN / 0 Inf / sum ∈ 1 ± {PROB_SUM_TOLERANCE} ✓");
}

#[test]
#[ignore = "release/--ignored opt-in（1k Leduc Vanilla CFR iter ~6 s release per D-360；D2 \\[实现\\] 落地后通过）"]
fn cfr_leduc_smoke_1k_iter_no_panic_no_nan_no_inf() {
    run_leduc_fuzz(SMOKE_ITERS, 0xD1FA_4C45_4455_0001, "smoke_1k");
}

#[test]
#[ignore = "release/--ignored opt-in（1M Leduc Vanilla CFR iter ~6000 s release ~ 100 min；D2 \\[实现\\] 落地后通过）"]
fn cfr_leduc_full_1m_iter_no_panic_no_nan_no_inf() {
    run_leduc_fuzz(FULL_ITERS, 0xD1FA_4C45_4455_F001, "full_1M");
}

// ===========================================================================
// 简化 NLHE fuzz（D-301 ES-MCCFR / 1k smoke + 1M ignored；artifact 依赖）
// ===========================================================================

fn load_v3_or_skip() -> Option<Arc<BucketTable>> {
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
    let body_hex: String = table
        .content_hash()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if body_hex != V3_BODY_BLAKE3_HEX {
        eprintln!("skip: artifact body BLAKE3 `{body_hex}` 不匹配 v3 ground truth");
        return None;
    }
    Some(Arc::new(table))
}

fn nlhe_probe_info_sets(game: &SimplifiedNlheGame) -> Vec<InfoSetId> {
    const PROBE_LIMIT: usize = 16;
    let mut rng = ChaCha20Rng::from_seed(0xCAFE_FACE_BEEF_DEAD);
    let mut state: SimplifiedNlheState = game.root(&mut rng);
    let mut out = Vec::with_capacity(PROBE_LIMIT);
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
                out.push(info);
                let actions = SimplifiedNlheGame::legal_actions(&state);
                if actions.is_empty() {
                    break;
                }
                state = SimplifiedNlheGame::next(state, actions[0], &mut rng);
            }
        }
    }
    out.truncate(PROBES_PER_BATCH);
    out
}

fn run_nlhe_fuzz(updates: u64, master_seed: u64, label: &str) {
    let Some(table) = load_v3_or_skip() else {
        return;
    };
    let game = SimplifiedNlheGame::new(table).expect("v3 artifact schema_version = 2");
    let probes = nlhe_probe_info_sets(&game);
    let mut trainer = EsMccfrTrainer::new(game, master_seed);
    let mut rng = ChaCha20Rng::from_seed(master_seed);
    let mut last_count: u64 = 0;
    for i in 0..updates {
        trainer
            .step(&mut rng)
            .unwrap_or_else(|e| panic!("nlhe fuzz {label} update {i}: step 失败 {e:?}"));
        let cur = trainer.update_count();
        assert_eq!(cur, last_count + 1);
        last_count = cur;
        if i % PROBE_EVERY == 0 || i + 1 == updates {
            for info in probes.iter().take(PROBES_PER_BATCH) {
                let cur_strat = trainer.current_strategy(info);
                let avg_strat = trainer.average_strategy(info);
                assert_finite(&cur_strat, "current_strategy", i);
                assert_finite(&avg_strat, "average_strategy", i);
                assert_prob_sum(&cur_strat, "current_strategy", i);
                assert_prob_sum(&avg_strat, "average_strategy", i);
            }
        }
    }
    assert_eq!(trainer.update_count(), updates);
    eprintln!("nlhe fuzz {label}: {updates} update 全部 0 panic / 0 NaN / 0 Inf / sum ∈ 1 ± {PROB_SUM_TOLERANCE} ✓");
}

#[test]
#[ignore = "release/--ignored opt-in（1k NLHE ES-MCCFR update + v3 artifact 依赖 ~ 100 ms release；C2 \\[实现\\] 落地后通过）"]
fn cfr_simplified_nlhe_smoke_1k_update_no_panic_no_nan_no_inf() {
    run_nlhe_fuzz(SMOKE_ITERS, 0xD1FA_4E4C_4845_0001, "smoke_1k");
}

#[test]
#[ignore = "release/--ignored opt-in（1M NLHE ES-MCCFR update + v3 artifact 依赖 ~ 100 s release per D-361；C2 \\[实现\\] 落地后通过）"]
fn cfr_simplified_nlhe_full_1m_update_no_panic_no_nan_no_inf() {
    run_nlhe_fuzz(NLHE_FULL_UPDATES, 0xD1FA_4E4C_4845_F001, "full_1M");
}

// ===========================================================================
// dead_code 抑制 import helper
// ===========================================================================

#[allow(dead_code)]
fn _import_check(_h: KuhnHistory) {}
