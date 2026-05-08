//! B1：10 个 fixed scenario 测试。
//!
//! 这些测试编码 `pluribus_stage1_workflow.md` §B1 列出的 10 个核心场景。
//! 它们驱动 [`GameState`] 的对外契约（API §4），断言终局状态。
//!
//! **A1 状态**：所有 GameState 方法 `unimplemented!()`，本文件中的每个 `#[test]`
//! 都会在第一次调用 `GameState::new` / `with_rng` 时 panic。预期行为：
//!
//! - `cargo test --no-run` 通过（编译验证签名匹配 API §4 + tests/common）。
//! - `cargo test scenarios` 全部 panic（此处验证 spec 由 B2 实现填齐）。
//!
//! **B2 阶段**：所有方法落地后，本文件中的断言激活；测试应保持原文，仅
//! 实现侧调整。
//!
//! 角色边界：本文件属 `[测试]` agent 产物。如果某条断言在 B2 阶段被认为不
//! 正确，由 `[实现]` agent 反馈、决策 agent review，再由 `[测试]` agent 修订。
//! `[实现]` agent 不允许直接改测试。

mod common;

use common::{
    build_dealing_order, card, chips, expected_total_chips, pick_unused_padding, seat, Invariants,
    StackedDeckRng,
};

use poker::{Action, GameState, PlayerStatus, SeatId, Street, TableConfig};

use std::collections::HashSet;

// ============================================================================
// 通用驱动器
// ============================================================================

/// 按 `(expected_seat, action)` 顺序应用动作；每步前断言 `current_player`，
/// 每步后跑一次 [`Invariants::check_all`]。
fn drive(state: &mut GameState, expected_total: u64, plan: &[(SeatId, Action)]) {
    for (i, (want_seat, action)) in plan.iter().enumerate() {
        let cp = state
            .current_player()
            .unwrap_or_else(|| panic!("step {i}: current_player == None, expected {want_seat:?}"));
        assert_eq!(
            cp, *want_seat,
            "step {i}: current_player mismatch (expected {want_seat:?}, got {cp:?})"
        );
        state
            .apply(*action)
            .unwrap_or_else(|e| panic!("step {i}: apply({action:?}) failed: {e}"));
        Invariants::check_all(state, expected_total)
            .unwrap_or_else(|e| panic!("step {i} (after {action:?}): {e}"));
    }
}

/// 6-max 默认配置 + 占位 seed=0，用于"动作机制无关于具体牌"场景。
fn default_state(seed: u64) -> (GameState, TableConfig) {
    let cfg = TableConfig::default_6max_100bb();
    let state = GameState::new(&cfg, seed);
    (state, cfg)
}

// ============================================================================
// 1. smoke_open_raise_call_check_to_river
// ============================================================================
//
// 6-max 默认 100BB。BTN 开局 raise 到 300，SB 弃，BB 跟，三街全 check，
// 走到 showdown。验证：状态机能从 preflop 一路推进到 Showdown，
// `board.len() == 5`，`is_terminal == true`，I-001 链条始终成立。
#[test]
fn smoke_open_raise_call_check_to_river() {
    let (mut s, cfg) = default_state(0);
    let total = expected_total_chips(&cfg);

    // Preflop：UTG=3, MP=4, CO=5, BTN=0 起手 raise 到 300，SB=1 弃，BB=2 跟。
    drive(
        &mut s,
        total,
        &[
            (seat(3), Action::Fold),
            (seat(4), Action::Fold),
            (seat(5), Action::Fold),
            (seat(0), Action::Raise { to: chips(300) }),
            (seat(1), Action::Fold),
            (seat(2), Action::Call),
        ],
    );

    // Postflop：BB 先动（postflop = SB 起，SB 已弃 → BB），BTN 后动。三街全 check。
    drive(
        &mut s,
        total,
        &[
            (seat(2), Action::Check),
            (seat(0), Action::Check),
            (seat(2), Action::Check),
            (seat(0), Action::Check),
            (seat(2), Action::Check),
            (seat(0), Action::Check),
        ],
    );

    assert!(s.is_terminal(), "smoke: 应到 showdown");
    assert_eq!(s.street(), Street::Showdown);
    assert_eq!(s.board().len(), 5);
    assert_eq!(s.pot(), chips(50 + 300 + 300));
    assert!(s.payouts().is_some());
}

