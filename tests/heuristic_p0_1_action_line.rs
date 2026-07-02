//! P0-1：`GameView` 逐座行动线记忆 + 逐街 aggressor 暴露（docs/scenario_heuristic_design.md §7 P0-1）。
//!
//! 纯增字段/访问器，默认决策不读它们（byte-equal 由构造保证）。这里验证：
//! - `seat_actions(seat)` 返回精确时序 + 权威街标；
//! - `aggressor_on_street(1/2/3)` 暴露以前被丢弃的 flop/turn/river aggressor；
//! - `seat_street_actions` 逐街过滤正确；
//! - 无街标（启发式分段）路径下街标由重放 `street_idx` 派生。

use poker::core::Street;
use poker::heuristic::view::{GameView, HistAction, Request, ValidActions};

fn valid(can_check: bool, can_call: bool, can_raise: bool, mn: u64, mx: u64) -> ValidActions {
    ValidActions {
        can_check,
        can_call,
        can_raise,
        min_raise: Some(mn),
        max_raise: Some(mx),
    }
}

/// (街, 动作串, to) 三元投影，便于精确断言时序。
fn proj(v: &GameView, seat: usize) -> Vec<(u8, &str, Option<u64>)> {
    v.seat_actions(seat)
        .iter()
        .map(|a| (a.street, a.action.as_str(), a.to))
        .collect()
}

#[test]
fn action_line_records_full_multistreet_hand_with_labels() {
    // n=3, button=0（多人 → sb=1, bb=2）。权威街标路径。
    // preflop: s0(BTN) raise→60, s1(SB) fold, s2(BB) call。   aggressor=s0
    // flop   : s2 check, s0 bet→80 (c-bet), s2 call。          aggressor=s0
    // turn   : s2 check, s0 check。                            aggressor=None
    // river  : s2 bet→200 (lead), s0 fold。                    aggressor=s2（当前街，留在 cur_aggressor）
    let ha = |seat: u8, action: &str, to: Option<u64>, street: u8| HistAction {
        seat,
        action: action.into(),
        to,
        street: Some(street),
    };
    let req = Request {
        hole: vec!["Ah".into(), "Kd".into()],
        board: vec![
            "7c".into(),
            "2d".into(),
            "9h".into(),
            "Qs".into(),
            "3s".into(),
        ], // river (5 张)
        button_seat: 0,
        my_seat: 2,
        num_seats: 3,
        small_blind: 10,
        big_blind: 20,
        actions: vec![
            ha(0, "raise", Some(60), 0),
            ha(1, "fold", None, 0),
            ha(2, "call", None, 0),
            ha(2, "check", None, 1),
            ha(0, "bet", Some(80), 1),
            ha(2, "call", None, 1),
            ha(2, "check", None, 2),
            ha(0, "check", None, 2),
            ha(2, "bet", Some(200), 3),
            ha(0, "fold", None, 3),
        ],
        valid: valid(false, true, true, 400, 940),
        stacks: vec![1000; 3],
        names: Default::default(),
        sb_seat: None,
        bb_seat: None,
    };
    let v = GameView::from_request(&req);
    assert_eq!(v.street, Street::River);

    // --- 逐座动作线：精确时序 + 权威街标 ---
    assert_eq!(
        proj(&v, 0),
        vec![
            (0, "raise", Some(60)),
            (1, "bet", Some(80)),
            (2, "check", None),
            (3, "fold", None),
        ],
        "seat0 应记全 raise/bet/check/fold 四动作，街标 0/1/2/3"
    );
    assert_eq!(proj(&v, 1), vec![(0, "fold", None)], "seat1 只有翻前 fold");
    assert_eq!(
        proj(&v, 2),
        vec![
            (0, "call", None),
            (1, "check", None),
            (1, "call", None),
            (2, "check", None),
            (3, "bet", Some(200)),
        ],
        "seat2 全线：call/check/call/check/bet"
    );
    // 越界座 → 空切片（never panic）。
    assert!(v.seat_actions(99).is_empty());

    // --- 逐街 aggressor：以前被丢弃的 flop/turn/river 现已暴露 ---
    assert_eq!(
        v.aggressor_on_street(0),
        Some(0),
        "preflop aggressor = seat0"
    );
    assert_eq!(
        v.aggressor_on_street(1),
        Some(0),
        "flop aggressor = seat0（曾被丢弃）"
    );
    assert_eq!(v.aggressor_on_street(2), None, "turn 过牌，无 aggressor");
    assert_eq!(
        v.aggressor_on_street(3),
        Some(2),
        "river aggressor = seat2（当前街，也须暴露）"
    );
    assert_eq!(v.aggressor_on_street(9), None, "越界街 → None");
    assert_eq!(
        v.street_aggressors,
        [Some(0), Some(0), None, Some(2)],
        "full per-street aggressor 数组"
    );

    // 既有导出保持一致（回归）。
    assert_eq!(v.preflop_aggressor, Some(0));
    assert_eq!(
        v.street_aggressor,
        Some(2),
        "当前街(river) aggressor 仍是 seat2"
    );

    // --- seat_street_actions 过滤 ---
    let s2_flop: Vec<&str> = v
        .seat_street_actions(2, 1)
        .map(|a| a.action.as_str())
        .collect();
    assert_eq!(s2_flop, vec!["check", "call"], "seat2 翻牌两动作");
    let s0_turn: Vec<&str> = v
        .seat_street_actions(0, 2)
        .map(|a| a.action.as_str())
        .collect();
    assert_eq!(s0_turn, vec!["check"], "seat0 转牌只 check");
    let s0_river: Vec<&str> = v
        .seat_street_actions(0, 3)
        .map(|a| a.action.as_str())
        .collect();
    assert_eq!(s0_river, vec!["fold"], "seat0 河牌 fold");
    let s1_flop: Vec<&str> = v
        .seat_street_actions(1, 1)
        .map(|a| a.action.as_str())
        .collect();
    assert!(s1_flop.is_empty(), "seat1 翻前已 fold，翻牌无动作");
}

