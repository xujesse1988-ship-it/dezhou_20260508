//! 阶段 3 B1 \[测试\]：Leduc Vanilla CFR 收敛门槛 + 曲线单调性 + 重复确定性 +
//! 零和约束（D-300 / D-332 / D-341 / D-348 / D-360 / D-362）。
//!
//! 四条核心 trip-wire：
//! - `leduc_vanilla_cfr_10k_iter_exploitability_less_than_0_1`：D-341 字面阈值
//!   `< 0.1 chips/game`，走 [`poker::training::exploitability::<LeducGame,
//!   LeducBestResponse>`]。release ignored（10K iter Leduc release `< 60 s` per
//!   D-360 + exploitability `< 1 s` per D-348 ≈ 累积 < 2 min）。
//! - `leduc_vanilla_cfr_curve_monotonic_non_increasing_at_1k_2k_5k_10k`：D-341
//!   curve 单调非升（允许相邻 ±5% 噪声）。共享 4 个 checkpoint trainer，避免
//!   重复 10K iter。
//! - `leduc_vanilla_cfr_fixed_seed_repeat_10_times_blake3_identical`：D-362
//!   重复 10 次同 seed BLAKE3 一致（10 次 × 10K iter ≈ 10 min release，
//!   release ignored）。BLAKE3 snapshot 走 reachable InfoSet 全集（B2 \[实现\]
//!   后实际 InfoSet 数由 `trainer.regret.len()` 读取）。
//! - `leduc_vanilla_cfr_zero_sum_invariant`：D-332 零和约束。Leduc 严格零和；
//!   trained σ 下 `|EV_0 + EV_1| < 1e-6`。release ignored（树规模 ~10⁴ ×
//!   recurse depth ~12 ≈ 数秒）。
//!
//! B1 \[测试\] 角色边界：本文件不修改 `src/training/`；A1 scaffold 阶段全部
//! `unimplemented!()`，本文件 active 测试会因 panic fail。B2 \[实现\] 落地后转绿。

use blake3::Hasher;
use poker::training::game::{Game, NodeKind, PlayerId};
use poker::training::leduc::{LeducAction, LeducGame, LeducInfoSet, LeducState};
use poker::training::{exploitability, LeducBestResponse, Trainer, VanillaCfrTrainer};
use poker::{ChaCha20Rng, RngSource};

/// D-360 SLO：10K iter Leduc Vanilla CFR `< 60 s` release。
const TRAINING_ITERS: u64 = 10_000;

/// D-341 字面阈值。
const LEDUC_EXPLOITABILITY_THRESHOLD: f64 = 0.1;

/// D-341 curve 单调性 sample point：1K / 2K / 5K / 10K（4 个 checkpoint）。
const CURVE_CHECKPOINTS: [u64; 4] = [1_000, 2_000, 5_000, 10_000];

/// fixed master seed；同型 Kuhn 测试用不同 const 避免跨 game cross-contamination。
const FIXED_SEED: u64 = 0xA5_A5_A5_A5_A5_A5_A5_A5;

/// D-341 字面 `< 0.1 chips/game`：10K iter Vanilla CFR Leduc exploitability。
#[test]
#[ignore = "release/--ignored opt-in（10K Leduc iter release < 60 s per D-360；B2 \\[实现\\] 落地后通过）"]
fn leduc_vanilla_cfr_10k_iter_exploitability_less_than_0_1() {
    let trainer = train_leduc_full(FIXED_SEED, TRAINING_ITERS);
    let avg_closure = |info: &LeducInfoSet, _n: usize| trainer.average_strategy(info);

    let expl = exploitability::<LeducGame, LeducBestResponse>(&LeducGame, &avg_closure);
    eprintln!(
        "Leduc 10K iter Vanilla CFR: exploitability = {expl:.6} chips/game (< {LEDUC_EXPLOITABILITY_THRESHOLD} target)"
    );
    assert!(
        expl < LEDUC_EXPLOITABILITY_THRESHOLD,
        "Leduc exploitability {expl} 超过 D-341 字面阈值 {LEDUC_EXPLOITABILITY_THRESHOLD}"
    );
    assert!(
        expl >= 0.0,
        "exploitability {expl} 必须非负（D-341 同 D-340 定义）"
    );
}