// ============================================================================
// 2. preflop_3bet_4bet_5bet_allin
// ============================================================================
//
// BTN 开 raise → SB 3bet → BTN 4bet → SB 5bet all-in → BTN call。
// 验证：D-035 min raise 链条接受逐级递增，最终所有非弃玩家进入 all-in 跳轮
// （D-036），状态机自动发完 board 进入 Showdown。
#[test]
fn preflop_3bet_4bet_5bet_allin() {
    let (mut s, cfg) = default_state(1);
    let total = expected_total_chips(&cfg);

    // 先把 UTG / MP / CO 全 fold 掉，留 BTN(0) / SB(1) / BB(2)。
    drive(
        &mut s,
        total,
        &[
            (seat(3), Action::Fold),
            (seat(4), Action::Fold),
            (seat(5), Action::Fold),
        ],
    );

    // BTN raise 300 (open) → SB raise 900 (3bet) → BB fold → BTN raise 2700 (4bet)
    // → SB AllIn (= 10000, 5bet) → BTN Call.
    drive(
        &mut s,
        total,
        &[
            (seat(0), Action::Raise { to: chips(300) }),
            (seat(1), Action::Raise { to: chips(900) }),
            (seat(2), Action::Fold),
            (seat(0), Action::Raise { to: chips(2700) }),
            (seat(1), Action::AllIn),
            (seat(0), Action::Call),
        ],
    );

    assert!(s.is_terminal(), "全员 all-in 后应进入 Showdown（D-036）");
    assert_eq!(s.street(), Street::Showdown);
    assert_eq!(s.board().len(), 5);
    let payouts = s.payouts().expect("终局应有 payouts");
    let net_sum: i64 = payouts.iter().map(|(_, n)| n).sum();
    assert_eq!(net_sum, 0, "payouts 净值之和必须为 0（零和）");
}

// ============================================================================
// 3. short_allin_does_not_reopen_raise (D-033 — 最关键 NLHE 陷阱)
// ============================================================================
//
// 场景：preflop 三人参与（BTN / SB / BB），SB 开 3bet，BB short all-in（差额
// < SB 3bet 差额），轮到 BTN 时按 D-033，BTN 的 `Raise` 必须被拒绝。
//
// 详细数值（默认 6-max 100BB，SB=50/BB=100）：
//   - BTN(seat 0) start stack = 10000
//   - SB(seat 1) start stack = 10000，posts 50
//   - BB(seat 2) start stack = 350 chips（小盲 100，强行让 short all-in 落于此）
//
// 我们需自定义 starting_stacks 让 BB 短码。drive：
//   BTN limp call(=100) → SB raise to 400（差额 350） → BB AllIn (= 350，BB 投入 350 - 100 = 250 chips diff，但其本街投入 = 350，差额相对前序 SB 的 to=400 为 -50：BB 没补足 SB 的 raise，是 "short call"，不是有效 raise。归一化为 Call 350 而非 Raise）。
//
// 实际上要构造 "short all-in 形成 incomplete RAISE" 需要 BB 投入额 > 当前最高
// committed 但差额 < 上一次有效 raise 差额。重新设计：
//   - BTN(0) limp 100; SB(1) raise to 300（差额 200，min raise 链条上界 = 200）；
//     BB(2) AllIn = 450（450 比 SB 的 300 大；但加注差额 = 450 - 300 = 150 < 200，
//     按 D-033 不重开 raise option）。
//   - 此时 BTN 想 raise，应被拒绝 → `RuleError::RaiseOptionNotReopened`。
//   - BTN 仍可 Call 或 Fold。
//
// 为达成 BB stack = 450（即 100 BB + 350 stack），自定义 starting_stacks。
#[test]
fn short_allin_does_not_reopen_raise() {
    let mut cfg = TableConfig::default_6max_100bb();
    cfg.starting_stacks[2] = chips(450); // BB seat 2
    let total = expected_total_chips(&cfg);
    let mut s = GameState::new(&cfg, 2);

    // UTG/MP/CO 弃 → BTN limp 100 → SB raise to 300 → BB AllIn (incomplete raise 450)
    drive(
        &mut s,
        total,
        &[
            (seat(3), Action::Fold),
            (seat(4), Action::Fold),
            (seat(5), Action::Fold),
            (seat(0), Action::Call),
            (seat(1), Action::Raise { to: chips(300) }),
            (seat(2), Action::AllIn),
        ],
    );

    // 此时 current_player == BTN（seat 0）；legal_actions 必须 raise_range = None。
    assert_eq!(s.current_player(), Some(seat(0)));
    let la = s.legal_actions();
    assert!(
        la.raise_range.is_none(),
        "D-033：BB short all-in 不重开 raise option，BTN 不应有 raise_range，got {:?}",
        la.raise_range
    );
    assert!(la.fold, "LA-003：fold 永远合法");
    assert!(la.call.is_some(), "BB 比 SB 高，BTN 仍需补 call");

    // 显式尝试 raise，应返回 RaiseOptionNotReopened（或同义的 IllegalAction）。
    let err = s
        .apply(Action::Raise { to: chips(900) })
        .expect_err("D-033 拒绝路径");
    let msg = format!("{err}");
    assert!(
        msg.contains("raise option not reopened") || msg.contains("illegal action"),
        "期望 RaiseOptionNotReopened，得到: {msg}"
    );
}

