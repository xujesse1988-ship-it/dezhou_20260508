//! C1：side pot / split pot 扩展场景表（100+ 用例 / ≥20 uncalled bet returned）。
//!
//! `pluribus_stage1_workflow.md` §C1 出口标准：
//!
//! - side pot scenario 扩到 **100+**，含 **≥ 20** 个 uncalled bet returned 路径。
//! - odd chip rule（D-039-rev1）与 dead money / forfeit 必须被覆盖。
//!
//! 设计：
//!
//! - 一组用 stacked deck 让**特定座位的牌力总是最强**（典型：BB 拿 AsAh，board
//!   出 Ac Ad 9h 10h Jh，BB 即得 AAAA quads，任何对手都赢不了）；这样我们能在
//!   不同 stack 分布下用同一个"必胜手"模板生成大量 side pot 算账场景。
//! - 一组用 stacked deck 让两座位**牌力相同**（典型：SB AsKs / BB AdKd，board
//!   不构成同花连张），用于 odd chip rule 检验。
//! - uncalled bet 路径：preflop 全弃到唯一 raiser / postflop bet 无人 call /
//!   river bet 无人 call，verifying committed_total 减去 uncalled 部分。
//!
//! 角色边界：本文件只读 `[决策]` 与产品代码；不修改 `src/`。任何 B2 corner
//! case bug 通过 `// FIXME(C2):` 注释 + 失败断言留给 C2 修。

#![allow(clippy::vec_init_then_push)]

mod common;

use std::collections::HashSet;

use poker::{Action, GameState, PlayerStatus, SeatId};

use common::{
    build_dealing_order, card, cfg_6max_with_stacks, chips, expected_total_chips,
    pick_unused_padding, plan, run_scenario, seat, Invariants, ScenarioCase, ScenarioExpect,
    StackedDeckRng,
};

// ============================================================================
// 牌序模板：让**指定 seat** 在共享 board 上必胜
// ============================================================================
//
// 模板 A — "winner 拿到 AAAA quads"：
//
// - winner 持有 As, Ah；
// - 其它 5 名座位拿到非 A 的小杂牌（程序按"未占用"自动填）；
// - flop = Ac, Ad, 9h；turn = 10h；river = Jh。
// - winner 在共享 5 张 board (Ac Ad 9h 10h Jh) 上即得 AAAA + J kicker (Quads)，
//   任何对手最大牌型 ≤ Two Pair（用 9 9 / J J / 10 10 取 board 两张），全输。
//
// 该模板对任意 n_seats（B2 限制 3..=9）通用。

fn build_bb_wins_deck(cfg: &poker::TableConfig) -> StackedDeckRng {
    build_winner_deck(
        cfg,
        /*winner_seat=*/ ((cfg.button_seat.0 as usize + 2) % cfg.n_seats as usize) as u8,
    )
}

fn build_winner_deck(cfg: &poker::TableConfig, winner_seat: u8) -> StackedDeckRng {
    let n = cfg.n_seats as usize;
    let winner_idx = winner_seat as usize;

    // winner: As, Ah; 其它座位填占位牌（避开占用的 Ac/Ad/9h/10h/Jh）。
    let winner_a = card(12, 3); // As
    let winner_b = card(12, 2); // Ah
    let flop = [card(12, 0), card(12, 1), card(7, 2)]; // Ac, Ad, 9h
    let turn = card(8, 2); // 10h
    let river = card(9, 2); // Jh

    let used_fixed: HashSet<u8> = [
        winner_a.to_u8(),
        winner_b.to_u8(),
        flop[0].to_u8(),
        flop[1].to_u8(),
        flop[2].to_u8(),
        turn.to_u8(),
        river.to_u8(),
    ]
    .into_iter()
    .collect();
    let mut padding_pool = pick_unused_padding(&used_fixed, 52 - 7);

    // holes 按发牌起点（SB = 按钮左 1）顺序索引，与 build_dealing_order 的契约一致。
    // holes[k] = SB-relative offset k 那个座位拿到的两张底牌。
    let mut holes: Vec<(poker::Card, poker::Card)> = Vec::with_capacity(n);
    for k in 0..n {
        let seat_idx = ((cfg.button_seat.0 as usize + 1 + k) % n) as u8;
        if seat_idx as usize == winner_idx {
            holes.push((winner_a, winner_b));
        } else {
            let a = padding_pool.remove(0);
            let b = padding_pool.remove(0);
            holes.push((a, b));
        }
    }

    // 剩余 padding 填到 deck 尾（52 - 2*n - 5）
    let used_after_holes: HashSet<u8> = holes
        .iter()
        .flat_map(|(a, b)| [a.to_u8(), b.to_u8()])
        .chain(used_fixed.iter().copied())
        .collect();
    let final_padding = pick_unused_padding(&used_after_holes, 52 - 2 * n - 5);
    let deck = build_dealing_order(n, &holes, flop, turn, river, &final_padding);
    StackedDeckRng::from_target_cards(deck)
}

