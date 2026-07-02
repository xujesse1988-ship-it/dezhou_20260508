//! `GameView → range::Sample` 在线适配器测试（在线 range 预测的桥接）。
//!
//! **反训练/服务特征漂移的守门测试**：手算一整手多街场景的每条事件的 (st,s,a,c,p,tc,hb)
//! 精确筹码上下文，断言 [`GameView::to_range_sample`] 产出的 `ev` 逐字段一致（口径 =
//! `tools/range_dataset_build.py` `finalize_live_hand`）。再把产出的 `Sample` 喂给真实
//! 流水线 `tokenize` + `RangeModel::causal_logpriors`，证明适配器的 `Sample` 端到端可消费。

use poker::heuristic::equity::EquityEngine;
use poker::heuristic::range::{Event, RangeModel};
use poker::heuristic::tokenizer::tokenize;
use poker::heuristic::view::{GameView, HistAction, Request, ValidActions};

/// 构造一个带权威街标签的历史动作。
fn ha(seat: u8, action: &str, to: Option<u64>, street: u8) -> HistAction {
    HistAction {
        seat,
        action: action.to_string(),
        to,
        street: Some(street),
    }
}

/// 具体多街场景（3 handed：BTN=0 / SB=1 / BB=2，sb=1 / bb=2，栈 200=100bb）：
///   preflop: BTN raise→6, SB fold, BB call
///   flop   : BB check, BTN bet→4, BB call
///   turn   : BB check, BTN check
///   river  : BB bet→10, BTN fold
/// hero = BTN(seat0)；预测目标 = BB(seat2)。覆盖 raise/fold/call/check/bet 全 delta/tc/hb 分支。
fn scenario_request() -> Request {
    Request {
        hole: vec!["Ac".into(), "Ad".into()],
        board: vec![
            "8s".into(),
            "6d".into(),
            "Td".into(),
            "2h".into(),
            "Kc".into(),
        ],
        button_seat: 0,
        my_seat: 0,
        num_seats: 3,
        small_blind: 1,
        big_blind: 2,
        actions: vec![
            ha(0, "raise", Some(6), 0), // BTN open to 3bb
            ha(1, "fold", None, 0),     // SB fold
            ha(2, "call", None, 0),     // BB call
            ha(2, "check", None, 1),    // BB check
            ha(0, "bet", Some(4), 1),   // BTN bet
            ha(2, "call", None, 1),     // BB call
            ha(2, "check", None, 2),    // BB check
            ha(0, "check", None, 2),    // BTN check
            ha(2, "bet", Some(10), 3),  // BB bet
            ha(0, "fold", None, 3),     // BTN fold
        ],
        valid: ValidActions::default(),
        stacks: vec![200, 200, 200],
        names: Default::default(),
        sb_seat: Some(1),
        bb_seat: Some(2),
    }
}

/// 手算的期望事件流（每字段独立心算，见 scenario 注释）。
fn expected_events() -> Vec<Event> {
    let ev = |st: i64, s: i64, a: &str, c: i64, p: i64, tc: i64, hb: i64| Event {
        st,
        s,
        a: a.to_string(),
        c,
        p,
        tc,
        hb,
    };
    vec![
        // preflop：种子 commits SB=1/BB=2，street_high[0]=2，pot=3。
        ev(0, 0, "raise", 6, 9, 2, 2), // BTN raise to 6：hb=2, tc=2-0, c=6, pot=6+1+2
        ev(0, 1, "fold", 1, 9, 5, 6),  // SB fold：hb=6, tc=6-1=5, c=1(不变), pot 不变
        ev(0, 2, "call", 6, 13, 4, 6), // BB call：hb=6, tc=6-2=4, c=6, pot=6+1+6
        // flop：本街 commits 重置，high=0，pot 累计=13。
        ev(1, 2, "check", 0, 13, 0, 0), // BB check
        ev(1, 0, "bet", 4, 17, 0, 0),   // BTN bet 4：hb=0, tc=0, c=4, pot=10+1+6
        ev(1, 2, "call", 4, 21, 4, 4),  // BB call：hb=4, tc=4, c=4, pot=10+1+10
        // turn：重置，pot 累计=21。
        ev(2, 2, "check", 0, 21, 0, 0), // BB check
        ev(2, 0, "check", 0, 21, 0, 0), // BTN check
        // river：重置，pot 累计=21。
        ev(3, 2, "bet", 10, 31, 0, 0), // BB bet 10：hb=0, tc=0, c=10, pot=10+1+20
        ev(3, 0, "fold", 0, 31, 10, 10), // BTN fold：hb=10, tc=10-0=10, c=0
    ]
}