// ============================================================================
// 4. min_raise_chain_after_short_allin (D-035)
// ============================================================================
//
// 与 #3 同结构：SB raise to 300（差额 200），BB short AllIn 450（差额 150）。
// 然后 BTN 选 Call（被允许），SB 进入下一动作时如果选 Raise，min raise 链条
// 应保留为 200（不是 150 — D-033 incomplete raise 不更新链条上限）。
//
// 这里我们让 BTN call 后 SB 回到行动权，断言 SB.legal_actions.raise_range.0
// >= SB 当前 committed + 200（即 to >= 500）。
#[test]
fn min_raise_chain_after_short_allin() {
    let mut cfg = TableConfig::default_6max_100bb();
    cfg.starting_stacks[2] = chips(450); // BB short
    let total = expected_total_chips(&cfg);
    let mut s = GameState::new(&cfg, 3);

    drive(
        &mut s,
        total,
        &[
            (seat(3), Action::Fold),
            (seat(4), Action::Fold),
            (seat(5), Action::Fold),
            (seat(0), Action::Call),
            (seat(1), Action::Raise { to: chips(300) }),
            (seat(2), Action::AllIn), // incomplete raise 450
            (seat(0), Action::Call),  // BTN 跟到 450
        ],
    );

    // SB 现在面临 BTN 的 call 与 BB 的 incomplete raise，需要补 call 到 450（差 150）
    // 或选择 raise；如果 raise，min_to 必须基于 SB 上轮的有效加注差额 200。
    assert_eq!(s.current_player(), Some(seat(1)));
    let la = s.legal_actions();
    let (min_to, _max_to) = la
        .raise_range
        .expect("SB 面对未匹配 incomplete raise，应仍可 raise");
    // SB 本街已 committed 300；min raise 差额 = 200（链条最大有效 raise 差额）。
    // 下界 to = max(已被加注上限 450, SB committed 300 + 200) = 500（取较大者；D-035）。
    assert_eq!(
        min_to,
        chips(500),
        "D-035：min raise 应保留为有效链条 200（=SB 上次差额），实际 min_to = {min_to:?}"
    );
}

