//! 请求/响应 serde 类型 + 卡牌解析 + [`GameView`] 下注状态重放。
//!
//! OpenPoker 只推「我方决策点」的扁平 `actions[]`（无街标签）。本模块把它重放成完整下注
//! 状态：逐街分段（检测下注轮关闭）、每座 committed、folded/allin、high_bet、pot、to_call、
//! 主动权、按钮相对位置。**永不 panic**：异常输入退化为「尽量合理」的状态，由 advisor 兜底。

use crate::core::Street;
use crate::core::{Card, Rank, Suit};
use crate::heuristic::range;
use serde::{Deserialize, Serialize};

// ===========================================================================
// IPC 类型（与 openpoker_advisor::Request/Response 协议兼容）
// ===========================================================================

/// 一个历史动作（OpenPoker 单位）。`to` = raise/bet 的本轮投入绝对总额。
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct HistAction {
    pub seat: u8,
    pub action: String,
    #[serde(default)]
    pub to: Option<u64>,
    /// 该动作所属街（0=preflop..3=river）。driver 权威；**全部动作都带**时按标签分段
    /// （跳过启发式下注轮关闭检测）；任一缺省则整手退化到启发式分段。
    #[serde(default)]
    pub street: Option<u8>,
}

/// 我方决策点的合法区间（OpenPoker 单位，权威）。
#[derive(Deserialize, Serialize, Debug, Clone, Default)]
pub struct ValidActions {
    #[serde(default)]
    pub can_check: bool,
    #[serde(default)]
    pub can_call: bool,
    #[serde(default)]
    pub can_raise: bool,
    #[serde(default)]
    pub min_raise: Option<u64>,
    #[serde(default)]
    pub max_raise: Option<u64>,
}

/// advisor 请求（driver 每决策点发一行）。
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct Request {
    pub hole: Vec<String>,
    #[serde(default)]
    pub board: Vec<String>,
    pub button_seat: u8,
    pub my_seat: u8,
    pub num_seats: u8,
    pub small_blind: u64,
    pub big_blind: u64,
    #[serde(default)]
    pub actions: Vec<HistAction>,
    pub valid: ValidActions,
    /// 各座 hand-start 真栈（OpenPoker 单位）。空 = 未知 → 默认 100bb。
    #[serde(default)]
    pub stacks: Vec<u64>,
    /// 各座玩家名（剥削画像用，可空）。
    #[serde(default)]
    pub names: std::collections::BTreeMap<u8, String>,
    /// 权威小盲座位（driver 紧凑指派；缺省退化到物理 btn+1 / HU btn）。
    #[serde(default)]
    pub sb_seat: Option<u8>,
    /// 权威大盲座位（driver 紧凑指派；缺省退化到物理 btn+2 / HU btn+1）。
    #[serde(default)]
    pub bb_seat: Option<u8>,
}

/// advisor → driver 响应。`amount` 仅 raise 携带（OpenPoker 单位 raise-to）。
#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq)]
pub struct Response {
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount: Option<u64>,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub street: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub equity: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub probs: Option<Vec<(String, f64)>>,
    // --- 纯日志度量（绝不参与决策；analyzer 用来按真实价位判过松/过紧）---
    /// 决策点底池（chips，含全部已投入）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pot: Option<u64>,
    /// 我方需跟注额（chips；不面对下注时 0）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_call: Option<u64>,
    /// 底池赔率 break-even equity = to_call/(pot+to_call)（不面对下注时 0）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_eq: Option<f64>,
    /// 仍在池中的对手数（不含我）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opps_in_pot: Option<u32>,
    /// Phase A 决策遥测（节点路径 + 场景 key + 驱动信号，只写不读；
    /// `docs/scenario_heuristic_design.md` §6）。旧日志无此字段 → serde default None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace: Option<crate::heuristic::trace::DecisionTrace>,
}

// ===========================================================================
// 卡牌解析
// ===========================================================================

/// 解析 OpenPoker 卡牌串（如 `"Td"` / `"As"` / `"2c"`）。失败返回 `None`。
pub fn parse_card(s: &str) -> Option<Card> {
    let s = s.trim();
    let mut it = s.chars();
    let r = it.next()?;
    let su = it.next()?;
    if it.next().is_some() {
        return None;
    }
    let rank = match r.to_ascii_uppercase() {
        '2' => Rank::Two,
        '3' => Rank::Three,
        '4' => Rank::Four,
        '5' => Rank::Five,
        '6' => Rank::Six,
        '7' => Rank::Seven,
        '8' => Rank::Eight,
        '9' => Rank::Nine,
        'T' => Rank::Ten,
        'J' => Rank::Jack,
        'Q' => Rank::Queen,
        'K' => Rank::King,
        'A' => Rank::Ace,
        _ => return None,
    };
    let suit = match su.to_ascii_lowercase() {
        'c' => Suit::Clubs,
        'd' => Suit::Diamonds,
        'h' => Suit::Hearts,
        's' => Suit::Spades,
        _ => return None,
    };
    Some(Card::new(rank, suit))
}

