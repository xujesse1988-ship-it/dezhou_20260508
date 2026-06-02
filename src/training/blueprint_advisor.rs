//! 跨抽象 blueprint advisor 引擎（`docs/six_max_nlhe_target.md` S5 §6 / `docs/temp/
//! openpoker_client_design_2026_06_02.md` §6 的共用底座）。
//!
//! 把 `tools/slumbot_advisor.rs` 的 off-tree 翻译核抽出来、去 HU 硬编码、参数化
//! N 座 / `TableConfig`，供两个消费方共用：
//! - **①六人 blueprint 互评**（受控自对弈，`tools/six_max_blueprint_h2h`）：一张权威
//!   `GameState`（N 座真实筹码）+ 每个 distinct blueprint 一份「抽象影子」
//!   [`SimplifiedNlheState`]，每个 applied 动作经 off-tree 翻译推进各影子。
//! - **②OpenPoker 实测客户端**（`tools/openpoker_advisor`，后置）：单影子、逐决策无状态。
//!
//! # off-tree 翻译两端（逐字复用 slumbot 已验逻辑）
//!
//! - **outgoing**（[`outgoing_action`]）：blueprint 选了抽象动作 → 以**真实** `GameState`
//!   pot 算同 ratio 档的 `to`，落进真实合法区间，输出可 apply 的 stage-1 [`Action`]。
//! - **incoming**（[`advance_shadow_by_applied`]）：对手在权威局打了抽象里没有的尺寸 →
//!   以**影子自身**几何 `map_off_tree` 选最近 ratio → 投影到影子当前节点合法集（塌
//!   AllIn 兜底）→ 推进影子。参考系是影子自己的 pot（每个 bot 按它训练时的抽象 pot
//!   感知对手下注，与训练一致；同 slumbot）。
//!
//! # 正确性边界（必读 —— off-tree 不解决「结构性」动作集差异）
//!
//! off-tree 只解决**下注尺寸**不在抽象里的问题。当两抽象的动作集**结构不同**
//! （典型：一方允许 open-limp、一方 `no_open_limp`），limp 进的多人池在 no-limp 抽象里
//! **没有对应节点** → 推进该影子时无法保持与权威局的回合顺序一致。本引擎**不静默吞掉**
//! 这种 desync：[`play_cross_abstraction_hand`] 每步比对每个影子的 `current_player` /
//! `street` 与权威局，不一致即返回 [`HandError::Desync`]，由评测层计数 + 排除该手，
//! 使结构性 gap 显式可见（`six_max_nlhe_target.md` S5 记录）。
//!
//! 推论（已在 S5 实测前预判）：`nolimp` vs `preopen`（都 no-limp、仅 preflop 开池尺寸
//! 2.25 vs 3.5BB 不同）= 纯尺寸差异 → off-tree 干净、近乎零 desync；任何牵涉 `baseline`
//! （含 open-limp）的对局会显著 desync，结果须谨慎。
//!
//! crate 零网络依赖（invariant）：本模块纯计算，网络 IO 留给 Python driver（②）。

use crate::abstraction::action::{AbstractAction, ActionAbstraction, StreetActionAbstraction};
use crate::abstraction::info::InfoSetId;
use crate::core::rng::{ChaCha20Rng, RngSource};
use crate::core::{Card, ChipAmount, PlayerStatus, Rank, Suit};
use crate::rules::action::Action;
use crate::rules::config::TableConfig;
use crate::rules::state::GameState;
use crate::training::game::Game;
use crate::training::nlhe::{SimplifiedNlheGame, SimplifiedNlheState};
use crate::training::nlhe_betting_tree::AbstractActionTag;
use crate::training::sampling::sample_discrete;

// ===========================================================================
// Card 字符串解析（"Ac" → Card） —— 与 slumbot 同语义（rank 大写 + suit 小写）。
// ===========================================================================

/// 解析一张牌字符串（rank 大写 + suit 小写，如 `"Ac"`/`"Td"`/`"9h"`/`"2s"`）。
/// Slumbot / OpenPoker 牌编码相同（`docs/...openpoker_client_design...` §0）。
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

/// Card → 字符串（round-trip / ② 出 board）。
pub fn card_to_string(c: Card) -> String {
    let r = match c.rank() {
        Rank::Two => '2',
        Rank::Three => '3',
        Rank::Four => '4',
        Rank::Five => '5',
        Rank::Six => '6',
        Rank::Seven => '7',
        Rank::Eight => '8',
        Rank::Nine => '9',
        Rank::Ten => 'T',
        Rank::Jack => 'J',
        Rank::Queen => 'Q',
        Rank::King => 'K',
        Rank::Ace => 'A',
    };
    let s = match c.suit() {
        Suit::Clubs => 'c',
        Suit::Diamonds => 'd',
        Suit::Hearts => 'h',
        Suit::Spades => 's',
    };
    format!("{r}{s}")
}

