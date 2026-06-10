//! OpenPoker HH（全桌手牌历史）JSONL → [`MultiwayHandInput`]（`realtime_search_openpoker_exec`
//! §4.2 数据管道，缺口⑥ live 半段）。
//!
//! driver（`tools/openpoker_play.py --hh-log`）每手 `hand_result` 落一行 JSONL（**OpenPoker
//! 单位**：SB10/BB20）；本模块做三件事，全部 **loud `Err`**（数据管道问题必须暴露并计数，
//! 选择性静默丢弃 = selection bias，与 `aivat_multiway` 同哲学）：
//!
//! 1. **单位缩放**：`scale = solver_BB(100) / op_BB`，整除且 SB 对齐才接受（与
//!    `openpoker_advisor` 同口径）。
//! 2. **动作转换重放**：driver 的 `{seat, action, to?}`（to = 本街累计到额）→ 具体 [`Action`]。
//!    Bet/Raise 种类按重放态当前合法集判（[`hist_to_concrete`]，与 advisor 共用同一函数）；
//!    AllIn/Call 的额由规则引擎从真栈推导（不信日志）。重放必须走到终局。
//! 3. **结算映射**：`U = (final_stacks[我] − hand_start[我]) × scale`——不依赖 `winners.amount`
//!    的 gross/net 约定；hand-start 真栈优先取 driver 回推的 `stacks_start`，缺则由
//!    `final − won + committed_total` 反推（已知缺口：all_in 无 amount 时 committed_total 短记——
//!    错值会被 [`MultiwayAivatEstimator`](crate::training::aivat_multiway::MultiwayAivatEstimator)
//!    的 U 重放校验 loud 拦下，不会静默给偏样本）。
//!
//! [`MultiwayAivatEstimator`]: crate::training::aivat_multiway::MultiwayAivatEstimator

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::core::{Card, ChipAmount, SeatId};
use crate::rules::action::Action;
use crate::rules::config::TableConfig;
use crate::rules::state::GameState;
use crate::training::aivat_multiway::MultiwayHandInput;
use crate::training::blueprint_advisor::parse_card;

/// solver 单位（与 `TableConfig::default_6max_100bb` 一致）。
const SOLVER_BB: u64 = 100;
const SOLVER_SB: u64 = 50;

/// 转换重放 seed：牌不被读（下注几何与牌无关），seed 只是 `GameState::new` 的强制输入。
const HH_REPLAY_SEED: u64 = 0x4F50_4848_5245_504C; // "OPHHREPL"

/// 把一条历史动作（`to` 已 ×scale 到 solver 单位）译成 stage-1 [`Action`]。
/// raise/bet 按 `real` 当前 legal（LA-002：无前序 bet → Bet，否则 Raise）选种类。
/// （原 `tools/openpoker_advisor.rs` 私有函数，HH 解析与 advisor 共用同一口径后移到这里。）
pub fn hist_to_concrete(real: &GameState, action: &str, to_solver: Option<u64>) -> Option<Action> {
    match action {
        "fold" => Some(Action::Fold),
        "check" => Some(Action::Check),
        "call" => Some(Action::Call),
        "all_in" | "allin" => Some(Action::AllIn),
        "raise" | "bet" => {
            let to = ChipAmount::new(to_solver?);
            if real.legal_actions().bet_range.is_some() {
                Some(Action::Bet { to })
            } else {
                Some(Action::Raise { to })
            }
        }
        _ => None,
    }
}

/// 一条 HH 动作（driver `HandState.actions` 原样；`to` = 该座本街累计到额，OpenPoker 单位）。
#[derive(Deserialize, Debug, Clone)]
pub struct HhAction {
    pub seat: u8,
    pub action: String,
    #[serde(default)]
    pub to: Option<f64>,
}

/// `hand_result` 的 winners 条目（服务端原样；amount = 该座从 pot 拿走的额）。
#[derive(Deserialize, Debug, Clone)]
pub struct HhWinner {
    pub seat: u8,
    #[serde(default)]
    pub amount: Option<f64>,
}

/// `hand_result` 原样字段（driver 不做有损映射）。座位键是字符串（JSON object key）。
#[derive(Deserialize, Debug, Clone)]
pub struct HhHandResult {
    #[serde(default)]
    pub winners: Vec<HhWinner>,
    #[serde(default)]
    pub final_stacks: BTreeMap<String, f64>,
    #[serde(default)]
    pub shown_cards: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub pot: Option<f64>,
}