/// D-341 curve 单调非升（1K / 2K / 5K / 10K 4 个 checkpoint，允许 ±5% 噪声）。
///
/// 实现路径：单 trainer 累积训练，在 `iters ∈ CURVE_CHECKPOINTS` 时 snapshot
/// exploitability；相邻两值断言 `e_next <= e_prev * 1.05`（容忍 5% 噪声向上漂移）。
#[test]
#[ignore = "release/--ignored opt-in（4 个 checkpoint × Leduc exploitability < 1 s + 10K iter < 60 s release；B2 \\[实现\\] 落地后通过）"]
fn leduc_vanilla_cfr_curve_monotonic_non_increasing_at_1k_2k_5k_10k() {
    let mut trainer = VanillaCfrTrainer::new(LeducGame, FIXED_SEED);
    let mut rng = ChaCha20Rng::from_seed(FIXED_SEED);
    let mut curve = Vec::with_capacity(CURVE_CHECKPOINTS.len());

    let mut cur_iter = 0u64;
    for &target in &CURVE_CHECKPOINTS {
        while cur_iter < target {
            trainer
                .step(&mut rng)
                .expect("Leduc Vanilla CFR step 期望成功");
            cur_iter += 1;
        }
        let snapshot = &trainer;
        let avg_closure = |info: &LeducInfoSet, _n: usize| snapshot.average_strategy(info);
        let expl = exploitability::<LeducGame, LeducBestResponse>(&LeducGame, &avg_closure);
        eprintln!("Leduc curve checkpoint iter={target}: exploitability = {expl:.6}");
        curve.push((target, expl));
    }

    for w in curve.windows(2) {
        let (prev_iter, prev_expl) = w[0];
        let (next_iter, next_expl) = w[1];
        let upper = prev_expl * 1.05;
        assert!(
            next_expl <= upper,
            "Leduc curve 从 iter={prev_iter} (expl={prev_expl:.6}) 到 iter={next_iter} (expl={next_expl:.6}) 上升超过 5% 容忍上界 {upper:.6}"
        );
    }

    let &(_, last_expl) = curve.last().unwrap();
    assert!(
        last_expl < LEDUC_EXPLOITABILITY_THRESHOLD,
        "10K iter checkpoint exploitability {last_expl} 应同时满足 D-341 字面阈值 < {LEDUC_EXPLOITABILITY_THRESHOLD}"
    );
}

/// D-362 重复确定性：fixed seed 重复 10 次训练，avg_strategy BLAKE3 全等。
///
/// 10 次 × 10K iter Leduc release < 60 s/run ≈ 10 min 累积，release ignored。
/// BLAKE3 snapshot 走 trainer 内部 `strategy_sum` 的全部 InfoSet（通过 trainer
/// `regret`/`strategy_sum` 字段公开访问；A1 scaffold 字段 `pub(crate)` 但 test
/// crate 与 product crate 不共享，需要走 trainer 公开 API `average_strategy` +
/// 枚举 InfoSet 来 hash）。
///
/// 实现路径：通过博弈树 DFS 枚举 reachable InfoSet → 对每个 InfoSet 调用
/// `average_strategy` → 写入 hasher。该路径与 Kuhn `enumerate_kuhn_info_sets()`
/// 静态枚举不同——Leduc InfoSet 数 ~288 由 D-311 rules 推导，但 reachable 子集
/// 由 game tree 决定，DFS 枚举更稳健。
#[test]
#[ignore = "release/--ignored opt-in（10 × 10K Leduc iter release ~10 min；B2 \\[实现\\] 落地后通过）"]
fn leduc_vanilla_cfr_fixed_seed_repeat_10_times_blake3_identical() {
    let mut first_hash: Option<[u8; 32]> = None;
    let repeat_count = 10;
    for run in 0..repeat_count {
        let trainer = train_leduc_full(FIXED_SEED, TRAINING_ITERS);
        let hash = blake3_avg_strategy_snapshot(&trainer);
        if let Some(expected) = first_hash {
            assert_eq!(
                hash, expected,
                "run #{run} BLAKE3 = {hash:x?} 偏离 run #0 {expected:x?}"
            );
        } else {
            first_hash = Some(hash);
            eprintln!("Leduc 10K iter Vanilla CFR seed=0x{FIXED_SEED:016x}: BLAKE3 = {hash:x?}");
        }
    }
    eprintln!("Leduc fixed-seed repeat {repeat_count} runs all BLAKE3 byte-equal ✓");
}