fn parse_cards(v: &[String]) -> Vec<Card> {
    v.iter().filter_map(|s| parse_card(s)).collect()
}

/// `Card` → range 侧卡串（rank ∈ "23456789TJQKA"，suit ∈ "cdhs"）——[`range::card_id`]
/// 的逆。用 `to_u8() = rank*4 + suit` 拆分，保证 `card_id(card_to_string(c)) == c.to_u8()`。
fn card_to_string(c: Card) -> String {
    const RANKS: &[u8; 13] = b"23456789TJQKA";
    const SUITS: &[u8; 4] = b"cdhs";
    let v = c.to_u8();
    let r = RANKS[(v / 4) as usize] as char;
    let s = SUITS[(v % 4) as usize] as char;
    format!("{r}{s}")
}

// ===========================================================================
// 位置桶（座位无关）
// ===========================================================================

/// 翻前位置桶——用「相对按钮的行动序」泛化，座位无关。
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PosBucket {
    Early,
    Mid,
    Late,
    SmallBlind,
    BigBlind,
}

// ===========================================================================
// GameView
// ===========================================================================

/// P0-1：单座逐动作记录（时序，带权威街标）。行动线记忆的原子单位。
#[derive(Clone, Debug)]
pub struct SeatAction {
    /// 动作所属街（0=preflop..3=river）。
    pub street: u8,
    /// 原始动作字符串（未小写化，保留 driver 原样）。
    pub action: String,
    /// raise/bet 的本轮投入绝对总额（其余动作 None）。
    pub to: Option<u64>,
}

/// 单座视角的重建下注状态。所有筹码量 = OpenPoker 单位（chips）；用 `*_bb()` 取 bb 标度。
#[derive(Clone, Debug)]
pub struct GameView {
    pub num_seats: u8,
    pub my_seat: u8,
    pub button_seat: u8,
    /// 解析后的小盲座（driver 权威优先；否则物理 btn+1 / HU btn）。位置桶按它分类。
    pub sb_seat: u8,
    /// 解析后的大盲座（driver 权威优先；否则物理 btn+2 / HU btn+1）。
    pub bb_seat: u8,
    pub small_blind: u64,
    pub big_blind: u64,
    pub street: Street,
    pub hole: Option<[Card; 2]>,
    pub board: Vec<Card>,

    /// 每座状态（下标 = 座位号）。
    pub folded: Vec<bool>,
    pub allin: Vec<bool>,
    /// 本手累计投入（含本街）。
    pub committed_total: Vec<u64>,
    /// 本街投入。
    pub committed_round: Vec<u64>,
    /// hand-start 栈（未知则默认 100bb 的 chips）。
    pub start_stack: Vec<u64>,

    /// 本街最高投入。
    pub high_bet: u64,
    /// 本街最后加注者（主动权）。
    pub street_aggressor: Option<u8>,
    /// 翻前是否有人加注（false = limp 底池）。
    pub preflop_raised: bool,
    /// 翻前最后加注者。
    pub preflop_aggressor: Option<u8>,
    /// 翻前 limper 数（开池前 just-call bb 的非盲位）。
    pub preflop_limpers: u32,
    /// 翻前加注次数（开池=1，3bet=2，4bet=3…）。
    pub preflop_num_raises: u32,

    pub valid: ValidActions,
    /// 座位 → 玩家名（剥削画像用）。
    pub names: std::collections::BTreeMap<u8, String>,

    /// P0-1：每座本手全部动作（下标 = 座位号，len = num_seats）。内层按时序，带权威街标。
    /// 纯增记忆，默认决策不读它 → byte-equal。
    pub seat_actions: Vec<Vec<SeatAction>>,
    /// P0-1：逐街最后加注者（0=preflop..3=river）。重放本就算了 `aggressor_by_street`，
    /// 但历史只导出 preflop + 当前街；这里补全被丢弃的 flop/turn/river aggressor。
    pub street_aggressors: [Option<u8>; 4],