/// 一手 HH JSONL 记录（driver `HandState::hh_record` 口径；未列字段 serde 忽略，
/// `actions_ext` v1 不消费、只随日志留作 committed 修复材料）。
#[derive(Deserialize, Debug, Clone)]
pub struct HhRecord {
    #[serde(default)]
    pub hand_id: Option<String>,
    pub button_seat: u8,
    pub my_seat: u8,
    pub num_seats: u8,
    pub small_blind: u64,
    pub big_blind: u64,
    #[serde(default)]
    pub hole: Option<Vec<String>>,
    #[serde(default)]
    pub board: Vec<String>,
    #[serde(default)]
    pub actions: Vec<HhAction>,
    #[serde(default)]
    pub names: BTreeMap<String, String>,
    #[serde(default)]
    pub stacks_start: Option<Vec<f64>>,
    #[serde(default)]
    pub committed_total: BTreeMap<String, f64>,
    pub hand_result: HhHandResult,
}

/// 转换结果：估计器输入 + 报表层要的换算元数据。
pub struct HhConverted {
    pub input: MultiwayHandInput,
    /// OpenPoker → solver 的筹码倍率（默认场 5）。
    pub scale: u64,
    /// OpenPoker 单位的 BB（mbb/g 已在 solver 单位算，这里只留审计）。
    pub big_blind_op: u64,
    pub hand_id: Option<String>,
}

fn to_chip(v: f64, what: &str) -> Result<u64, String> {
    if !v.is_finite() || v < 0.0 || v.fract() != 0.0 || v > 1e15 {
        return Err(format!("{what} 非法筹码值 {v}"));
    }
    Ok(v as u64)
}

fn seat_get<T>(m: &BTreeMap<String, T>, seat: usize) -> Option<&T> {
    m.get(&seat.to_string())
}

/// 该座从 pot 拿走的总额（winners 可多条 = side pot 拆分）。计入回推时 amount 缺失 → `Err`。
fn won_amount(rec: &HhRecord, seat: usize) -> Result<f64, String> {
    let mut sum = 0.0;
    for w in &rec.hand_result.winners {
        if w.seat as usize == seat {
            sum += w
                .amount
                .ok_or_else(|| format!("winners[seat={seat}] 缺 amount（无法回推真栈）"))?;
        }
    }
    Ok(sum)
}