/// D-332 零和约束：trained avg_strategy 下 `|EV_0 + EV_1| < 1e-6`。
///
/// Leduc 严格零和（D-316 chip 净收益直接当 utility）；走博弈树枚举两遍累积
/// EV_0 / EV_1 检查 sum。release ignored（Leduc 全树 DFS ~10⁴ × depth ~12 ≈ 数秒）。
#[test]
#[ignore = "release/--ignored opt-in（Leduc 全树 DFS × 2 + 10K iter 训练 ~60 s release；B2 \\[实现\\] 落地后通过）"]
fn leduc_vanilla_cfr_zero_sum_invariant() {
    let trainer = train_leduc_full(FIXED_SEED, TRAINING_ITERS);
    let avg_closure = |info: &LeducInfoSet, _n: usize| trainer.average_strategy(info);

    let ev_0 = compute_expected_value::<LeducGame>(&LeducGame, &avg_closure, 0);
    let ev_1 = compute_expected_value::<LeducGame>(&LeducGame, &avg_closure, 1);
    let sum = ev_0 + ev_1;
    eprintln!("Leduc zero-sum invariant: EV_0 = {ev_0:.9}, EV_1 = {ev_1:.9}, sum = {sum:.3e}");
    assert!(
        sum.abs() < 1e-6,
        "Leduc |EV_0 + EV_1| = {} 超过 1e-6 D-332 容差",
        sum.abs()
    );
}

// ===========================================================================
// 辅助函数：训练 + 博弈树枚举 + reachable InfoSet 枚举 + BLAKE3 snapshot
// ===========================================================================

/// 跑 `iters` 次 Vanilla CFR step，返回 trainer。
///
/// 同型 `cfr_kuhn::train_kuhn_full`；fixed seed 由 [`VanillaCfrTrainer::new`]
/// 内部走 D-335 SplitMix64 派生 sub-stream，step 不消费外部 rng（Vanilla CFR
/// full-tree 全确定性枚举）。
fn train_leduc_full(master_seed: u64, iters: u64) -> VanillaCfrTrainer<LeducGame> {
    let mut trainer = VanillaCfrTrainer::new(LeducGame, master_seed);
    let mut rng = ChaCha20Rng::from_seed(master_seed);
    for _ in 0..iters {
        trainer
            .step(&mut rng)
            .expect("Leduc Vanilla CFR step 期望成功");
    }
    trainer
}

/// 走完整博弈树枚举计算 `target_player` 视角 EV（同型 `cfr_kuhn::compute_expected_value`）。
fn compute_expected_value<G: Game>(
    game: &G,
    strategy: &dyn Fn(&G::InfoSet, usize) -> Vec<f64>,
    target_player: PlayerId,
) -> f64 {
    let mut rng = ChaCha20Rng::from_seed(0xBEEF_DEAD_BEEF_DEAD);
    let root = game.root(&mut rng);
    recurse_ev::<G>(&root, strategy, target_player, &mut rng)
}