// ============================================================================
// 5. two_way_side_pot_basic
// ============================================================================
//
// 三玩家进入 showdown，两个不同的 all-in 级别 → 1 个 main pot（3-way）+
// 1 个 side pot（2-way，"two-way side pot"）。验证 D-038 排序与 pot 划分。
//
// 数值：
//   - BTN(0) starting = 1000
//   - SB(1)  starting = 500   （短码）
//   - BB(2)  starting = 1000
//   - 其它座位（3/4/5）starting = 0 后期 fold（用 starting=10000，preflop 全弃）
//
// 简化：让 UTG/MP/CO 全弃。preflop：BTN AllIn(=1000)，SB AllIn(=500)，BB Call(=1000)。
//   - SB committed_total = 500
//   - BTN / BB committed_total = 1000
//   - main pot（3-way @ 500）= 1500；side pot（2-way @ 500）= 1000
//
// 用 stacked deck 让 BB 拿到顶配 board → BB 赢两个 pot；SB 输 main 与不参与 side。
// 验证：BB net = +500（main 1500-1000） + 500（side 1000-... 等）= +1000。
//   BTN net = -1000；SB net = -500。
//
// 牌序：让 BB 持 AsAh，board 出 AcAd2c3c5c → BB 拿四条 A（不可击败）。
#[test]
fn two_way_side_pot_basic() {
    let mut cfg = TableConfig::default_6max_100bb();
    cfg.starting_stacks[1] = chips(500); // SB short
    let total = expected_total_chips(&cfg);

    // 构造 deck：发牌起点 = 按钮左 1 = SB(1)。D-028 顺序：
    //   deck[0]=SB-c0, deck[1]=BB-c0, deck[2]=UTG-c0, deck[3]=MP-c0, deck[4]=CO-c0, deck[5]=BTN-c0,
    //   deck[6]=SB-c1, deck[7]=BB-c1, deck[8]=UTG-c1, deck[9]=MP-c1, deck[10]=CO-c1, deck[11]=BTN-c1,
    //   deck[12..15]=flop, deck[15]=turn, deck[16]=river.
    let holes = vec![
        (card(0, 0), card(1, 1)),   // SB(1): 2c, 2d (随便给短码 SB 弱牌)
        (card(12, 3), card(12, 2)), // BB(2): As, Ah
        (card(2, 0), card(3, 1)),   // UTG(3)
        (card(4, 0), card(5, 1)),   // MP(4)
        (card(6, 0), card(7, 1)),   // CO(5)
        (card(0, 1), card(1, 0)),   // BTN(0)（被发到 deck[5]/[11]）
    ];
    // build_dealing_order 内部按 holes[k] = 第 k 个发牌座位 (即 SB, BB, UTG, MP, CO, BTN)
    let flop = [card(12, 0), card(12, 1), card(8, 2)]; // Ac, Ad, 10h
    let turn = card(9, 2); // Jh
    let river = card(10, 2); // Qh

    let used: HashSet<u8> = holes
        .iter()
        .flat_map(|(a, b)| [a.to_u8(), b.to_u8()])
        .chain(
            [flop[0], flop[1], flop[2], turn, river]
                .iter()
                .map(|c| c.to_u8()),
        )
        .collect();
    let padding = pick_unused_padding(&used, 52 - 2 * 6 - 5);
    let deck = build_dealing_order(6, &holes, flop, turn, river, &padding);
    let mut rng = StackedDeckRng::from_target_cards(deck);
    let mut s = GameState::with_rng(&cfg, /*seed_label*/ 0, &mut rng);

    drive(
        &mut s,
        total,
        &[
            // UTG/MP/CO 弃
            (seat(3), Action::Fold),
            (seat(4), Action::Fold),
            (seat(5), Action::Fold),
            // BTN AllIn (=1000)，SB AllIn (=500)，BB Call (=1000)
            (seat(0), Action::AllIn),
            (seat(1), Action::AllIn),
            (seat(2), Action::Call),
        ],
    );

    assert!(s.is_terminal(), "all-in 跳轮（D-036）应到 Showdown");
    assert_eq!(s.board().len(), 5);
    let payouts = s.payouts().expect("终局必须有 payouts");

    // BB(seat 2) 用 AAAA 必胜两个 pot。
    let net = |sid: u8| -> i64 {
        payouts
            .iter()
            .find(|(s, _)| s.0 == sid)
            .map(|(_, n)| *n)
            .unwrap_or_else(|| panic!("seat {sid} not in payouts"))
    };
    assert_eq!(net(2), 1000, "BB 赢两个 pot 净 +1000");
    assert_eq!(net(0), -1000, "BTN 输全部投入");
    assert_eq!(net(1), -500, "SB 输短码全部 (= 500)");
    let net_sum: i64 = payouts.iter().map(|(_, n)| n).sum();
    assert_eq!(net_sum, 0, "payouts 必须零和");
}

