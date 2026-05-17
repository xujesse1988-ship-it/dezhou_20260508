//! H3 简化 heads-up NLHE blueprint 评测闭环测试。

use std::path::PathBuf;
use std::sync::Arc;

use poker::training::game::{Game, NodeKind};
use poker::training::{
    estimate_simplified_nlhe_lbr, evaluate_blueprint_vs_baseline, EsMccfrTrainer,
    NlheBaselinePolicy, NlheEvaluationConfig, NlheEvaluationError, NlheLbrConfig, Trainer,
};
use poker::{BucketTable, ChaCha20Rng, RngSource, SimplifiedNlheGame};

const V3_ARTIFACT_PATH: &str = "artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin";
const V3_BODY_BLAKE3_HEX: &str = "67ee555439f2c918698650c05f40a7a5e9e812280ceb87fc3c6590add98650cd";

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
        NlheBaselinePolicy::CallStation,
        NlheBaselinePolicy::OverlyTight,
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