/// 批量解析牌字符串。
pub fn parse_cards(list: &[String]) -> Result<Vec<Card>, String> {
    list.iter().map(|s| parse_card(s)).collect()
}

// ===========================================================================
// 抽象动作集投影（off-tree 两端共用）
// ===========================================================================

/// 在合法动作集里找指定 tag 的 [`AbstractAction`]（带正确 `to` 标签）。
pub fn find_tag(legal: &[AbstractAction], tag: AbstractActionTag) -> Option<AbstractAction> {
    legal
        .iter()
        .copied()
        .find(|a| AbstractActionTag::of(a) == tag)
}

/// 把 `map_off_tree` 选出的 tag 投影到当前合法集：tag 在则取之；不在（该 ratio 在抽象
/// pot 下已塌进 AllIn slot，或被 A3×A4 规则剪掉）则退到 AllIn 兜底。两者都缺 → Err
/// （决策节点必有 ≥1 动作，不该发生）。
pub fn project_tag_onto(
    legal: &[AbstractAction],
    tag: AbstractActionTag,
) -> Result<AbstractAction, String> {
    if let Some(a) = find_tag(legal, tag) {
        return Ok(a);
    }
    if let Some(a) = find_tag(legal, AbstractActionTag::AllIn) {
        return Ok(a);
    }
    Err(format!(
        "投影失败：tag {tag:?} 与 AllIn 都不在合法集 {legal:?}"
    ))
}

// ===========================================================================
// outgoing：抽象动作 → 真实可 apply 的 stage-1 Action（尺寸以真实 pot 算）
// ===========================================================================

/// 进攻动作要发的种类（`to > bet_level` 分支用；`to <= bet_level` 一律退化为 `Call`）。
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum AggrKind {
    Bet,
    Raise,
    AllIn,
}

/// 把目标 `to`（本街累计下注到）翻成真实 [`Action`]：`to` **严格高于**当前下注水位
/// （`max committed_this_round`）才构成合法 bet/raise（增量 > 0）；否则不是加注 = 跟注。
///
/// 关键 case（slumbot `bet_or_call` 同语义）：对手 all-in 覆盖我方时，我方「AllIn」的
/// `to == 对手下注水位`（甚至更低，all-in for less），增量 0 其实是 all-in 跟注 →
/// 发 [`Action::Call`]（否则规则引擎判非法 bet）。
fn finish_aggressive(real: &GameState, to: u64, kind: AggrKind) -> Action {
    let bet_level = real
        .players()
        .iter()
        .map(|p| p.committed_this_round.as_u64())
        .max()
        .unwrap_or(0);
    if to > bet_level {
        match kind {
            AggrKind::Bet => Action::Bet {
                to: ChipAmount::new(to),
            },
            AggrKind::Raise => Action::Raise {
                to: ChipAmount::new(to),
            },
            AggrKind::AllIn => Action::AllIn,
        }
    } else {
        Action::Call
    }
}

/// 把 blueprint 选中的抽象动作翻译成真实 [`Action`]，尺寸以**真实** `real` state 算。
///
/// `Bet`/`Raise` 找真实 pot 下同 ratio 档的 `to`；该档在真实 pot 下已塌成 AllIn（被
/// `abstract_actions` 折叠）则发 all-in。返回值可直接 `GameState::apply`。这是
/// slumbot `outgoing_incr` 的 [`Action`] 版（后者 = 本函数 + 字符串化），两者共用一处
/// 逻辑保证 slumbot 行为不变。
pub fn outgoing_action(
    real: &GameState,
    abstraction: &StreetActionAbstraction,
    chosen: AbstractAction,
) -> Result<Action, String> {
    Ok(match chosen {
        AbstractAction::Fold => Action::Fold,
        AbstractAction::Check => Action::Check,
        AbstractAction::Call { .. } => Action::Call,
        AbstractAction::AllIn { .. } => {
            let to = real
                .legal_actions()
                .all_in_amount
                .ok_or("选中 AllIn 但 real 无 all_in_amount（无筹码？）")?
                .as_u64();
            finish_aggressive(real, to, AggrKind::AllIn)
        }
        AbstractAction::Bet { ratio_label, .. } | AbstractAction::Raise { ratio_label, .. } => {
            // 同一抽象作用在真实 pot 上，找同 ratio 的档取其真实 `to`。
            let real_actions = abstraction.abstract_actions(real);
            let mut found: Option<(AggrKind, u64)> = None;
            for a in real_actions.iter() {
                match a {
                    AbstractAction::Bet { to, ratio_label: r }
                        if r.as_milli() == ratio_label.as_milli() =>
                    {
                        found = Some((AggrKind::Bet, to.as_u64()));
                        break;
                    }
                    AbstractAction::Raise { to, ratio_label: r }
                        if r.as_milli() == ratio_label.as_milli() =>
                    {
                        found = Some((AggrKind::Raise, to.as_u64()));
                        break;
                    }
                    _ => {}
                }
            }
            let (kind, to) = match found {
                Some(x) => x,
                None => {
                    // 该档在真实 pot 下塌成 AllIn → 发 all-in（仍走 bet_or_call 判退化跟注）。
                    let to = real
                        .legal_actions()
                        .all_in_amount
                        .ok_or("选中 Bet/Raise 档但真实侧既无该档也无 all_in_amount")?
                        .as_u64();
                    (AggrKind::AllIn, to)
                }
            };
            finish_aggressive(real, to, kind)
        }
    })
}

