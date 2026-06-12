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
//! 3. **结算映射（live 校准 2026-06-11）**：本手**发牌座位 = `final_stacks` 的键**（真实桌常
//!    短桌：6 座只发 4 家，盲注跳过空座）→ 整手重映射到 0..n_dealt 紧凑 ring（driver 按满桌
//!    猜的盲注/`stacks_start` 在短桌手必错、被忽略）。`U = (final_stacks[我] − hand_start[我])
//!    × scale`；hand-start 真栈分两层：先按回推路给底值——满桌且 driver 已回推 → 取
//!    `stacks_start`，否则按 **net 结算约定**重建（实测 `winners.amount` = 净赢：赢家 start =
//!    final − Σamount、非赢家 = final + 盲注 + Σ`contribution_delta`）——再用
//!    `actions_ext.stack_before` **直读逐座覆盖**（U-fail 修复 2026-06-12：座位首动作前投入
//!    恒 = 盲注 → start = stack_before + 盲注，权威值，关掉两条回推路的 all-in 缺口）。
//!    残余错值仍被
//!    [`MultiwayAivatEstimator`](crate::training::aivat_multiway::MultiwayAivatEstimator)
//!    的 U 重放校验 loud 拦下，不会静默给偏样本。
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

/// 服务端会发**显式 `null`**（live 实测：非摊牌手 `shown_cards: null`；driver 对缺失键也落
/// null）——`#[serde(default)]` 只兜缺失不兜 null，map/list 字段一律配本函数。
fn null_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// `hand_result` 原样字段（driver 不做有损映射）。座位键是字符串（JSON object key）。
#[derive(Deserialize, Debug, Clone)]
pub struct HhHandResult {
    #[serde(default, deserialize_with = "null_default")]
    pub winners: Vec<HhWinner>,
    #[serde(default, deserialize_with = "null_default")]
    pub final_stacks: BTreeMap<String, f64>,
    #[serde(default, deserialize_with = "null_default")]
    pub shown_cards: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub pot: Option<f64>,
}

/// `actions_ext` 条目（driver 平行落的 player_action 原始字段子集；只解析要用的——
/// `contribution_delta` = 该动作的真实投入增量，重建 hand-start 真栈用，比 driver 自跟
/// committed 可靠：driver 盲注 seeding 按满桌 (btn+1/+2) 推、**短桌手必错**；
/// `stack_before` = 该动作前该座真实剩余栈（服务端权威），hand-start 直读覆盖用
/// （U-fail 修复 2026-06-12，见 [`hh_to_multiway_input`] 栈重建段）。
#[derive(Deserialize, Debug, Clone, Default)]
pub struct HhActionExt {
    #[serde(default)]
    pub contribution_delta: Option<f64>,
    #[serde(default)]
    pub stack_before: Option<f64>,
}