/// HH 记录 → [`MultiwayHandInput`]（solver 单位）。任何不一致 loud `Err`（调用方计数）。
pub fn hh_to_multiway_input(rec: &HhRecord) -> Result<HhConverted, String> {
    let n = rec.num_seats as usize;
    if !(2..=6).contains(&n) {
        return Err(format!("num_seats {n} 越界"));
    }
    let our = rec.my_seat as usize;
    if our >= n || rec.button_seat as usize >= n {
        return Err(format!(
            "my_seat {our} / button_seat {} 越界（{n} 座）",
            rec.button_seat
        ));
    }
    if rec.big_blind == 0 || SOLVER_BB % rec.big_blind != 0 {
        return Err(format!(
            "scale 非整数：solver BB {SOLVER_BB} / op BB {}",
            rec.big_blind
        ));
    }
    let scale = SOLVER_BB / rec.big_blind;
    if rec.small_blind * scale != SOLVER_SB {
        return Err(format!(
            "SB 不对齐：op SB {} ×{scale} ≠ solver SB {SOLVER_SB}",
            rec.small_blind
        ));
    }

    // ---- hand-start 真栈（OpenPoker 单位）----
    let start_op: Vec<u64> = match &rec.stacks_start {
        Some(ss) => {
            if ss.len() != n {
                return Err(format!("stacks_start 长度 {} ≠ {n}", ss.len()));
            }
            ss.iter()
                .enumerate()
                .map(|(i, &v)| to_chip(v, &format!("stacks_start[{i}]")))
                .collect::<Result<_, _>>()?
        }
        None => (0..n)
            .map(|s| {
                let fin = *seat_get(&rec.hand_result.final_stacks, s).ok_or_else(|| {
                    format!("final_stacks 缺座 {s}（无 stacks_start 时无法回推）")
                })?;
                let won = won_amount(rec, s)?;
                let com = seat_get(&rec.committed_total, s).copied().unwrap_or(0.0);
                to_chip(fin - won + com, &format!("回推 start[{s}]"))
            })
            .collect::<Result<_, _>>()?,
    };

    let config = TableConfig {
        n_seats: n as u8,
        starting_stacks: start_op
            .iter()
            .map(|&s| ChipAmount::new(s.checked_mul(scale).expect("to_chip 已限 1e15")))
            .collect(),
        small_blind: ChipAmount::new(SOLVER_SB),
        big_blind: ChipAmount::new(SOLVER_BB),
        ante: ChipAmount::ZERO,
        button_seat: SeatId(rec.button_seat),
    };

    // ---- 动作转换重放（actor 逐步校验；Bet/Raise 种类按重放态合法集判）----
    let mut st = GameState::new(&config, HH_REPLAY_SEED);
    let mut actions: Vec<(SeatId, Action)> = Vec::with_capacity(rec.actions.len());
    for (i, h) in rec.actions.iter().enumerate() {
        let seat = SeatId(h.seat);
        if st.current_player() != Some(seat) {
            return Err(format!(
                "转换重放第 {i} 步：期望行动者 {:?} ≠ 日志 seat {}（短桌/空座/丢消息？）",
                st.current_player(),
                h.seat
            ));
        }
        let to_solver = match h.to {
            Some(t) => Some(to_chip(t, &format!("actions[{i}].to"))? * scale),
            None => None,
        };
        let a = hist_to_concrete(&st, &h.action, to_solver)
            .ok_or_else(|| format!("转换重放第 {i} 步：动作 {:?} 不可译", h.action))?;
        st.apply(a)
            .map_err(|e| format!("转换重放第 {i} 步 apply({a:?}) 非法: {e:?}"))?;
        actions.push((seat, a));
    }
    if !st.is_terminal() {
        return Err("动作序结束但未到终局（HH 不完整？）".to_string());
    }

    // ---- 牌面 ----
    let hole_strs = rec.hole.as_ref().ok_or("缺 hole（没收到 hole_cards？）")?;
    if hole_strs.len() != 2 {
        return Err(format!("hole 须 2 张，得 {}", hole_strs.len()));
    }
    let our_hole = [parse_card(&hole_strs[0])?, parse_card(&hole_strs[1])?];
    let board: Vec<Card> = rec
        .board
        .iter()
        .map(|s| parse_card(s))
        .collect::<Result<_, _>>()?;
    let mut revealed: Vec<Option<[Card; 2]>> = vec![None; n];
    for (k, cards) in &rec.hand_result.shown_cards {
        let s: usize = k
            .parse()
            .map_err(|_| format!("shown_cards 座位键非数字: {k:?}"))?;
        if s >= n {
            return Err(format!("shown_cards 座 {s} 越界"));
        }
        if cards.len() != 2 {
            return Err(format!("shown_cards[{s}] 须 2 张，得 {}", cards.len()));
        }
        revealed[s] = Some([parse_card(&cards[0])?, parse_card(&cards[1])?]);
    }

    // ---- U（solver chips）= (final − start) × scale ----
    let fin_our = *seat_get(&rec.hand_result.final_stacks, our)
        .ok_or_else(|| format!("final_stacks 缺我方座 {our}"))?;
    if !fin_our.is_finite() || fin_our.fract() != 0.0 {
        return Err(format!("final_stacks[{our}] 非法筹码值 {fin_our}"));
    }
    let winnings = (fin_our as i64 - start_op[our] as i64) * scale as i64;

    Ok(HhConverted {
        input: MultiwayHandInput {
            config,
            our_seat: SeatId(rec.my_seat),
            our_hole,
            revealed,
            board,
            actions,
            winnings,
        },
        scale,
        big_blind_op: rec.big_blind,
        hand_id: rec.hand_id.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::training::aivat_multiway::MultiwayAivatEstimator;

    /// 与 `tools/openpoker_play.py` selftest 场景 5 的 canned 序列**同一手**（两边互为
    /// oracle）：button=0、我=BB 座 2，preflop SB call/BB check → flop SB bet 20/BB call →
    /// turn/river 双 check → 摊牌我赢 80（净 +40 op = +200 chips）。
    fn canned_showdown_line() -> String {
        r#"{
          "hh": 1, "ts": 0, "hand_id": "hh-selftest-1",
          "button_seat": 0, "my_seat": 2, "num_seats": 6,
          "small_blind": 10, "big_blind": 20,
          "hole": ["Ah", "Kd"],
          "board": ["7h", "2c", "Ks", "5d", "9c"],
          "street": "river",
          "actions": [
            {"seat": 3, "action": "fold"}, {"seat": 4, "action": "fold"},
            {"seat": 5, "action": "fold"}, {"seat": 0, "action": "fold"},
            {"seat": 1, "action": "call"}, {"seat": 2, "action": "check"},
            {"seat": 1, "action": "bet", "to": 20}, {"seat": 2, "action": "call"},
            {"seat": 1, "action": "check"}, {"seat": 2, "action": "check"},
            {"seat": 1, "action": "check"}, {"seat": 2, "action": "check"}
          ],
          "actions_ext": [{}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}],
          "names": {"0": "bot0", "1": "bot1", "2": "bot2", "3": "bot3", "4": "bot4", "5": "bot5"},
          "stacks_start": [2000, 2000, 2000, 2000, 2000, 2000],
          "committed_total": {"0": 0, "1": 40, "2": 40, "3": 0, "4": 0, "5": 0},
          "hand_result": {
            "winners": [{"seat": 2, "stack": 2040, "amount": 80, "hand_description": "pair of kings"}],
            "pot": 80,
            "final_stacks": {"0": 2000, "1": 1960, "2": 2040, "3": 2000, "4": 2000, "5": 2000},
            "shown_cards": {"1": ["Qs", "Qd"], "2": ["Ah", "Kd"]}
          }
        }"#
        .to_string()
    }

    #[test]
    fn canned_showdown_converts_and_estimates() {
        let rec: HhRecord = serde_json::from_str(&canned_showdown_line()).expect("parse");
        let conv = hh_to_multiway_input(&rec).expect("convert");
        assert_eq!(conv.scale, 5);
        assert_eq!(conv.input.winnings, 200, "净 +40 op × 5");
        assert_eq!(conv.input.config.starting_stacks.len(), 6);
        assert_eq!(conv.input.config.starting_stacks[0].as_u64(), 10_000);
        assert_eq!(conv.input.actions.len(), 12);
        // flop 首注在转换重放里必须译成 Bet（LA-002），不是 Raise。
        assert!(matches!(conv.input.actions[6].1, Action::Bet { .. }));

        let est = MultiwayAivatEstimator::new(None);
        let r = est.estimate_hand(&conv.input).expect("estimate");
        assert_eq!(r.raw, 200.0);
        // 最后动作（river check）时 board 已 5 张 → 无纯发牌段。
        assert!(!r.has_runout);
        assert_eq!(r.aivat, r.raw);
        assert_eq!(r.our_rel_pos, 2, "BB 相对 button=0 是 2");
    }

    /// turn all-in → river 纯发牌段：c_runout 生效（44 张补全），AIVAT = U − c_runout。
    /// SB(1) turn all_in（引擎从真栈推导额，HH 不带 to）、BB(2,我) call。
    #[test]
    fn turn_allin_runout_estimates() {
        let line = r#"{
          "button_seat": 0, "my_seat": 2, "num_seats": 6,
          "small_blind": 10, "big_blind": 20,
          "hole": ["Ah", "Kd"],
          "board": ["7h", "2c", "Ks", "5d", "9c"],
          "actions": [
            {"seat": 3, "action": "fold"}, {"seat": 4, "action": "fold"},
            {"seat": 5, "action": "fold"}, {"seat": 0, "action": "fold"},
            {"seat": 1, "action": "call"}, {"seat": 2, "action": "check"},
            {"seat": 1, "action": "check"}, {"seat": 2, "action": "check"},
            {"seat": 1, "action": "all_in"}, {"seat": 2, "action": "call"}
          ],
          "stacks_start": [2000, 2000, 2000, 2000, 2000, 2000],
          "committed_total": {"0": 0, "1": 2000, "2": 2000, "3": 0, "4": 0, "5": 0},
          "hand_result": {
            "winners": [{"seat": 2, "stack": 4000, "amount": 4000}],
            "pot": 4000,
            "final_stacks": {"0": 2000, "1": 0, "2": 4000, "3": 2000, "4": 2000, "5": 2000},
            "shown_cards": {"1": ["Qs", "Qd"], "2": ["Ah", "Kd"]}
          }
        }"#;
        let rec: HhRecord = serde_json::from_str(line).expect("parse");
        let conv = hh_to_multiway_input(&rec).expect("convert");
        assert_eq!(conv.input.winnings, 10_000, "净 +2000 op × 5");
        assert!(matches!(conv.input.actions[8].1, Action::AllIn));

        let est = MultiwayAivatEstimator::new(None);
        let r = est.estimate_hand(&conv.input).expect("estimate");
        assert!(r.has_runout, "turn 锁定 → river 纯发牌段");
        // 牌堆 = 52 − (board 前 4 + 我方 2 + 亮牌 2) = 44，k=1。
        assert_eq!(r.n_runout_completions, 44);
        assert_eq!(r.aivat, r.raw - r.c_runout);
        assert!(r.c_runout.abs() < 10_000.0 + 1e-9);
    }

    /// final_stacks 被篡改 → U 与重放结算不一致 → 估计器 loud Err（不静默给偏样本）。
    #[test]
    fn tampered_final_stacks_is_loud_err() {
        let rec: HhRecord =
            serde_json::from_str(&canned_showdown_line().replace(r#""2": 2040"#, r#""2": 2060"#))
                .expect("parse");
        let conv = hh_to_multiway_input(&rec).expect("convert 本身不查 U");
        let est = MultiwayAivatEstimator::new(None);
        let e = est.estimate_hand(&conv.input).unwrap_err();
        assert!(e.contains("U 校验失败"), "应报 U 校验失败，得 {e}");
    }

    /// 无 stacks_start（如 BB walk 手没有 your_turn）→ 由 final − won + committed_total 回推。
    /// button=0：UTG..SB 全 fold，我=BB 收 SB 的 10（gross 30、committed 20、净 +10 op）。
    #[test]
    fn walk_hand_reconstructs_without_stacks_start() {
        let line = r#"{
          "button_seat": 0, "my_seat": 2, "num_seats": 6,
          "small_blind": 10, "big_blind": 20,
          "hole": ["Ah", "Kd"],
          "board": [],
          "actions": [
            {"seat": 3, "action": "fold"}, {"seat": 4, "action": "fold"},
            {"seat": 5, "action": "fold"}, {"seat": 0, "action": "fold"},
            {"seat": 1, "action": "fold"}
          ],
          "stacks_start": null,
          "committed_total": {"0": 0, "1": 10, "2": 20, "3": 0, "4": 0, "5": 0},
          "hand_result": {
            "winners": [{"seat": 2, "amount": 30}],
            "pot": 30,
            "final_stacks": {"0": 2000, "1": 1990, "2": 2010, "3": 2000, "4": 2000, "5": 2000}
          }
        }"#;
        let rec: HhRecord = serde_json::from_str(line).expect("parse");
        let conv = hh_to_multiway_input(&rec).expect("convert");
        assert_eq!(
            conv.input.config.starting_stacks[1].as_u64(),
            10_000,
            "SB 回推 2000 op"
        );
        assert_eq!(
            conv.input.config.starting_stacks[2].as_u64(),
            10_000,
            "BB 回推 2000 op"
        );
        assert_eq!(conv.input.winnings, 50, "+10 op × 5");
        let est = MultiwayAivatEstimator::new(None);
        let r = est.estimate_hand(&conv.input).expect("estimate");
        assert_eq!(r.raw, 50.0);
        assert!(!r.has_runout);
    }

    /// 服务端 JSON 常给 float（2000.0）：整数值的 float 必须照吃，真分数才 Err。
    #[test]
    fn float_integral_amounts_tolerated() {
        let line = canned_showdown_line()
            .replace(
                r#""stacks_start": [2000, 2000, 2000, 2000, 2000, 2000]"#,
                r#""stacks_start": [2000.0, 2000.0, 2000.0, 2000.0, 2000.0, 2000.0]"#,
            )
            .replace(r#""2": 2040"#, r#""2": 2040.0"#);
        let rec: HhRecord = serde_json::from_str(&line).expect("parse");
        let conv = hh_to_multiway_input(&rec).expect("convert");
        assert_eq!(conv.input.winnings, 200);

        let frac: HhRecord = serde_json::from_str(&canned_showdown_line().replace(
            r#""stacks_start": [2000, 2000, 2000, 2000, 2000, 2000]"#,
            r#""stacks_start": [2000.5, 2000, 2000, 2000, 2000, 2000]"#,
        ))
        .expect("parse");
        assert!(hh_to_multiway_input(&frac).is_err(), "分数筹码应 Err");
    }

    /// 行动者对不上（短桌 / 空座 / 丢消息）→ 转换重放 loud Err。
    #[test]
    fn wrong_actor_order_is_err() {
        let rec: HhRecord = serde_json::from_str(&canned_showdown_line().replace(
            r#"{"seat": 3, "action": "fold"}"#,
            r#"{"seat": 4, "action": "fold"}"#,
        ))
        .expect("parse");
        let e = match hh_to_multiway_input(&rec) {
            Err(e) => e,
            Ok(_) => panic!("行动者错位应 Err"),
        };
        assert!(e.contains("期望行动者"), "应报行动者错位，得 {e}");
    }
}
