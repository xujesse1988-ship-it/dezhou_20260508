//! Phase A 决策遥测（trace）正确性套件（`docs/scenario_heuristic_design.md` §6 Phase A）。
//!
//! 覆盖：trace 非空、节点 id 落在既定清单内、完整前缀路径（§3.2 规则 3）；场景 key 维度
//! （街/池型/角色/面对/位置）从 GameView 读取正确；trace 只描述不干预——`policy_action`/
//! `policy_amount` 与最终输出一致、legalize 未触发时 `legalized=None`；raise 不可用时的
//! policy 内降级仍归因原分支节点；无 hole 落 `fallback.no_hole`；Response 原样透传 trace。

use poker::heuristic::config::HeuristicConfig;
use poker::heuristic::view::{HistAction, Request, ValidActions};
use poker::heuristic::{DecisionAction, HeuristicBot};

fn small_bot() -> HeuristicBot {
    HeuristicBot::new(HeuristicConfig {
        equity: poker::heuristic::config::EquityConfig {
            samples: 120,
            preflop_table_samples: 400,
            seed: 1,
        },
        ..Default::default()
    })
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

fn ha(seat: u8, action: &str, to: Option<u64>) -> HistAction {
    HistAction {
        seat,
        action: action.into(),
        to,
        street: None,
    }
}

/// 翻前无人加注 + 强牌开池：叶节点 `pre.unopened.open`、完整前缀路径、场景维度正确、
/// trace 描述与最终动作一致（legalize 未触发）。
#[test]
fn trace_preflop_unopened_open() {
    let bot = small_bot();
    let req = Request {
        hole: vec!["Ah".into(), "As".into()],
        board: vec![],
        button_seat: 0,
        my_seat: 3,
        num_seats: 6,
        small_blind: 10,
        big_blind: 20,
        actions: vec![],
        valid: valid(false, true, true, 40, 2000),
        stacks: vec![2000; 6],
        names: Default::default(),
        sb_seat: None,
        bb_seat: None,
    };
    let d = bot.decide_request(&req);
    let tr = d.trace.as_ref().expect("trace 必须非空");
    assert_eq!(
        tr.path,
        vec!["pre", "pre.unopened", "pre.unopened.open"],
        "完整前缀路径（§3.2 规则 3）"
    );
    assert_eq!(tr.scenario.street, "pre");
    assert_eq!(tr.scenario.pot_type, "limped", "num_raises=0 → limped 桶");
    assert_eq!(tr.scenario.facing, "unopened");
    assert_eq!(tr.scenario.role, "none", "无人加注且 hero 非 aggressor");
    assert_eq!(tr.scenario.pos, "mid", "seat3/btn0/6max → Mid 桶");
    assert_eq!(tr.scenario.num_active, 6);
    assert!(tr.strength.unwrap() > 0.7, "AA 强度信号应记录且很高");
    assert!(
        tr.cmps
            .iter()
            .any(|c| c.what == "preflop.open_thr" && c.pass),
        "开池阈值比较应记录且通过: {:?}",
        tr.cmps
    );
    // trace 只描述不干预：policy 输出与最终动作一致，legalize 未触发。
    assert!(tr.legalized.is_none());
    assert_eq!(tr.policy_action, "raise");
    match d.action {
        DecisionAction::Raise(to) => assert_eq!(tr.policy_amount, Some(to)),
        ref a => panic!("AA first-in 应加注，实得 {a:?}"),
    }
}

/// 翻前 BB 面对 3bet：节点落 `pre.facing_raise.*` 族，场景 3bp/3bet/blind_def/bb。
#[test]
fn trace_preflop_facing_3bet_scenario_dims() {
    let bot = small_bot();
    let req = Request {
        hole: vec!["Ah".into(), "Kh".into()],
        board: vec![],
        button_seat: 7,
        my_seat: 0,
        num_seats: 9,
        small_blind: 1,
        big_blind: 2,
        actions: vec![
            ha(3, "raise", Some(6)),
            ha(4, "fold", None),
            ha(5, "fold", None),
            ha(6, "fold", None),
            ha(7, "raise", Some(18)),
            ha(8, "fold", None),
        ],
        valid: valid(false, true, true, 34, 200),
        stacks: vec![200; 9],
        names: Default::default(),
        sb_seat: None,
        bb_seat: None,
    };
    let d = bot.decide_request(&req);
    let tr = d.trace.as_ref().expect("trace 必须非空");
    let leaf = tr.path.last().unwrap();
    assert!(
        leaf.starts_with("pre.facing_raise."),
        "面对 3bet 应落 pre.facing_raise.* 族，实得 {leaf}"
    );
    assert_eq!(tr.path[0], "pre");
    assert_eq!(tr.path[1], "pre.facing_raise");
    assert_eq!(tr.scenario.street, "pre");
    assert_eq!(tr.scenario.pot_type, "3bp", "open+3bet=2 次加注 → 3bp");
    assert_eq!(tr.scenario.facing, "3bet");
    assert_eq!(tr.scenario.role, "blind_def", "BB 非 aggressor 面对加注");
    assert_eq!(tr.scenario.pos, "bb");
    assert!(
        tr.cmps.iter().any(|c| c.what == "preflop.reraise_thr"),
        "再加注阈值比较应记录: {:?}",
        tr.cmps
    );
    // 描述一致性。
    assert_eq!(tr.policy_action, action_name(&d.action));
}

/// 翻后 srp、BB 过牌位（无人下注）：节点落 `post.unopened.*` 族，equity/made 信号记录。
#[test]
fn trace_postflop_unopened_scenario_dims() {
    let bot = small_bot();
    // 真实日志首手 flop 决策点（heuristic9.rs 同款）：button=4, 6-max, BB=me 跟 open 看翻牌。
    let req = Request {
        hole: vec!["2c".into(), "Kc".into()],
        board: vec!["8s".into(), "6d".into(), "Td".into()],
        button_seat: 4,
        my_seat: 0,
        num_seats: 6,
        small_blind: 10,
        big_blind: 20,
        actions: vec![
            ha(1, "fold", None),
            ha(2, "fold", None),
            ha(3, "fold", None),
            ha(4, "raise", Some(40)),
            ha(5, "fold", None),
            ha(0, "call", None),
        ],
        valid: valid(true, false, true, 20, 1960),
        stacks: vec![2000, 1416, 1322, 6020, 11891, 1460],
        names: Default::default(),
        sb_seat: None,
        bb_seat: None,
    };
    let d = bot.decide_request(&req);
    let tr = d.trace.as_ref().expect("trace 必须非空");
    let leaf = tr.path.last().unwrap();
    assert!(
        leaf.starts_with("post.unopened."),
        "flop 无人下注应落 post.unopened.* 族，实得 {leaf}"
    );
    assert_eq!(tr.scenario.street, "flop");
    assert_eq!(tr.scenario.pot_type, "srp", "单加注池");
    assert_eq!(tr.scenario.facing, "unopened");
    assert_eq!(tr.scenario.role, "blind_def");
    assert_eq!(tr.scenario.num_active, 2);
    assert!(tr.equity.is_some(), "翻后 equity 必然已算、必须记录");
    assert_eq!(tr.made.as_deref(), Some("Air"), "K2 无对 → Air");
    assert_eq!(tr.draw, Some(false));
    assert!(
        tr.cmps.iter().any(|c| c.what == "postflop.value_thr"),
        "价值阈值比较应记录: {:?}",
        tr.cmps
    );
}

/// policy 想价值下注但 raise 不可用（policy 内降级为 check）：trace 仍归因原分支节点
/// `post.unopened.value_bet`，policy_action 记降级后的实际输出。
#[test]
fn trace_value_bet_downgrade_keeps_branch_node() {
    let bot = small_bot();
    let req = Request {
        hole: vec!["Ah".into(), "5h".into()],
        board: vec!["Kh".into(), "9h".into(), "2h".into()], // 坚果同花
        button_seat: 0,
        my_seat: 0,
        num_seats: 2,
        small_blind: 10,
        big_blind: 20,
        actions: vec![],
        valid: valid(true, false, false, 0, 0), // 不能加注/跟注，只能 check
        stacks: vec![2000; 2],
        names: Default::default(),
        sb_seat: None,
        bb_seat: None,
    };
    let d = bot.decide_request(&req);
    assert_eq!(d.action, DecisionAction::Check, "raise 不可用 → 降级 check");
    assert!(
        d.source.contains("downgrade"),
        "source 应带降级标记: {}",
        d.source
    );
    let tr = d.trace.as_ref().expect("trace 必须非空");
    assert_eq!(
        tr.path.last().unwrap(),
        "post.unopened.value_bet",
        "降级不改归因：仍是价值下注分支"
    );
    assert_eq!(tr.policy_action, "check", "记录进入 legalize 前的实际输出");
    assert!(
        tr.legalized.is_none(),
        "降级发生在 policy 内，legalize 未触发"
    );
}

/// 无手牌信息：落 `fallback.no_hole` 节点（兜底率分子，§8④）。
#[test]
fn trace_no_hole_fallback_node() {
    let bot = small_bot();
    let req = Request {
        hole: vec![],
        board: vec![],
        button_seat: 0,
        my_seat: 1,
        num_seats: 6,
        small_blind: 10,
        big_blind: 20,
        actions: vec![],
        valid: valid(true, false, false, 0, 0),
        stacks: vec![2000; 6],
        names: Default::default(),
        sb_seat: None,
        bb_seat: None,
    };
    let d = bot.decide_request(&req);
    assert!(d.source.starts_with("fallback:no_hole"));
    let tr = d.trace.as_ref().expect("fallback 也必须带 trace");
    assert_eq!(tr.path, vec!["fallback", "fallback.no_hole"]);
    assert_eq!(tr.policy_action, "check");
}

/// trace 只描述不干预：一批多样请求上，trace 恒非空，且（legalize 未触发时）
/// `policy_action`/`policy_amount` 与最终动作/尺寸完全一致；Response 原样透传 trace。
#[test]
fn trace_is_descriptive_not_prescriptive() {
    let bot = small_bot();
    let mk = |hole: Vec<&str>, board: Vec<&str>, n: u8, va: ValidActions| Request {
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
        sb_seat: None,
        bb_seat: None,
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
        mk(vec!["3c", "8d"], vec![], 9, valid(false, true, false, 0, 0)),
    ];
    for req in reqs {
        let d = bot.decide_request(&req);
        let tr = d
            .trace
            .as_ref()
            .unwrap_or_else(|| panic!("trace 必须非空: valid={:?} hole={:?}", req.valid, req.hole));
        assert!(!tr.path.is_empty(), "节点路径必须非空");
        if tr.legalized.is_none() {
            assert_eq!(
                tr.policy_action,
                action_name(&d.action),
                "trace 与最终动作不一致（leaf={}）",
                tr.path.last().unwrap()
            );
            let amt = match d.action {
                DecisionAction::Raise(to) => Some(to),
                _ => None,
            };
            assert_eq!(tr.policy_amount, amt, "trace 与最终尺寸不一致");
        }
        // advisor 层透传：Response 带同一份 trace。
        let resp = bot.respond(&req);
        let rt = resp.trace.as_ref().expect("Response 必须带 trace");
        assert_eq!(rt.path, tr.path, "Response 透传的 trace 路径应一致");
    }
}

fn action_name(a: &DecisionAction) -> &'static str {
    match a {
        DecisionAction::Fold => "fold",
        DecisionAction::Check => "check",
        DecisionAction::Call => "call",
        DecisionAction::Raise(_) => "raise",
    }
}
