//! 简化 NLHE 动作串重放（abs+real lockstep）—— `slumbot_advisor` 与 `aivat_eval`
//! 共用的**单一**重放源（`docs/aivat_eval.md` §6 [v2 硬要求]）。
//!
//! Slumbot 的一手 = 一个 action 串（如 `b200c/kb200f`）。重放在两态上 lockstep 推进：
//! - **abs**（[`SimplifiedNlheState`]）：抽象 betting tree 位置（`current_node_id`）+
//!   训练序合法动作集；牌无关（dummy 发牌）。
//! - **real**（[`GameState`]）：真实筹码 / pot / 合法下注区间 / 终局结算；牌无关
//!   （dummy 发牌——bet 尺寸由 `b<N>` 串决定，与牌无关；showdown 赢家才依赖牌，
//!   estimator 用日志真实牌另算，不读 dummy payouts）。
//!
//! 两条用途：
//! - [`replay`]：把 tokens 重放到末态，返回末态两态快照（advisor 决策路径）。
//! - [`replay_trajectory`]：重放**整手**并记录沿途每个决策节点 + 每街首决策节点 +
//!   终局类型 + 各决策处 real 快照（AIVAT estimator 用，见 `aivat_nlhe.rs`）。
//!
//! a\* 还原、σ 动作集合、child 定位一律走本模块的 `legal_actions` / tree，杜绝
//! advisor 与 estimator 两份漂移。

use crate::abstraction::action::{AbstractAction, ActionAbstraction, StreetActionAbstraction};
use crate::abstraction::info::StreetTag;
use crate::core::rng::ChaCha20Rng;
use crate::core::ChipAmount;
use crate::core::{Card, Rank, Suit};
use crate::rules::action::Action;
use crate::rules::config::TableConfig;
use crate::rules::state::GameState;
use crate::training::game::Game;
use crate::training::nlhe::{SimplifiedNlheGame, SimplifiedNlheState};
use crate::training::nlhe_betting_tree::{AbstractActionTag, NodeId};

/// 各 [`StreetTag`] 已发公共牌张数（Preflop/Flop/Turn/River = 0..3）。
pub const BOARD_LEN: [usize; 4] = [0, 3, 4, 5];

const ABS_DUMMY_SEED: u64 = 0x4142_5344_554d_4d59; // "ABSDUMMY"
const REAL_DUMMY_SEED: u64 = 0x5245_414c_4453_4544; // "REALDSED"

// ===========================================================================
// Card 字符串解析（"Ac" → Card）
// ===========================================================================

/// 解析一张 Slumbot 牌字符串（rank 大写 + suit 小写，如 `"Ac"`/`"Td"`/`"9h"`/`"2s"`）。
pub fn parse_card(s: &str) -> Result<Card, String> {
    let b = s.as_bytes();
    if b.len() != 2 {
        return Err(format!("card {s:?} 必须是 2 字符（rank+suit）"));
    }
    let rank = match b[0] {
        b'2' => Rank::Two,
        b'3' => Rank::Three,
        b'4' => Rank::Four,
        b'5' => Rank::Five,
        b'6' => Rank::Six,
        b'7' => Rank::Seven,
        b'8' => Rank::Eight,
        b'9' => Rank::Nine,
        b'T' => Rank::Ten,
        b'J' => Rank::Jack,
        b'Q' => Rank::Queen,
        b'K' => Rank::King,
        b'A' => Rank::Ace,
        other => return Err(format!("非法 rank 字符 {:?} in {s:?}", other as char)),
    };
    let suit = match b[1] {
        b'c' => Suit::Clubs,
        b'd' => Suit::Diamonds,
        b'h' => Suit::Hearts,
        b's' => Suit::Spades,
        other => return Err(format!("非法 suit 字符 {:?} in {s:?}", other as char)),
    };
    Ok(Card::new(rank, suit))
}

/// 解析一组牌字符串。
pub fn parse_cards(list: &[String]) -> Result<Vec<Card>, String> {
    list.iter().map(|s| parse_card(s)).collect()
}

// ===========================================================================
// action 串 token 化（k/c/f/b<N>/'/'）
// ===========================================================================

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum Token {
    Check,
    Call,
    Fold,
    /// `b<N>`：本街下注到 N（绝对，含 preflop 盲注；与求解器 `to` 同语义）。
    BetTo(u64),
    /// `/`：分街分隔符（状态机自动切街，这里仅跳过 + 校验）。
    StreetSep,
}