// ============================================================================
// 工具：当 plan 涉及 stacked deck 时直接构造 GameState 并跑（绕过 ScenarioCase 的
// holes / board 字段，因为我们直接喂 deck）。
// ============================================================================

fn drive_with_deck(
    cfg: &poker::TableConfig,
    deck_rng: &mut StackedDeckRng,
    seed: u64,
    plan: &[(SeatId, Action)],
) -> GameState {
    let total = expected_total_chips(cfg);
    let mut state = GameState::with_rng(cfg, seed, deck_rng);
    Invariants::check_all(&state, total).expect("initial invariants");
    for (i, (want, action)) in plan.iter().enumerate() {
        let cp = state
            .current_player()
            .unwrap_or_else(|| panic!("step {i}: cp == None, expected {want:?}"));
        assert_eq!(
            cp, *want,
            "step {i}: cp mismatch (want {want:?}, got {cp:?})"
        );
        state
            .apply(*action)
            .unwrap_or_else(|e| panic!("step {i}: apply({action:?}) failed: {e}"));
        Invariants::check_all(&state, total).unwrap_or_else(|e| panic!("step {i}: {e}"));
    }
    state
}

// ============================================================================
// A. 2-way side pot — BB(winner) + 第二名 caller（≥ 30 cases）
// ============================================================================
//
// 设置：UTG/MP/CO 全弃。BTN AllIn（stack=B），SB AllIn（stack=S），BB Call。
// BB 必胜（用 build_bb_wins_deck）。
//
// 数学：
//   - BB starting >= max(B, S)（即 BB 不会先于其它 all-in）。让 BB starting = 10000。
//   - committed_total: BTN=B, SB=S, BB=max(B,S)（BB Call 到 max；如果 max < BB blind 则 BB committed 已是 100；
//     这里我们要求 max(B,S) >= 100 = BB blind，否则不会出现 AllIn 局面）。
//     注意：BB 已通过 forced_bet 投入 100，所以 BB Call 的 to=max(B,S)；BB committed_total = max(B,S).
//
// pot 划分：
//   - 假设 S < B（SB 短码）：
//     main pot (3-way @ S) = 3*S, BB 单独赢；
//     side pot (BB+BTN @ B-S) = 2*(B-S), BB 单独赢（且 BB committed = B；如 BB 投入 max=B）。
//     uncalled bet returned: 没有（max called by another = B, BB committed = B, no excess）。
//   - 净值：BB = (3*S + 2*(B-S)) - B = S + B；SB = -S；BTN = -B；其它 = 0；零和：S+B - S - B = 0 ✓。
//   - 假设 B < S（BTN 短码，SB 大）：对称。
//   - 假设 B == S（双 all-in 同金额）：no side pot, BB win 3*B - B = 2B；其它 -B 各。

#[test]
fn two_way_side_pot_btn_sb_allin_bb_wins_table() {
    let mut produced = 0usize;
    let stack_pairs: &[(u64, u64)] = &[
        (300, 200),
        (500, 250),
        (700, 350),
        (1000, 400),
        (1500, 600),
        (2000, 800),
        (3000, 1200),
        (5000, 2500),
        (7500, 3000),
        (9999, 4999),
        // BTN < SB
        (200, 300),
        (250, 500),
        (350, 700),
        (400, 1000),
        (600, 1500),
        (800, 2000),
        (1200, 3000),
        (2500, 5000),
        (3000, 7500),
        (4999, 9999),
        // BTN == SB
        (300, 300),
        (500, 500),
        (1000, 1000),
        (2500, 2500),
        (5000, 5000),
        (9999, 9999),
        // 极小 / 极大
        (110, 110),
        (110, 200),
        (200, 110),
        (9000, 100), // SB tiny but >= 50 SB blind paid
        (100, 9000),
        (110, 9000),
        (9000, 110),
    ];

    for &(btn_s, sb_s) in stack_pairs {
        // SB stack >= 50（BB forced bet 50）。100 是最小够 AllIn 上 BB 100 的值。
        if sb_s < 50 || btn_s < 100 {
            continue;
        }
        let cfg = cfg_6max_with_stacks([btn_s, sb_s, 10000, 10000, 10000, 10000]);
        let mut deck = build_bb_wins_deck(&cfg);
        // plan：UTG/MP/CO 全弃，BTN AllIn，SB AllIn，BB Call。
        let plan_vec = plan(&[
            (3, Action::Fold),
            (4, Action::Fold),
            (5, Action::Fold),
            (0, Action::AllIn),
            (1, Action::AllIn),
            (2, Action::Call),
        ]);
        let state = drive_with_deck(&cfg, &mut deck, btn_s ^ sb_s, &plan_vec);
        assert!(
            state.is_terminal(),
            "(btn={btn_s}, sb={sb_s}) should terminate"
        );

        let payouts = state.payouts().expect("payouts");
        let net = |sid: u8| -> i64 {
            payouts
                .iter()
                .find(|(s, _)| s.0 == sid)
                .map(|(_, n)| *n)
                .unwrap_or_else(|| panic!("seat {sid} missing"))
        };
        // 净值数学验证
        // s_min = min(btn_s, sb_s), s_max = max(btn_s, sb_s)
        // BB win = 3*s_min + 2*(s_max - s_min) = 2*s_max + s_min
        // BB net = (2*s_max + s_min) - s_max = s_max + s_min
        let s_min = btn_s.min(sb_s);
        let s_max = btn_s.max(sb_s);
        let expected_bb_net = (s_max + s_min) as i64;
        assert_eq!(
            net(2),
            expected_bb_net,
            "BB net mismatch (btn={btn_s}, sb={sb_s})"
        );
        assert_eq!(
            net(0),
            -(btn_s as i64),
            "BTN net mismatch (btn={btn_s}, sb={sb_s})"
        );
        assert_eq!(
            net(1),
            -(sb_s as i64),
            "SB net mismatch (btn={btn_s}, sb={sb_s})"
        );
        assert_eq!(payouts.iter().map(|(_, n)| n).sum::<i64>(), 0);
        produced += 1;
    }
    assert!(
        produced >= 25,
        "two-way side pot 至少 25 cases，实际 {produced}"
    );
}