    /// 在线 range 适配器的因果事件流（全手行动线，按重放序）。每个动作一条，字段口径
    /// 与离线 `tools/range_dataset_build.py` `finalize_live_hand` 逐字段一致（防训练/服务
    /// 特征漂移；见 [`GameView::to_range_sample`] 与 `tests/heuristic_gameview_sample.rs`）。
    /// 盲注是种子承诺（不发事件），与离线一致。
    pub events: Vec<range::Event>,
}

impl GameView {
    /// 默认 100bb 折算成 chips（栈未知时用）。
    fn default_stack_chips(big_blind: u64) -> u64 {
        big_blind.saturating_mul(100).max(1)
    }

    /// 从请求重放出下注状态。容错：解析异常时尽量返回合理近似（never panic）。
    pub fn from_request(req: &Request) -> GameView {
        let n = req.num_seats.max(2) as usize;
        let bb = req.big_blind.max(1);
        let sb = req.small_blind.max(1);
        let board = parse_cards(&req.board);
        let hole = {
            let hc = parse_cards(&req.hole);
            if hc.len() == 2 {
                Some([hc[0], hc[1]])
            } else {
                None
            }
        };

        let mut start_stack = vec![Self::default_stack_chips(bb); n];
        for (i, s) in req.stacks.iter().enumerate() {
            if i < n && *s > 0 {
                start_stack[i] = *s;
            }
        }

        // 盲注座位：driver 权威优先（紧凑指派，正确处理稀疏占座 / 服务端 HU 无特例）；
        // 缺省退化到物理座（HU 特例 button=SB，多人 btn+1/btn+2）。
        let btn = (req.button_seat as usize) % n;
        let (sb_seat, bb_seat) = match (req.sb_seat, req.bb_seat) {
            (Some(sbs), Some(bbs)) => ((sbs as usize) % n, (bbs as usize) % n),
            _ if n == 2 => (btn, (btn + 1) % n),
            _ => ((btn + 1) % n, (btn + 2) % n),
        };

        let mut folded = vec![false; n];
        let mut allin = vec![false; n];
        let mut committed_total = vec![0u64; n];
        let mut committed_round = vec![0u64; n];
        let mut has_acted = vec![false; n];
        // P0-1：逐座动作线累加器（纯增，不参与任何既有计算/分段/决策）。
        let mut seat_actions: Vec<Vec<SeatAction>> = vec![Vec::new(); n];
        // 在线 range 适配器的因果事件流累加器（纯增；口径见 range_dataset_build.finalize_live_hand）。
        let mut events: Vec<range::Event> = Vec::new();

        // 发盲注。
        let post =
            |seat: usize, amt: u64, ct: &mut [u64], cr: &mut [u64], ss: &[u64], al: &mut [bool]| {
                let pay = amt.min(ss[seat]);
                ct[seat] += pay;
                cr[seat] += pay;
                if pay >= ss[seat] {
                    al[seat] = true;
                }
            };
        post(
            sb_seat,
            sb,
            &mut committed_total,
            &mut committed_round,
            &start_stack,
            &mut allin,
        );
        post(
            bb_seat,
            bb,
            &mut committed_total,
            &mut committed_round,
            &start_stack,
            &mut allin,
        );
        let mut high_bet = committed_round.iter().copied().max().unwrap_or(bb);
        let mut cur_aggressor: Option<u8> = None;

        let mut street_idx: u8 = 0;
        let mut aggressor_by_street: [Option<u8>; 4] = [None; 4];
        let mut limpers_preflop: u32 = 0;
        let mut preflop_raises: u32 = 0;
        let mut pending_advance = false;

        // 街分段模式：当 driver 给**每个**动作都打了街标签时，按标签权威推进，跳过启发式
        // 下注轮关闭检测——这样幻影空座/不规则序列不再扰乱分段。任一动作缺标签则整手退化到
        // 启发式分段（与历史完全一致）。空 actions 走启发式路径（对空序两者等价）。
        let use_street_labels =
            !req.actions.is_empty() && req.actions.iter().all(|a| a.street.is_some());

        // 下注轮关闭检测：所有 live & 非 all-in & 有栈 的座都已行动且追平 high_bet。
        let round_closed = |folded: &[bool],
                            allin: &[bool],
                            has_acted: &[bool],
                            committed_round: &[u64],
                            start_stack: &[u64],
                            high_bet: u64|
         -> bool {
            let mut live_actors = 0;
            for s in 0..n {
                if folded[s] || allin[s] {
                    continue;
                }
                if start_stack[s] == 0 {
                    continue;
                }
                live_actors += 1;
                if !has_acted[s] || committed_round[s] != high_bet {
                    return false;
                }
            }
            live_actors >= 1
        };

        let advance_street =
            |street_idx: &mut u8,
             committed_round: &mut [u64],
             has_acted: &mut [bool],
             high_bet: &mut u64,
             cur_aggressor: &mut Option<u8>,
             aggressor_by_street: &mut [Option<u8>; 4]| {
                let idx = *street_idx as usize;
                if idx < 4 {
                    aggressor_by_street[idx] = *cur_aggressor;
                }
                for s in 0..n {
                    committed_round[s] = 0;
                    has_acted[s] = false;
                }
                *high_bet = 0;
                *cur_aggressor = None;
                *street_idx = street_idx.saturating_add(1);
            };

        for a in &req.actions {
            if use_street_labels {
                // 权威街标签：推进到该动作所属街（跳过启发式关闭检测，幻影空座不再扰动分段）。
                let want = a.street.unwrap_or(0).min(3);
                while street_idx < want {
                    advance_street(
                        &mut street_idx,
                        &mut committed_round,
                        &mut has_acted,
                        &mut high_bet,
                        &mut cur_aggressor,
                        &mut aggressor_by_street,
                    );
                }
            } else if pending_advance {
                // 启发式：上一个动作触发了关闭 → 本动作属于下一街，先推进。
                advance_street(
                    &mut street_idx,
                    &mut committed_round,
                    &mut has_acted,
                    &mut high_bet,
                    &mut cur_aggressor,
                    &mut aggressor_by_street,
                );
                pending_advance = false;
            }

            let s = (a.seat as usize) % n;
            let act = a.action.to_ascii_lowercase();
            // --- range 事件：捕获动作**前**的本街上下文（口径 = finalize_live_hand）---
            // st = 本动作所属街（重放当前街）；hb = 动作前本街最高投入；
            // tc = 动作前需跟额 = max(0, hb - 本座本街已投)。
            let ev_st = (street_idx as i64).min(3);
            let ev_hb = high_bet;
            let ev_commit_before = committed_round[s];
            let ev_tc = ev_hb.saturating_sub(ev_commit_before);
            match act.as_str() {
                "fold" => {
                    folded[s] = true;
                    has_acted[s] = true;
                }
                "check" => {
                    has_acted[s] = true;
                }
                "call" => {
                    let need = high_bet.saturating_sub(committed_round[s]);
                    let avail = start_stack[s].saturating_sub(committed_total[s]);
                    let pay = need.min(avail);
                    committed_round[s] += pay;
                    committed_total[s] += pay;
                    if pay >= avail {
                        allin[s] = true;
                    }
                    has_acted[s] = true;
                    // limp 统计：翻前、无人加注、刚好补平 bb、非盲位。
                    if street_idx == 0
                        && cur_aggressor.is_none()
                        && committed_round[s] == bb
                        && s != sb_seat
                        && s != bb_seat
                    {
                        limpers_preflop += 1;
                    }
                }
                "bet" | "raise" | "allin" | "all_in" | "all-in" => {
                    let target = match a.to {
                        Some(t) => t,
                        None => {
                            // 无 to：all-in 推全栈，其余退化成至少补平。
                            if act.starts_with("all") {
                                committed_total[s].max(committed_round[s])
                                    + start_stack[s].saturating_sub(committed_total[s])
                            } else {
                                high_bet.max(bb)
                            }
                        }
                    };
                    // raise-to 自纠：a.to 是 server 权威 raise-to。若它超过本座**记录**起始栈，
                    // 就是 start 被低估的铁证（没人能 raise-to 超过自己起始栈）→ 把有效起始栈抬到
                    // 至少 target，而非把加注夹小到错误小栈。只信权威 a.to（None-fallback 的 target
                    // 是猜测，不抬栈，仍按总栈夹）。安全、never panic；sim 送正确栈 → 此分支不触发
                    // → 重建零漂移。
                    if let Some(t) = a.to {
                        if t > start_stack[s] {
                            start_stack[s] = t;
                        }
                    }
                    let avail_total = start_stack[s];
                    let target = target.min(avail_total); // 已自纠则 no-op；否则仍夹总栈
                    let delta = target.saturating_sub(committed_round[s]);
                    let already_in = committed_total[s];
                    let room = avail_total.saturating_sub(already_in);
                    let pay = delta.min(room);
                    committed_round[s] += pay;
                    committed_total[s] += pay;
                    if committed_total[s] >= avail_total {
                        allin[s] = true;
                    }
                    if committed_round[s] > high_bet {
                        // 真加注：重开行动。
                        high_bet = committed_round[s];
                        cur_aggressor = Some(s as u8);
                        if street_idx == 0 {
                            preflop_raises += 1;
                        }
                        for t in 0..n {
                            if t != s && !folded[t] && !allin[t] {
                                has_acted[t] = false;
                            }
                        }
                    }
                    has_acted[s] = true;
                }
                _ => {
                    // 未知动作（post/blind/ante/…）：忽略，不影响重放。
                    has_acted[s] = true;
                }
            }

            // P0-1：记录本动作到该座动作线。街标沿用既有推导（权威 a.street 优先，缺省用重放
            // 当前街 street_idx）——只读，不改任何既有变量/分段/决策。
            let act_street = a.street.map(|st| st.min(3)).unwrap_or(street_idx.min(3));
            seat_actions[s].push(SeatAction {
                street: act_street,
                action: a.action.clone(),
                to: a.to,
            });

            // --- range 事件：动作**后**的承诺/底池（口径 = finalize_live_hand）---
            // c = 本座本街累计投入（动作后）；p = 全局底池（动作后，含盲注种子，随 delta 单调增）；
            // delta = c - commit_before 由 tokenizer/model 侧从 c 反推，无需显存。
            // a 归一化到 AGGR 集合识别的口径（"all-in" → "all_in"）。
            let ev_a = if act == "all-in" {
                "all_in".to_string()
            } else {
                act.clone()
            };
            events.push(range::Event {
                st: ev_st,
                s: s as i64,
                a: ev_a,
                c: committed_round[s] as i64,
                p: committed_total.iter().sum::<u64>() as i64,
                tc: ev_tc as i64,
                hb: ev_hb as i64,
            });

            if !use_street_labels
                && round_closed(
                    &folded,
                    &allin,
                    &has_acted,
                    &committed_round,
                    &start_stack,
                    high_bet,
                )
            {
                pending_advance = true;
            }
        }
        // 收尾 pending（最后一个动作恰好关闭本街 → 我方在下一街首先行动）。
        if pending_advance {
            advance_street(
                &mut street_idx,
                &mut committed_round,
                &mut has_acted,
                &mut high_bet,
                &mut cur_aggressor,
                &mut aggressor_by_street,
            );
        }

        // 用 board 长度校准街（重放漂移时以 board 为准；强制对齐到当前街）。
        let target_street = match board.len() {
            0 => 0u8,
            3 => 1,
            4 => 2,
            5 => 3,
            _ => street_idx, // 非常规张数，信重放
        };
        while street_idx < target_street {
            advance_street(
                &mut street_idx,
                &mut committed_round,
                &mut has_acted,
                &mut high_bet,
                &mut cur_aggressor,
                &mut aggressor_by_street,
            );
        }

        let street = match target_street {
            0 => Street::Preflop,
            1 => Street::Flop,
            2 => Street::Turn,
            _ => Street::River,
        };

        // 仍在翻前时 aggressor 还在 cur_aggressor（未存进 by_street）。
        let preflop_aggressor = if target_street == 0 {
            cur_aggressor
        } else {
            aggressor_by_street[0]
        };
        let preflop_raised = preflop_aggressor.is_some();

        // P0-1：暴露被丢弃的逐街 aggressor。已完成街在 aggressor_by_street 里；当前街的 aggressor
        // 仍留在 cur_aggressor（尚未 flush，与上面 preflop_aggressor 的当前街特例同理），补进当前槽。
        let mut street_aggressors = aggressor_by_street;
        {
            let cur = (street_idx as usize).min(3);
            if street_aggressors[cur].is_none() {
                street_aggressors[cur] = cur_aggressor;
            }
        }

        GameView {
            num_seats: n as u8,
            my_seat: (req.my_seat as usize % n) as u8,
            button_seat: btn as u8,
            sb_seat: sb_seat as u8,
            bb_seat: bb_seat as u8,
            small_blind: sb,
            big_blind: bb,
            street,
            hole,
            board,
            folded,
            allin,
            committed_total,
            committed_round,
            start_stack,
            high_bet,
            street_aggressor: cur_aggressor,
            preflop_raised,
            preflop_aggressor,
            preflop_limpers: limpers_preflop,
            preflop_num_raises: preflop_raises,
            valid: req.valid.clone(),
            names: req.names.clone(),
            seat_actions,
            street_aggressors,
            events,
        }
    }

