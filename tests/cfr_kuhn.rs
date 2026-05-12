//! 阶段 3 B1 \[测试\]：Kuhn Vanilla CFR closed-form Nash anchor + 收敛门槛 +
//! 重复确定性 + 零和约束（D-300 / D-303 / D-332 / D-340 / D-360 / D-362）。
//!
//! 四条核心 trip-wire：
//! - `kuhn_vanilla_cfr_10k_iter_player_1_ev_close_to_minus_one_over_eighteen`：
//!   D-340 closed-form anchor。10K iter 后 player 0 视角 EV（trained σ vs trained σ）
//!   应逼近 `-1/18 ≈ -0.05556`，差距 `< 1e-3`。release ignored（10K iter Kuhn
//!   单线程 release `< 1 s`，但 dev box default profile 慢 ~10×）。
//! - `kuhn_vanilla_cfr_10k_iter_exploitability_less_than_0_01`：D-340 + path.md
//!   §阶段 3 字面 `< 0.01 chips/game` 字面门槛，走
//!   [`poker::training::exploitability::<KuhnGame, KuhnBestResponse>`]。
//! - `kuhn_vanilla_cfr_fixed_seed_repeat_1000_times_blake3_identical`：D-362
//!   重复 1000 次同 seed 同 host 同 toolchain 训练，avg_strategy 跨 12 InfoSet
//!   的 BLAKE3 全部 byte-equal。release ignored（1000 次 × 10K iter 约几分钟级）。
//! - `kuhn_vanilla_cfr_zero_sum_invariant_ev_sum_below_1e_minus_6`：D-332 零和
//!   约束。在 trained avg_strategy 下走完整博弈树枚举，`|EV_0 + EV_1| < 1e-6`。
//!
//! B1 \[测试\] 角色边界：本文件不修改 `src/training/`；A1 \[实现\] scaffold 当前
//! 全部方法体 `unimplemented!()`，本文件 active 测试会因 panic fail，移交 B2
//! \[实现\] 落地后转绿。这正是 B1 → B2 工程契约的预期形态。

use blake3::Hasher;
use poker::training::game::{Game, NodeKind, PlayerId};
use poker::training::kuhn::{KuhnAction, KuhnGame, KuhnHistory, KuhnInfoSet, KuhnState};
use poker::training::{exploitability, KuhnBestResponse, Trainer, VanillaCfrTrainer};
use poker::{ChaCha20Rng, RngSource};

/// Kuhn closed-form anchor（D-340）。
///
/// player 1（0-indexed = 0）的 Nash EV 为 `-1/18`（标准结论）；训练后 player 1
/// 视角 EV（trained σ_0 vs trained σ_1）应逼近该值。差距 `< 1e-3` 是 10K iter
/// Vanilla CFR Kuhn 实测可达的强 upper bound（Zinkevich 2007 理论上界 `~0.098`，
/// 实测通常 `100×` tighter，详见 D-300 详解收敛性段）。
const KUHN_PLAYER_1_NASH_EV: f64 = -1.0 / 18.0;

/// 训练 iter 数（D-360 SLO `< 1 s` release）。
const TRAINING_ITERS: u64 = 10_000;

/// fixed master seed（D-335 sub-stream root）。所有 4 条测试共用，
/// 让 BLAKE3 byte-equal 跨测试可交叉验证。
const FIXED_SEED: u64 = 0x5A_5A_5A_5A_5A_5A_5A_5A;

/// D-340 closed-form anchor：10K iter 后 player 1 EV 逼近 `-1/18`，差距 `< 1e-3`。
///
/// release ignored（10K iter Kuhn 单线程 release `< 1 s` per D-360，dev profile
/// 慢 ~10×；本测试 + exploitability + zero_sum 三条共享同 trained Trainer，
/// 累积 release time 仍 `< 5 s` 量级）。
#[test]
#[ignore = "release/--ignored opt-in（10K Vanilla CFR iter release < 1 s per D-360；B2 \\[实现\\] 落地后通过）"]
fn kuhn_vanilla_cfr_10k_iter_player_1_ev_close_to_minus_one_over_eighteen() {
    let trainer = train_kuhn_full(FIXED_SEED, TRAINING_ITERS);
    let avg_closure = |info: &KuhnInfoSet, _n: usize| trainer.average_strategy(info);

    // EV 计算走博弈树枚举：root chance 节点起步，walk 全 6 deal × decision tree。
    let ev_player_0 = compute_expected_value::<KuhnGame>(&KuhnGame, &avg_closure, 0);
    let diff = (ev_player_0 - KUHN_PLAYER_1_NASH_EV).abs();
    eprintln!(
        "Kuhn 10K iter Vanilla CFR: player 1 EV = {ev_player_0:.6} (target = {KUHN_PLAYER_1_NASH_EV:.6}, diff = {diff:.6})"
    );
    assert!(
        diff < 1e-3,
        "Kuhn player 1 EV {ev_player_0} 偏离 closed-form anchor {KUHN_PLAYER_1_NASH_EV} 超过 1e-3 (diff = {diff})"
    );
}

