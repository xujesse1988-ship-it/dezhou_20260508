//! `equity::equity_river_exact` + `equity::equity_hist_8` 单测
//! （`docs/bucket_feature_design.md` §2.1）。
//!
//! 覆盖：
//! - `equity_river_exact` 与 `MonteCarloEquity::equity(river_exact=true)` byte-equal
//!   （新顶层公开 fn 不漂 existing v3 baseline）
//! - hist_8 各分量 ∈ [0, 1] 且和 = 1.0
//! - hist_8 在 trivial 极端 hole / board 上的分布方向（坚果 / 烂牌）合理

use std::sync::Arc;

use poker::abstraction::equity::{equity_hist_8, equity_river_exact};
use poker::eval::NaiveHandEvaluator;
use poker::{Card, ChaCha20Rng, EquityCalculator, HandEvaluator, MonteCarloEquity, Rank, Suit};

fn c(r: Rank, s: Suit) -> Card {
    Card::new(r, s)
}

fn evaluator() -> Arc<dyn HandEvaluator> {
    Arc::new(NaiveHandEvaluator)
}

#[test]
fn equity_river_exact_matches_montecarlo_river_exact_path() {
    // 任挑一手 + 5-card board，确认新 standalone fn 与既有
    // MonteCarloEquity::with_river_exact(true).equity(...) byte-equal。
    let hole = [c(Rank::Ace, Suit::Hearts), c(Rank::Ace, Suit::Diamonds)];
    let board = [
        c(Rank::Two, Suit::Clubs),
        c(Rank::Seven, Suit::Diamonds),
        c(Rank::Jack, Suit::Spades),
        c(Rank::King, Suit::Hearts),
        c(Rank::Four, Suit::Clubs),
    ];

    let eval = evaluator();
    let standalone = equity_river_exact(hole, &board, &*eval);

    let mc = MonteCarloEquity::new(Arc::clone(&eval)).with_river_exact(true);
    // river_exact 路径不消费 RNG（impl detail），任意 seed 都不影响结果
    let mut rng = ChaCha20Rng::from_seed(0xdead_beef_dead_beefu64);
    let via_trait = mc.equity(hole, &board, &mut rng).expect("ok");

    assert_eq!(
        standalone.to_bits(),
        via_trait.to_bits(),
        "equity_river_exact diverges from MonteCarloEquity::equity(river_exact=true): \
         standalone={standalone} via_trait={via_trait}"
    );
}

#[test]
fn equity_river_exact_quad_aces_dominates() {
    // hole = AA, board = AAxxx → 几乎稳赢（除非 board 也成 quad / flush）。
    let hole = [c(Rank::Ace, Suit::Hearts), c(Rank::Ace, Suit::Diamonds)];
    let board = [
        c(Rank::Ace, Suit::Clubs),
        c(Rank::Ace, Suit::Spades),
        c(Rank::Two, Suit::Hearts),
        c(Rank::Seven, Suit::Diamonds),
        c(Rank::Five, Suit::Clubs),
    ];
    let eq = equity_river_exact(hole, &board, &*evaluator());
    assert!(eq > 0.99, "quad aces equity should be > 0.99, got {eq}");
}

#[test]
fn equity_river_exact_busted_hand_low() {
    // hole = 2c3d, board = all higher straight / flush risks → low equity
    let hole = [c(Rank::Two, Suit::Clubs), c(Rank::Three, Suit::Diamonds)];
    let board = [
        c(Rank::Ace, Suit::Hearts),
        c(Rank::King, Suit::Hearts),
        c(Rank::Queen, Suit::Hearts),
        c(Rank::Jack, Suit::Hearts),
        c(Rank::Ten, Suit::Hearts),
    ];
    // Note: board 已成 royal flush，hero 也用 board 的 royal → tie if opp 也用 board。
    // 实际上 hero 的 2c3d 完全没参与最佳 5-card hand（board itself = royal flush），
    // 任何 opp 都同样使用 board → 全 tie，equity = 0.5。
    let eq = equity_river_exact(hole, &board, &*evaluator());
    assert!(
        (eq - 0.5).abs() < 1e-9,
        "equity on shared royal flush board must be exactly 0.5, got {eq}"
    );
}

#[test]
fn equity_hist_8_sums_to_one_flop() {
    // 任一 flop 状态 → 1081 future outcomes，hist 总和必须 = 1.0
    let hole = [c(Rank::Eight, Suit::Spades), c(Rank::Nine, Suit::Spades)];
    let board = [
        c(Rank::Seven, Suit::Spades),
        c(Rank::Six, Suit::Hearts),
        c(Rank::Five, Suit::Diamonds),
    ];
    let hist = equity_hist_8(hole, &board, &*evaluator());
    let sum: f64 = hist.iter().sum();
    assert!(
        (sum - 1.0).abs() < 1e-9,
        "hist_8 sum must be 1.0, got {sum} (hist = {hist:?})"
    );
    for (i, &v) in hist.iter().enumerate() {
        assert!(
            (0.0..=1.0).contains(&v),
            "hist_8 bin {i} = {v} out of [0, 1]"
        );
    }
}

#[test]
fn equity_hist_8_sums_to_one_turn() {
    // 任一 turn 状态 → 46 future outcomes
    let hole = [c(Rank::Eight, Suit::Spades), c(Rank::Nine, Suit::Spades)];
    let board = [
        c(Rank::Seven, Suit::Spades),
        c(Rank::Six, Suit::Hearts),
        c(Rank::Five, Suit::Diamonds),
        c(Rank::King, Suit::Clubs),
    ];
    let hist = equity_hist_8(hole, &board, &*evaluator());
    let sum: f64 = hist.iter().sum();
    assert!(
        (sum - 1.0).abs() < 1e-9,
        "hist_8 sum must be 1.0, got {sum} (hist = {hist:?})"
    );
}

#[test]
fn equity_hist_8_nuts_skewed_right() {
    // hole = AsAh, board = AcAdKs → flop set quads / boat 几乎稳赢；
    // hist mass 应该集中在 bin 6-7。
    let hole = [c(Rank::Ace, Suit::Spades), c(Rank::Ace, Suit::Hearts)];
    let board = [
        c(Rank::Ace, Suit::Clubs),
        c(Rank::Ace, Suit::Diamonds),
        c(Rank::King, Suit::Spades),
    ];
    let hist = equity_hist_8(hole, &board, &*evaluator());
    let right_mass = hist[6] + hist[7];
    assert!(
        right_mass > 0.95,
        "quad-aces flop hist right-tail mass should be > 0.95, got {right_mass} \
         (hist = {hist:?})"
    );
}

#[test]
fn equity_hist_8_trash_skewed_left() {
    // hole = 2c3d on board = AsKhQs（无对、无 draw）→ hist 偏左
    let hole = [c(Rank::Two, Suit::Clubs), c(Rank::Three, Suit::Diamonds)];
    let board = [
        c(Rank::Ace, Suit::Spades),
        c(Rank::King, Suit::Hearts),
        c(Rank::Queen, Suit::Spades),
    ];
    let hist = equity_hist_8(hole, &board, &*evaluator());
    let left_mass = hist[0] + hist[1] + hist[2];
    assert!(
        left_mass > 0.6,
        "23-trash flop hist left-tail mass should be > 0.6, got {left_mass} \
         (hist = {hist:?})"
    );
}