// ===========================================================================
// incoming：用 applied 真实动作推进一个抽象影子（off-tree）
// ===========================================================================

/// 把 applied 到权威局的真实 [`Action`] 翻译成本影子的抽象动作并推进影子（原地）。
///
/// 参考系 = **影子自身几何**（`shadow.abs.map_off_tree(&shadow.game_state, to)`，同
/// slumbot）。`applied_is_all_in` = 该动作在**权威局**是否令行动者 all-in；为真时强制
/// 影子也走 AllIn（保持与权威 all-in 同步，slumbot `real_is_all_in` 同义）。
///
/// 返回推进影子用的抽象动作（便于日志 / 测试）。失败（影子合法集缺对应 tag = 结构性
/// gap，如 limp 进 no-limp 影子）返回 `Err` → 调用方按 desync 处理。
pub fn advance_shadow_by_applied(
    shadow: &mut SimplifiedNlheState,
    applied: Action,
    applied_is_all_in: bool,
    rng: &mut dyn RngSource,
) -> Result<AbstractAction, String> {
    let legal_abs = SimplifiedNlheGame::legal_actions(shadow);
    let abs_action = match applied {
        Action::Fold => find_tag(&legal_abs, AbstractActionTag::Fold)
            .ok_or("incoming Fold 在影子当前节点不合法")?,
        Action::Check => {
            // Check 是被动动作：影子无 Check 节点 = 结构性 gap（如 no-limp 抽象无 limped-pot
            // 的 BB-check 节点）。不静默映射到别的 kind —— 显式报 desync（kind 不可改）。
            find_tag(&legal_abs, AbstractActionTag::Check).ok_or(
                "incoming 被动 Check 在影子无对应（结构性 gap：抽象不含该 limped/checked 节点）",
            )?
        }
        Action::Call => {
            if let Some(a) = find_tag(&legal_abs, AbstractActionTag::Call) {
                a
            } else if applied_is_all_in {
                // 合法的 all-in 跟注：影子里该 Call 被折进 AllIn 槽（AA-004-rev1）→ 走 AllIn。
                find_tag(&legal_abs, AbstractActionTag::AllIn)
                    .ok_or("incoming all-in Call 但影子无 AllIn 槽")?
            } else {
                // 被动 Call（open-limp / cold-call）但影子无 Call 节点 = 结构性 gap（典型：
                // open-limp 进 no_open_limp 影子）。passive→AllIn 是 kind 变（会污染回合 /
                // 价值），显式报 desync 而非静默塌 AllIn（`六max...` S5 正确性边界）。
                return Err(
                    "incoming 被动 Call（open-limp/cold-call）在影子无对应（结构性 gap：\
                     no-limp 抽象不含 open-limp 节点）"
                        .to_string(),
                );
            }
        }
        Action::AllIn => find_tag(&legal_abs, AbstractActionTag::AllIn)
            .ok_or("incoming AllIn 在影子当前节点不合法")?,
        Action::Bet { to } | Action::Raise { to } => {
            // 以影子几何选最近 ratio，投影到影子合法集（塌 AllIn 兜底）。
            let raw = shadow.abs.map_off_tree(&shadow.game_state, to);
            let raw_tag = AbstractActionTag::of(&raw);
            let projected = project_tag_onto(&legal_abs, raw_tag)?;
            let abs_collapsed =
                matches!(AbstractActionTag::of(&projected), AbstractActionTag::AllIn);
            if applied_is_all_in || abs_collapsed {
                // 任一参考系塌 all-in → 影子也走 AllIn（与权威 all-in 同步）。
                find_tag(&legal_abs, AbstractActionTag::AllIn)
                    .ok_or("incoming Bet/Raise 塌 AllIn 但影子无 AllIn（投影兜底失败）")?
            } else {
                projected
            }
        }
    };
    let advanced = SimplifiedNlheGame::next(shadow.clone(), abs_action, rng);
    *shadow = advanced;
    Ok(abs_action)
}