/// 把 Slumbot action 串切成 token 序列。语法错误（未知字符 / 缺 bet size /
/// bet size 非数字）→ Err。
pub fn tokenize(action: &str) -> Result<Vec<Token>, String> {
    let bytes = action.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'k' => {
                out.push(Token::Check);
                i += 1;
            }
            b'c' => {
                out.push(Token::Call);
                i += 1;
            }
            b'f' => {
                out.push(Token::Fold);
                i += 1;
            }
            b'/' => {
                out.push(Token::StreetSep);
                i += 1;
            }
            b'b' => {
                i += 1;
                let start = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                if i == start {
                    return Err(format!("'b' 后缺 bet size @ {start} in {action:?}"));
                }
                let n: u64 = action[start..i]
                    .parse()
                    .map_err(|e| format!("bet size 非整数 {:?}: {e}", &action[start..i]))?;
                out.push(Token::BetTo(n));
            }
            other => {
                return Err(format!(
                    "action 串非法字符 {:?} in {action:?}",
                    other as char
                ));
            }
        }
    }
    Ok(out)
}

// ===========================================================================
// 两态 lockstep 重放
// ===========================================================================

/// 重放末态的两态快照（advisor 决策点用）。
pub struct LockstepState {
    pub abs: SimplifiedNlheState,
    pub real: GameState,
}

/// 两态都从 root 起、dummy 发牌，按 `tokens` 同步推进。返回末态快照。
///
/// - `k`/`c`/`f`：两态 apply 对应动作（abs 取同 tag 的合法动作）。
/// - `b<N>`：抽象影子 `map_off_tree` 选最近 ratio → 投影到 abs 合法集（塌进 AllIn 兜底）
///   → abs apply；real apply `Bet/Raise{to:N}`（N≥real all-in 或 abs 塌 AllIn 时两态走
///   AllIn 保持 lockstep）。
///
/// 不在终局报错（留给 caller 判）；任一 apply 非法 / 两态 current_player 漂移即 Err。
pub fn replay(
    game: &SimplifiedNlheGame,
    abstraction: &StreetActionAbstraction,
    tokens: &[Token],
) -> Result<LockstepState, String> {
    let cfg = TableConfig::default_hu_200bb();
    let mut rng = ChaCha20Rng::from_seed(ABS_DUMMY_SEED);
    let mut abs = game.root(&mut rng);
    let mut real = GameState::new(&cfg, REAL_DUMMY_SEED);

    for (idx, token) in tokens.iter().enumerate() {
        if *token == Token::StreetSep {
            continue;
        }
        if real.is_terminal() || real.current_player().is_none() {
            return Err(format!(
                "token #{idx} {token:?} 出现在终局之后（real terminal）"
            ));
        }
        let (abs_action, real_action) = resolve_actions(abstraction, &abs, &real, *token)?;
        real.apply(real_action).map_err(|e| {
            format!("token #{idx} {token:?} real.apply({real_action:?}) 非法: {e:?}")
        })?;
        abs = SimplifiedNlheGame::next(abs, abs_action, &mut rng);

        let abs_cp = abs.game_state.current_player();
        let real_cp = real.current_player();
        if abs_cp != real_cp {
            return Err(format!(
                "token #{idx} {token:?} 后两态 current_player 漂移: abs={abs_cp:?} real={real_cp:?}"
            ));
        }
    }
    Ok(LockstepState { abs, real })
}