    // --- 派生量 ---

    #[inline]
    pub fn pot_chips(&self) -> u64 {
        self.committed_total.iter().sum()
    }

    #[inline]
    pub fn pot_bb(&self) -> f64 {
        self.pot_chips() as f64 / self.big_blind as f64
    }

    /// 我方需跟注额（chips）。
    #[inline]
    pub fn to_call_chips(&self) -> u64 {
        let me = self.my_seat as usize;
        self.high_bet.saturating_sub(self.committed_round[me])
    }

    #[inline]
    pub fn to_call_bb(&self) -> f64 {
        self.to_call_chips() as f64 / self.big_blind as f64
    }

    /// 我方当前剩余栈（chips）。
    #[inline]
    pub fn my_stack_chips(&self) -> u64 {
        let me = self.my_seat as usize;
        self.start_stack[me].saturating_sub(self.committed_total[me])
    }

    #[inline]
    pub fn my_stack_bb(&self) -> f64 {
        self.my_stack_chips() as f64 / self.big_blind as f64
    }

    /// 未弃牌人数（含我）。
    pub fn num_active(&self) -> u32 {
        self.folded.iter().filter(|f| !**f).count() as u32
    }

    /// 未弃牌对手数（不含我）。
    pub fn num_opponents(&self) -> u32 {
        self.num_active().saturating_sub(1)
    }