// ===========================================================================
// ① 跨抽象 N 座对局：一张权威 GameState + 每 distinct blueprint 一份影子
// ===========================================================================

/// 一个 distinct blueprint 参赛者：它的 game（树 / 抽象 / 桶）+ 策略查询面 + 标签。
///
/// `strategy(info, n)` 返回该 infoset 的 `n` 维分布（空 / 全零 → 调用方按 uniform 兜底）。
/// 通常 = `|info, _n| trainer.average_strategy(*info)`。
pub struct Contestant<'a> {
    pub game: &'a SimplifiedNlheGame,
    pub strategy: &'a dyn Fn(&InfoSetId, usize) -> Vec<f64>,
    pub label: String,
}

/// 一手跨抽象对局的失败原因。
#[derive(Clone, Debug)]
pub enum HandError {
    /// 影子与权威局回合顺序 / 街漂移（off-tree 无法忠实表达 = 结构性 gap）。计数 + 排除。
    Desync(String),
    /// 权威局 apply 非法（outgoing 应恒产合法动作；出现即真 bug）→ 上抛。
    Illegal(String),
    /// 单手执行超过 `max_actions`（评测 bug 防死循环）。
    NonTerminal,
}

const SHADOW_ROOT_SEED: u64 = 0x5348_4144_4f57_5f30; // "SHADOW_0"：影子 root 发牌用（牌弃用）。

/// 跑一手 N 座对局：权威 [`GameState`] 持真实筹码 + 牌；座 `i` 由
/// `contestants[seat_blueprint[i]]` 驱动。每决策：取该座 blueprint 影子当前节点 + 该座
/// **真实**手牌 → 查策略 → 采样 → [`outgoing_action`] 落真实动作 apply 权威局 → 推进所有
/// 影子（行动者影子按所选抽象动作字面推进，其余影子 [`advance_shadow_by_applied`]）。
/// 每步比对每个影子 `current_player` / `street` 与权威局，漂移即 [`HandError::Desync`]。
///
/// 返回 per-seat 净 PnL（chips，下标 = `SeatId`）。`sample_rng` 驱动策略采样（可复现）。
#[allow(clippy::type_complexity)]
pub fn play_cross_abstraction_hand(
    contestants: &[Contestant],
    seat_blueprint: &[usize],
    config: &TableConfig,
    hand_seed: u64,
    sample_rng: &mut dyn RngSource,
    max_actions: usize,
) -> Result<Vec<f64>, HandError> {
    let n = config.n_seats as usize;
    assert_eq!(seat_blueprint.len(), n, "seat_blueprint 长度须等于座位数");

    // 权威局：真实筹码 + 真实发牌（hand_seed 决定牌）。
    let mut auth = GameState::new(config, hand_seed);

    // 每 distinct blueprint 一份影子（影子的牌弃用，只用其 current_node_id / 合法集）。
    let mut shadow_rng = ChaCha20Rng::from_seed(SHADOW_ROOT_SEED ^ hand_seed);
    let mut shadows: Vec<SimplifiedNlheState> = contestants
        .iter()
        .map(|c| c.game.root(&mut shadow_rng))
        .collect();

    for _ in 0..max_actions {
        if auth.is_terminal() {
            return finalize_payoffs(&auth, n);
        }
        let Some(actor) = auth.current_player() else {
            return finalize_payoffs(&auth, n);
        };
        let actor_idx = actor.0 as usize;
        let bp_idx = seat_blueprint[actor_idx];

        // sync 守门：行动者影子必须在同一座 / 同一街，否则 info_set 口径错位（甚至 panic）。
        {
            let sh = &shadows[bp_idx];
            if sh.game_state.current_player() != Some(actor) {
                return Err(HandError::Desync(format!(
                    "actor {actor:?} 但 blueprint[{}]={} 影子 current_player={:?}",
                    bp_idx,
                    contestants[bp_idx].label,
                    sh.game_state.current_player()
                )));
            }
            if sh.game_state.street() != auth.street() {
                return Err(HandError::Desync(format!(
                    "actor {actor:?} street 漂移：权威 {:?} vs 影子 {:?}",
                    auth.street(),
                    sh.game_state.street()
                )));
            }
        }

        // 该座真实手牌 + 真实 board → 抽象影子树位置 → infoset → 策略。
        let hole = auth.players()[actor_idx]
            .hole_cards
            .ok_or_else(|| HandError::Illegal(format!("actor {actor:?} 无手牌")))?;
        let board: Vec<Card> = auth.board().to_vec();
        let node_id = shadows[bp_idx].current_node_id;
        let legal_abs = SimplifiedNlheGame::legal_actions(&shadows[bp_idx]);
        if legal_abs.is_empty() {
            return Err(HandError::Desync(format!(
                "blueprint[{}] 影子合法集为空 @ actor {actor:?}",
                bp_idx
            )));
        }
        let info = contestants[bp_idx]
            .game
            .info_set_for_cards(node_id, hole, &board);
        let dist = strategy_distribution(&info, &legal_abs, contestants[bp_idx].strategy);
        let chosen = sample_discrete(&dist, sample_rng);

        // outgoing：真实 pot 算尺寸 → apply 权威局。
        let applied = outgoing_action(&auth, contestants[bp_idx].game.abstraction(), chosen)
            .map_err(HandError::Illegal)?;
        auth.apply(applied)
            .map_err(|e| HandError::Illegal(format!("权威 apply({applied:?}) 非法: {e:?}")))?;
        let applied_is_all_in = auth.players()[actor_idx].status == PlayerStatus::AllIn;

        // 推进所有影子：行动者影子按字面所选动作，其余按 incoming 翻译。
        for (idx, sh) in shadows.iter_mut().enumerate() {
            if idx == bp_idx {
                let advanced = SimplifiedNlheGame::next(sh.clone(), chosen, sample_rng);
                *sh = advanced;
            } else {
                advance_shadow_by_applied(sh, applied, applied_is_all_in, sample_rng)
                    .map_err(HandError::Desync)?;
            }
        }

        // desync 检测：apply 后每个影子的回合位必须与权威一致（含都终局）。
        for (idx, sh) in shadows.iter().enumerate() {
            if sh.game_state.current_player() != auth.current_player() {
                return Err(HandError::Desync(format!(
                    "apply 后 blueprint[{}]={} 影子 current_player={:?} ≠ 权威 {:?}",
                    idx,
                    contestants[idx].label,
                    sh.game_state.current_player(),
                    auth.current_player()
                )));
            }
        }
    }
    Err(HandError::NonTerminal)
}