#[test]
fn action_line_street_derived_from_replay_when_labels_absent() {
    // 无街标 → 启发式分段；街标由重放 street_idx 派生（走 push 的 fallback 分支）。
    // HU n=2, button=0（HU 特例 sb=0, bb=1）。
    // preflop: s0 raise→60, s1 call（关闭）。
    // flop   : s1 bet→40, s0 call。
    let req = Request {
        hole: vec!["Ah".into(), "Kd".into()],
        board: vec!["7c".into(), "2d".into(), "9h".into()], // flop (3 张)
        button_seat: 0,
        my_seat: 0,
        num_seats: 2,
        small_blind: 10,
        big_blind: 20,
        actions: vec![
            HistAction {
                seat: 0,
                action: "raise".into(),
                to: Some(60),
                street: None,
            },
            HistAction {
                seat: 1,
                action: "call".into(),
                to: None,
                street: None,
            },
            HistAction {
                seat: 1,
                action: "bet".into(),
                to: Some(40),
                street: None,
            },
            HistAction {
                seat: 0,
                action: "call".into(),
                to: None,
                street: None,
            },
        ],
        valid: valid(true, false, true, 40, 940),
        stacks: vec![1000; 2],
        names: Default::default(),
        sb_seat: None,
        bb_seat: None,
    };
    let v = GameView::from_request(&req);
    assert_eq!(v.street, Street::Flop);

    // 街标由重放推导：preflop 两动作 @0，flop 两动作 @1。
    assert_eq!(
        proj(&v, 0),
        vec![(0, "raise", Some(60)), (1, "call", None)],
        "seat0：翻前 raise + 翻牌 call"
    );
    assert_eq!(
        proj(&v, 1),
        vec![(0, "call", None), (1, "bet", Some(40))],
        "seat1：翻前 call + 翻牌 bet"
    );

    // 逐街 aggressor 暴露（flop aggressor 曾被丢弃；无标路径也须捕获）。
    assert_eq!(
        v.aggressor_on_street(0),
        Some(0),
        "preflop aggressor = seat0"
    );
    assert_eq!(
        v.aggressor_on_street(1),
        Some(1),
        "flop aggressor = seat1（曾被丢弃，现暴露）"
    );
}