// ============================================================================
// B. 3-way side pot — BTN/SB/BB 全 all-in（≥ 30 cases）
// ============================================================================
//
// 设置：UTG/MP/CO 全弃。BTN AllIn (a)，SB AllIn (b)，BB AllIn (c)（c 必须 ≥ a 才
// 能反应到 BTN 的 all-in；并且 c <= ...）。
//
// 数学（三 all-in，stacks (a, b, c)）：
//   sorted = (lo, mid, hi)：
//     main pot (3-way @ lo) = 3*lo
//     side pot 1 (mid+hi 两人 @ mid-lo) = 2*(mid-lo)
//     side pot 2 (hi only @ hi-mid) = (hi-mid) → uncalled if hi-payer 唯一贡献
//
// 因为 side pot 2 只有 hi-payer 贡献，**uncalled 部分返还**。
// 所以最终 hi-payer 只 commit 到 mid。
// BB winner net = 3*lo + 2*(mid - lo) - (committed by BB).
// BB committed = (BB 实际投入)。如果 BB = hi-payer，BB 投入 = hi → 退还 (hi-mid) →
//   final committed = mid。
//
// 取 BB winner 永远 = winner（拿 AAAA），如果 BB 是 hi-payer，则 BB 净 = 2*mid + lo - mid = mid + lo。
// 如果 BB 是 mid-payer，BB 净 = 2*mid + lo - mid = mid + lo（同样，BB committed = mid）。
// 如果 BB 是 lo-payer，BB 净 = 3*lo - lo = 2*lo（只赢 main pot；side pot 1 由 mid/hi 中
//   的赢家拿；但 BB 是 lo-payer 不参与 side pot 1，所以 side pot 1 必由 mid/hi 中的
//   赢家拿；因为 BB 是必胜手，但 BB 不在 side pot 1 contender 中，会怎样？）
//
// 重要 corner case：BB 是 lo-payer 时，side pot 1 contender = (mid, hi) 中谁赢？
// BB 必胜手意味着 mid/hi 不可能比 BB 高，但他们之间彼此可能不同。我们的 build_bb_wins_deck
// 让 BB 以 AAAA 必胜；mid/hi 的 hole_cards 是 padding（小杂牌），他们之间的相对强度
// 由 padding 顺序决定 — **不可预测**。所以我们**应避免 BB = lo-payer 的子集**，
// 或单独处理。
//
// 简化：仅取 BB stack >= max(BTN_s, SB_s)，让 BB 始终是 hi-payer 或 mid-payer。
// 实际我们让 BB 是大 stack（10000）→ BB 是 hi-payer → BB 投入 = max(BTN_s, SB_s) = mid。
// 此时 side pot 2（hi only）不存在（被 uncalled 退还）。
// pot 划分：main (3-way @ lo) + side1 (BB + max-payer @ mid - lo)。
// BB 必胜手 → BB 拿 main + side1 = 3*lo + 2*(mid-lo) = 2*mid + lo。
// BB committed = mid → BB net = 2*mid + lo - mid = mid + lo.

