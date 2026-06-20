//! 启发式 9-max 引擎正确性套件。
//!
//! 覆盖：卡牌解析、169 规范化双射、下注重建（对照真实 OpenPoker 序列）、equity 单调性、
//! made-hand 分类、以及**安全不变量**（任意请求都出合法动作、永不 panic）。

use poker::core::Street;
use poker::core::{Card, Rank, Suit};
use poker::heuristic::config::HeuristicConfig;
use poker::heuristic::equity::{canonical_index, EquityEngine};
use poker::heuristic::features::{made_class, MadeClass};
use poker::heuristic::view::{parse_card, GameView, HistAction, Request, ValidActions};
use poker::heuristic::{HeuristicBot, Request as R2};

fn c(s: &str) -> Card {
    parse_card(s).unwrap()
}

fn valid(can_check: bool, can_call: bool, can_raise: bool, mn: u64, mx: u64) -> ValidActions {
    ValidActions {
        can_check,
        can_call,
        can_raise,
        min_raise: Some(mn),
        max_raise: Some(mx),
    }
}

#[test]
fn card_parse_roundtrip() {
    assert_eq!(c("2c"), Card::new(Rank::Two, Suit::Clubs));
    assert_eq!(c("As"), Card::new(Rank::Ace, Suit::Spades));
    assert_eq!(c("Td"), Card::new(Rank::Ten, Suit::Diamonds));
    assert_eq!(c("kh"), Card::new(Rank::King, Suit::Hearts));
    assert!(parse_card("Xz").is_none());
    assert!(parse_card("A").is_none());
    assert!(parse_card("Ahh").is_none());
}

#[test]
fn canonical_index_bijective_169() {
    // 所有 1326 个 2-card 组合映到 0..169，且恰好覆盖 169 个类。
    let mut seen = [false; 169];
    let mut classes = std::collections::HashSet::new();
    for a in 0..52u8 {
        for b in (a + 1)..52u8 {
            let idx = canonical_index(Card::from_u8(a).unwrap(), Card::from_u8(b).unwrap());
            assert!(idx < 169);
            seen[idx] = true;
            classes.insert(idx);
        }
    }
    assert_eq!(classes.len(), 169, "应恰好 169 个规范类");
    assert!(seen.iter().all(|&x| x));
    // pair 在 0..13，suited 13..91，offsuit 91..169。
    assert_eq!(canonical_index(c("Ac"), c("Ad")), 12); // AA = pair rank 12
    let aks = canonical_index(c("Ac"), c("Kc"));
    let ako = canonical_index(c("Ac"), c("Kd"));
    assert!((13..91).contains(&aks), "AKs 在 suited 区");
    assert!((91..169).contains(&ako), "AKo 在 offsuit 区");
}

#[test]
fn betting_reconstruction_matches_canned() {
    // 真实日志首手的 flop 决策点：button=4, 6-max, sb10/bb20。
    // preflop: s1/s2/s3 fold, s4 raise to40, s5(SB) fold, s0(BB,me) call。
    let req = Request {
        hole: vec!["2c".into(), "Kc".into()],
        board: vec!["8s".into(), "6d".into(), "Td".into()],
        button_seat: 4,
        my_seat: 0,
        num_seats: 6,
        small_blind: 10,
        big_blind: 20,
        actions: vec![
            HistAction {
                seat: 1,
                action: "fold".into(),
                to: None,
            },
            HistAction {
                seat: 2,
                action: "fold".into(),
                to: None,
            },
            HistAction {
                seat: 3,
                action: "fold".into(),
                to: None,
            },
            HistAction {
                seat: 4,
                action: "raise".into(),
                to: Some(40),
            },
            HistAction {
                seat: 5,
                action: "fold".into(),
                to: None,
            },
            HistAction {
                seat: 0,
                action: "call".into(),
                to: None,
            },
        ],
        valid: valid(true, false, true, 20, 1960),
        stacks: vec![2000, 1416, 1322, 6020, 11891, 1460],
        names: Default::default(),
    };
    let v = GameView::from_request(&req);
    // 翻牌街，pot = 40(s4) + 40(me) + 10(s5 SB dead) = 90。
    assert_eq!(v.street, Street::Flop);
    assert_eq!(v.pot_chips(), 90, "pot 应含死钱 SB 10");
    assert_eq!(v.to_call_chips(), 0, "翻牌无人下注，不需跟注");
    assert_eq!(v.num_active(), 2, "只剩 s4 与 me");
    assert_eq!(v.num_opponents(), 1);
    assert!(v.folded[1] && v.folded[2] && v.folded[3] && v.folded[5]);
    assert!(!v.folded[0] && !v.folded[4]);
    // 翻前加注者 = s4（按钮位）。
    assert_eq!(v.preflop_aggressor, Some(4));
    assert_eq!(v.preflop_num_raises, 1);
}

