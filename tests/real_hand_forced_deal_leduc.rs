//! 强制发牌（[`Game::root_for_traverser`]）在 Leduc 上的外部对照
//! （`docs/temp/real_hand_forced_deal_design_2026_10_25.md` V1）。
//!
//! Leduc 的信息集本来就精确到牌，等价于实时搜索里「真实手牌单独开信息集」之后的情形。包一层
//! [`ForcedDealLeduc`]：玩家 0 当遍历者时以概率 `q` 直接发 0 号牌，其余照常走机会节点。强制发牌
//! 只改变各信息集被访问的频率（每个信息集乘一个常数），不改变 ES-MCCFR 的收敛点 → 可剥削度应与
//! 不强制发牌的基线同量级。
//!
//! - `forced_deal_q0_matches_plain_leduc`：q = 0 时与原版 `LeducGame` 训练逐位相同（默认路径不动）。
//! - `forced_deal_q1_always_deals_card_0`：q = 1 时玩家 0 遍历的每一手都拿 0 号牌。
//! - `leduc_forced_deal_exploitability_matches_baseline`（`--ignored`，release）：5 个种子，
//!   q = 0.5 的平均可剥削度 ≤ 基线均值 + 2 倍基线标准差。

use poker::error::GameVariant;
use poker::training::game::{Game, NodeKind, PlayerId};
use poker::training::leduc::{LeducAction, LeducGame, LeducInfoSet, LeducState};
use poker::training::{exploitability, EsMccfrTrainer, LeducBestResponse, Trainer};
use poker::{ChaCha20Rng, RngSource};

/// 玩家 0 当遍历者时以概率 `q` 直接发 0 号牌的 Leduc。
struct ForcedDealLeduc {
    q: f64,
}

impl Game for ForcedDealLeduc {
    type State = LeducState;
    type Action = LeducAction;
    type InfoSet = LeducInfoSet;

    const VARIANT: GameVariant = <LeducGame as Game>::VARIANT;

    fn n_players(&self) -> usize {
        LeducGame.n_players()
    }

    fn root(&self, rng: &mut dyn RngSource) -> LeducState {
        LeducGame.root(rng)
    }

    fn root_for_traverser(&self, traverser: PlayerId, rng: &mut dyn RngSource) -> LeducState {
        let state = LeducGame.root(rng);
        if self.q > 0.0 && traverser == 0 {
            let u = (rng.next_u64() >> 11) as f64 / ((1u64 << 53) as f64);
            if u < self.q {
                // 根节点就是「发玩家 0 的私牌」机会节点。
                return LeducGame::next(state, LeducAction::Deal0, rng);
            }
        }
        state
    }

    fn current(state: &LeducState) -> NodeKind {
        LeducGame::current(state)
    }

    fn info_set(state: &LeducState, actor: PlayerId) -> LeducInfoSet {
        LeducGame::info_set(state, actor)
    }

    fn legal_actions(state: &LeducState) -> Vec<LeducAction> {
        LeducGame::legal_actions(state)
    }

    fn next(state: LeducState, action: LeducAction, rng: &mut dyn RngSource) -> LeducState {
        LeducGame::next(state, action, rng)
    }

    fn chance_distribution(state: &LeducState) -> Vec<(LeducAction, f64)> {
        LeducGame::chance_distribution(state)
    }

    fn payoff(state: &LeducState, player: PlayerId) -> f64 {
        LeducGame::payoff(state, player)
    }
}

fn train<G: Game>(game: G, seed: u64, updates: u64) -> EsMccfrTrainer<G> {
    train_with(game, seed, updates, None)
}

/// `lcfr_period = Some(p)` → LCFR-MCCFR（同两边，方差更小）；`None` = vanilla ES-MCCFR。
fn train_with<G: Game>(
    game: G,
    seed: u64,
    updates: u64,
    lcfr_period: Option<u64>,
) -> EsMccfrTrainer<G> {
    let mut trainer = EsMccfrTrainer::new(game, seed);
    if let Some(p) = lcfr_period {
        trainer = trainer.with_lcfr_period(p);
    }
    let mut rng = ChaCha20Rng::from_seed(seed);
    for _ in 0..updates {
        trainer.step(&mut rng).expect("step");
    }
    trainer
}

fn leduc_exploitability(trainer: &EsMccfrTrainer<ForcedDealLeduc>) -> f64 {
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
fn forced_deal_q0_matches_plain_leduc() {
    let plain = train(LeducGame, 0x51, 2_000);
    let forced = train(ForcedDealLeduc { q: 0.0 }, 0x51, 2_000);
    assert_eq!(plain.regret_table().len(), forced.regret_table().len());
    for (info, regret) in plain.regret_table().inner() {
        let other = forced
            .regret_table()
            .inner()
            .get(info)
            .expect("same infosets");
        let bits = |v: &[f64]| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
        assert_eq!(bits(regret), bits(other), "{info:?}");
        assert_eq!(plain.average_strategy(info), forced.average_strategy(info));
    }
}

#[test]
fn forced_deal_q1_always_deals_card_0() {
    let game = ForcedDealLeduc { q: 1.0 };
    let mut rng = ChaCha20Rng::from_seed(0xD0);
    for _ in 0..200 {
        let s = game.root_for_traverser(0, &mut rng);
        assert_eq!(s.cards[0], 0, "player 0 must hold card 0");
        let s = game.root_for_traverser(1, &mut rng);
        assert!(matches!(ForcedDealLeduc::current(&s), NodeKind::Chance));
    }
}

#[test]
#[ignore = "release/--ignored opt-in（2 × 5 seeds × Leduc ES-MCCFR + 精确可剥削度）"]
fn leduc_forced_deal_exploitability_matches_baseline() {
    let updates: u64 = std::env::var("LEDUC_FORCED_UPDATES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2_000_000);
    // LEDUC_FORCED_LCFR_PERIOD 设了 → 两边都用 LCFR-MCCFR（同 period）。
    let lcfr: Option<u64> = std::env::var("LEDUC_FORCED_LCFR_PERIOD")
        .ok()
        .and_then(|v| v.parse().ok());
    let seeds = [0x11u64, 0x22, 0x33, 0x44, 0x55];
    let run = |q: f64| -> Vec<f64> {
        seeds
            .iter()
            .map(|&s| leduc_exploitability(&train_with(ForcedDealLeduc { q }, s, updates, lcfr)))
            .collect()
    };
    let stats = |v: &[f64]| {
        let n = v.len() as f64;
        let mean = v.iter().sum::<f64>() / n;
        let var = v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1.0);
        (mean, var.sqrt())
    };
    let base = run(0.0);
    let forced = run(0.5);
    let (bm, bs) = stats(&base);
    let (fm, fs) = stats(&forced);
    eprintln!("updates={updates} lcfr={lcfr:?} base={base:?} mean={bm:.5} sd={bs:.5}");
    eprintln!("updates={updates} lcfr={lcfr:?} forced(q=0.5)={forced:?} mean={fm:.5} sd={fs:.5}");
    assert!(
        fm <= bm + 2.0 * bs,
        "forced-deal exploitability {fm:.5} > baseline {bm:.5} + 2 × {bs:.5}"
    );
}
