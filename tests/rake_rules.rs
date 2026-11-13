//! 抽水规则回归（`TableConfig::rake_bps` ≠ 0）。
//!
//! 语义（按 wopo ante 桌实测定，对照脚本 `tools/wopo_rake_check.py`）：终局结算时，每个座位从
//! **独赢**的底池（主池 / 边池）拿到的筹码合计 × `rake_bps` / 10000 向下取整扣一份，每座封顶
//! `rake_cap`（0 = 不封顶）；平分的池不抽；没人跟的注先退回、不计入；翻前结束也抽。收益不再零和，
//! 各座净收益之和 = −抽水合计。`rake_bps == 0`（默认）时与历史逐位相同。本文件用手算期望值锁死边角。

mod common;

use std::collections::HashSet;

use poker::{Action, Card, GameState, HandHistory, TableConfig};

use common::{build_dealing_order, card, chips, pick_unused_padding, seat, StackedDeckRng};

/// wopo 的抽水：4%、每座封顶 10BB（BB = 100 chips）。
fn wopo_rake(cfg: TableConfig) -> TableConfig {
    cfg.with_rake(400, chips(1_000))
}

/// 指定若干座位的底牌和五张公共牌，其余座位发剩下的牌。
fn deck(cfg: &TableConfig, fixed_holes: &[(u8, (Card, Card))], board: [Card; 5]) -> StackedDeckRng {
    let n = cfg.n_seats as usize;
    let fixed: HashSet<u8> = fixed_holes
        .iter()
        .flat_map(|&(_, (a, b))| [a.to_u8(), b.to_u8()])
        .chain(board.iter().map(|c| c.to_u8()))
        .collect();
    let mut pool = pick_unused_padding(&fixed, 52 - fixed.len());
    // holes[k] = 按钮左手第 k+1 个座位（发牌起点 SB）的底牌，与 build_dealing_order 契约一致。
    let holes: Vec<_> = (0..n)
        .map(|k| {
            let s = ((cfg.button_seat.0 as usize + 1 + k) % n) as u8;
            match fixed_holes.iter().find(|&&(fs, _)| fs == s) {
                Some(&(_, h)) => h,
                None => (pool.remove(0), pool.remove(0)),
            }
        })
        .collect();
    let used: HashSet<u8> = holes
        .iter()
        .flat_map(|(a, b)| [a.to_u8(), b.to_u8()])
        .chain(board.iter().map(|c| c.to_u8()))
        .collect();
    let padding = pick_unused_padding(&used, 52 - 2 * n - 5);
    StackedDeckRng::from_target_cards(build_dealing_order(
        n,
        &holes,
        [board[0], board[1], board[2]],
        board[3],
        board[4],
        &padding,
    ))
}

/// 公共牌 Ac Ad 9h Th Jh：拿 As Ah 的人四条 A 必胜。
fn quads_board() -> [Card; 5] {
    [card(12, 0), card(12, 1), card(7, 2), card(8, 2), card(9, 2)]
}

fn aces() -> (Card, Card) {
    (card(12, 3), card(12, 2)) // As Ah
}

fn drive(state: &mut GameState, plan: &[(u8, Action)]) {
    for (i, &(want, action)) in plan.iter().enumerate() {
        assert_eq!(state.current_player(), Some(seat(want)), "step {i}");
        state
            .apply(action)
            .unwrap_or_else(|e| panic!("step {i}: apply({action:?}) failed: {e}"));
    }
}

fn nets(state: &GameState) -> Vec<i64> {
    let mut v = state.payouts().expect("terminal");
    v.sort_by_key(|(s, _)| s.0);
    v.into_iter().map(|(_, pnl)| pnl).collect()
}

/// 全弃到 BB（6 座、每座 ante 10）：大盲没人跟的那一半（50）先退回，底池 = 6×10 + 50 + 50 = 160，
/// BB 独赢 → 抽 ⌊160 × 4%⌋ = 6。BB 净 +100 − 6 = +94，其余座位和不抽水时一样；翻前结束照抽。
/// wopo 实测同此口径：50/100、每座 ante 100 的 8 人桌走盲，`total_pot` = 900（不是 950），扣 36。
#[test]
fn walk_is_raked_on_the_matched_pot() {
    let cfg = wopo_rake(TableConfig::default_6max_100bb().with_ante(chips(10)));
    let mut state = GameState::new(&cfg, 1);
    drive(
        &mut state,
        &[
            (3, Action::Fold),
            (4, Action::Fold),
            (5, Action::Fold),
            (0, Action::Fold),
            (1, Action::Fold),
        ],
    );
    assert!(state.is_terminal());
    assert_eq!(nets(&state), vec![-10, -60, 94, -10, -10, -10]);
}