/// 权威终局 → per-seat 净 PnL（下标 = SeatId）。
fn finalize_payoffs(auth: &GameState, n: usize) -> Result<Vec<f64>, HandError> {
    let payouts = auth
        .payouts()
        .ok_or_else(|| HandError::Illegal("终局但 payouts() == None".to_string()))?;
    let mut out = vec![0.0_f64; n];
    for (seat, pnl) in payouts {
        out[seat.0 as usize] = pnl as f64;
    }
    Ok(out)
}

/// 把 blueprint 原始分布归一成 `(action, prob)` 列表；空 / 全零 → uniform 兜底
/// （与 `nlhe_eval::strategy_distribution` 同口径，但这里产出 `(AbstractAction, f64)`
/// 直接喂 [`sample_discrete`]，且对长度不符 / 非法概率宽容处理为 uniform —— 评测层
/// 不因单点策略坏数据中断整轮）。
fn strategy_distribution(
    info: &InfoSetId,
    actions: &[AbstractAction],
    strategy: &dyn Fn(&InfoSetId, usize) -> Vec<f64>,
) -> Vec<(AbstractAction, f64)> {
    let raw = strategy(info, actions.len());
    let uniform = || {
        let p = 1.0 / actions.len() as f64;
        actions.iter().copied().map(|a| (a, p)).collect::<Vec<_>>()
    };
    if raw.len() != actions.len() {
        return uniform();
    }
    let mut sum = 0.0;
    for &p in &raw {
        if p.is_finite() && p > 0.0 {
            sum += p;
        }
    }
    if !(sum.is_finite() && sum > 0.0) {
        return uniform();
    }
    actions
        .iter()
        .copied()
        .zip(raw)
        .filter(|(_, p)| p.is_finite() && *p > 0.0)
        .map(|(a, p)| (a, p / sum))
        .collect()
}

// ===========================================================================
// ① 评测：hero 轮坐全部座 vs field 占其余座，按位置拆 mbb/g + desync 统计
// ===========================================================================

/// 跨抽象 blueprint 互评配置（hero 每座 `hands_per_seat` 手，固定 seed 可复现）。
#[derive(Clone, Copy, Debug)]
pub struct CrossH2hConfig {
    pub hands_per_seat: u64,
    pub seed: u64,
    pub max_actions_per_hand: usize,
}