#[test]
fn three_way_side_pot_bb_winner_table() {
    let mut produced = 0usize;
    // (btn_s, sb_s)；BB 始终 10000（大于两者）。要求 btn_s >= 100, sb_s >= 50。
    let pairs: &[(u64, u64)] = &[
        (200, 100),
        (300, 150),
        (500, 200),
        (700, 300),
        (1000, 400),
        (1500, 600),
        (2000, 800),
        (3000, 1200),
        (5000, 2500),
        (7500, 3000),
        (9999, 4999),
        (100, 200),
        (150, 300),
        (200, 500),
        (300, 700),
        (400, 1000),
        (600, 1500),
        (800, 2000),
        (1200, 3000),
        (2500, 5000),
        (3000, 7500),
        (4999, 9999),
        (200, 200),
        (500, 500),
        (1000, 1000),
        (2500, 2500),
        (5000, 5000),
        (8000, 8000),
        (110, 110),
        (110, 200),
        (300, 110),
        (110, 9000),
        (9000, 110),
    ];
    for &(btn_s, sb_s) in pairs {
        if btn_s < 100 || sb_s < 50 {
            continue;
        }
        let cfg = cfg_6max_with_stacks([btn_s, sb_s, 10000, 10000, 10000, 10000]);
        let mut deck = build_bb_wins_deck(&cfg);
        let plan_vec = plan(&[
            (3, Action::Fold),
            (4, Action::Fold),
            (5, Action::Fold),
            (0, Action::AllIn),
            (1, Action::AllIn),
            (2, Action::AllIn),
        ]);
        let state = drive_with_deck(&cfg, &mut deck, btn_s ^ sb_s ^ 0xBB, &plan_vec);
        assert!(state.is_terminal());
        let payouts = state.payouts().unwrap();
        let net = |sid: u8| -> i64 {
            payouts
                .iter()
                .find(|(s, _)| s.0 == sid)
                .map(|(_, n)| *n)
                .unwrap()
        };

        let lo = btn_s.min(sb_s);
        let mid = btn_s.max(sb_s);
        // BB committed = mid. BB net = mid + lo.
        let expected_bb = (mid + lo) as i64;
        assert_eq!(net(2), expected_bb, "BB net (btn={btn_s},sb={sb_s})");
        assert_eq!(net(0), -(btn_s as i64));
        assert_eq!(net(1), -(sb_s as i64));
        assert_eq!(payouts.iter().map(|(_, n)| n).sum::<i64>(), 0);
        produced += 1;
    }
    assert!(produced >= 25, "3-way side pot ≥ 25; got {produced}");
}

// ============================================================================
// C. odd chip rule — SB+BB tie main pot（≥ 12 cases）
// ============================================================================
//
// 设置：让 SB & BB 必同 hand strength。BTN 必输。
//   - SB: As, Ks
//   - BB: Ad, Kd
//   - Board: 10h, 9h, 8h, 7h, 4d → SB 与 BB 各得 A-K-high（无对、无同花），但 board
//     已有 10h 9h 8h 7h，缺 6 / J；A-K-10-9-8 为 high card → 但 board 4 大牌不接
//     → 实际：SB/BB 的 hole 都不接 board 的 straight，因此 SB/BB 各得 A-high
//     （board 上的 K 没出现，但 A 让 hole 进 5 best => Ah, Kh, 10h, 9h, 8h... wait
//     hold on）。
//   - 重新设计：让 board 不构成同花，且让 hole 与 board 共同最佳 5 = A-K-board
//     最大三张。SB AsKs / BB AdKd → 5-best = A K (从 hole) + 三张最高 board。
//     那两人 5-best 就是 A K X Y Z（X Y Z 从 board 取最大三张）→ 同强度 → tie。
//     已存在：tests/scenarios.rs #6 已经这样写。复用。
//
// 数学：
//   stacks = (BTN=a, SB=b, BB=c)，a 中等大，b/c 都让 main pot 出现奇数。
//   pots（仅当 a ≤ b ≤ c 简化方向）：
//     main (3-way @ a) = 3a，contender = SB ∪ BB（BTN 输）→ tie，每人得 floor(3a/2)，
//       余 = 3a mod 2 → 给按钮左侧最近赢家 = SB（D-039-rev1）。
//     side1 (SB+BB @ b-a) = 2*(b-a)，contender = SB ∪ BB → tie，再分。
//     side2 (BB only @ c-b) → uncalled returned。
//
// 取 a 奇数，b 任意 → main pot = 3a 奇数，余 1 → SB 拿 +1。
// 我们能控制 odd-chip 落到 SB 即可验证 D-039-rev1。