/// 没人跟的注不计入抽水基数：UTG 加注到 300、全弃。退回 200 后底池 = 50 + 100 + 100 = 250 →
/// 抽 10，UTG 净 +150 − 10 = +140（按 450 抽会是 18）。
#[test]
fn uncalled_bet_is_returned_before_rake() {
    let cfg = wopo_rake(TableConfig::default_6max_100bb());
    let mut state = GameState::new(&cfg, 2);
    drive(
        &mut state,
        &[
            (3, Action::Raise { to: chips(300) }),
            (4, Action::Fold),
            (5, Action::Fold),
            (0, Action::Fold),
            (1, Action::Fold),
            (2, Action::Fold),
        ],
    );
    assert!(state.is_terminal());
    assert_eq!(nets(&state), vec![0, -50, -100, 140, 0, 0]);
}

/// 封顶：UTG / SB / BB 三家各 100BB 全下，UTG 四条 A 独赢 30000 → 4% = 1200，封顶 1000。
/// UTG 净 +20000 − 1000；不封顶（`rake_cap` = 0）时扣足 1200。
#[test]
fn rake_is_capped_per_seat() {
    let plan = [
        (3, Action::AllIn),
        (4, Action::Fold),
        (5, Action::Fold),
        (0, Action::Fold),
        (1, Action::Call),
        (2, Action::Call),
    ];
    let base = TableConfig::default_6max_100bb();
    for (cfg, rake) in [
        (wopo_rake(base.clone()), 1_000),
        (base.clone().with_rake(400, chips(0)), 1_200),
        (base.clone(), 0),
    ] {
        let mut d = deck(&cfg, &[(3, aces())], quads_board());
        let mut state = GameState::with_rng(&cfg, 3, &mut d);
        drive(&mut state, &plan);
        assert!(state.is_terminal());
        assert_eq!(
            nets(&state),
            vec![0, -10_000, -10_000, 20_000 - rake, 0, 0],
            "rake {rake}"
        );
    }
}

/// 平分的池不抽：SB As Kd、BB Ah Kc，公共牌 2c 7d 9h Js 3c，两家同是 A 高。SB 加注到 300、BB 跟，
/// 一路过牌到摊牌，600 各分 300 → 两家净 0，全桌之和 0。
#[test]
fn split_pot_pays_no_rake() {
    let cfg = wopo_rake(TableConfig::default_6max_100bb());
    let board = [card(0, 0), card(5, 1), card(7, 2), card(9, 3), card(1, 0)];
    let holes = [
        (1, (card(12, 3), card(11, 1))),
        (2, (card(12, 2), card(11, 0))),
    ];
    let mut d = deck(&cfg, &holes, board);
    let mut state = GameState::with_rng(&cfg, 4, &mut d);
    let mut plan = vec![
        (3, Action::Fold),
        (4, Action::Fold),
        (5, Action::Fold),
        (0, Action::Fold),
        (1, Action::Raise { to: chips(300) }),
        (2, Action::Call),
    ];
    for _ in 0..3 {
        plan.extend([(1, Action::Check), (2, Action::Check)]);
    }
    drive(&mut state, &plan);
    assert!(state.is_terminal());
    assert_eq!(nets(&state), vec![0, 0, 0, 0, 0, 0]);
}