/// D-340 + path.md §阶段 3 字面 `< 0.01 chips/game` 收敛门槛。
///
/// 走 [`exploitability::<KuhnGame, KuhnBestResponse>`] 公开 API（D-340 spec：
/// `exploitability = (BR_1(σ_2) + BR_2(σ_1)) / 2`）。Kuhn 12 InfoSet × 2 action
/// 全枚举可瞬时完成（D-348 `< 100 ms` release）。
#[test]
fn kuhn_vanilla_cfr_10k_iter_exploitability_less_than_0_01() {
    let trainer = train_kuhn_full(FIXED_SEED, TRAINING_ITERS);
    let avg_closure = |info: &KuhnInfoSet, _n: usize| trainer.average_strategy(info);

    let expl = exploitability::<KuhnGame, KuhnBestResponse>(&KuhnGame, &avg_closure);
    eprintln!("Kuhn 10K iter Vanilla CFR: exploitability = {expl:.6} chips/game (< 0.01 target)");
    assert!(
        expl < 0.01,
        "Kuhn exploitability {expl} 超过 path.md §阶段 3 字面 0.01 门槛"
    );
    assert!(
        expl >= 0.0,
        "exploitability {expl} 必须非负（D-340 定义 `(BR_1 + BR_2) / 2`，BR_i 必 ≥ Nash EV_i）"
    );
}

/// D-362 重复确定性：fixed seed 重复 1000 次训练，avg_strategy BLAKE3 全等。
///
/// 1000 次 × 10K iter Kuhn release `< 1 s` per training run → 累积 ~16 min 量级，
/// release ignored 走 vultr 复跑。BLAKE3 输入 = 12 InfoSet × 2 f64 (LE bytes) =
/// 192 bytes / training run。
///
/// 跨 host / 跨架构 byte-equal 是 D-347 stage 3 头号 determinism 不变量（继承
/// stage 1 + stage 2 byte-equal 政策）。
#[test]
#[ignore = "release/--ignored opt-in（1000 × 10K Kuhn iter release ~16 min；B2 \\[实现\\] 落地后通过）"]
fn kuhn_vanilla_cfr_fixed_seed_repeat_1000_times_blake3_identical() {
    let mut first_hash: Option<[u8; 32]> = None;
    let repeat_count = 1000;
    for run in 0..repeat_count {
        let trainer = train_kuhn_full(FIXED_SEED, TRAINING_ITERS);
        let hash = blake3_avg_strategy_snapshot(&trainer);
        if let Some(expected) = first_hash {
            assert_eq!(
                hash, expected,
                "run #{run} BLAKE3 = {hash:x?} 偏离 run #0 {expected:x?}"
            );
        } else {
            first_hash = Some(hash);
            eprintln!("Kuhn 10K iter Vanilla CFR seed=0x{FIXED_SEED:016x}: BLAKE3 = {hash:x?}");
        }
    }
    eprintln!("Kuhn fixed-seed repeat {repeat_count} runs all BLAKE3 byte-equal ✓");
}

/// D-332 零和约束：trained avg_strategy 下 `|EV_0 + EV_1| < 1e-6`。
///
/// Kuhn 零和：每个 terminal 严格 `payoff(s, 0) + payoff(s, 1) = 0`（D-316 chip
/// 净收益直接当 utility）；任意 σ 下 EV sum 应严格 0（浮点累积误差 < 1e-6）。
/// 测试通过 walk 同博弈树两遍（target=0 / target=1）独立累积，覆盖 payoff 函数
/// 零和性 + tree 累积顺序无关性。
#[test]
fn kuhn_vanilla_cfr_zero_sum_invariant_ev_sum_below_1e_minus_6() {
    let trainer = train_kuhn_full(FIXED_SEED, TRAINING_ITERS);
    let avg_closure = |info: &KuhnInfoSet, _n: usize| trainer.average_strategy(info);

    let ev_0 = compute_expected_value::<KuhnGame>(&KuhnGame, &avg_closure, 0);
    let ev_1 = compute_expected_value::<KuhnGame>(&KuhnGame, &avg_closure, 1);
    let sum = ev_0 + ev_1;
    eprintln!("Kuhn zero-sum invariant: EV_0 = {ev_0:.9}, EV_1 = {ev_1:.9}, sum = {sum:.3e}");
    assert!(
        sum.abs() < 1e-6,
        "Kuhn |EV_0 + EV_1| = {} 超过 1e-6 D-332 容差",
        sum.abs()
    );
}