impl Default for CrossH2hConfig {
    fn default() -> Self {
        Self {
            hands_per_seat: 50_000,
            seed: 0x5835_4831_5f48_3268, // "X5H1_H2h"-ish
            max_actions_per_hand: 512,
        }
    }
}

/// 跨抽象 hero(A) vs field(B) 互评结果。mbb/g 从 **hero** 视角，正数 = hero 净赢。
/// `desync_hands` = 因结构性 gap 排除的手数（off-tree 无法忠实表达），其占比高时结果不可信。
#[derive(Clone, Debug)]
pub struct CrossAbstractionH2hReport {
    pub hero_label: String,
    pub field_label: String,
    pub n_players: usize,
    pub hands_attempted: u64,
    pub hands_counted: u64,
    pub desync_hands: u64,
    pub illegal_hands: u64,
    pub hero_total_chips: f64,
    pub mbb_per_game: f64,
    pub standard_error_mbb_per_game: f64,
    pub ci95_low_mbb_per_game: f64,
    pub ci95_high_mbb_per_game: f64,
    /// 按相对按钮位置 offset 拆的 mbb/g（0 = BTN、1 = SB、2 = BB、3 = UTG、...）。
    pub per_position_mbb_per_game: Vec<f64>,
    /// 各位置计入的手数（desync 排除后），用于读 per-position 的可信度。
    pub per_position_hands: Vec<u64>,
}

/// hero(A) 依次坐遍全部 `n_players` 座（每座 `hands_per_seat` 手）、其余座全用 field(B)。
/// 一张权威 `GameState` 跑，hero 与 field 各持自己抽象的影子（[`play_cross_abstraction_hand`]）。
/// desync 手计数并从统计中排除；按相对按钮位置拆 hero 的 mbb/g。
///
/// `config` 必须是 hero / field 两 game 共用的桌配置（同 N 座 / 同码深 = `default_6max_100bb`）。
/// **正确性**：hero / field 结构性动作集不同（如 limp vs no-limp）会触发大量 desync —— 报告
/// 的 `desync_hands` 占比即该对比可信度，调用方须据此判读（`six_max_nlhe_target.md` S5）。
pub fn evaluate_cross_abstraction_h2h(
    hero: &Contestant,
    field: &Contestant,
    config: &TableConfig,
    h2h_config: &CrossH2hConfig,
) -> CrossAbstractionH2hReport {
    let n = config.n_seats as usize;
    let button = config.button_seat.0 as usize;
    // 参赛者表：索引 0 = hero，1 = field。
    let contestants = [
        Contestant {
            game: hero.game,
            strategy: hero.strategy,
            label: hero.label.clone(),
        },
        Contestant {
            game: field.game,
            strategy: field.strategy,
            label: field.label.clone(),
        },
    ];

    let mut hero_pnls: Vec<f64> = Vec::with_capacity(h2h_config.hands_per_seat as usize * n);
    let mut per_pos_sum = vec![0.0_f64; n];
    let mut per_pos_hands = vec![0u64; n];
    let mut desync_hands = 0u64;
    let mut illegal_hands = 0u64;
    let mut attempted = 0u64;

    for hero_seat in 0..n {
        // hero 坐 hero_seat，其余座是 field。
        let mut seat_bp = vec![1usize; n];
        seat_bp[hero_seat] = 0;
        let offset = (hero_seat + n - button) % n;
        for hand_idx in 0..h2h_config.hands_per_seat {
            attempted += 1;
            let hand_seed = mix3(h2h_config.seed, hero_seat as u64, hand_idx);
            let mut sample_rng = ChaCha20Rng::from_seed(mix3(hand_seed, 0xA5A5_A5A5, 1));
            match play_cross_abstraction_hand(
                &contestants,
                &seat_bp,
                config,
                hand_seed,
                &mut sample_rng,
                h2h_config.max_actions_per_hand,
            ) {
                Ok(pnls) => {
                    let pnl = pnls[hero_seat];
                    hero_pnls.push(pnl);
                    per_pos_sum[offset] += pnl;
                    per_pos_hands[offset] += 1;
                }
                Err(HandError::Desync(_)) | Err(HandError::NonTerminal) => desync_hands += 1,
                Err(HandError::Illegal(_)) => illegal_hands += 1,
            }
        }
    }

    let bb_chips = config.big_blind.as_u64() as f64;
    let scale = 1000.0 / bb_chips;
    let stats = sample_stats(&hero_pnls);
    let mean_mbb = stats.mean * scale;
    let se_mbb = stats.standard_error * scale;
    let per_position_mbb_per_game = per_pos_sum
        .iter()
        .zip(&per_pos_hands)
        .map(|(s, h)| if *h > 0 { (s / *h as f64) * scale } else { 0.0 })
        .collect();

    CrossAbstractionH2hReport {
        hero_label: hero.label.clone(),
        field_label: field.label.clone(),
        n_players: n,
        hands_attempted: attempted,
        hands_counted: hero_pnls.len() as u64,
        desync_hands,
        illegal_hands,
        hero_total_chips: hero_pnls.iter().sum(),
        mbb_per_game: mean_mbb,
        standard_error_mbb_per_game: se_mbb,
        ci95_low_mbb_per_game: mean_mbb - 1.96 * se_mbb,
        ci95_high_mbb_per_game: mean_mbb + 1.96 * se_mbb,
        per_position_mbb_per_game,
        per_position_hands: per_pos_hands,
    }
}