/// 主池、边池赢家不同，各抽各的：UTG 只有 10BB 全下拿四条 A；SB 9s 9d（葫芦）、BB 2c 3d（一对 A）
/// 各 100BB 全下。主池 3×1000 = 3000 归 UTG → 抽 120，净 +2000 − 120；边池 2×9000 = 18000 归 SB →
/// 抽 720，净 +8000 − 720；BB −10000。全桌之和 = −840。
#[test]
fn main_and_side_pot_winners_each_pay_their_own_rake() {
    let mut cfg = wopo_rake(TableConfig::default_6max_100bb());
    cfg.starting_stacks[3] = chips(1_000);
    let holes = [
        (3, aces()),
        (1, (card(7, 3), card(7, 1))),
        (2, (card(0, 0), card(1, 1))),
    ];
    let mut d = deck(&cfg, &holes, quads_board());
    let mut state = GameState::with_rng(&cfg, 5, &mut d);
    drive(
        &mut state,
        &[
            (3, Action::AllIn),
            (4, Action::Fold),
            (5, Action::Fold),
            (0, Action::Fold),
            (1, Action::AllIn),
            (2, Action::Call),
        ],
    );
    assert!(state.is_terminal());
    let got = nets(&state);
    assert_eq!(got, vec![0, 8_000 - 720, -10_000, 2_000 - 120, 0, 0]);
    assert_eq!(got.iter().sum::<i64>(), -840);
}

/// 同一人独赢主池和边池：封顶按座位算一次，不是每个池各封顶一次。同上局面但四条 A 在 SB，封顶 500：
/// SB 拿 3000 + 18000 = 21000，4% = 840 → 封顶 500（按池各封顶会是 120 + 500 = 620）。
#[test]
fn one_seat_winning_main_and_side_pot_hits_the_cap_once() {
    let mut cfg = TableConfig::default_6max_100bb().with_rake(400, chips(500));
    cfg.starting_stacks[3] = chips(1_000);
    let holes = [(1, aces()), (2, (card(0, 0), card(1, 1)))];
    let mut d = deck(&cfg, &holes, quads_board());
    let mut state = GameState::with_rng(&cfg, 6, &mut d);
    drive(
        &mut state,
        &[
            (3, Action::AllIn),
            (4, Action::Fold),
            (5, Action::Fold),
            (0, Action::Fold),
            (1, Action::AllIn),
            (2, Action::Call),
        ],
    );
    assert!(state.is_terminal());
    assert_eq!(nets(&state), vec![0, 11_000 - 500, -10_000, -1_000, 0, 0]);
}

/// `rake_on`：向下取整、封顶、0 比例恒 0（此时封顶值不起作用）。
#[test]
fn rake_on_floors_and_caps() {
    let cfg = wopo_rake(TableConfig::default_6max_100bb());
    assert_eq!(cfg.rake_on(0), 0);
    assert_eq!(cfg.rake_on(24), 0);
    assert_eq!(cfg.rake_on(25), 1);
    assert_eq!(cfg.rake_on(1_450), 58);
    assert_eq!(cfg.rake_on(24_999), 999);
    assert_eq!(cfg.rake_on(25_000), 1_000);
    assert_eq!(cfg.rake_on(1_000_000), 1_000);
    let none = TableConfig::default_6max_100bb().with_rake(0, chips(1_000));
    assert_eq!(none.rake_on(1_000_000), 0);
}

/// history 往返：抽水写进 proto（tag 10 / 11）、读回逐字段相同，replay 到同一终局（含抽水后的收益）。
/// 不抽水时不编码：字节与没有这两个字段的旧 history 相同。
#[test]
fn history_roundtrip_keeps_rake() {
    let cfg = wopo_rake(TableConfig::default_6max_100bb().with_ante(chips(10)));
    let mut state = GameState::new(&cfg, 7);
    drive(
        &mut state,
        &[
            (3, Action::Fold),
            (4, Action::Fold),
            (5, Action::Fold),
            (0, Action::Fold),
            (1, Action::Fold),
        ],
    );
    let bytes = state.hand_history().to_proto();
    let back = HandHistory::from_proto(&bytes).expect("decode");
    assert_eq!(back.config.rake_bps, 400);
    assert_eq!(back.config.rake_cap, chips(1_000));
    let replayed = back.replay().expect("replay");
    assert_eq!(replayed.payouts(), state.payouts());
    assert_eq!(nets(&replayed)[2], 94);

    let plain = GameState::new(&TableConfig::default_6max_100bb(), 7);
    let zero = GameState::new(&TableConfig::default_6max_100bb().with_rake(0, chips(0)), 7);
    assert_eq!(
        plain.hand_history().to_proto(),
        zero.hand_history().to_proto()
    );
}

#[test]
#[should_panic(expected = "rake_bps")]
fn rake_above_100_percent_rejected() {
    let cfg = TableConfig::default_6max_100bb().with_rake(10_001, chips(0));
    let _ = GameState::new(&cfg, 8);
}