    /// 仍可行动（未弃未 all-in）的对手数。
    pub fn num_acting_opponents(&self) -> u32 {
        (0..self.num_seats as usize)
            .filter(|&s| s != self.my_seat as usize && !self.folded[s] && !self.allin[s])
            .count() as u32
    }

    /// 有效栈（chips）= min(我的栈, 仍在场对手里最大的栈)。
    /// 取最大对手栈是因为「有效可博弈深度」由最深的能跟到底的对手决定（多人池近似）。
    pub fn effective_stack_chips(&self) -> u64 {
        let me = self.my_seat as usize;
        let my_stack = self.my_stack_chips();
        let mut max_opp = 0u64;
        for s in 0..self.num_seats as usize {
            if s == me || self.folded[s] {
                continue;
            }
            let opp_stack = self.start_stack[s].saturating_sub(self.committed_total[s]);
            max_opp = max_opp.max(opp_stack);
        }
        if max_opp == 0 {
            my_stack
        } else {
            my_stack.min(max_opp)
        }
    }

    /// SPR = 有效栈 / 底池（决策点之后的承诺规模）。
    pub fn spr(&self) -> f64 {
        let pot = self.pot_chips().max(1);
        self.effective_stack_chips() as f64 / pot as f64
    }

    /// 底池赔率所需 equity = to_call / (pot + to_call)。无需跟注时 0。
    pub fn pot_odds(&self) -> f64 {
        let call = self.to_call_chips();
        if call == 0 {
            return 0.0;
        }
        let pot = self.pot_chips();
        call as f64 / (pot + call) as f64
    }