/// 一手 HH JSONL 记录（driver `HandState::hh_record` 口径；未列字段 serde 忽略）。
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
    #[serde(default, deserialize_with = "null_default")]
    pub board: Vec<String>,
    #[serde(default, deserialize_with = "null_default")]
    pub actions: Vec<HhAction>,
    #[serde(default, deserialize_with = "null_default")]
    pub actions_ext: Vec<HhActionExt>,
    #[serde(default, deserialize_with = "null_default")]
    pub names: BTreeMap<String, String>,
    #[serde(default)]
    pub stacks_start: Option<Vec<f64>>,
    #[serde(default, deserialize_with = "null_default")]
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
    /// 本手实际发牌座位数（短桌手 < 桌面 num_seats；input 的座位已重映射到 0..n_dealt）。
    /// 注意 rel_pos / VF-1 在短桌 ring 上语义偏移——任意固定 VF 仍无偏，只是降方差弱些。
    pub n_dealt: usize,
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

    // ---- 本手实际发牌座位 = final_stacks 的键（live 校准 2026-06-11）----
    // 真实桌常短桌（6 座只发 4 家）：盲注跳过空座 → driver 按满桌 (btn+1/+2) seed 的盲注
    // 与 stacks_start 回推在短桌手**必错**；发牌座位表只能从 hand_result 取，并把整手
    // 重映射到 0..n_dealt 的紧凑 ring 上（引擎口径的盲注/行动序在该 ring 上天然成立）。
    let mut dealt: Vec<usize> = Vec::with_capacity(rec.hand_result.final_stacks.len());
    for key in rec.hand_result.final_stacks.keys() {
        let s: usize = key
            .parse()
            .map_err(|_| format!("final_stacks 座位键非数字: {key:?}"))?;
        if s >= n {
            return Err(format!("final_stacks 座 {s} 越界（桌面 {n} 座）"));
        }
        dealt.push(s);
    }
    dealt.sort_unstable();
    dealt.dedup();
    let n_dealt = dealt.len();
    if n_dealt < 2 {
        return Err(format!("final_stacks 仅 {n_dealt} 座，构不成一手"));
    }
    let remap = |orig: usize| -> Result<usize, String> {
        dealt
            .binary_search(&orig)
            .map_err(|_| format!("座 {orig} 不在本手发牌座位 {dealt:?} 中"))
    };
    let btn_new = remap(rec.button_seat as usize)?;
    let my_new = remap(our)?;

    // 盲注 = button 起顺时针前两个发牌座。OpenPoker 的 HU 盲注按环规则贴（live 校准
    // 2026-06-11 两轮 smoke 实测：button 发 BB、非 button 发 SB——**非标准 HU**，原
    // 「n_dealt==2：button=SB」假设错，导致 HU 手全部转换失败）→ 不设特例：n_dealt==2 时
    // sb=(btn+1)%2=非 button、bb=button。
    let (sb_new, bb_new) = ((btn_new + 1) % n_dealt, (btn_new + 2) % n_dealt);
    // OpenPoker HU 的**行动序是角色序**（preflop SB 先、postflop BB 先 = 标准 HU 的角色
    // 顺序，只是盲注角色贴错了 button）→ 把**引擎 button 设为 OpenPoker 的 SB 座**做
    // role-for-role 对齐（引擎 n=2 标准 HU：button=SB preflop 先动、BB postflop 先动，
    // D-022b-rev1），盲注与两条街行动序全同（只是 button 标签贴在谁头上不同）。
    // postflop HU 手 live 实证可重放（smoke2 打满 5 街的 HU 手转换 15/15 全过）。
    let engine_btn = if n_dealt == 2 { sb_new } else { btn_new };

    // ---- hand-start 真栈（OP 单位，按新 id 索引）----
    // 满桌且 driver 已回推 → 可信（盲注 seeding 正确）。否则（短桌手 / walk 手无 your_turn）
    // 重建。结算约定 = live 校准（2026-06-11，对账两手实测）：`winners.amount` 是**净赢**
    // （只含对手投入，自家投入隐式返还）→ 赢家 start = final − Σamount；非赢家 start =
    // final + contributed；contributed = 引擎口径盲注 + Σ contribution_delta（actions_ext；
    // 缺 delta 时满桌退 driver committed_total，短桌无可退 → Err）。
    // 已知角落：bet 被 all-in-for-less 跟注后的超额返还不在 delta 里（非赢家 start 高估）→
    // 被估计器 U 重放校验 loud 拦下（计数，不静默偏样本）。
    // 两条回推路之上还有 stack_before 直读逐座覆盖，见下方 override 段。
    if !rec.actions_ext.is_empty() && rec.actions_ext.len() != rec.actions.len() {
        return Err(format!(
            "actions_ext 长度 {} ≠ actions {}",
            rec.actions_ext.len(),
            rec.actions.len()
        ));
    }
    let mut start_op: Vec<u64> = match &rec.stacks_start {
        Some(ss) if n_dealt == n => {
            if ss.len() != n {
                return Err(format!("stacks_start 长度 {} ≠ {n}", ss.len()));
            }
            ss.iter()
                .enumerate()
                .map(|(i, &v)| to_chip(v, &format!("stacks_start[{i}]")))
                .collect::<Result<_, _>>()?
        }
        _ => {
            let mut contributed: Vec<f64> = (0..n_dealt)
                .map(|j| {
                    if j == sb_new {
                        rec.small_blind as f64
                    } else if j == bb_new {
                        rec.big_blind as f64
                    } else {
                        0.0
                    }
                })
                .collect();
            for (i, h) in rec.actions.iter().enumerate() {
                let new_id = remap(h.seat as usize)?;
                match rec.actions_ext.get(i).and_then(|e| e.contribution_delta) {
                    Some(d) => contributed[new_id] += d,
                    None if n_dealt == n => {
                        // 旧日志无 delta：满桌时 driver committed_total 的盲注 seeding 正确，可退。
                        contributed = (0..n_dealt)
                            .map(|j| {
                                seat_get(&rec.committed_total, dealt[j])
                                    .copied()
                                    .unwrap_or(0.0)
                            })
                            .collect();
                        break;
                    }
                    None => {
                        return Err(format!(
                            "actions[{i}] 缺 contribution_delta（短桌手无法重建投入）"
                        ));
                    }
                }
            }
            (0..n_dealt)
                .map(|j| {
                    let orig = dealt[j];
                    let fin = *seat_get(&rec.hand_result.final_stacks, orig)
                        .expect("dealt 即 final_stacks 键集");
                    let won = won_amount(rec, orig)?;
                    let est = if won > 0.0 {
                        fin - won
                    } else {
                        fin + contributed[j]
                    };
                    to_chip(est, &format!("回推 start[seat{orig}]"))
                })
                .collect::<Result<_, _>>()?
        }
    };
    // ---- stack_before 直读逐座覆盖（U-fail 修复 2026-06-12，基线臂发现①）----
    // 服务端 `player_action.stack_before` = 该动作前该座真实剩余栈（OP 单位、权威）。任一座
    // **首个动作**前的投入恒 = 自己的盲注（任何更多投入都必经一次动作）→ start =
    // stack_before(首动作) + 盲注。两条回推路都有已知缺口（driver stacks_start 的 committed
    // 跟踪在 all_in 无 amount 线漏记 = 基线 5 手 +533BB 大锅 U-fail 根因；net 结算回推在
    // bet 被 all-in-for-less 跟注的超额返还角落高估），直读没有。缺 ext（旧日志）/ 无动作
    // 的座（walk / 盲注全下）维持原回推；盲注全下座 posted < 盲注但它必无动作、不会被
    // 错误加回整额盲注。
    {
        let mut seen = vec![false; n_dealt];
        for (i, h) in rec.actions.iter().enumerate() {
            let j = remap(h.seat as usize)?;
            if seen[j] {
                continue;
            }
            seen[j] = true;
            if let Some(sb_val) = rec.actions_ext.get(i).and_then(|e| e.stack_before) {
                let blind = if j == sb_new {
                    rec.small_blind as f64
                } else if j == bb_new {
                    rec.big_blind as f64
                } else {
                    0.0
                };
                start_op[j] = to_chip(
                    sb_val + blind,
                    &format!("stack_before 直读 start[seat{}]", h.seat),
                )?;
            }
        }
    }

    let config = TableConfig {
        n_seats: n_dealt as u8,
        starting_stacks: start_op
            .iter()
            .map(|&s| ChipAmount::new(s.checked_mul(scale).expect("to_chip 已限 1e15")))
            .collect(),
        small_blind: ChipAmount::new(SOLVER_SB),
        big_blind: ChipAmount::new(SOLVER_BB),
        ante: ChipAmount::ZERO,
        button_seat: SeatId(engine_btn as u8),
    };

    // ---- 动作转换重放（座位重映射；actor 逐步校验；Bet/Raise 种类按重放态合法集判）----
    let mut st = GameState::new(&config, HH_REPLAY_SEED);
    let mut actions: Vec<(SeatId, Action)> = Vec::with_capacity(rec.actions.len());
    for (i, h) in rec.actions.iter().enumerate() {
        let seat = SeatId(remap(h.seat as usize)? as u8);
        if st.current_player() != Some(seat) {
            return Err(format!(
                "转换重放第 {i} 步：期望行动者 {:?} ≠ 日志 seat {}（重映射后 {seat:?}；丢消息？）",
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
    let mut revealed: Vec<Option<[Card; 2]>> = vec![None; n_dealt];
    for (key, cards) in &rec.hand_result.shown_cards {
        let s: usize = key
            .parse()
            .map_err(|_| format!("shown_cards 座位键非数字: {key:?}"))?;
        if cards.len() != 2 {
            return Err(format!("shown_cards[{s}] 须 2 张，得 {}", cards.len()));
        }
        revealed[remap(s)?] = Some([parse_card(&cards[0])?, parse_card(&cards[1])?]);
    }

    // ---- U（solver chips）= (final − start) × scale ----
    let fin_our = *seat_get(&rec.hand_result.final_stacks, our)
        .ok_or_else(|| format!("final_stacks 缺我方座 {our}"))?;
    if !fin_our.is_finite() || fin_our.fract() != 0.0 {
        return Err(format!("final_stacks[{our}] 非法筹码值 {fin_our}"));
    }
    let winnings = (fin_our as i64 - start_op[my_new] as i64) * scale as i64;

    Ok(HhConverted {
        input: MultiwayHandInput {
            config,
            our_seat: SeatId(my_new as u8),
            our_hole,
            revealed,
            board,
            actions,
            winnings,
        },
        scale,
        big_blind_op: rec.big_blind,
        n_dealt,
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

    /// U-fail 修复（2026-06-12，基线臂发现①）：all-in-for-less 大锅 + driver `stacks_start`
    /// 对短码对手错值（committed 跟踪的 all_in 缺口）→ `actions_ext.stack_before` 直读覆盖修正
    /// （首动作前投入恒 = 盲注：SB 座 990+10=1000）。**负对照**：剥掉 stack_before（模拟旧日志）
    /// → 沿用错的 stacks_start → 估计器 U 重放校验必 `Err`（= 基线 5 手 +533BB 被剔的机制）；
    /// 正路径转换 + 估计全过且 start 取直读值。两边都断言，证 override 真在修这个 bug。
    #[test]
    fn allin_for_less_stack_before_override_fixes_ufail() {
        // 真值：SB(1) 实际 1000 起手、turn all-in-for-less；BB(2,我) 2000 起手 call 后摊牌胜。
        // driver stacks_start 把 seat1 错记成 1280（all_in 无 amount 的 committed 缺口）。
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
          "actions_ext": [
            {"stack_before": 2000, "contribution_delta": 0},
            {"stack_before": 2000, "contribution_delta": 0},
            {"stack_before": 2000, "contribution_delta": 0},
            {"stack_before": 2000, "contribution_delta": 0},
            {"stack_before": 990, "contribution_delta": 10},
            {"stack_before": 1980, "contribution_delta": 0},
            {"stack_before": 980, "contribution_delta": 0},
            {"stack_before": 1980, "contribution_delta": 0},
            {"stack_before": 980, "contribution_delta": 980},
            {"stack_before": 1980, "contribution_delta": 980}
          ],
          "stacks_start": [2000, 1280, 2000, 2000, 2000, 2000],
          "committed_total": {"0": 0, "1": 1000, "2": 1000, "3": 0, "4": 0, "5": 0},
          "hand_result": {
            "winners": [{"seat": 2, "amount": 1000}],
            "pot": 2000,
            "final_stacks": {"0": 2000, "1": 0, "2": 3000, "3": 2000, "4": 2000, "5": 2000},
            "shown_cards": {"1": ["Qs", "Qd"], "2": ["Ah", "Kd"]}
          }
        }"#;
        let rec: HhRecord = serde_json::from_str(line).expect("parse");
        let conv = hh_to_multiway_input(&rec).expect("convert");
        // override 生效：seat1 = 990+10(SB)=1000、我(BB) = 1980+20=2000（×5 solver 单位）。
        assert_eq!(conv.input.config.starting_stacks[1].as_u64(), 5_000);
        assert_eq!(conv.input.config.starting_stacks[2].as_u64(), 10_000);
        assert_eq!(conv.input.winnings, 5_000, "净 +1000 op × 5");
        let est = MultiwayAivatEstimator::new(None);
        let r = est.estimate_hand(&conv.input).expect("estimate");
        assert_eq!(r.raw, 5_000.0);
        assert!(r.has_runout, "turn 锁定 → river 纯发牌段");

        // 负对照：剥 stack_before（旧日志形态）→ 退错的 stacks_start → U 重放校验必拦。
        let mut rec_old = rec.clone();
        for e in rec_old.actions_ext.iter_mut() {
            e.stack_before = None;
        }
        let conv_old = hh_to_multiway_input(&rec_old).expect("convert（错 start 在估计层才暴露）");
        assert_eq!(
            conv_old.input.config.starting_stacks[1].as_u64(),
            6_400,
            "无直读 → 沿用 driver 错值 1280×5"
        );
        let err = est
            .estimate_hand(&conv_old.input)
            .expect_err("错 start 必被 U 重放校验拦下");
        assert!(err.contains("U 校验"), "应是 U 校验失败，实得: {err}");
    }

    /// HU 短桌手（live 2026-06-11 smoke 真实手 9d818d2b 的数字）：OpenPoker HU 是环规则
    /// 推广——button(op1) 发 BB、非 button(op0) 发 SB 且先动（**非标准 HU**）。引擎 n=2 走
    /// 标准 HU（button=SB）→ 引擎 button 须设为 OpenPoker 的 SB 座对齐。修前按
    /// 「n_dealt==2：button=SB」假设，本手第 0 步就 seat mismatch（smoke 24/25 全挂）。
    #[test]
    fn hu_hand_openpoker_ring_convention_converts() {
        let line = r#"{
          "button_seat": 1, "my_seat": 1, "num_seats": 6,
          "small_blind": 10, "big_blind": 20,
          "hole": ["Ah", "Kd"],
          "board": [],
          "actions": [
            {"seat": 0, "action": "raise", "to": 50}, {"seat": 1, "action": "fold"}
          ],
          "actions_ext": [{"contribution_delta": 40}, {"contribution_delta": 0}],
          "stacks_start": null,
          "committed_total": {"0": 0, "1": 0},
          "hand_result": {
            "winners": [{"seat": 0, "amount": 20}],
            "pot": 70,
            "final_stacks": {"0": 2030, "1": 1970}
          }
        }"#;
        let rec: HhRecord = serde_json::from_str(line).expect("parse");
        let conv = hh_to_multiway_input(&rec).expect("convert");
        assert_eq!(conv.n_dealt, 2);
        // 引擎 button = OpenPoker 的 SB 座（新 id 0）：引擎标准 HU 下 button=SB 先动 ✓。
        assert_eq!(conv.input.config.button_seat, SeatId(0));
        // 回推 start：SB(op0) = 2030 − 20(净赢) = 2010；BB(op1) = 1970 + 20(盲) = 1990。
        assert_eq!(conv.input.config.starting_stacks[0].as_u64(), 10_050);
        assert_eq!(conv.input.config.starting_stacks[1].as_u64(), 9_950);
        assert_eq!(conv.input.winnings, -100, "我=BB 丢 20 op × 5");
        let est = MultiwayAivatEstimator::new(None);
        let r = est.estimate_hand(&conv.input).expect("estimate");
        assert_eq!(r.raw, -100.0);
    }

    /// HU 手打满 5 街（live 2026-06-11 smoke2 真实手 072b89a9 的数字）：OpenPoker HU 行动序
    /// 是**角色序**（preflop SB 先、postflop BB 先）→ 引擎 button=SB 座的 role-for-role 对齐
    /// 连 postflop 都成立。preflop：SB limp、BB raise 44、SB call；flop/turn/river 全 check
    /// （每街 BB=op0 先动）；摊牌 BB 的 JJ+33 胜。
    #[test]
    fn hu_postflop_role_order_converts_and_estimates() {
        let line = r#"{
          "button_seat": 0, "my_seat": 1, "num_seats": 6,
          "small_blind": 10, "big_blind": 20,
          "hole": ["Kd", "Qh"],
          "board": ["Jc", "3s", "9c", "3h", "2h"],
          "actions": [
            {"seat": 1, "action": "call"}, {"seat": 0, "action": "raise", "to": 44},
            {"seat": 1, "action": "call"}, {"seat": 0, "action": "check"},
            {"seat": 1, "action": "check"}, {"seat": 0, "action": "check"},
            {"seat": 1, "action": "check"}, {"seat": 0, "action": "check"},
            {"seat": 1, "action": "check"}
          ],
          "actions_ext": [
            {"contribution_delta": 10}, {"contribution_delta": 24}, {"contribution_delta": 24},
            {"contribution_delta": 0}, {"contribution_delta": 0}, {"contribution_delta": 0},
            {"contribution_delta": 0}, {"contribution_delta": 0}, {"contribution_delta": 0}
          ],
          "stacks_start": null,
          "committed_total": {"0": 0, "1": 0},
          "hand_result": {
            "winners": [{"seat": 0, "amount": 44}],
            "pot": 88,
            "final_stacks": {"0": 5044, "1": 1876},
            "shown_cards": {"0": ["6d", "Jd"], "1": ["Kd", "Qh"]}
          }
        }"#;
        let rec: HhRecord = serde_json::from_str(line).expect("parse");
        let conv = hh_to_multiway_input(&rec).expect("convert（修前 postflop 行动序对不上）");
        assert_eq!(conv.n_dealt, 2);
        assert_eq!(
            conv.input.config.button_seat,
            SeatId(1),
            "引擎 button = SB 座"
        );
        assert_eq!(conv.input.winnings, -220, "我=SB 丢 44 op × 5");
        let est = MultiwayAivatEstimator::new(None);
        let r = est.estimate_hand(&conv.input).expect("estimate");
        assert_eq!(r.raw, -220.0);
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

    /// 无 stacks_start（如 BB walk 手没有 your_turn）→ net 约定回推（live 校准：amount=净赢）：
    /// button=0：UTG..SB 全 fold，我=BB 净收 SB 的 10（amount=10；committed_total 旧日志回退路）。
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
            "winners": [{"seat": 2, "amount": 10}],
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

    /// 短桌手（live 实测 2026-06-11：6 座桌只发 4 家，`final_stacks` 只含被发牌座；盲注跳过
    /// 空座 → driver 满桌 seeding 的 stacks_start 必错、必须忽略）：按 final_stacks 键集重映射
    /// 成 4 座 ring + actions_ext delta 重建投入 + net 结算约定回推。形状取自真实第 6 手：
    /// button=3、在场 {0,1,3,4} → SB=原4(我)、BB=原0、preflop 首动=原1；原1 raise to 60
    /// 全 fold 收盲（net amount=30，uncalled 40 隐式返还）。
    #[test]
    fn short_handed_hand_remaps_to_dealt_ring() {
        let line = r#"{
          "button_seat": 3, "my_seat": 4, "num_seats": 6,
          "small_blind": 10, "big_blind": 20,
          "hole": ["Ah", "Kd"],
          "board": [],
          "actions": [
            {"seat": 1, "action": "raise", "to": 60}, {"seat": 3, "action": "fold"},
            {"seat": 4, "action": "fold"}, {"seat": 0, "action": "fold"}
          ],
          "actions_ext": [{"contribution_delta": 60}, {"contribution_delta": 0},
                          {"contribution_delta": 0}, {"contribution_delta": 0}],
          "stacks_start": [8737, 5000, 1500, 1750, 2100, 1820],
          "committed_total": {"0": 0, "1": 60, "2": 0, "3": 0, "4": 10, "5": 20},
          "hand_result": {
            "winners": [{"seat": 1, "amount": 30}],
            "pot": 30,
            "final_stacks": {"0": 8737, "1": 5030, "3": 1750, "4": 2090},
            "shown_cards": null
          }
        }"#;
        let rec: HhRecord = serde_json::from_str(line).expect("parse");
        let conv = hh_to_multiway_input(&rec).expect("convert");
        assert_eq!(conv.n_dealt, 4);
        assert_eq!(conv.input.config.n_seats, 4);
        assert_eq!(conv.input.config.button_seat, SeatId(2), "原3 → 新2");
        // net 约定重建（OP→×5）：赢家原1 = 5030−30；原0(真 BB，driver 误标原5) = 8737+20；
        // 原3 = 1750+0；原4(SB,我) = 2090+10。注意 stacks_start 里 driver 给原0 的 8737 是错的。
        let st: Vec<u64> = conv
            .input
            .config
            .starting_stacks
            .iter()
            .map(|c| c.as_u64())
            .collect();
        assert_eq!(st, vec![8757 * 5, 5000 * 5, 1750 * 5, 2100 * 5]);
        assert_eq!(conv.input.our_seat, SeatId(3), "原4 → 新3");
        assert_eq!(conv.input.winnings, -50, "SB fold 净 −10 op × 5");
        let est = MultiwayAivatEstimator::new(None);
        let r = est.estimate_hand(&conv.input).expect("estimate");
        assert_eq!(r.raw, -50.0);
        assert!(!r.has_runout);
    }

    /// 服务端发**显式 null**（live 实测 2026-06-11 首手：非摊牌 `shown_cards: null`，
    /// winners 条目还带 `name`/`hand_description: null`）→ 必须照吃，不许 parse Err。
    #[test]
    fn explicit_null_fields_tolerated() {
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
          "names": null,
          "stacks_start": [2000, 2000, 2000, 2000, 2000, 2000],
          "committed_total": null,
          "hand_result": {
            "winners": [{"seat": 2, "amount": 10, "name": "jesse_xu", "hand_description": null}],
            "pot": 30,
            "final_stacks": {"0": 2000, "1": 1990, "2": 2010, "3": 2000, "4": 2000, "5": 2000},
            "shown_cards": null
          }
        }"#;
        let rec: HhRecord = serde_json::from_str(line).expect("显式 null 应照吃");
        let conv = hh_to_multiway_input(&rec).expect("convert");
        assert_eq!(conv.input.winnings, 50);
        let est = MultiwayAivatEstimator::new(None);
        assert!(est.estimate_hand(&conv.input).is_ok());
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