/// 给定一个 token，算出 `(abs 侧抽象动作, real 侧 stage-1 动作)`。
pub fn resolve_actions(
    abstraction: &StreetActionAbstraction,
    abs: &SimplifiedNlheState,
    real: &GameState,
    token: Token,
) -> Result<(AbstractAction, Action), String> {
    let legal_abs = SimplifiedNlheGame::legal_actions(abs);
    match token {
        Token::Check => Ok((
            find_tag(&legal_abs, AbstractActionTag::Check).ok_or("Check 在 abs 当前节点不合法")?,
            Action::Check,
        )),
        Token::Call => Ok((
            // call-of-all-in：abstract_actions 按 AA-004-rev1 把该 Call 折进 AllIn 槽
            // → 无 Call 边时落到 AllIn；real 侧 Action::Call 由规则引擎归一化为 all-in
            // 跟注（→ 终局），与 abs 一致。
            find_tag(&legal_abs, AbstractActionTag::Call)
                .or_else(|| find_tag(&legal_abs, AbstractActionTag::AllIn))
                .ok_or("Call/AllIn 在 abs 当前节点都不合法")?,
            Action::Call,
        )),
        Token::Fold => Ok((
            find_tag(&legal_abs, AbstractActionTag::Fold).ok_or("Fold 在 abs 当前节点不合法")?,
            Action::Fold,
        )),
        Token::BetTo(n) => {
            let raw = abstraction.map_off_tree(&abs.game_state, ChipAmount::new(n));
            let raw_tag = AbstractActionTag::of(&raw);
            let abs_action = project_tag_onto(&legal_abs, raw_tag)?;
            let projected_tag = AbstractActionTag::of(&abs_action);

            let real_all_in_to = real.legal_actions().all_in_amount.map(|c| c.as_u64());
            let real_is_all_in = matches!(real_all_in_to, Some(cap) if n >= cap);
            let abs_collapsed = matches!(projected_tag, AbstractActionTag::AllIn);

            if real_is_all_in || abs_collapsed {
                let abs_all_in = find_tag(&legal_abs, AbstractActionTag::AllIn)
                    .ok_or("AllIn 在 abs 当前节点不合法（投影兜底失败）")?;
                Ok((abs_all_in, Action::AllIn))
            } else {
                let real_action = match projected_tag {
                    AbstractActionTag::Bet(_) => Action::Bet {
                        to: ChipAmount::new(n),
                    },
                    AbstractActionTag::Raise(_) => Action::Raise {
                        to: ChipAmount::new(n),
                    },
                    AbstractActionTag::Call => Action::Call,
                    AbstractActionTag::Check => Action::Check,
                    AbstractActionTag::Fold => Action::Fold,
                    AbstractActionTag::AllIn => unreachable!("AllIn 已在上面分支处理"),
                };
                Ok((abs_action, real_action))
            }
        }
        Token::StreetSep => unreachable!("StreetSep 由 replay 跳过，不进 resolve_actions"),
    }
}

/// 在合法动作集里找指定 tag 的 [`AbstractAction`]（带正确 `to` 标签）。
pub fn find_tag(legal: &[AbstractAction], tag: AbstractActionTag) -> Option<AbstractAction> {
    legal
        .iter()
        .copied()
        .find(|a| AbstractActionTag::of(a) == tag)
}

/// 把 `map_off_tree` 选出的 tag 投影到 abs 当前合法集：tag 在则取之。不在时分两种：
/// **① 被规则剪掉的加注档**（节点仍有更大合法加注）→ 在合法加注阶梯上向上取最近一档（ratio
/// ≥ 选中档的最小合法加注）；**② 选中档塌进 AllIn**（短码：比所有合法加注都大）→ 退 AllIn。
/// 两者都缺 → Err（决策节点必有 ≥1 动作）。与 [`crate::training::blueprint_advisor::project_tag_onto`]
/// 同逻辑（off-tree 投影单一源；Slumbot `default_6_action` 无剪档 → ① 永不触发 = byte-equal）。
pub fn project_tag_onto(
    legal: &[AbstractAction],
    tag: AbstractActionTag,
) -> Result<AbstractAction, String> {
    if let Some(a) = find_tag(legal, tag) {
        return Ok(a);
    }
    // ① 剪档：向上投到 ratio ≥ 选中档的最小合法加注（Bet/Raise），而非塌 AllIn。
    if let AbstractActionTag::Bet(r) | AbstractActionTag::Raise(r) = tag {
        let target = r.as_milli();
        let up = legal
            .iter()
            .copied()
            .filter_map(|a| match a {
                AbstractAction::Bet { ratio_label, .. }
                | AbstractAction::Raise { ratio_label, .. } => Some((ratio_label.as_milli(), a)),
                _ => None,
            })
            .filter(|(m, _)| *m >= target)
            .min_by_key(|(m, _)| *m)
            .map(|(_, a)| a);
        if let Some(a) = up {
            return Ok(a);
        }
    }
    // ② 塌全下（或无更大合法加注）→ AllIn 兜底。
    if let Some(a) = find_tag(legal, AbstractActionTag::AllIn) {
        return Ok(a);
    }
    Err(format!(
        "投影失败：tag {tag:?} 与 AllIn 都不在 abs 合法集 {legal:?}"
    ))
}