    /// 我是否本街/本手主动权方（翻前加注者或本街最后加注者）。
    pub fn i_have_initiative(&self) -> bool {
        let me = Some(self.my_seat);
        self.street_aggressor == me
            || (self.street_aggressor.is_none() && self.preflop_aggressor == me)
    }

    pub fn i_am_preflop_aggressor(&self) -> bool {
        self.preflop_aggressor == Some(self.my_seat)
    }

    /// 我相对按钮的行动序 index（按钮=0, SB=1, BB=2, ...）。
    pub fn my_button_index(&self) -> u8 {
        let n = self.num_seats;
        ((self.my_seat as i32 - self.button_seat as i32).rem_euclid(n as i32)) as u8
    }

    /// 翻前位置桶（座位无关）。盲位按**解析后的** sb_seat/bb_seat 分类（driver 权威优先，
    /// 正确处理稀疏占座 / 服务端 HU 无特例）；非盲位按相对按钮行动序分 Late/Mid/Early。
    /// 物理回退时 sb_seat=btn+1、bb_seat=btn+2（HU=btn/btn+1）→ 与历史 button-index 判定等价。
    pub fn my_pos_bucket(&self) -> PosBucket {
        let n = self.num_seats;
        let me = self.my_seat;
        if me == self.sb_seat {
            return PosBucket::SmallBlind;
        }
        if me == self.bb_seat {
            return PosBucket::BigBlind;
        }
        // 非盲位：按相对按钮行动序。idx==0(BTN) 或 idx∈[3, n-1]。
        let idx = self.my_button_index();
        if idx == 0 || idx == n - 1 {
            PosBucket::Late
        } else if idx >= n.saturating_sub(3) {
            PosBucket::Mid
        } else {
            PosBucket::Early
        }
    }

