//! Leduc ES-MCCFR 平均策略可剥削度门槛
//! （`docs/temp/es_mccfr_leduc_convergence_issue_2026_10_25.md` §7）。
//!
//! 两人博弈下平均策略在对手节点按权重 1 累积（[`EsMccfrTrainer::average_at_opponent_nodes`]）。
//! 旧写法（遍历者节点按自身 reach 累积）在 Leduc 上 100 万次后停在 0.15–0.18 不再下降；
//! 新写法 200 万次 5 种子 0.0127–0.0146，2000 万次 0.0037 ± 0.0003。
//!
//! - `leduc_es_mccfr_100k_exploitability_below_0_1`（默认套件）：1 种子 10 万次 < 0.1
//!   （新写法实测 0.057–0.073，旧写法 0.24–0.45）。
//! - `leduc_es_mccfr_2m_exploitability_below_0_03`（`--ignored`，release）：5 种子各 200 万次，
//!   每个 < 0.03（旧写法最好的种子也有 0.047）。

use poker::training::leduc::{LeducGame, LeducInfoSet};
use poker::training::{exploitability, EsMccfrTrainer, LeducBestResponse, Trainer};
use poker::ChaCha20Rng;

fn train(seed: u64, updates: u64) -> EsMccfrTrainer<LeducGame> {
    let mut trainer = EsMccfrTrainer::new(LeducGame, seed);
    assert!(trainer.average_at_opponent_nodes());
    let mut rng = ChaCha20Rng::from_seed(seed);
    for _ in 0..updates {
        trainer.step(&mut rng).expect("step");
    }
    trainer
}

fn avg_exploitability(trainer: &EsMccfrTrainer<LeducGame>) -> f64 {
    let strategy = |info: &LeducInfoSet, n: usize| {
        let s = trainer.average_strategy(info);
        if s.len() == n {
            s
        } else {
            vec![1.0 / n as f64; n]
        }
    };
    exploitability::<LeducGame, LeducBestResponse>(&LeducGame, &strategy)
}

#[test]
fn leduc_es_mccfr_100k_exploitability_below_0_1() {
    let expl = avg_exploitability(&train(0x11, 100_000));
    eprintln!("Leduc ES-MCCFR 100k seed 0x11: exploitability = {expl:.5}");
    assert!(expl < 0.1, "exploitability {expl:.5} ≥ 0.1");
}

#[test]
#[ignore = "release/--ignored opt-in（5 seeds × 2M Leduc ES-MCCFR + 精确可剥削度）"]
fn leduc_es_mccfr_2m_exploitability_below_0_03() {
    let seeds = [0x11u64, 0x22, 0x33, 0x44, 0x55];
    let expl: Vec<f64> = seeds
        .iter()
        .map(|&s| avg_exploitability(&train(s, 2_000_000)))
        .collect();
    eprintln!("Leduc ES-MCCFR 2M seeds {seeds:x?}: exploitability = {expl:.5?}");
    for (s, e) in seeds.iter().zip(&expl) {
        assert!(*e < 0.03, "seed {s:#x}: exploitability {e:.5} ≥ 0.03");
    }
}