#[test]
fn gameview_sample_events_exact() {
    let req = scenario_request();
    let view = GameView::from_request(&req);
    let bb_seat = view.bb_seat as usize; // = 2（预测目标）
    let sample = view.to_range_sample(bb_seat, None);

    let expected = expected_events();

    // 事件流长度精确。
    assert_eq!(
        sample.ev.len(),
        expected.len(),
        "事件数不符：got {} want {}",
        sample.ev.len(),
        expected.len()
    );

    // 逐事件精确断言 (st,s,a,c,p,tc,hb)——反漂移守门。
    for (i, (got, want)) in sample.ev.iter().zip(expected.iter()).enumerate() {
        assert_eq!(
            got, want,
            "事件 {i} 不符：\n  got  = {got:?}\n  want = {want:?}"
        );
    }

    // 标量上下文字段。
    assert_eq!(sample.sb, 1, "sb 座位");
    assert_eq!(sample.bb, 2, "bb 座位");
    assert_eq!(sample.sbamt, 1, "小盲额");
    assert_eq!(sample.bbamt, 2, "大盲额");
    assert_eq!(sample.tseat, 2, "目标座位 = BB");
    assert_eq!(sample.ns, Some(3), "在手座位数");
    assert_eq!(sample.inh, vec![0, 1, 2], "在手座位列表");
    assert_eq!(sample.btn, Some(0), "按钮座位");
    assert_eq!(sample.hseat, Some(0), "hero 座位 = BTN");
    assert_eq!(
        sample.hhole,
        Some(vec!["Ac".to_string(), "Ad".to_string()]),
        "hero 底牌"
    );
    assert_eq!(
        sample.board,
        vec![
            "8s".to_string(),
            "6d".to_string(),
            "Td".to_string(),
            "2h".to_string(),
            "Kc".to_string()
        ],
        "board 卡串"
    );

    // stacks：盲注已扣（离线口径）——SB 200-1=199，BB 200-2=198，BTN 不扣=200。
    assert_eq!(
        sample.stacks.get("0").copied(),
        Some(200),
        "BTN 栈（未扣盲）"
    );
    assert_eq!(
        sample.stacks.get("1").copied(),
        Some(199),
        "SB 栈（扣小盲）"
    );
    assert_eq!(
        sample.stacks.get("2").copied(),
        Some(198),
        "BB 栈（扣大盲）"
    );

    // npr/pagg：从 ev 用离线公式重算（Sample 内部由 causal 模型同法推导）。
    let aggr = ["bet", "raise", "allin", "all_in"];
    let npr = sample
        .ev
        .iter()
        .filter(|e| e.st == 0 && aggr.contains(&e.a.as_str()))
        .count();
    let pagg = sample
        .ev
        .iter()
        .rev()
        .find(|e| e.st == 0 && aggr.contains(&e.a.as_str()))
        .map(|e| e.s);
    assert_eq!(npr, 1, "翻前加注次数");
    assert_eq!(pagg, Some(0), "翻前最后加注者 = BTN");

    // 默认画像（prof=None → 空 life/w50，n_shows=0）。
    assert_eq!(sample.prof.life.hands, 0.0, "默认画像 life.hands=0");
    assert_eq!(sample.prof.n_shows, 0.0, "默认画像 n_shows=0");
}

#[test]
fn gameview_sample_pipeline_smoke() {
    // 适配器产出的 Sample 端到端可被真实流水线消费（不 panic + 有限）。
    let req = scenario_request();
    let view = GameView::from_request(&req);
    let sample = view.to_range_sample(view.bb_seat as usize, None);

    // 1) tokenizer：非空且全有限。
    let feats = tokenize(&sample);
    assert!(!feats.is_empty(), "tokenize 应产出 token");
    assert!(
        feats.iter().all(|f| f.iter().all(|x| x.is_finite())),
        "token 特征应全有限"
    );

    // 2) RangeModel 因果 logprior：board 5 张 → 全 4 街应有快照且有限。
    let engine = EquityEngine::new(2500, 1);
    let model = RangeModel::new(&engine);
    let res = model.causal_logpriors(&sample);
    for s_idx in 0..4 {
        let snap = res.street_logprior[s_idx]
            .unwrap_or_else(|| panic!("街 {s_idx} 应有快照（board 5 张）"));
        assert!(
            snap.iter().all(|x| x.is_finite()),
            "街 {s_idx} 的 logprior 应全有限"
        );
    }
}