// ============================================================================
// 6. three_way_side_pot_with_odd_chip (D-039)
// ============================================================================
//
// 三个不同 all-in 级别 → main pot + 2 个 side pot。让其中一个 pot 的总额
// 不能整除获胜人数，触发 odd chip rule（D-039 按按钮左侧最近顺序分配）。
//
// 设计：4 玩家进入摊牌（UTG/MP/CO 弃，BTN/SB/BB 都进入），但让其中两人
// 平分某 pot：比如 SB 与 BB 同强度 → tie → 分 main pot；odd chip 给"按钮
// 左侧最近的获胜者"= SB（按钮左 1 = SB seat 1）。
//
// stacks: BTN=300, SB=200, BB=400（其它座位 10000，preflop 全弃）。
// preflop：BTN AllIn 300, SB AllIn 200, BB Call 300.
//   - main pot（3-way @ 200）= 600
//   - side pot（2-way BTN/BB @ 100）= 200
// 让 SB 与 BB 牌力相同（均胜 BTN）→ main pot SB 与 BB 平分（600/2=300，
// 整除，无 odd chip）。
//
// 为触发 odd chip：把 main pot 总额改成奇数。改 SB 的 ante 或调整 BTN 短码到 301。
// 实际更简单：starting = (BTN=301, SB=200, BB=401)。preflop 同样 all-in。
//   - main pot（3-way @ 200）= 600
//   - side pot 1（BTN+BB @ 101）= 202
//   - 加一个 BB 自身余的部分... 三层 all-in 应是： level1=200(三人), level2=301(两人BTN/BB), level3=401(一人BB).
//   - main = 200*3 = 600; side1 = (301-200)*2 = 202; side2 = (401-301)*1 = 100 (uncalled, 返还 BB).
//
// SB+BB tie（同 5-card holding），BTN 输：
//   - main 600 → SB / BB 平分 = 300 each（整除，无 odd chip）
//   - side1 202 → BB 独赢（SB 已不在 side1）= 202
// 还是没奇数。改 SB starting = 199（tied @ 199 三人）→ main = 597 → tie 两人 = 298 余 1。
// odd chip → 按钮左 1 = SB seat 1（SB 是赢家集合之一）→ 给 SB。
//
// 重新设置：BTN=301 (uncalled excess to BB later), SB=199, BB=401.
//   实际 all-in 数额：SB=199, BTN=301, BB=401（升序）
//   main pot（3-way @ 199）= 597
//   side1 pot（BTN+BB @ 102）= 204
//   side2 pot（BB only @ 100）= 100 → uncalled bet 返还 BB
// SB+BB tie：main 597 → SB / BB 平分 → 各 298 余 1 chip → odd chip → 按钮左 1 = SB
// → SB 拿 299，BB 拿 298。
// side1 204 → BB 独胜（SB 不在），BB 拿 204。
//
// 最终：SB net = 299 - 199 = +100；BB net = 298 + 204 - 301 = +201；BTN net = -301。
//   净和 = 0 ✓。
//
// 验证 odd chip 给了 SB（按按钮左 1 顺序）。
#[test]
fn three_way_side_pot_with_odd_chip() {
    let mut cfg = TableConfig::default_6max_100bb();
    cfg.starting_stacks[0] = chips(301); // BTN
    cfg.starting_stacks[1] = chips(199); // SB
    cfg.starting_stacks[2] = chips(401); // BB
    let total = expected_total_chips(&cfg);

    // SB 与 BB 同牌力：都拿 As,Ks 与 Ad,Kd → 用 As/Ks 做 SB（同牌力问题：花色不参与
    // NLHE，故 AsKs 和 AdKd 在共享 board 上同牌力 → tie）。
    let holes = vec![
        (card(12, 3), card(11, 3)), // SB(1): As, Ks
        (card(12, 1), card(11, 1)), // BB(2): Ad, Kd
        (card(0, 0), card(1, 0)),   // UTG(3)
        (card(2, 0), card(3, 0)),   // MP(4)
        (card(4, 0), card(5, 0)),   // CO(5)
        (card(0, 1), card(1, 1)),   // BTN(0): 2c, 3c (低对都做不出)
    ];
    let flop = [card(8, 2), card(7, 2), card(6, 2)]; // 10h, 9h, 8h
    let turn = card(5, 2); // 7h
    let river = card(2, 1); // 4d

    let used: HashSet<u8> = holes
        .iter()
        .flat_map(|(a, b)| [a.to_u8(), b.to_u8()])
        .chain(
            [flop[0], flop[1], flop[2], turn, river]
                .iter()
                .map(|c| c.to_u8()),
        )
        .collect();
    let padding = pick_unused_padding(&used, 52 - 2 * 6 - 5);
    let deck = build_dealing_order(6, &holes, flop, turn, river, &padding);
    let mut rng = StackedDeckRng::from_target_cards(deck);
    let mut s = GameState::with_rng(&cfg, 0, &mut rng);

    drive(
        &mut s,
        total,
        &[
            (seat(3), Action::Fold),
            (seat(4), Action::Fold),
            (seat(5), Action::Fold),
            (seat(0), Action::AllIn), // BTN 301
            (seat(1), Action::AllIn), // SB 199
            (seat(2), Action::AllIn), // BB 401（自动 call BTN 301，余 100 uncalled）
        ],
    );

    assert!(s.is_terminal());
    let payouts = s.payouts().expect("终局必须有 payouts");
    let net = |sid: u8| -> i64 {
        payouts
            .iter()
            .find(|(s, _)| s.0 == sid)
            .map(|(_, n)| *n)
            .unwrap()
    };
    assert_eq!(net(0), -301, "BTN 输 301");
    assert_eq!(
        net(1),
        100,
        "SB net = +100（odd chip 给按钮左 1 = SB；299 - 199 = 100）"
    );
    assert_eq!(net(2), 201, "BB net = +201");
    assert_eq!(payouts.iter().map(|(_, n)| n).sum::<i64>(), 0);
}

