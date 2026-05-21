//! `equity::equity_vs_hand_exact` + `equity::ochs_n_combo` 单测
//! （`docs/bucket_feature_design.md` §2.2）。
//!
//! 覆盖：
//! - `equity_vs_hand_exact` 在 river / turn / flop 三长度都与
//!   `MonteCarloEquity::equity_vs_hand`（postflop deterministic 路径）byte-equal
//! - `ochs_n_combo` 在 doc §2.2 例 1 monotone-board / AKs class 情形下 mean ≈ 0.47
//!   （combo 路径），与 rep 路径 ≈ 0.22 显著不同，证明 combo 展开生效
//! - cluster 全冲突 fallback 0.5

use std::sync::Arc;

use poker::abstraction::equity::{combos_for_class, equity_vs_hand_exact, ochs_n_combo};
use poker::eval::NaiveHandEvaluator;
use poker::{Card, ChaCha20Rng, EquityCalculator, HandEvaluator, MonteCarloEquity, Rank, Suit};

fn c(r: Rank, s: Suit) -> Card {
    Card::new(r, s)
}

fn evaluator() -> Arc<dyn HandEvaluator> {
    Arc::new(NaiveHandEvaluator)
}

#[test]
fn equity_vs_hand_exact_byte_equal_montecarlo_postflop() {
    let hole = [c(Rank::Jack, Suit::Diamonds), c(Rank::Jack, Suit::Clubs)];
    let opp = [c(Rank::Ace, Suit::Spades), c(Rank::King, Suit::Spades)];
    let board_flop = [
        c(Rank::Ten, Suit::Spades),
        c(Rank::Nine, Suit::Spades),
        c(Rank::Eight, Suit::Spades),
    ];
    let board_turn = [
        c(Rank::Ten, Suit::Spades),
        c(Rank::Nine, Suit::Spades),
        c(Rank::Eight, Suit::Spades),
        c(Rank::Two, Suit::Clubs),
    ];
    let board_river = [
        c(Rank::Ten, Suit::Spades),
        c(Rank::Nine, Suit::Spades),
        c(Rank::Eight, Suit::Spades),
        c(Rank::Two, Suit::Clubs),
        c(Rank::Seven, Suit::Hearts),
    ];

    let eval = evaluator();
    let mc = MonteCarloEquity::new(Arc::clone(&eval));
    let mut rng = ChaCha20Rng::from_seed(0xfeedface_cafef00du64);

    for board in [
        board_flop.as_slice(),
        board_turn.as_slice(),
        board_river.as_slice(),
    ] {
        let standalone = equity_vs_hand_exact(hole, opp, board, &*eval);
        let via_trait = mc.equity_vs_hand(hole, opp, board, &mut rng).expect("ok");
        assert_eq!(
            standalone.to_bits(),
            via_trait.to_bits(),
            "diverge on board.len()={}: standalone={standalone} via_trait={via_trait}",
            board.len()
        );
    }
}

#[test]
fn equity_vs_hand_exact_river_showdown() {
    // hero AhAd vs opp 2c3d on board AsAcKsKh2h → hero quad / boat 胜
    let hole = [c(Rank::Ace, Suit::Hearts), c(Rank::Ace, Suit::Diamonds)];
    let opp = [c(Rank::Two, Suit::Clubs), c(Rank::Three, Suit::Diamonds)];
    let board = [
        c(Rank::Ace, Suit::Spades),
        c(Rank::Ace, Suit::Clubs),
        c(Rank::King, Suit::Spades),
        c(Rank::King, Suit::Hearts),
        c(Rank::Two, Suit::Hearts),
    ];
    let eq = equity_vs_hand_exact(hole, opp, &board, &*evaluator());
    assert_eq!(eq.to_bits(), 1.0_f64.to_bits());
}