#[test]
fn betting_reconstruction_preflop_3bet() {
    // 9-max：UTG open to 3bb, BTN 3bet to 9bb, me(BB) facing 3bet。
    let req = Request {
        hole: vec!["Ah".into(), "Kh".into()],
        board: vec![],
        button_seat: 7, // 9-max, BTN=7, SB=8, BB=0(me)
        my_seat: 0,
        num_seats: 9,
        small_blind: 1,
        big_blind: 2,
        actions: vec![
            HistAction {
                seat: 3,
                action: "raise".into(),
                to: Some(6),
            }, // UTG open 3bb=6
            HistAction {
                seat: 4,
                action: "fold".into(),
                to: None,
            },
            HistAction {
                seat: 5,
                action: "fold".into(),
                to: None,
            },
            HistAction {
                seat: 6,
                action: "fold".into(),
                to: None,
            },
            HistAction {
                seat: 7,
                action: "raise".into(),
                to: Some(18),
            }, // BTN 3bet
            HistAction {
                seat: 8,
                action: "fold".into(),
                to: None,
            }, // SB fold
        ],
        valid: valid(false, true, true, 34, 200),
        stacks: vec![200; 9],
        names: Default::default(),
    };
    let v = GameView::from_request(&req);
    assert_eq!(v.street, Street::Preflop);
    assert_eq!(v.preflop_num_raises, 2, "open + 3bet = 2 次加注");
    assert_eq!(v.high_bet, 18);
    // me(BB) committed 2（盲注），to_call = 18 - 2 = 16。
    assert_eq!(v.to_call_chips(), 16);
    assert_eq!(v.preflop_aggressor, Some(7));
}

#[test]
fn equity_monotonic_and_extremes() {
    let eq = EquityEngine::new(800, 42);
    // AA 强度 > 72o。
    let aa = eq.hand_strength(c("Ac"), c("Ad"));
    let trash = eq.hand_strength(c("7c"), c("2d"));
    assert!(aa > 0.8, "AA 强度应很高: {aa}");
    assert!(aa > trash + 0.3, "AA 应远强于 72o");

    // 绝对坚果（皇家同花顺，不可被平/被超）equity ≈ 1。
    let board = [c("Qs"), c("Js"), c("Ts"), c("2c"), c("3d")];
    let nut = eq.equity([c("As"), c("Ks")], &board, &[0.0, 0.0], 600, 1);
    assert!(nut > 0.99, "皇家同花顺应必胜: {nut}");

    // equity 随对手数增加单调下降（边际牌）。
    let board2 = [c("Ah"), c("Kd"), c("7c")];
    let e1 = eq.equity([c("As"), c("Qs")], &board2, &[0.3], 1500, 7);
    let e3 = eq.equity([c("As"), c("Qs")], &board2, &[0.3, 0.3, 0.3], 1500, 7);
    assert!(e1 > e3, "顶对面对更多对手 equity 应更低: {e1} vs {e3}");
}

#[test]
fn made_class_cases() {
    // 顶对：AhKh on A72。
    assert_eq!(
        made_class([c("Ah"), c("Kh")], &[c("Ad"), c("7c"), c("2s")]),
        MadeClass::TopPair
    );
    // 超对：QQ on J72。
    assert_eq!(
        made_class([c("Qh"), c("Qd")], &[c("Js"), c("7c"), c("2s")]),
        MadeClass::TopPair
    );
    // 暗三 / set：77 on 7K2。
    assert_eq!(
        made_class([c("7h"), c("7d")], &[c("7s"), c("Kc"), c("2s")]),
        MadeClass::Strong
    );
    // 同花 = Monster。
    assert_eq!(
        made_class([c("Ah"), c("5h")], &[c("Kh"), c("9h"), c("2h")]),
        MadeClass::Monster
    );
    // 空气：QJ on 7 7 2（board 对、我无对）。
    assert_eq!(
        made_class([c("Qh"), c("Jd")], &[c("7s"), c("7c"), c("2d")]),
        MadeClass::Air
    );
}