// ============================================================================
// 7. uncalled_bet_returned (D-040)
// ============================================================================
//
// preflop：UTG/MP/CO 全弃，BTN raise 300，SB 弃，BB 弃。BTN 是最后 raiser，
// 没 caller → 超出"最高被 call 金额"（= BB = 100）的部分（200）返还 BTN。
// 实际 pot = 50 + 100 + 100 = 250，全部归 BTN（无人挑战）。
// BTN net = +150（赢 SB 50 + BB 100 = 150）；SB net = -50；BB net = -100。
#[test]
fn uncalled_bet_returned() {
    let (mut s, cfg) = default_state(7);
    let total = expected_total_chips(&cfg);

    drive(
        &mut s,
        total,
        &[
            (seat(3), Action::Fold),
            (seat(4), Action::Fold),
            (seat(5), Action::Fold),
            (seat(0), Action::Raise { to: chips(300) }),
            (seat(1), Action::Fold),
            (seat(2), Action::Fold),
        ],
    );

    assert!(s.is_terminal(), "全员弃（除 BTN 外）→ 终局");
    let payouts = s.payouts().expect("终局必须有 payouts");
    let net = |sid: u8| -> i64 {
        payouts
            .iter()
            .find(|(s, _)| s.0 == sid)
            .map(|(_, n)| *n)
            .unwrap()
    };
    assert_eq!(
        net(0),
        150,
        "BTN 净赢 SB 50 + BB 100 = 150（D-040 退多余 200）"
    );
    assert_eq!(net(1), -50);
    assert_eq!(net(2), -100);

    // 终局 BTN 应只在 pot 中留下 100（被 BB 跟到的部分），剩余 200 已退还 stack。
    let btn = s
        .players()
        .iter()
        .find(|p| p.seat == seat(0))
        .expect("BTN must be present");
    assert_eq!(
        btn.committed_total,
        chips(100),
        "uncalled 部分（200）从 committed_total 中扣回"
    );
}