// ===========================================================================
// outgoing 翻译（advisor 出 incr 串；estimator 不用——保留以维持 §6 单一源）
// ===========================================================================

/// 把策略选中的抽象动作翻译成 Slumbot incr 串，尺寸以**真实** state 算。
pub fn outgoing_incr(
    real: &GameState,
    abstraction: &StreetActionAbstraction,
    chosen: AbstractAction,
) -> Result<String, String> {
    match chosen {
        AbstractAction::Fold => Ok("f".to_string()),
        AbstractAction::Check => Ok("k".to_string()),
        AbstractAction::Call { .. } => Ok("c".to_string()),
        AbstractAction::AllIn { .. } => {
            let to = real
                .legal_actions()
                .all_in_amount
                .ok_or("选中 AllIn 但 real 无 all_in_amount（无筹码？）")?;
            Ok(bet_or_call(real, to.as_u64()))
        }
        AbstractAction::Bet { ratio_label, .. } | AbstractAction::Raise { ratio_label, .. } => {
            let real_actions = abstraction.abstract_actions(real);
            for a in real_actions.iter() {
                match a {
                    AbstractAction::Bet { to, ratio_label: r }
                    | AbstractAction::Raise { to, ratio_label: r }
                        if r.as_milli() == ratio_label.as_milli() =>
                    {
                        return Ok(bet_or_call(real, to.as_u64()));
                    }
                    _ => {}
                }
            }
            let to = real
                .legal_actions()
                .all_in_amount
                .ok_or("选中 Bet/Raise 档但真实侧既无该档也无 all_in_amount")?;
            Ok(bet_or_call(real, to.as_u64()))
        }
    }
}

/// `to` 严格高于当前下注水位 → `b<to>`；否则是（all-in）跟注 → `c`。
pub fn bet_or_call(real: &GameState, to: u64) -> String {
    let bet_level = real
        .players()
        .iter()
        .map(|p| p.committed_this_round.as_u64())
        .max()
        .unwrap_or(0);
    if to > bet_level {
        format!("b{to}")
    } else {
        "c".to_string()
    }
}

// ===========================================================================
// 整手轨迹（AIVAT estimator 用）
// ===========================================================================

/// 一个决策节点（我方或对方）在重放轨迹里的快照。
pub struct ReplayDecision {
    /// 抽象 betting tree 节点 id（= 日志 `info_set >> 38`，estimator 会断言）。
    pub node_id: NodeId,
    /// 行动座位（0 = SB/button，1 = BB）。
    pub actor: u8,
    pub street: StreetTag,
    /// 选中动作在 `legal_actions(node)` / `children` / 日志 `action_probs` 里的下标。
    pub chosen_idx: usize,
    /// 该决策点的 real 状态快照（apply **之前**）。estimator 用它做 V_child 终局展开
    /// （clone + apply 终局动作读 committed/payouts）。
    pub real_before: GameState,
}

/// 整手终局类型。
pub enum TerminalKind {
    /// 一方弃牌（U = 日志 winnings；无 runout）。
    Fold,
    /// 摊牌（betting 关闭）。`lock_street < River` ⟺ 有 runout（锁定后还有发牌）。
    Showdown { lock_street: StreetTag },
}

/// 整手重放轨迹。
pub struct ReplayTrajectory {
    /// 沿途所有决策（我方 + 对方），按时间序。
    pub decisions: Vec<ReplayDecision>,
    /// 每街首个决策节点（index = `StreetTag as usize`）；`None` = 该街无决策
    /// （fold 前未到 / runout 街）→ 该街牌事件归 c_runout 不归 c_b。
    pub street_first_decision: [Option<NodeId>; 4],
    pub terminal: TerminalKind,
    /// 终局 real 状态（committed_total 已定；estimator 读 m = min(committed_total)）。
    pub final_real: GameState,
}