fn recurse_ev<G: Game>(
    state: &G::State,
    strategy: &dyn Fn(&G::InfoSet, usize) -> Vec<f64>,
    target_player: PlayerId,
    rng: &mut dyn RngSource,
) -> f64 {
    match G::current(state) {
        NodeKind::Terminal => G::payoff(state, target_player),
        NodeKind::Chance => {
            let dist = G::chance_distribution(state);
            let mut value = 0.0;
            for (action, prob) in dist {
                let next_state = G::next(state.clone(), action, rng);
                value += prob * recurse_ev::<G>(&next_state, strategy, target_player, rng);
            }
            value
        }
        NodeKind::Player(actor) => {
            let info = G::info_set(state, actor);
            let actions = G::legal_actions(state);
            let probs = strategy(&info, actions.len());
            assert_eq!(
                probs.len(),
                actions.len(),
                "strategy length {} 与 legal_actions length {} 不一致",
                probs.len(),
                actions.len()
            );
            let mut value = 0.0;
            for (action, p) in actions.into_iter().zip(probs) {
                let next_state = G::next(state.clone(), action, rng);
                value += p * recurse_ev::<G>(&next_state, strategy, target_player, rng);
            }
            value
        }
    }
}

/// 走博弈树 DFS 收集 reachable InfoSet 全集（按 actor + 私有信息 + 公开历史）。
/// 走过的 InfoSet 累积到 BLAKE3 hasher，配 trainer.average_strategy 输出。
///
/// 顺序：DFS 走博弈树时第一次访问的 InfoSet 在前；这是 deterministic 的
/// （chance_distribution / legal_actions 顺序 D-310 / D-311 / D-324 锁定）；
/// 跨重复训练 + 跨 host byte-equal（D-347）。
fn blake3_avg_strategy_snapshot(trainer: &VanillaCfrTrainer<LeducGame>) -> [u8; 32] {
    let mut hasher = Hasher::new();
    // HashSet 仅用作 dedupe 集；写入 hasher 的顺序由 DFS traversal 决定（chance
    // _distribution / legal_actions 顺序 D-310 / D-311 / D-324 锁定 deterministic）。
    let mut visited: std::collections::HashSet<LeducInfoSet> = std::collections::HashSet::new();
    let mut rng = ChaCha20Rng::from_seed(0xCAFE_F00D_DEAD_BEEF);
    let root = LeducGame.root(&mut rng);
    collect_info_sets_dfs(&root, trainer, &mut visited, &mut hasher, &mut rng);
    hasher.finalize().into()
}

fn collect_info_sets_dfs(
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
                collect_info_sets_dfs(&next_state, trainer, visited, hasher, rng);
            }
        }
        NodeKind::Player(actor) => {
            let info = LeducGame::info_set(state, actor);
            if visited.insert(info.clone()) {
                let strategy = trainer.average_strategy(&info);
                // 写入 InfoSet 标识 + strategy values（让 hash 同时覆盖 InfoSet
                // 编码与 strategy；任一漂移 BLAKE3 变化）。
                hasher.update(&[info.actor]);
                hasher.update(&[info.private_card]);
                hasher.update(&[info.public_card.unwrap_or(0xFF)]);
                hasher.update(&[info.street as u8]);
                hasher.update(&(info.history.len() as u32).to_le_bytes());
                for a in &info.history {
                    hasher.update(&[*a as u8]);
                }
                hasher.update(&(strategy.len() as u32).to_le_bytes());
                for &p in &strategy {
                    hasher.update(&p.to_le_bytes());
                }
            }
            let actions = LeducGame::legal_actions(state);
            for action in actions {
                let next_state = LeducGame::next(state.clone(), action, rng);
                collect_info_sets_dfs(&next_state, trainer, visited, hasher, rng);
            }
        }
    }
}

/// dead_code 抑制 helper：让 `LeducState` / `LeducAction` 在 trip-wire 之外保持
/// 可用 import（同型 cfr_kuhn._import_check），避免 `-D warnings` 阻塞。
#[allow(dead_code)]
fn _import_check(_state: LeducState, _action: LeducAction) {}