// ===========================================================================
// 辅助函数：训练 + 博弈树枚举 + BLAKE3 snapshot
// ===========================================================================

/// 跑 `iters` 次 Vanilla CFR step，返回 trainer。
///
/// fixed seed 由 [`VanillaCfrTrainer::new`] 内部走 D-335 SplitMix64 派生 sub-stream
/// 给每 iter / 每 op_id 使用；step 内部 RNG 由 [`derive_substream_seed`] 派生，
/// 不消费外部 rng（D-335 / D-308 / D-309 显式注入 vs 隐式 thread_rng 边界）。
///
/// 当前 A1 scaffold 阶段 `unimplemented!()`，本函数调用时 panic；B2 \[实现\] 落地
/// 后转为正常返回。
fn train_kuhn_full(master_seed: u64, iters: u64) -> VanillaCfrTrainer<KuhnGame> {
    let mut trainer = VanillaCfrTrainer::new(KuhnGame, master_seed);
    // 占位 RNG：Vanilla CFR `step` 不消费外部 rng（D-300 详解：full-tree 全确定性
    // 枚举，仅 sub-stream seed 派生用）。但 [`Trainer::step`] 签名要求 `&mut dyn
    // RngSource`，传 ChaCha20Rng instance 占位，避免 `unsafe` 路径。
    let mut rng = ChaCha20Rng::from_seed(master_seed);
    for _ in 0..iters {
        trainer
            .step(&mut rng)
            .expect("Vanilla CFR step 期望成功（D-330 容差仅 warn 不 panic）");
    }
    trainer
}

/// 走完整博弈树枚举计算 `target_player` 视角 EV（D-300 详解 Vanilla CFR 同型
/// 递归算法的"读取"版本，但不更新 regret / strategy_sum）。
///
/// 算法（DFS recursive，D-334）：
/// - Terminal：返回 `G::payoff(state, target_player)`。
/// - Chance：`Σ_o p(o) × expected_value(state.next(o), ...)`（D-300 chance node
///   enumeration；rng 在 chance 应用时不消费，[实现] 走 deterministic `next(state,
///   action, _rng)` 应用具体 outcome）。
/// - Player：`Σ_a σ(I, a) × expected_value(state.next(a), ...)`（D-300 decision
///   node weighted by joint strategy）。
///
/// 占位 rng 在递归内复用同一 ChaCha20Rng instance；chance node 在 D-300 详解
/// 字面 "对每个 chance outcome o，累积 `Σ_o p(o) × recurse(state.next(o), ...)`"
/// 路径下 next 接受 action 参数并 deterministic 应用——rng 不被消费，传 dummy
/// instance 合规。若 [实现] 选择让 chance node 内部强制重新 sample 而忽略
/// `action` 参数，本测试会 fail，移交 B2 \[实现\] 与 D-315 / API-300-revM 边界
/// 评估。
fn compute_expected_value<G: Game>(
    game: &G,
    strategy: &dyn Fn(&G::InfoSet, usize) -> Vec<f64>,
    target_player: PlayerId,
) -> f64 {
    let mut rng = ChaCha20Rng::from_seed(0xDEAD_BEEF_DEAD_BEEF);
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

/// 12 KuhnInfoSet 全集枚举（与 `tests/regret_matching_numeric.rs::enumerate_kuhn_info_sets`
/// 同型），用于 BLAKE3 snapshot 顺序锁。
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

/// BLAKE3 stable snapshot：12 InfoSet × 2 f64 LE bytes = 192 bytes / hash。
///
/// 跨 host / 跨架构 byte-equal 不变量（D-347）的具体实例：固定 InfoSet 枚举
/// 顺序 + f64::to_le_bytes 让 hash 输入端 byte-equal；BLAKE3 finalize 输出 32
/// byte digest。
fn blake3_avg_strategy_snapshot(trainer: &VanillaCfrTrainer<KuhnGame>) -> [u8; 32] {
    let mut hasher = Hasher::new();
    for info in enumerate_kuhn_info_sets() {
        let strategy = trainer.average_strategy(&info);
        assert_eq!(
            strategy.len(),
            2,
            "Kuhn 每个 InfoSet 全为 2-action（D-310 规则）",
        );
        for &p in &strategy {
            hasher.update(&p.to_le_bytes());
        }
    }
    hasher.finalize().into()
}

/// dead_code 抑制 helper：让 `KuhnState` / `KuhnAction` 在 trip-wire 之外保持
/// 可用 import（部分 B2 \[实现\] 后才被 active 测试用），避免 `cargo test
/// --no-run` 阶段 `unused_imports` warning 阻塞 `-D warnings`。
#[allow(dead_code)]
fn _import_check(_state: KuhnState, _action: KuhnAction) {}