/// 重放整手，记录轨迹（estimator 用）。与 [`replay`] 同一 lockstep 推进逻辑，额外
/// 在每个决策点 push 一条 [`ReplayDecision`]（含 real 快照）+ 记录每街首决策节点 +
/// 终局类型。`tokens` 必须是**完整一手**（重放到终局）。
pub fn replay_trajectory(
    game: &SimplifiedNlheGame,
    abstraction: &StreetActionAbstraction,
    tokens: &[Token],
) -> Result<ReplayTrajectory, String> {
    let cfg = TableConfig::default_hu_200bb();
    let mut rng = ChaCha20Rng::from_seed(ABS_DUMMY_SEED);
    let mut abs = game.root(&mut rng);
    let mut real = GameState::new(&cfg, REAL_DUMMY_SEED);

    let mut decisions: Vec<ReplayDecision> = Vec::with_capacity(8);
    let mut street_first_decision: [Option<NodeId>; 4] = [None; 4];
    let mut last_street = StreetTag::Preflop;

    for (idx, token) in tokens.iter().enumerate() {
        if *token == Token::StreetSep {
            continue;
        }
        if real.is_terminal() || real.current_player().is_none() {
            return Err(format!(
                "token #{idx} {token:?} 出现在终局之后（real terminal）"
            ));
        }

        let actor = abs
            .game_state
            .current_player()
            .ok_or_else(|| format!("token #{idx}: abs 无 current_player"))?
            .0;
        let node_id = abs.current_node_id;
        let street = game.tree().node(node_id).street;
        if street_first_decision[street as usize].is_none() {
            street_first_decision[street as usize] = Some(node_id);
        }

        let (abs_action, real_action) = resolve_actions(abstraction, &abs, &real, *token)?;
        let chosen_tag = AbstractActionTag::of(&abs_action);
        let legal = SimplifiedNlheGame::legal_actions(&abs);
        let chosen_idx = legal
            .iter()
            .position(|a| AbstractActionTag::of(a) == chosen_tag)
            .ok_or_else(|| format!("token #{idx}: chosen tag {chosen_tag:?} 不在 legal"))?;

        decisions.push(ReplayDecision {
            node_id,
            actor,
            street,
            chosen_idx,
            real_before: real.clone(),
        });
        last_street = street;

        real.apply(real_action).map_err(|e| {
            format!("token #{idx} {token:?} real.apply({real_action:?}) 非法: {e:?}")
        })?;
        abs = SimplifiedNlheGame::next(abs, abs_action, &mut rng);

        let abs_cp = abs.game_state.current_player();
        let real_cp = real.current_player();
        if abs_cp != real_cp {
            return Err(format!(
                "token #{idx} {token:?} 后两态 current_player 漂移: abs={abs_cp:?} real={real_cp:?}"
            ));
        }
    }

    if !real.is_terminal() {
        return Err("重放结束但 real 非终局（action 串不完整？）".to_string());
    }

    // 弃牌 ⟺ 有玩家 Folded；否则摊牌（lock_street = 最后一个决策的街）。
    let folded = real
        .players()
        .iter()
        .any(|p| p.status == crate::core::PlayerStatus::Folded);
    let terminal = if folded {
        TerminalKind::Fold
    } else {
        TerminalKind::Showdown {
            lock_street: last_street,
        }
    };

    Ok(ReplayTrajectory {
        decisions,
        street_first_decision,
        terminal,
        final_real: real,
    })
}

/// 终局 real 状态里两座位的 cumulative committed（matched amount = min）。
pub fn committed_totals(real: &GameState) -> [u64; 2] {
    let p = real.players();
    [p[0].committed_total.as_u64(), p[1].committed_total.as_u64()]
}

/// 抽象动作 tag → strategy 日志里的短名（去 bet/raise 前缀，与 `tools/slumbot_play.py`
/// `_short_action` + advisor `action_label` 对齐）：`fold`/`check`/`call`/`allin`/
/// `0.5pot`/`1pot`/`2pot`。estimator 用它把日志 `action_probs`（按名 keyed）对齐到
/// tree legal 顺序——绕开 serde_json 默认按字典序重排 object key 的坑。
pub fn tag_short_name(tag: AbstractActionTag) -> String {
    match tag {
        AbstractActionTag::Fold => "fold".to_string(),
        AbstractActionTag::Check => "check".to_string(),
        AbstractActionTag::Call => "call".to_string(),
        AbstractActionTag::AllIn => "allin".to_string(),
        AbstractActionTag::Bet(r) | AbstractActionTag::Raise(r) => {
            format!("{}pot", r.as_milli() as f64 / 1000.0)
        }
    }
}