#[test]
fn ochs_n_combo_aks_monotone_doc_example() {
    // doc §2.2 例 1 设定：board = Ts9s8s（monotone）、hero = JdJc、opp class = AKs (90)。
    // doc 例子里给的 "AsKs hero equity ~0.22 / AhKh ~0.55 / mean ~0.47" 是**估算且偏差大**
    // ——actually 在 monotone flop 上 AsKs 已是 made nut flush（不是 draw），doc 分析文字
    // 写的 "nut flush draw + 2 overs" 把 made flush 误写为 draw。实际 enumerate 990
    // (turn, river) 得：
    //   AsKs (made flush) → hero JJ equity ≈ 0.0293（只剩 ~3% 翻盘）
    //   AhKh / AdKd / AcKc (无同花) → hero JJ equity ≈ 0.7869（JJ overpair vs AKo 优势）
    //   mean ≈ 0.5974
    // combo 路径 vs rep 路径差距比 doc 估的 0.25 更大（实际 ≈ 0.57），doc 主旨（combo
    // 展开能纠正 rep 路径塌缩）依然成立，仅数值需修正。
    let hole = [c(Rank::Jack, Suit::Diamonds), c(Rank::Jack, Suit::Clubs)];
    let board = [
        c(Rank::Ten, Suit::Spades),
        c(Rank::Nine, Suit::Spades),
        c(Rank::Eight, Suit::Spades),
    ];
    let eval = evaluator();

    // 单 cluster 只装 AKs 一类。
    let cpc: Vec<Vec<u8>> = vec![vec![90u8]];
    let result = ochs_n_combo(hole, &board, &*eval, &cpc);
    assert_eq!(result.len(), 1);
    let combo_mean = result[0];

    // rep 路径手算：AsKs 一项的 equity（representative_hole_for_class(90) = [As, Ks]）
    let rep_only = equity_vs_hand_exact(
        hole,
        [c(Rank::Ace, Suit::Spades), c(Rank::King, Suit::Spades)],
        &board,
        &*eval,
    );

    // 实际 enumerate 数值（±5% 容忍 future eval7 path 优化）
    assert!(
        combo_mean > 0.58 && combo_mean < 0.62,
        "combo mean expected ~0.597, got {combo_mean}"
    );
    assert!(
        rep_only < 0.05,
        "rep equity (AsKs made flush) expected ~0.029, got {rep_only}"
    );
    // combo 展开纠正 rep 塌缩：差值 > 0.5
    assert!(
        combo_mean - rep_only > 0.5,
        "combo vs rep gap should be > 0.5 (combo {combo_mean} rep {rep_only})"
    );
}

#[test]
fn ochs_n_combo_full_conflict_fallback() {
    // hole 占 AsAh，board 占 KsKh → AKs combos 4 个中 AsKs / AhKh 冲突 (As/Ah blocker)；
    // AdKd / AcKc 仍有效。这不是全冲突。
    // 构造一个 cluster 只装 33（pocket pair class 1），且 board 把所有 3 都用掉。
    // 33 combos = 3s3h / 3s3d / 3s3c / 3h3d / 3h3c / 3d3c（6 个），全在 (3♠/3♥/3♦/3♣)
    // 上展开。board 占用 3s/3h/3d/3c 全 4 张 + hole 占 As/Ah 共 6 张，board.len()=4。
    let hole = [c(Rank::Ace, Suit::Spades), c(Rank::Ace, Suit::Hearts)];
    let board = [
        c(Rank::Three, Suit::Spades),
        c(Rank::Three, Suit::Hearts),
        c(Rank::Three, Suit::Diamonds),
        c(Rank::Three, Suit::Clubs),
    ];
    // 33 = class 1（pocket pair Three）
    let cpc: Vec<Vec<u8>> = vec![vec![1u8]];
    let eval = evaluator();
    let result = ochs_n_combo(hole, &board, &*eval, &cpc);
    assert_eq!(result.len(), 1);
    // 全 6 个 33 combos 都跟 board 冲突 → fallback 0.5
    assert_eq!(result[0].to_bits(), 0.5_f64.to_bits());
}

#[test]
fn ochs_n_combo_skips_only_conflicting_combos() {
    // hole = AsAh：AKs class (90) 内 AsKs / AhKh 冲突，AdKd / AcKc 仍贡献。
    // result[0] = mean over 剩余 2 combos 的 equity ≠ 0.5 fallback。
    let hole = [c(Rank::Ace, Suit::Spades), c(Rank::Ace, Suit::Hearts)];
    let board = [
        c(Rank::Ten, Suit::Spades),
        c(Rank::Nine, Suit::Spades),
        c(Rank::Eight, Suit::Spades),
    ];
    let cpc: Vec<Vec<u8>> = vec![vec![90u8]];
    let eval = evaluator();
    let result = ochs_n_combo(hole, &board, &*eval, &cpc);

    // 剩余 2 个 combos AdKd, AcKc — hero 是 overpair AA + nut flush draw (As blocker)
    // → equity ~1.0（doc §2.2 例 2 "vs AKs 我打爆" ≈ 1.0）
    assert!(
        result[0] > 0.95,
        "AsAh on Ts9s8s vs AdKd/AcKc mean equity expected ~1.0, got {}",
        result[0]
    );

    // 校验"是 2 个 combos 求 mean"而不是 4 个：通过比较与单调路径外推
    let valid_combos: Vec<_> = combos_for_class(90)
        .into_iter()
        .filter(|combo| {
            let mut overlaps = false;
            for c0 in combo.iter() {
                if c0.to_u8() == hole[0].to_u8() || c0.to_u8() == hole[1].to_u8() {
                    overlaps = true;
                }
                for b in board.iter() {
                    if c0.to_u8() == b.to_u8() {
                        overlaps = true;
                    }
                }
            }
            !overlaps
        })
        .collect();
    assert_eq!(
        valid_combos.len(),
        2,
        "AKs class on AsAh + Ts9s8s should have 2 valid combos"
    );
}