    /// 我是否 in-position（按钮相对仍在场对手——行动序在最后）。粗判：我的 button_index
    /// 比所有仍在场对手都大（更靠近按钮后）。
    pub fn in_position(&self) -> bool {
        let me_idx = self.my_button_index();
        let n = self.num_seats as usize;
        for s in 0..n {
            if s == self.my_seat as usize || self.folded[s] {
                continue;
            }
            let idx = ((s as i32 - self.button_seat as i32).rem_euclid(n as i32)) as u8;
            // post-flop 行动序：SB 先（idx 1）... button 最后（idx 0 视作最大）。
            let my_rank = if me_idx == 0 { n as u8 } else { me_idx };
            let opp_rank = if idx == 0 { n as u8 } else { idx };
            if opp_rank > my_rank {
                return false;
            }
        }
        true
    }

    /// 翻前面对的加注 raise-to（chips）= high_bet（若有人加注），否则 0（first-in / limp）。
    pub fn facing_raise_chips(&self) -> u64 {
        if self.preflop_raised && self.street == Street::Preflop {
            self.high_bet
        } else {
            0
        }
    }

    // --- P0-1：行动线记忆只读访问器（不参与决策）---

    /// 某座本手全部动作（时序，带权威街标）。越界返回空切片。
    pub fn seat_actions(&self, seat: usize) -> &[SeatAction] {
        self.seat_actions
            .get(seat)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// 指定街（0=preflop..3=river）的最后加注者。越界或无人加注返回 None。
    pub fn aggressor_on_street(&self, street: u8) -> Option<u8> {
        self.street_aggressors
            .get(street as usize)
            .copied()
            .flatten()
    }

    /// 某座在指定街的动作（时序过滤）。
    pub fn seat_street_actions(
        &self,
        seat: usize,
        street: u8,
    ) -> impl Iterator<Item = &SeatAction> {
        self.seat_actions(seat)
            .iter()
            .filter(move |sa| sa.street == street)
    }

    // --- P0-1：行动线派生量（纯只读；decide 不读，可进 trace 场景维度）---

    /// Current street index 0..3 (preflop..river; Showdown folds into river).
    fn street_index(&self) -> u8 {
        match self.street {
            Street::Preflop => 0,
            Street::Flop => 1,
            Street::Turn => 2,
            _ => 3,
        }
    }

    /// True when `seat`'s postflop betting line marks their range as "capped": the seat
    /// has taken at least one passive flop-or-later action (check/call) and **no**
    /// postflop aggressive action (bet/raise/all-in) this hand. Passive-only lines
    /// rarely contain the strongest holdings (those tend to bet or raise), so the top
    /// of the range is capped. A seat with no postflop check/call yet is NOT capped
    /// (unknown ≠ passive); fold and unrecognized actions neither cap nor uncap.
    pub fn villain_capped(&self, seat: usize) -> bool {
        let mut acted_passive_postflop = false;
        for sa in self.seat_actions(seat) {
            if sa.street == 0 {
                continue;
            }
            match sa.action.to_ascii_lowercase().as_str() {
                "check" | "call" => acted_passive_postflop = true,
                "bet" | "raise" | "allin" | "all_in" | "all-in" => return false,
                _ => {}
            }
        }
        acted_passive_postflop
    }

    /// True when **every** remaining opponent (not folded, hero excluded) is capped per
    /// [`GameView::villain_capped`]. False preflop (no postflop line yet) and false when
    /// there is no remaining opponent (no vacuous truth).
    pub fn villains_capped(&self) -> bool {
        let mut any = false;
        for s in 0..self.num_seats as usize {
            if s == self.my_seat as usize || self.folded[s] {
                continue;
            }
            any = true;
            if !self.villain_capped(s) {
                return false;
            }
        }
        any
    }

    /// Consecutive-street barrel count hero is currently facing: the number of
    /// consecutive postflop streets, ending at the current street, on which the **same
    /// opponent** has been the last aggressor. 0 when preflop, when the current street
    /// has no aggressor, or when the current-street aggressor is hero. Example: villain
    /// bets flop and turn → while hero faces the turn bet this returns 2 (double barrel).
    pub fn facing_barrel_n(&self) -> u8 {
        let cur = self.street_index() as usize;
        if cur == 0 {
            return 0;
        }
        let aggr = match self.street_aggressors[cur] {
            Some(s) if s != self.my_seat => s,
            _ => return 0,
        };
        let mut n = 0u8;
        let mut k = cur;
        while k >= 1 && self.street_aggressors[k] == Some(aggr) {
            n += 1;
            k -= 1;
        }
        n
    }

    /// True when hero currently faces a delayed c-bet: the street is the turn, the flop
    /// checked through (no flop aggressor), and the preflop aggressor — an opponent —
    /// is the turn aggressor (they skipped the flop c-bet and bet the turn instead).
    pub fn is_delayed_cbet(&self) -> bool {
        self.street == Street::Turn
            && self.street_aggressors[1].is_none()
            && self.street_aggressors[2].is_some()
            && self.street_aggressors[2] == self.preflop_aggressor
            && self.preflop_aggressor != Some(self.my_seat)
    }

    /// 为在此决策点预测 `opp_seat` 的 range 构造 [`range::Sample`]。
    ///
    /// 桥接离线已落地（golden 对拍通过）的 range 流水线（tokenizer → range → mlrange）到
    /// **在线** live 决策点：`ev` 用重放期记录的全手因果事件流（口径逐字段对齐
    /// `tools/range_dataset_build.py` `finalize_live_hand`，防训练/服务特征漂移），其余
    /// 上下文（盲注/座位/board/栈/画像）从 `GameView` 直取。`tseat` 只切换「目标座位」，
    /// `ev` 在所有目标间共享。
    ///
    /// - `prof`：该对手的画像快照（life/w50/n_shows）；`None` → 默认空画像
    ///   （range_threshold_shift=0，prof_features 取缺省）。从引擎 OpponentTable 接线是后续项。
    /// - `stacks`：**盲注已扣**的座位栈（= hand-start 栈 − 该座盲注种子），与离线 `stacks`
    ///   同口径——tokenizer 用 `stack_of + seed_commit` 加回盲注还原 hand-start 栈，故此处必须
    ///   预扣，否则盲位栈特征会多算一个盲注。
    /// - `hseat`/`hhole`：hero 座位/底牌（离线 card-removal 仅在 `hseat != tseat` 时把 hero
    ///   底牌当死牌——门控在离线 `causal_logpriors` 内，这里如实填 hero 自身）。
    /// - `ns`/`inh`：在手座位数/列表（= 有过动作 ∪ 盲位 ∪ hero），与 featurizer `len(in_hand)` 同口径。
    pub fn to_range_sample(&self, opp_seat: usize, prof: Option<range::Prof>) -> range::Sample {
        let n = self.num_seats as usize;
        // 该座的盲注种子承诺（用于把 hand-start 栈折成「盲注已扣」栈）。
        let seed_commit_of = |seat: usize| -> u64 {
            if seat == self.sb_seat as usize {
                self.small_blind
            } else if seat == self.bb_seat as usize {
                self.big_blind
            } else {
                0
            }
        };
        // stacks：盲注已扣（离线口径）；键 = 座位十进制字符串。
        let mut stacks = std::collections::HashMap::with_capacity(n);
        for seat in 0..n {
            let post_blind = self.start_stack[seat].saturating_sub(seed_commit_of(seat));
            stacks.insert(seat.to_string(), post_blind as i64);
        }
        // 在手座位：有过动作 ∪ 盲位 ∪ hero（离线 in_hand = 发牌座 ∪ 出现在动作里的座）。
        let mut inh: Vec<i64> = (0..n)
            .filter(|&seat| {
                !self.seat_actions[seat].is_empty()
                    || seat == self.sb_seat as usize
                    || seat == self.bb_seat as usize
                    || seat == self.my_seat as usize
            })
            .map(|seat| seat as i64)
            .collect();
        inh.sort_unstable();
        let ns = inh.len() as i64;

        let board: Vec<String> = self.board.iter().map(|&c| card_to_string(c)).collect();
        let hhole = self
            .hole
            .map(|h| vec![card_to_string(h[0]), card_to_string(h[1])]);

        range::Sample {
            sb: self.sb_seat as i64,
            bb: self.bb_seat as i64,
            sbamt: self.small_blind as i64,
            bbamt: self.big_blind as i64,
            tseat: opp_seat as i64,
            hseat: Some(self.my_seat as i64),
            hhole,
            board,
            ev: self.events.clone(),
            prof: prof.unwrap_or_default(),
            ns: Some(ns),
            inh,
            btn: Some(self.button_seat as i64),
            stacks,
        }
    }
}