// ============================================================================
// 8. walk_to_bb
// ============================================================================
//
// 全员（含 SB）弃到 BB，BB 不行动而直接收下盲注。
// 净值：BB +50（SB 死钱），SB -50，其它玩家 0。
#[test]
fn walk_to_bb() {
    let (mut s, cfg) = default_state(8);
    let total = expected_total_chips(&cfg);

    drive(
        &mut s,
        total,
        &[
            (seat(3), Action::Fold),
            (seat(4), Action::Fold),
            (seat(5), Action::Fold),
            (seat(0), Action::Fold), // BTN
            (seat(1), Action::Fold), // SB（最后一个 fold → BB walk）
        ],
    );

    assert!(s.is_terminal(), "BB walk 终局");
    assert_eq!(s.current_player(), None);
    let payouts = s.payouts().expect("终局必须有 payouts");
    let net = |sid: u8| -> i64 {
        payouts
            .iter()
            .find(|(s, _)| s.0 == sid)
            .map(|(_, n)| *n)
            .unwrap()
    };
    assert_eq!(net(2), 50, "BB 净 +50（SB 50 死钱归 BB）");
    assert_eq!(net(1), -50);
    assert_eq!(net(0), 0);
    assert_eq!(net(3), 0);
}

// ============================================================================
// 9. all_players_allin_runs_out_board (D-036)
// ============================================================================
//
// preflop 二人 all-in，状态机直接发完 5 张 board → Showdown。验证 D-036
// 多街快进时序：同一个 apply 调用内 board 长度从 0 变为 5，街变为 Showdown。
#[test]
fn all_players_allin_runs_out_board() {
    let (mut s, cfg) = default_state(9);
    let total = expected_total_chips(&cfg);

    drive(
        &mut s,
        total,
        &[
            (seat(3), Action::Fold),
            (seat(4), Action::Fold),
            (seat(5), Action::Fold),
            (seat(0), Action::AllIn), // BTN AllIn = 10000
            (seat(1), Action::Fold),  // SB 弃（避免 3-way 复杂化）
            (seat(2), Action::AllIn), // BB AllIn (≤ 10000 BB 已投 100，再投 9900)
        ],
    );

    assert!(s.is_terminal(), "二人 all-in 后 D-036 跳转 Showdown");
    assert_eq!(s.street(), Street::Showdown);
    assert_eq!(s.board().len(), 5, "D-036：board 一次性补齐 5 张");
    assert!(s.payouts().is_some());

    // 双方 stack 应为 0（all-in，且 BB 总额 ≤ BTN 总额，无 uncalled bet 返还）。
    for p in s.players() {
        if p.seat == seat(0) || p.seat == seat(2) {
            assert_eq!(p.stack, chips(0), "all-in 玩家 stack 应为 0");
            assert_eq!(p.status, PlayerStatus::AllIn);
        }
    }
}

// ============================================================================
// 10. last_aggressor_shows_first (D-037)
// ============================================================================
//
// preflop BTN raise（voluntary），SB/BB call，三街全 check，到 river showdown。
// last_aggressor = BTN（preflop 唯一 voluntary aggressor），所以 showdown_order[0]
// = BTN。其余按按钮左侧依次：SB → BB（SB 在 BTN 左 1）。
#[test]
fn last_aggressor_shows_first() {
    let (mut s, cfg) = default_state(10);
    let total = expected_total_chips(&cfg);

    drive(
        &mut s,
        total,
        &[
            (seat(3), Action::Fold),
            (seat(4), Action::Fold),
            (seat(5), Action::Fold),
            (seat(0), Action::Raise { to: chips(300) }),
            (seat(1), Action::Call),
            (seat(2), Action::Call),
        ],
    );
    // 三街全 check：postflop 顺序 = SB → BB → BTN
    drive(
        &mut s,
        total,
        &[
            (seat(1), Action::Check),
            (seat(2), Action::Check),
            (seat(0), Action::Check),
            (seat(1), Action::Check),
            (seat(2), Action::Check),
            (seat(0), Action::Check),
            (seat(1), Action::Check),
            (seat(2), Action::Check),
            (seat(0), Action::Check),
        ],
    );

    assert!(s.is_terminal());
    let order = &s.hand_history().showdown_order;
    assert_eq!(
        order.first(),
        Some(&seat(0)),
        "D-037：last_aggressor BTN 先亮"
    );
    // 其余顺序 = BTN 左侧依次未弃牌座位：SB(1), BB(2)
    assert_eq!(order.get(1), Some(&seat(1)), "BTN 左 1 = SB");
    assert_eq!(order.get(2), Some(&seat(2)), "BTN 左 2 = BB");
}
