//! H3 简化 heads-up NLHE blueprint 评测闭环测试。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use poker::training::game::{Game, NodeKind};
use poker::training::nlhe::{SimplifiedNlheAction, SimplifiedNlheState};
use poker::training::{
    estimate_simplified_nlhe_lbr, evaluate_blueprint_vs_baseline,
    evaluate_blueprint_vs_blueprint_multiway, EsMccfrTrainer, NlheBaselinePolicy,
    NlheEvaluationConfig, NlheEvaluationError, NlheLbrConfig, Trainer,
};
use poker::{BucketTable, ChaCha20Rng, RngSource, SeatId, SimplifiedNlheGame};

const V3_ARTIFACT_PATH: &str = "artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin";
const V3_BODY_BLAKE3_HEX: &str = "67ee555439f2c918698650c05f40a7a5e9e812280ceb87fc3c6590add98650cd";
const H3_CHECKPOINT_100M_PATH: &str =
    "artifacts/phase3_post_history_fix_1b/nlhe_es_mccfr_auto_000100000008.ckpt";
const H3_CHECKPOINT_500M_PATH: &str =
    "artifacts/phase3_post_history_fix_1b/nlhe_es_mccfr_auto_000500000016.ckpt";
const H3_CHECKPOINT_1B_PATH: &str =
    "artifacts/phase3_post_history_fix_1b/nlhe_es_mccfr_final_001000000000.ckpt";
const H3_HEAD_TO_HEAD_BB_CHIPS: f64 = 100.0;

fn load_v3_or_skip() -> Option<Arc<BucketTable>> {
    let path = PathBuf::from(V3_ARTIFACT_PATH);
    if !path.exists() {
        eprintln!("skip: v3 artifact `{V3_ARTIFACT_PATH}` 不存在");
        return None;
    }
    let table = match BucketTable::open(&path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("skip: BucketTable::open failed: {e:?}");
            return None;
        }
    };
    let body_hex: String = table
        .content_hash()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if body_hex != V3_BODY_BLAKE3_HEX {
        eprintln!("skip: v3 body hash mismatch: {body_hex}");
        return None;
    }
    Some(Arc::new(table))
}

fn make_game(table: Arc<BucketTable>) -> SimplifiedNlheGame {
    SimplifiedNlheGame::new(table).expect("v3 bucket table should construct SimplifiedNlheGame")
}