/// 安全不变量：大量边角请求都出合法动作、永不 panic。
#[test]
fn respond_always_legal_never_panics() {
    let bot = HeuristicBot::new(HeuristicConfig {
        equity: poker::heuristic::config::EquityConfig {
            samples: 120,
            preflop_table_samples: 400,
            seed: 1,
        },
        ..Default::default()
    });
    // 一组多样请求：不同人数、街、合法集、含坏数据。
    let mk = |hole: Vec<&str>, board: Vec<&str>, n: u8, va: ValidActions| R2 {
        hole: hole.into_iter().map(|s| s.to_string()).collect(),
        board: board.into_iter().map(|s| s.to_string()).collect(),
        button_seat: 0,
        my_seat: 1,
        num_seats: n,
        small_blind: 1,
        big_blind: 2,
        actions: vec![],
        valid: va,
        stacks: vec![200; n as usize],
        names: Default::default(),
    };
    let reqs = vec![
        mk(
            vec!["Ah", "Ad"],
            vec![],
            9,
            valid(false, true, true, 6, 200),
        ),
        mk(
            vec!["7c", "2d"],
            vec![],
            2,
            valid(true, false, true, 2, 200),
        ),
        mk(
            vec!["Kh", "Qh"],
            vec!["Kd", "7c", "2s"],
            6,
            valid(true, false, true, 2, 198),
        ),
        mk(
            vec!["5c", "6c"],
            vec!["7c", "8c", "Ad", "Kh"],
            3,
            valid(false, true, true, 20, 180),
        ),
        mk(
            vec!["Js", "Td"],
            vec!["2c", "5d", "9h", "Qs", "Ac"],
            8,
            valid(false, true, false, 0, 0),
        ),
        // 坏数据：空 hole、非法 board。
        mk(vec![], vec!["zz"], 5, valid(true, false, false, 0, 0)),
        // 只能 fold（面对全下、不能加注）。
        mk(vec!["3c", "8d"], vec![], 9, valid(false, true, false, 0, 0)),
    ];
    for req in reqs {
        let resp = bot.respond(&req);
        // 动作必须在合法集内。
        let ok = match resp.action.as_str() {
            "fold" => true, // fold 总可退化
            "check" => req.valid.can_check,
            "call" => req.valid.can_call,
            "raise" => {
                req.valid.can_raise
                    && resp
                        .amount
                        .map(|a| {
                            a >= req.valid.min_raise.unwrap_or(0)
                                && a <= req.valid.max_raise.unwrap_or(u64::MAX)
                        })
                        .unwrap_or(false)
            }
            "none" => true,
            _ => false,
        };
        assert!(
            ok,
            "非法响应: action={} amount={:?} valid={:?}",
            resp.action, resp.amount, req.valid
        );
    }
}

/// raise 金额始终夹在 [min_raise, max_raise]。
#[test]
fn raise_amount_clamped() {
    let bot = HeuristicBot::new(HeuristicConfig {
        equity: poker::heuristic::config::EquityConfig {
            samples: 150,
            preflop_table_samples: 400,
            seed: 5,
        },
        ..Default::default()
    });
    // AA 翻前必加注，金额夹到 [min,max]。
    let req = R2 {
        hole: vec!["Ah".into(), "As".into()],
        board: vec![],
        button_seat: 0,
        my_seat: 0,
        num_seats: 6,
        small_blind: 1,
        big_blind: 2,
        actions: vec![],
        valid: valid(false, false, true, 4, 50),
        stacks: vec![200; 6],
        names: Default::default(),
    };
    let resp = bot.respond(&req);
    if resp.action == "raise" {
        let a = resp.amount.unwrap();
        assert!((4..=50).contains(&a), "raise amount {a} 越界");
    }
}