#[derive(Clone, Copy)]
struct Stats {
    mean: f64,
    standard_error: f64,
}

fn sample_stats(xs: &[f64]) -> Stats {
    if xs.is_empty() {
        return Stats {
            mean: 0.0,
            standard_error: 0.0,
        };
    }
    let nn = xs.len() as f64;
    let mean = xs.iter().sum::<f64>() / nn;
    if xs.len() == 1 {
        return Stats {
            mean,
            standard_error: 0.0,
        };
    }
    let var = xs.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / (nn - 1.0);
    Stats {
        mean,
        standard_error: var.sqrt() / nn.sqrt(),
    }
}

fn mix3(seed: u64, a: u64, b: u64) -> u64 {
    mix64(seed ^ a.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ b.wrapping_mul(0xBF58_476D_1CE4_E5B9))
}

fn mix64(mut x: u64) -> u64 {
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

// ===========================================================================
// 测试（card / outgoing 对照 slumbot / 跨抽象 lockstep + 守恒 + 结构 gap 检测）
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abstraction::bucket_table::{BucketConfig, BucketTable};
    use crate::training::nlhe_betting_tree::{first_small_6max, first_small_preopen_6max};
    use std::sync::Arc;

    fn stub_table() -> Arc<BucketTable> {
        Arc::new(BucketTable::stub_for_postflop(
            BucketConfig::default_500_500_500(),
        ))
    }

    #[test]
    fn card_round_trip_all_52() {
        for v in 0u8..52 {
            let c = Card::from_u8(v).unwrap();
            let s = card_to_string(c);
            let back = parse_card(&s).unwrap_or_else(|e| panic!("{s} parse: {e}"));
            assert_eq!(back, c);
            assert_eq!(back.to_u8(), v);
        }
        assert!(parse_card("X1").is_err());
        assert!(parse_card("Ax").is_err());
        assert!(parse_card("A").is_err());
    }

    /// 6-max nolimp vs preopen（都 no-limp，仅 preflop 开池尺寸不同 = 纯尺寸差异）：
    /// 一批 seed 的对局应**全部完成、零 desync、6 座 PnL 守恒（Σ==0）**。这是 off-tree
    /// 在「无结构性 gap」时忠实的核心 lockstep 证明（能让错算法 fail：任何回合漂移 /
    /// 记账错都会破守恒或触发 desync）。桶 = HU 占位 stub（只验机制，与训练值无关）。
    #[test]
    fn nolimp_vs_preopen_no_desync_and_conserves() {
        let table = stub_table();
        let cfg = TableConfig::default_6max_100bb();
        let (nl_abs, nl_rules) = {
            let (a, mut r) = first_small_6max(3);
            r.no_open_limp = true;
            (a, r)
        };
        let nolimp = SimplifiedNlheGame::new_with_abstraction(
            Arc::clone(&table),
            cfg.clone(),
            nl_abs,
            nl_rules,
        )
        .expect("nolimp game");
        let (pp_abs, pp_rules) = first_small_preopen_6max(3);
        let preopen = SimplifiedNlheGame::new_with_abstraction(
            Arc::clone(&table),
            cfg.clone(),
            pp_abs,
            pp_rules,
        )
        .expect("preopen game");

        // 均匀策略（机制测试，与训练值无关）。
        let uniform = |_info: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let hero = Contestant {
            game: &nolimp,
            strategy: &uniform,
            label: "nolimp".into(),
        };
        let field = Contestant {
            game: &preopen,
            strategy: &uniform,
            label: "preopen".into(),
        };
        let contestants = [
            Contestant {
                game: hero.game,
                strategy: hero.strategy,
                label: hero.label.clone(),
            },
            Contestant {
                game: field.game,
                strategy: field.strategy,
                label: field.label.clone(),
            },
        ];

        let n = cfg.n_seats as usize;
        let mut desyncs = 0;
        for hero_seat in 0..n {
            let mut seat_bp = vec![1usize; n];
            seat_bp[hero_seat] = 0;
            for hand in 0..200u64 {
                let hand_seed = 0xD15E_A5E0 ^ ((hero_seat as u64) << 32) ^ hand;
                let mut rng = ChaCha20Rng::from_seed(hand_seed ^ 0x9999);
                match play_cross_abstraction_hand(
                    &contestants,
                    &seat_bp,
                    &cfg,
                    hand_seed,
                    &mut rng,
                    512,
                ) {
                    Ok(pnls) => {
                        let sum: f64 = pnls.iter().sum();
                        assert!(sum.abs() < 1e-6, "6 座 PnL 须守恒 Σ==0，实得 {sum}");
                        assert_eq!(pnls.len(), n);
                    }
                    Err(HandError::Desync(_)) => desyncs += 1,
                    Err(e) => panic!("nolimp×preopen 非 desync 失败: {e:?}"),
                }
            }
        }
        // 纯尺寸差异不该有结构性 desync（极端 all-in 边界巧合允许极少量；这里要求严格 0
        // 以钉死「无结构 gap」假设——若将来抽象改动引入 gap，本断言会立刻 fail）。
        assert_eq!(
            desyncs, 0,
            "nolimp×preopen（无结构 gap）不该 desync，实得 {desyncs}"
        );
    }

    /// 结构性 gap 检测确实生效：baseline（含 open-limp）vs nolimp（no-limp）—— baseline 一旦
    /// limp，nolimp 影子无对应节点 → 必触发 desync。本测验证引擎**不静默吞掉**该 gap
    /// （desync_hands > 0），即正确性边界可见。用 RandomNoFold-ish 强制 limp 的策略放大触发。
    #[test]
    fn baseline_vs_nolimp_detects_structural_gap() {
        let table = stub_table();
        let cfg = TableConfig::default_6max_100bb();
        // baseline = first_small（允许 open-limp）。
        let (b_abs, b_rules) = first_small_6max(3);
        let baseline = SimplifiedNlheGame::new_with_abstraction(
            Arc::clone(&table),
            cfg.clone(),
            b_abs,
            b_rules,
        )
        .expect("baseline game");
        let (nl_abs, nl_rules) = {
            let (a, mut r) = first_small_6max(3);
            r.no_open_limp = true;
            (a, r)
        };
        let nolimp = SimplifiedNlheGame::new_with_abstraction(
            Arc::clone(&table),
            cfg.clone(),
            nl_abs,
            nl_rules,
        )
        .expect("nolimp game");

        // 偏好 Call（含 open-limp）的策略：把概率压在 Call tag 上，逼出 limped 池。
        let prefer_limp = |_info: &InfoSetId, n: usize| {
            // 无法直接看 action tag（只有 n），退而求其次：均匀即可——baseline UTG 均匀
            // 也有 ~1/k limp 概率，200 手足以触发若干 limped 池。
            vec![1.0 / n as f64; n]
        };
        let baseline_c = Contestant {
            game: &baseline,
            strategy: &prefer_limp,
            label: "baseline".into(),
        };
        let nolimp_c = Contestant {
            game: &nolimp,
            strategy: &prefer_limp,
            label: "nolimp".into(),
        };
        let contestants = [
            Contestant {
                game: baseline_c.game,
                strategy: baseline_c.strategy,
                label: baseline_c.label.clone(),
            },
            Contestant {
                game: nolimp_c.game,
                strategy: nolimp_c.strategy,
                label: nolimp_c.label.clone(),
            },
        ];
        let n = cfg.n_seats as usize;
        let mut desyncs = 0u64;
        let mut completed = 0u64;
        // baseline 坐 1 座、nolimp 占其余：baseline limp → nolimp 影子撞 gap。
        let mut seat_bp = vec![1usize; n];
        seat_bp[3] = 0; // UTG = baseline（开池位，limp 高发）。
        for hand in 0..400u64 {
            let hand_seed = 0x0BAD_5EED ^ hand;
            let mut rng = ChaCha20Rng::from_seed(hand_seed ^ 0x3333);
            match play_cross_abstraction_hand(
                &contestants,
                &seat_bp,
                &cfg,
                hand_seed,
                &mut rng,
                512,
            ) {
                Ok(_) => completed += 1,
                Err(HandError::Desync(_)) => desyncs += 1,
                Err(e) => panic!("意外非 desync 失败: {e:?}"),
            }
        }
        assert!(
            desyncs > 0,
            "baseline(limp)×nolimp 应检出结构性 gap（desync>0），实得 desync={desyncs} completed={completed}"
        );
    }
}