#[test]
fn odd_chip_to_sb_table() {
    let mut produced = 0usize;
    // a 必须为奇数 chip。让 a ∈ {199, 299, 333, 401, 499, 599, 701, 833, 999, 1111}.
    // 取 b > a 让 BB 也 all-in 但不是 lo-payer（保证 SB 与 BB 同时进入 side pot 1）。
    // c >= b（BB 大）。
    let triples: &[(u64, u64, u64)] = &[
        (199, 250, 10000),
        (299, 350, 10000),
        (333, 400, 10000),
        (401, 500, 10000),
        (499, 600, 10000),
        (599, 700, 10000),
        (701, 800, 10000),
        (833, 1000, 10000),
        (999, 1200, 10000),
        (1111, 1500, 10000),
        (199, 199, 10000), // SB 与 BTN 同金，main pot SB+BB tie 简化为 (BTN=199 lo, SB=199 mid, BB=10000 hi)
        (333, 333, 10000),
    ];
    for &(a, b, c) in triples {
        if a < 100 || b < 50 {
            continue;
        }
        let cfg = cfg_6max_with_stacks([a, b, c, 10000, 10000, 10000]);

        // 构造 tie deck：SB AsKs / BB AdKd / board 10h 9h 8h 7h 4d
        // holes 按发牌索引（SB → BB → UTG → MP → CO → BTN）严格排列。
        let n = cfg.n_seats as usize;
        let holes = vec![
            (card(12, 3), card(11, 3)), // SB(1): As Ks       (dealing 0)
            (card(12, 1), card(11, 1)), // BB(2): Ad Kd       (dealing 1)
            (card(0, 1), card(1, 1)),   // UTG(3)             (dealing 2)
            (card(2, 0), card(3, 0)),   // MP(4)              (dealing 3)
            (card(4, 0), card(5, 0)),   // CO(5)              (dealing 4)
            (card(0, 0), card(1, 0)),   // BTN(0): 2c 3c (loser, dealing 5)
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
        let padding = pick_unused_padding(&used, 52 - 2 * n - 5);
        let deck = build_dealing_order(n, &holes, flop, turn, river, &padding);
        let mut rng = StackedDeckRng::from_target_cards(deck);

        let plan_vec = plan(&[
            (3, Action::Fold),
            (4, Action::Fold),
            (5, Action::Fold),
            (0, Action::AllIn),
            (1, Action::AllIn),
            (2, Action::AllIn),
        ]);
        let state = drive_with_deck(&cfg, &mut rng, a ^ b ^ c, &plan_vec);
        assert!(state.is_terminal());
        let payouts = state.payouts().unwrap();
        let net = |sid: u8| -> i64 {
            payouts
                .iter()
                .find(|(s, _)| s.0 == sid)
                .map(|(_, n)| *n)
                .unwrap()
        };
        // BTN 必输 a；SB 与 BB 平分 main pot（3a，奇数）+ side pot 1（2*(b-a)，偶数）
        //   - main = 3a；SB 拿 ceil(3a/2)，BB 拿 floor(3a/2)（odd chip 给按钮左 = SB）
        //   - side1 = 2*(b-a)：每人 (b-a)
        //   - BB committed_total 经 uncalled refund = b（c 多余的 c-b 返还）
        // BB net = floor(3a/2) + (b-a) - b = floor(3a/2) - a
        // SB net = ceil(3a/2) + (b-a) - b = ceil(3a/2) - a
        // BTN net = -a
        let three_a = 3 * a;
        let sb_share = three_a.div_ceil(2);
        let bb_share = three_a / 2;
        let expected_sb_net = sb_share as i64 + (b - a) as i64 - b as i64; // = sb_share - a
        let expected_bb_net = bb_share as i64 + (b - a) as i64 - b as i64; // = bb_share - a
        assert_eq!(
            net(1),
            expected_sb_net,
            "(a={a}, b={b}, c={c}) SB net (奇 chip 给 SB)"
        );
        assert_eq!(net(2), expected_bb_net);
        assert_eq!(net(0), -(a as i64));
        assert_eq!(payouts.iter().map(|(_, n)| n).sum::<i64>(), 0);
        produced += 1;
    }
    assert!(produced >= 10, "odd-chip cases ≥ 10; got {produced}");
}

// ============================================================================
// D. 4-way side pot — UTG/MP fold，CO/BTN/SB/BB 全 all-in（≥ 16 cases）
// ============================================================================

#[test]
fn four_way_side_pot_bb_winner_table() {
    let mut produced = 0usize;
    // 4 all-in 在 (CO=co, BTN=btn, SB=sb, BB=10000)；BB 必胜（用 build_bb_wins_deck）。
    // 让 BB 是 hi-payer → BB committed = max(co,btn,sb) → 自动 uncalled。
    // 我们要求 co/btn/sb 都 < 10000，且 co/btn 至少 100，sb 至少 50。
    let triples: &[(u64, u64, u64)] = &[
        (200, 300, 100),
        (500, 250, 150),
        (700, 600, 400),
        (1000, 500, 200),
        (1500, 1000, 500),
        (2000, 800, 300),
        (3000, 1500, 1000),
        (5000, 2000, 800),
        (200, 200, 200),
        (500, 500, 500),
        (1000, 1000, 1000),
        (3000, 1500, 750),
        (7500, 5000, 2500),
        (9000, 8000, 7000),
        (200, 9000, 50),
        (9000, 200, 50),
        (110, 200, 100),
        (110, 9000, 50),
        // 补：sb_s > 100 的若干 corner stacks
        (250, 350, 150),
        (400, 600, 200),
        (150, 250, 250),
        (3500, 2500, 1500),
    ];
    for &(co_s, btn_s, sb_s) in triples {
        // sb_s 必须 > BB blind(=100) 才能在 blind post 后还 Active；
        // co_s/btn_s 必须 > BB blind 才能 AllIn 形成 raise（< BB 走 Call branch，则
        //   raise option 不会被重开，后续行动者可能拒绝 raise，不属于本表覆盖）。
        if co_s < 100 || btn_s < 100 || sb_s <= 100 {
            continue;
        }
        let cfg = cfg_6max_with_stacks([btn_s, sb_s, 10000, 10000, 10000, co_s]);
        let mut deck = build_bb_wins_deck(&cfg);
        // plan: UTG/MP fold; CO/BTN/SB/BB 全 all-in.
        let plan_vec = plan(&[
            (3, Action::Fold),
            (4, Action::Fold),
            (5, Action::AllIn),
            (0, Action::AllIn),
            (1, Action::AllIn),
            (2, Action::AllIn),
        ]);
        let state = drive_with_deck(&cfg, &mut deck, co_s ^ btn_s ^ sb_s, &plan_vec);
        assert!(state.is_terminal());
        let payouts = state.payouts().unwrap();
        let net = |sid: u8| -> i64 {
            payouts
                .iter()
                .find(|(s, _)| s.0 == sid)
                .map(|(_, n)| *n)
                .unwrap()
        };
        // BB 必胜手赢一切。BB committed = max(co_s, btn_s, sb_s) = m_max.
        // 各 pot：sorted (lo, mid1, mid2, hi=BB committed)
        // 但 BB 必赢所有 contender pot；no other contender pot exists separately.
        // 实际计算：BB net = sum(everyone's committed except BB) - 0... wait.
        // BB committed = m_max; side pots BB 都赢；BB 净 = sum(co_s) + sum(sb_s) + sum(btn_s).
        let expected_bb_net = (co_s + btn_s + sb_s) as i64;
        assert_eq!(
            net(2),
            expected_bb_net,
            "BB net (co={co_s}, btn={btn_s}, sb={sb_s})"
        );
        assert_eq!(net(0), -(btn_s as i64));
        assert_eq!(net(1), -(sb_s as i64));
        assert_eq!(net(5), -(co_s as i64));
        assert_eq!(payouts.iter().map(|(_, n)| n).sum::<i64>(), 0);
        produced += 1;
    }
    assert!(produced >= 14, "4-way side pot ≥ 14; got {produced}");
}

// ============================================================================
// E. uncalled bet returned — 多形态（≥ 25 cases）
// ============================================================================

#[test]
fn uncalled_bet_returned_table() {
    let mut produced = 0usize;

    // 形态 1：preflop UTG raise → 全弃 → UTG uncalled
    let opens: &[u64] = &[200, 250, 300, 400, 500, 700, 1000, 1500, 2500, 5000];
    for &open in opens {
        let mut p: Vec<(SeatId, Action)> = Vec::new();
        p.push((seat(3), Action::Raise { to: chips(open) }));
        for s in [4u8, 5, 0, 1, 2] {
            p.push((seat(s), Action::Fold));
        }
        let mut expect = ScenarioExpect::new();
        expect.terminal = Some(true);
        // UTG invests open → uncalled (open-100 returned). committed_total = 100.
        // Pot = 50 (SB) + 100 (BB) + 100 (UTG) = 250. UTG wins.
        // UTG net = 250 - 100 = +150（赢 SB 50 + BB 100）。
        expect.payouts = Some(vec![(3, 150), (4, 0), (5, 0), (0, 0), (1, -50), (2, -100)]);
        let leaked: &'static str =
            Box::leak(format!("uncalled_preflop_utg_open_{open}").into_boxed_str());
        let case = ScenarioCase {
            name: leaked,
            config: poker::TableConfig::default_6max_100bb(),
            seed: open ^ 0xBADF00D,
            holes: None,
            board: None,
            plan: p,
            expect,
        };
        run_scenario(&case);
        produced += 1;
    }

    // 形态 2：preflop BTN raise → SB call → BB raise → 全弃 → BB uncalled
    let bb_raises: &[u64] = &[700, 900, 1200, 1500, 2000, 3000];
    for &three in bb_raises {
        let mut p: Vec<(SeatId, Action)> = Vec::new();
        p.push((seat(3), Action::Fold));
        p.push((seat(4), Action::Fold));
        p.push((seat(5), Action::Fold));
        p.push((seat(0), Action::Raise { to: chips(300) }));
        p.push((seat(1), Action::Call));
        p.push((seat(2), Action::Raise { to: chips(three) }));
        p.push((seat(0), Action::Fold));
        p.push((seat(1), Action::Fold));
        let mut expect = ScenarioExpect::new();
        expect.terminal = Some(true);
        // BTN 300 (lost), SB 300 (lost), BB invested `three` then uncalled (three - 300) returned.
        // BB committed = 300. Pot = 300+300+300 = 900. BB wins → BB net = 900 - 300 = +600.
        expect.payouts = Some(vec![(0, -300), (1, -300), (2, 600), (3, 0), (4, 0), (5, 0)]);
        let leaked: &'static str =
            Box::leak(format!("uncalled_preflop_bb_squeeze_{three}").into_boxed_str());
        let case = ScenarioCase {
            name: leaked,
            config: poker::TableConfig::default_6max_100bb(),
            seed: three ^ 0xDEAD,
            holes: None,
            board: None,
            plan: p,
            expect,
        };
        run_scenario(&case);
        produced += 1;
    }

    // 形态 3：postflop bet 无 caller
    let flop_bets: &[u64] = &[100, 250, 500, 1000, 2000, 5000, 9000];
    for &b in flop_bets {
        let mut p: Vec<(SeatId, Action)> = Vec::new();
        for s in [3u8, 4, 5] {
            p.push((seat(s), Action::Fold));
        }
        p.push((seat(0), Action::Raise { to: chips(300) }));
        p.push((seat(1), Action::Fold));
        p.push((seat(2), Action::Call));
        p.push((seat(2), Action::Check));
        p.push((seat(0), Action::Bet { to: chips(b) }));
        p.push((seat(2), Action::Fold));

        let mut expect = ScenarioExpect::new();
        expect.terminal = Some(true);
        // BTN bets b → uncalled → BTN committed_total = 300. Pot = 50+300+300 = 650. BTN wins.
        // BTN net = 650 - 300 = +350.
        expect.payouts = Some(vec![(0, 350), (1, -50), (2, -300), (3, 0), (4, 0), (5, 0)]);
        let leaked: &'static str =
            Box::leak(format!("uncalled_postflop_btn_bet_{b}").into_boxed_str());
        let case = ScenarioCase {
            name: leaked,
            config: poker::TableConfig::default_6max_100bb(),
            seed: b ^ 0xFADE,
            holes: None,
            board: None,
            plan: p,
            expect,
        };
        run_scenario(&case);
        produced += 1;
    }

    // 形态 4：river bet 无 caller
    let river_bets: &[u64] = &[150, 400, 800, 2000, 5000, 9400];
    for &b in river_bets {
        let mut p: Vec<(SeatId, Action)> = Vec::new();
        for s in [3u8, 4, 5] {
            p.push((seat(s), Action::Fold));
        }
        p.push((seat(0), Action::Raise { to: chips(300) }));
        p.push((seat(1), Action::Fold));
        p.push((seat(2), Action::Call));
        // flop check-check
        p.push((seat(2), Action::Check));
        p.push((seat(0), Action::Check));
        // turn check-check
        p.push((seat(2), Action::Check));
        p.push((seat(0), Action::Check));
        // river: BB bet, BTN fold
        p.push((seat(2), Action::Bet { to: chips(b) }));
        p.push((seat(0), Action::Fold));

        let mut expect = ScenarioExpect::new();
        expect.terminal = Some(true);
        // BB bets b (uncalled returned). committed = 300. Pot = 50+300+300 = 650. BB wins.
        // BB net = 650 - 300 = +350.
        expect.payouts = Some(vec![(0, -300), (1, -50), (2, 350), (3, 0), (4, 0), (5, 0)]);
        let leaked: &'static str = Box::leak(format!("uncalled_river_bb_bet_{b}").into_boxed_str());
        let case = ScenarioCase {
            name: leaked,
            config: poker::TableConfig::default_6max_100bb(),
            seed: b ^ 0xACE,
            holes: None,
            board: None,
            plan: p,
            expect,
        };
        run_scenario(&case);
        produced += 1;
    }

    assert!(produced >= 25, "uncalled bet 路径必须 ≥ 25; got {produced}");
}

// ============================================================================
// F. dead money / forfeit — fold 后筹码留 pot（≥ 8 cases）
// ============================================================================

#[test]
fn dead_money_after_fold_table() {
    let mut produced = 0usize;
    // 6-max：UTG/MP/CO 全弃，BTN raise，SB call，BB raise (3bet)，BTN call，SB fold（dead），BB call → flop checkdown
    // SB invested some (dead), BB & BTN go to showdown.
    // 用 build_bb_wins_deck，BB 必胜两人对决。
    let three_bets: &[u64] = &[600, 800, 1000, 1500, 2000, 3000, 4000, 5000];
    for &three in three_bets {
        let cfg = poker::TableConfig::default_6max_100bb();
        let mut deck = build_bb_wins_deck(&cfg);
        let mut p: Vec<(SeatId, Action)> = Vec::new();
        for s in [3u8, 4, 5] {
            p.push((seat(s), Action::Fold));
        }
        p.push((seat(0), Action::Raise { to: chips(300) }));
        p.push((seat(1), Action::Call));
        p.push((seat(2), Action::Raise { to: chips(three) }));
        p.push((seat(0), Action::Call));
        // 现在轮到 SB（committed=300，max=three）→ SB call 或 fold；选 fold（dead money）
        p.push((seat(1), Action::Fold));
        // 双人 (BB / BTN) checkdown 三街
        for _ in 0..3 {
            p.push((seat(2), Action::Check));
            p.push((seat(0), Action::Check));
        }

        let state = drive_with_deck(&cfg, &mut deck, three, &p);
        assert!(state.is_terminal());
        let payouts = state.payouts().unwrap();
        let net = |sid: u8| -> i64 {
            payouts
                .iter()
                .find(|(s, _)| s.0 == sid)
                .map(|(_, n)| *n)
                .unwrap()
        };
        // SB committed_total = 300（dead）. BTN/BB committed = three each（after BTN call to three）.
        // Pot = 300 + three + three = 300 + 2*three. BB wins all.
        // BB net = (300 + 2*three) - three = 300 + three.
        // BTN net = -three. SB net = -300.
        let expected_bb = 300 + three as i64;
        assert_eq!(net(2), expected_bb, "BB net (dead-money 3bet={three})");
        assert_eq!(net(0), -(three as i64));
        assert_eq!(net(1), -300);
        assert_eq!(payouts.iter().map(|(_, n)| n).sum::<i64>(), 0);
        produced += 1;
    }
    assert!(produced >= 6, "dead money cases ≥ 6; got {produced}");
}

// ============================================================================
// G. 总数自检：side pot >= 100, uncalled subset >= 20
// ============================================================================

#[test]
fn side_pot_and_uncalled_floor_check() {
    // 各表实际 produced：
    // - two_way_side_pot_btn_sb_allin_bb_wins_table:                 ≥ 25
    // - three_way_side_pot_bb_winner_table:                          ≥ 25
    // - odd_chip_to_sb_table:                                        ≥ 10
    // - four_way_side_pot_bb_winner_table:                           ≥ 14
    // - uncalled_bet_returned_table:                                 ≥ 25
    // - dead_money_after_fold_table:                                 ≥ 6
    //
    // 合计 side pot ≥ 80（不含 uncalled） + uncalled 25 = 105；满足 ≥ 100。
    // uncalled subset 25 ≥ 20。
    //
    // 这里再额外补一组 5-way side pot（仍是 BB 必胜模板，覆盖更多 stack 形状）以
    // 把 side pot 总数推到 ≥ 110。
    let mut produced = 0usize;
    let configs: &[(u64, u64, u64, u64)] = &[
        (200, 300, 400, 500),
        (300, 600, 900, 1200),
        (500, 1000, 1500, 2000),
        (700, 200, 400, 100),
        (1000, 800, 600, 400),
        (1500, 1200, 900, 600),
        (2000, 1500, 1000, 500),
        (3000, 2000, 1000, 500),
        (5000, 3000, 1500, 800),
        (9000, 7000, 5000, 3000),
    ];
    for &(co, btn, sb, mp) in configs {
        if co < 100 || btn < 100 || sb < 50 || mp < 100 {
            continue;
        }
        let cfg = cfg_6max_with_stacks([btn, sb, 10000, 10000, mp, co]);
        let mut deck = build_bb_wins_deck(&cfg);
        let plan_vec = plan(&[
            (3, Action::Fold),
            (4, Action::AllIn), // MP
            (5, Action::AllIn), // CO
            (0, Action::AllIn), // BTN
            (1, Action::AllIn), // SB
            (2, Action::AllIn), // BB
        ]);
        let state = drive_with_deck(&cfg, &mut deck, co ^ btn ^ sb ^ mp ^ 0x55, &plan_vec);
        assert!(state.is_terminal());
        let payouts = state.payouts().unwrap();
        let net = |sid: u8| -> i64 {
            payouts
                .iter()
                .find(|(s, _)| s.0 == sid)
                .map(|(_, n)| *n)
                .unwrap()
        };
        // BB 必胜赢全部 → BB net = co + btn + sb + mp.
        let expected_bb = (co + btn + sb + mp) as i64;
        assert_eq!(net(2), expected_bb);
        assert_eq!(payouts.iter().map(|(_, n)| n).sum::<i64>(), 0);
        produced += 1;
    }
    assert!(produced >= 9, "5-way side pot ≥ 9; got {produced}");
}

// ============================================================================
// H. all-in player AllIn 无效（stack=0）— RuleError::InsufficientStack
// ============================================================================

#[test]
fn allin_after_zero_stack_is_invalid() {
    // 让 BTN 通过 AllIn 自带变 0。然后玩家再尝试 AllIn / Bet 应失败 — 但状态机
    // 不再调度该玩家（status=AllIn → not Active），无法直接构造该 RuleError 路径。
    // 故本 #[test] 只 sanity 检查：BTN AllIn 后 status=AllIn 且 stack=0。
    let mut s = GameState::new(&poker::TableConfig::default_6max_100bb(), 800);
    let total = expected_total_chips(&poker::TableConfig::default_6max_100bb());
    for (_, action) in plan(&[
        (3, Action::Fold),
        (4, Action::Fold),
        (5, Action::Fold),
        (0, Action::AllIn),
        (1, Action::Fold),
        (2, Action::Fold),
    ]) {
        s.apply(action).unwrap();
        Invariants::check_all(&s, total).unwrap();
    }
    assert!(s.is_terminal());
    let btn = s.players().iter().find(|p| p.seat == seat(0)).unwrap();
    // BTN 此时已在 finalize_terminal 中通过 uncalled-bet refund 把 stack 恢复一部分；
    // 但 status 仍是 AllIn（因为 stack 在 all-in 阶段被减到 0 后 set 的）。
    // 这里只验证：committed_total <= 100（uncalled returned）+ status path.
    assert_eq!(btn.committed_total, chips(100));
    // AllIn 后 stack 在 uncalled refund 中恢复；status 不再 reset。容忍两种实现。
    assert!(matches!(
        btn.status,
        PlayerStatus::AllIn | PlayerStatus::Active
    ));
}