fn load_h3_checkpoint_or_skip(
    path: &str,
    table: Arc<BucketTable>,
) -> Option<EsMccfrTrainer<SimplifiedNlheGame>> {
    let path_ref = Path::new(path);
    if !path_ref.exists() {
        eprintln!("skip: checkpoint `{path}` 不存在");
        return None;
    }
    let game = make_game(table);
    match <EsMccfrTrainer<SimplifiedNlheGame> as Trainer<SimplifiedNlheGame>>::load_checkpoint(
        path_ref, game,
    ) {
        Ok(t) => Some(t),
        Err(e) => {
            eprintln!("skip: load_checkpoint({path}) failed: {e:?}");
            None
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct HeadToHeadConfig {
    hands_per_seat: u64,
    seed: u64,
    max_actions_per_hand: usize,
}

#[derive(Clone, Debug)]
struct HeadToHeadReport {
    hands: u64,
    hands_per_seat: u64,
    seed: u64,
    new_total_chips: f64,
    new_mbb_per_game: f64,
    standard_error_mbb_per_game: f64,
    ci95_low_mbb_per_game: f64,
    ci95_high_mbb_per_game: f64,
    new_as_sb_mbb_per_game: f64,
    new_as_bb_mbb_per_game: f64,
}

#[derive(Clone, Copy)]
struct HeadToHeadStats {
    mean: f64,
    standard_error: f64,
}

fn evaluate_checkpoint_head_to_head(
    table: Arc<BucketTable>,
    new_trainer: &EsMccfrTrainer<SimplifiedNlheGame>,
    old_trainer: &EsMccfrTrainer<SimplifiedNlheGame>,
    config: HeadToHeadConfig,
) -> Result<HeadToHeadReport, String> {
    if config.hands_per_seat == 0 {
        return Err("hands_per_seat must be > 0".to_string());
    }
    if config.max_actions_per_hand == 0 {
        return Err("max_actions_per_hand must be > 0".to_string());
    }

    let game = make_game(table);
    let mut all_new_pnl = Vec::with_capacity((config.hands_per_seat * 2) as usize);
    let mut new_as_sb_total = 0.0;
    let mut new_as_bb_total = 0.0;

    for new_seat in [SeatId(0), SeatId(1)] {
        for hand_idx in 0..config.hands_per_seat {
            let seed = mix3(config.seed, new_seat.0 as u64, hand_idx);
            let mut rng = ChaCha20Rng::from_seed(seed);
            let root = game.root(&mut rng);
            let terminal = rollout_checkpoint_head_to_head(
                root,
                new_seat,
                new_trainer,
                old_trainer,
                &mut rng,
                config.max_actions_per_hand,
            )?;
            let pnl = SimplifiedNlheGame::payoff(&terminal, new_seat.0);
            if new_seat == SeatId(0) {
                new_as_sb_total += pnl;
            } else {
                new_as_bb_total += pnl;
            }
            all_new_pnl.push(pnl);
        }
    }

    let hands = all_new_pnl.len() as u64;
    let stats = h2h_sample_stats(&all_new_pnl);
    let scale = 1000.0 / H3_HEAD_TO_HEAD_BB_CHIPS;
    let mean_mbb = stats.mean * scale;
    let se_mbb = stats.standard_error * scale;
    Ok(HeadToHeadReport {
        hands,
        hands_per_seat: config.hands_per_seat,
        seed: config.seed,
        new_total_chips: all_new_pnl.iter().sum(),
        new_mbb_per_game: mean_mbb,
        standard_error_mbb_per_game: se_mbb,
        ci95_low_mbb_per_game: mean_mbb - 1.96 * se_mbb,
        ci95_high_mbb_per_game: mean_mbb + 1.96 * se_mbb,
        new_as_sb_mbb_per_game: (new_as_sb_total / config.hands_per_seat as f64) * scale,
        new_as_bb_mbb_per_game: (new_as_bb_total / config.hands_per_seat as f64) * scale,
    })
}

fn rollout_checkpoint_head_to_head(
    mut state: SimplifiedNlheState,
    new_seat: SeatId,
    new_trainer: &EsMccfrTrainer<SimplifiedNlheGame>,
    old_trainer: &EsMccfrTrainer<SimplifiedNlheGame>,
    rng: &mut dyn RngSource,
    max_actions: usize,
) -> Result<SimplifiedNlheState, String> {
    for _ in 0..max_actions {
        match SimplifiedNlheGame::current(&state) {
            NodeKind::Terminal => return Ok(state),
            NodeKind::Chance => {
                let dist = SimplifiedNlheGame::chance_distribution(&state);
                let action = poker::training::sampling::sample_discrete(&dist, rng);
                state = SimplifiedNlheGame::next(state, action, rng);
            }
            NodeKind::Player(actor) => {
                let trainer = if SeatId(actor) == new_seat {
                    new_trainer
                } else {
                    old_trainer
                };
                let action = sample_hybrid_checkpoint_action(&state, actor, trainer, rng)?;
                state = SimplifiedNlheGame::next(state, action, rng);
            }
        }
    }
    Err(format!(
        "head-to-head rollout did not terminate within {max_actions} actions"
    ))
}

fn sample_hybrid_checkpoint_action(
    state: &SimplifiedNlheState,
    actor: u8,
    trainer: &EsMccfrTrainer<SimplifiedNlheGame>,
    rng: &mut dyn RngSource,
) -> Result<SimplifiedNlheAction, String> {
    let actions = SimplifiedNlheGame::legal_actions(state);
    if actions.is_empty() {
        return Err(format!(
            "empty legal actions at non-terminal state: {:?}",
            SimplifiedNlheGame::current(state)
        ));
    }
    let info = SimplifiedNlheGame::info_set(state, actor);
    let raw = hybrid_checkpoint_strategy(trainer, &info);
    let dist = h2h_strategy_distribution(&actions, &raw)?;
    Ok(poker::training::sampling::sample_discrete(&dist, rng))
}

fn hybrid_checkpoint_strategy(
    trainer: &EsMccfrTrainer<SimplifiedNlheGame>,
    info: &poker::InfoSetId,
) -> Vec<f64> {
    let degenerate = match trainer.strategy_sum().inner().get(info) {
        None => true,
        Some(v) => v.iter().sum::<f64>() <= 0.0,
    };
    if degenerate {
        trainer.current_strategy(info)
    } else {
        trainer.average_strategy(info)
    }
}

fn h2h_strategy_distribution(
    actions: &[SimplifiedNlheAction],
    raw: &[f64],
) -> Result<Vec<(SimplifiedNlheAction, f64)>, String> {
    if raw.is_empty() {
        let p = 1.0 / actions.len() as f64;
        return Ok(actions.iter().copied().map(|a| (a, p)).collect());
    }
    if raw.len() != actions.len() {
        return Err(format!(
            "strategy length mismatch: expected {}, got {}",
            actions.len(),
            raw.len()
        ));
    }
    let mut sum = 0.0;
    for (idx, &p) in raw.iter().enumerate() {
        if !p.is_finite() || p < 0.0 {
            return Err(format!("invalid strategy probability at {idx}: {p}"));
        }
        sum += p;
    }
    if !sum.is_finite() || sum <= 0.0 {
        return Err(format!("invalid strategy sum: {sum}"));
    }
    Ok(actions
        .iter()
        .copied()
        .zip(raw.iter().copied())
        .filter(|(_, p)| *p > 0.0)
        .map(|(action, p)| (action, p / sum))
        .collect())
}

fn h2h_sample_stats(xs: &[f64]) -> HeadToHeadStats {
    let n = xs.len() as f64;
    let mean = xs.iter().sum::<f64>() / n;
    if xs.len() == 1 {
        return HeadToHeadStats {
            mean,
            standard_error: 0.0,
        };
    }
    let var = xs
        .iter()
        .map(|x| {
            let d = x - mean;
            d * d
        })
        .sum::<f64>()
        / (n - 1.0);
    HeadToHeadStats {
        mean,
        standard_error: var.sqrt() / n.sqrt(),
    }
}

fn mix3(seed: u64, a: u64, b: u64) -> u64 {
    mix64(seed ^ a.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ b.wrapping_mul(0xBF58_476D_1CE4_E5B9))
}

fn mix64(mut x: u64) -> u64 {
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

#[test]
fn h3_baseline_policies_only_return_legal_actions() {
    let Some(table) = load_v3_or_skip() else {
        return;
    };
    let game = make_game(table);
    let mut rng = ChaCha20Rng::from_seed(0x4833_504f_4c49_4359);
    let mut state = game.root(&mut rng);
    let policies = [
        NlheBaselinePolicy::Random,
        NlheBaselinePolicy::RandomNoFold,
        NlheBaselinePolicy::CallStation,
        NlheBaselinePolicy::OverlyTight,
        NlheBaselinePolicy::EquityEv,
    ];

    let mut checked = 0usize;
    for _ in 0..64 {
        match SimplifiedNlheGame::current(&state) {
            NodeKind::Terminal => {
                state = game.root(&mut rng);
            }
            NodeKind::Chance => {
                let dist = SimplifiedNlheGame::chance_distribution(&state);
                state = SimplifiedNlheGame::next(
                    state,
                    poker::training::sampling::sample_discrete(&dist, &mut rng),
                    &mut rng,
                );
            }
            NodeKind::Player(_) => {
                let actions = SimplifiedNlheGame::legal_actions(&state);
                for policy in policies {
                    let action = policy
                        .select_action(&state, &actions, &mut rng)
                        .expect("baseline should choose a legal action");
                    assert!(
                        actions.contains(&action),
                        "{policy:?} returned {action:?}, not in {actions:?}"
                    );
                    checked += 1;
                }
                let idx = (rng.next_u64() as usize) % actions.len();
                state = SimplifiedNlheGame::next(state, actions[idx], &mut rng);
            }
        }
    }
    assert!(checked >= 24, "expected to check multiple policy decisions");
}

#[test]
fn h3_random_no_fold_baseline_skips_fold_when_possible() {
    let Some(table) = load_v3_or_skip() else {
        return;
    };
    let game = make_game(table);
    let mut rng = ChaCha20Rng::from_seed(0x4833_4e4f_464f_4c44);
    let mut state = game.root(&mut rng);
    let mut checked = 0usize;

    for _ in 0..64 {
        match SimplifiedNlheGame::current(&state) {
            NodeKind::Terminal => {
                state = game.root(&mut rng);
            }
            NodeKind::Chance => {
                let dist = SimplifiedNlheGame::chance_distribution(&state);
                state = SimplifiedNlheGame::next(
                    state,
                    poker::training::sampling::sample_discrete(&dist, &mut rng),
                    &mut rng,
                );
            }
            NodeKind::Player(_) => {
                let actions = SimplifiedNlheGame::legal_actions(&state);
                if actions
                    .iter()
                    .any(|a| matches!(a, SimplifiedNlheAction::Fold))
                    && actions
                        .iter()
                        .any(|a| !matches!(a, SimplifiedNlheAction::Fold))
                {
                    for _ in 0..32 {
                        let action = NlheBaselinePolicy::RandomNoFold
                            .select_action(&state, &actions, &mut rng)
                            .expect("random-no-fold should choose a legal action");
                        assert!(
                            !matches!(action, SimplifiedNlheAction::Fold),
                            "random-no-fold returned Fold from {actions:?}"
                        );
                        assert!(actions.contains(&action));
                    }
                    checked += 1;
                }
                let idx = (rng.next_u64() as usize) % actions.len();
                state = SimplifiedNlheGame::next(state, actions[idx], &mut rng);
            }
        }
    }
    assert!(
        checked > 0,
        "expected at least one spot with fold plus alternatives"
    );
}

#[test]
fn h3_blueprint_empty_strategy_falls_back_uniform_but_mismatch_errors() {
    let Some(table) = load_v3_or_skip() else {
        return;
    };
    let game = make_game(table);
    let cfg = NlheEvaluationConfig {
        hands_per_seat: 2,
        seed: 0x4833_454d_5054_5900,
        max_actions_per_hand: 512,
    };

    let uniform = |_info: &poker::InfoSetId, _n: usize| Vec::new();
    let report = evaluate_blueprint_vs_baseline(&game, &uniform, NlheBaselinePolicy::Random, &cfg)
        .expect("empty strategy should use uniform fallback");
    assert_eq!(report.hands, 4);
    assert!(report.mbb_per_game.is_finite());

    let bad = |_info: &poker::InfoSetId, _n: usize| vec![1.0];
    let err = evaluate_blueprint_vs_baseline(&game, &bad, NlheBaselinePolicy::Random, &cfg)
        .expect_err("non-empty wrong-length strategy must fail");
    assert!(matches!(
        err,
        NlheEvaluationError::StrategyLengthMismatch { .. }
    ));
}

#[test]
fn h3_small_trained_evaluation_is_finite_and_deterministic() {
    let Some(table) = load_v3_or_skip() else {
        return;
    };
    let train_game = make_game(Arc::clone(&table));
    let eval_game = make_game(table);
    let mut trainer = EsMccfrTrainer::new(train_game, 0x4833_534d_4f4b_4500);
    let mut rng = ChaCha20Rng::from_seed(0x4833_534d_4f4b_4500);
    for _ in 0..100 {
        trainer.step(&mut rng).expect("100 update smoke");
    }
    let strategy = |info: &poker::InfoSetId, _n: usize| trainer.average_strategy(info);
    let cfg = NlheEvaluationConfig {
        hands_per_seat: 500,
        seed: 0x4833_4556_414c_1000,
        max_actions_per_hand: 512,
    };

    let r1 =
        evaluate_blueprint_vs_baseline(&eval_game, &strategy, NlheBaselinePolicy::Random, &cfg)
            .expect("small H3 eval should pass");
    let r2 =
        evaluate_blueprint_vs_baseline(&eval_game, &strategy, NlheBaselinePolicy::Random, &cfg)
            .expect("small H3 eval should be repeatable");

    assert_eq!(r1.hands, 1_000);
    assert!(r1.mbb_per_game.is_finite());
    assert!(r1.standard_error_mbb_per_game.is_finite());
    assert!(r1.ci95_low_mbb_per_game <= r1.ci95_high_mbb_per_game);
    assert_eq!(r1.mbb_per_game, r2.mbb_per_game);
    assert_eq!(
        r1.standard_error_mbb_per_game,
        r2.standard_error_mbb_per_game
    );
}

#[test]
fn s5_blueprint_vs_blueprint_multiway_is_finite_deterministic_and_seat_aware() {
    let Some(table) = load_v3_or_skip() else {
        return;
    };
    // 两个不同 seed / 不同步数的 trainer → 两套不同策略（同一 game/tree）。
    let mut trainer_a = EsMccfrTrainer::new(make_game(Arc::clone(&table)), 0x5335_5f41_0000_0000);
    let mut trainer_b = EsMccfrTrainer::new(make_game(Arc::clone(&table)), 0x5335_5f42_0000_0000);
    let mut rng_a = ChaCha20Rng::from_seed(0x5335_5f41_0000_0000);
    let mut rng_b = ChaCha20Rng::from_seed(0x5335_5f42_0000_0000);
    for _ in 0..150 {
        trainer_a.step(&mut rng_a).expect("A train smoke");
    }
    for _ in 0..60 {
        trainer_b.step(&mut rng_b).expect("B train smoke");
    }
    let eval_game = make_game(table);
    let strat_a = |info: &poker::InfoSetId, _n: usize| trainer_a.average_strategy(info);
    let strat_b = |info: &poker::InfoSetId, _n: usize| trainer_b.average_strategy(info);
    let cfg = NlheEvaluationConfig {
        hands_per_seat: 400,
        seed: 0x5335_5f45_5641_4c00,
        max_actions_per_hand: 512,
    };

    let ab1 = evaluate_blueprint_vs_blueprint_multiway(&eval_game, &strat_a, &strat_b, &cfg)
        .expect("A vs B should pass");
    let ab2 = evaluate_blueprint_vs_blueprint_multiway(&eval_game, &strat_a, &strat_b, &cfg)
        .expect("A vs B should be repeatable");
    let ba = evaluate_blueprint_vs_blueprint_multiway(&eval_game, &strat_b, &strat_a, &cfg)
        .expect("B vs A should pass");

    // plumbing：HU game → 2 座、hands = hands_per_seat × n_players、per-position 长度 = n_players。
    assert_eq!(ab1.n_players, eval_game.n_players());
    assert_eq!(ab1.hands, cfg.hands_per_seat * ab1.n_players as u64);
    assert_eq!(ab1.per_position_mbb_per_game.len(), ab1.n_players);
    assert!(ab1.mbb_per_game.is_finite());
    assert!(ab1.standard_error_mbb_per_game.is_finite());
    assert!(ab1.ci95_low_mbb_per_game <= ab1.ci95_high_mbb_per_game);

    // determinism：固定 seed 完全可复现。
    assert_eq!(ab1.mbb_per_game, ab2.mbb_per_game);
    assert_eq!(ab1.per_position_mbb_per_game, ab2.per_position_mbb_per_game);

    // seat-aware：交换 hero/opponent 必须改变结果——否则说明 hero_seat 分支被忽略
    //（即 bug：两座用了同一套策略）。两 trainer 策略不同 → A-vs-B ≠ B-vs-A。
    assert_ne!(ab1.mbb_per_game, ba.mbb_per_game);
}

#[test]
fn h3_lbr_proxy_is_finite_and_seed_deterministic() {
    let Some(table) = load_v3_or_skip() else {
        return;
    };
    let train_game = make_game(Arc::clone(&table));
    let eval_game = make_game(table);
    let mut trainer = EsMccfrTrainer::new(train_game, 0x4833_4c42_5250_0000);
    let mut rng = ChaCha20Rng::from_seed(0x4833_4c42_5250_0000);
    for _ in 0..10 {
        trainer.step(&mut rng).expect("10 update smoke");
    }
    let strategy = |info: &poker::InfoSetId, _n: usize| trainer.average_strategy(info);
    let cfg = NlheLbrConfig {
        probes: 16,
        rollouts_per_action: 2,
        seed: 0x4833_4c42_5250_1000,
        max_actions_per_probe: 128,
        max_actions_per_rollout: 512,
    };

    let a =
        estimate_simplified_nlhe_lbr(&eval_game, &strategy, &cfg).expect("LBR proxy should pass");
    let b = estimate_simplified_nlhe_lbr(&eval_game, &strategy, &cfg)
        .expect("LBR proxy should be repeatable");

    assert!(a.probes_used > 0);
    assert!(a.mean_best_response_chips.is_finite());
    assert!(a.standard_error_chips.is_finite());
    assert_eq!(a.mean_best_response_chips, b.mean_best_response_chips);
    assert_eq!(a.standard_error_chips, b.standard_error_chips);
}

#[test]
#[ignore = "release/--ignored opt-in；加载 H3 100M/500M/1B checkpoint 并跑 10K fixed-seed head-to-head"]
fn h3_checkpoint_head_to_head_curve_pairs_10k_are_finite() {
    let Some(table) = load_v3_or_skip() else {
        return;
    };
    let Some(trainer_100m) =
        load_h3_checkpoint_or_skip(H3_CHECKPOINT_100M_PATH, Arc::clone(&table))
    else {
        return;
    };
    let Some(trainer_500m) =
        load_h3_checkpoint_or_skip(H3_CHECKPOINT_500M_PATH, Arc::clone(&table))
    else {
        return;
    };
    let Some(trainer_1b) = load_h3_checkpoint_or_skip(H3_CHECKPOINT_1B_PATH, Arc::clone(&table))
    else {
        return;
    };
    assert_eq!(trainer_100m.update_count(), 100_000_008);
    assert_eq!(trainer_500m.update_count(), 500_000_016);
    assert_eq!(trainer_1b.update_count(), 1_000_000_000);

    let cfg = HeadToHeadConfig {
        hands_per_seat: 5_000,
        seed: 0x4833_4832_4821_0001,
        max_actions_per_hand: 512,
    };

    let pairs = [
        (
            "500M_vs_100M",
            H3_CHECKPOINT_500M_PATH,
            &trainer_500m,
            H3_CHECKPOINT_100M_PATH,
            &trainer_100m,
        ),
        (
            "1B_vs_500M",
            H3_CHECKPOINT_1B_PATH,
            &trainer_1b,
            H3_CHECKPOINT_500M_PATH,
            &trainer_500m,
        ),
        (
            "1B_vs_100M",
            H3_CHECKPOINT_1B_PATH,
            &trainer_1b,
            H3_CHECKPOINT_100M_PATH,
            &trainer_100m,
        ),
    ];

    for (label, later_path, later, earlier_path, earlier) in pairs {
        let report = evaluate_checkpoint_head_to_head(Arc::clone(&table), later, earlier, cfg)
            .unwrap_or_else(|e| panic!("{label} head-to-head should evaluate: {e}"));

        eprintln!(
            "H3 checkpoint H2H {label}: later={} earlier={} hands={} seed=0x{:016x} later_vs_earlier={:.3} mbb/g SE={:.3} 95%CI=[{:.3}, {:.3}] later_as_SB={:.3} later_as_BB={:.3} total_chips={:.0}",
            later_path,
            earlier_path,
            report.hands,
            report.seed,
            report.new_mbb_per_game,
            report.standard_error_mbb_per_game,
            report.ci95_low_mbb_per_game,
            report.ci95_high_mbb_per_game,
            report.new_as_sb_mbb_per_game,
            report.new_as_bb_mbb_per_game,
            report.new_total_chips,
        );

        assert_eq!(report.hands, cfg.hands_per_seat * 2);
        assert_eq!(report.hands_per_seat, cfg.hands_per_seat);
        assert!(report.new_total_chips.is_finite());
        assert!(report.new_mbb_per_game.is_finite());
        assert!(report.standard_error_mbb_per_game.is_finite());
        assert!(report.ci95_low_mbb_per_game <= report.ci95_high_mbb_per_game);
    }
}
