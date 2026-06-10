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

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use rayon::prelude::*;

use crate::abstraction::action::{AbstractAction, ActionAbstraction, StreetActionAbstraction};
use crate::abstraction::info::InfoSetId;
use crate::core::rng::{ChaCha20Rng, RngSource};
use crate::core::{Card, ChipAmount, PlayerStatus, Rank, Street, Suit};
use crate::rules::action::Action;
use crate::rules::config::TableConfig;
use crate::rules::state::GameState;
use crate::training::game::Game;
use crate::training::nlhe::{SimplifiedNlheGame, SimplifiedNlheState};
use crate::training::nlhe_betting_tree::AbstractActionTag;
use crate::training::sampling::sample_discrete;
use crate::training::subgame::{should_search, subgame_search, ResolveRoot, SubgameSearchConfig};
use crate::training::subgame_leaf_value::LeafValueTables;

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
/// 通常 = `|info, _n| trainer.average_strategy(*info)`。`+ Sync`：评测层 rayon 并行跑独立
/// 手时跨线程只读共享（`DenseNlheEsMccfrTrainer::average_strategy` 是 `&self` 只读）。
pub struct Contestant<'a> {
    pub game: &'a SimplifiedNlheGame,
    pub strategy: &'a (dyn Fn(&InfoSetId, usize) -> Vec<f64> + Sync),
    pub label: String,
    /// S6 实时搜索（6a）：`Some` = 该参赛者在命中触发面（[`should_search`]）的决策点用
    /// subgame re-solve 出分布、失败回落 blueprint；`None` = 纯 blueprint（默认、byte-equal
    /// 旧行为）。用于「search-on vs search-off」不退化探针（`tools/six_max_search_probe`）。
    pub search: Option<SubgameSearchConfig>,
    /// S6 6b：depth-limit 搜索的 blueprint 叶子续局值表（[`LeafValueTables`]）。`search` 的
    /// `depth_limit=true` 时必须 `Some`（否则该次搜索 `Err` 回落 blueprint）；`depth_limit=false`
    /// （6a 解到终局）时不被读，传 `None`。
    pub leaf_values: Option<Arc<LeafValueTables>>,
}

/// 实时搜索调用计数（跨 rayon 线程共享，`Relaxed` 即可——只做总量统计、不做同步）。
/// `attempts` = 命中触发面且配了 search 的决策点数；`successes` = 其中 [`subgame_search`]
/// 返回 `Ok`（真用了搜索分布）的数。`attempts - successes` = 回落 blueprint 数。fallback 率
/// 高 → 不退化 CI 主要由「与 blueprint 相同的决策」主导，须据此解读（见 `subgame.rs` confound）。
#[derive(Default)]
pub struct SearchObserver {
    pub attempts: AtomicU64,
    pub successes: AtomicU64,
    /// 审核遥测：subgame CFR 的 traverser 在 `[0, n_seats)` 轮转（`trainer.rs` ES `step`），但
    /// 子树里**弃牌 / all-in 座位零决策节点**（规则引擎只让 `Active` 座当 `current_player`）→
    /// 这些座当 traverser 的那一步纯零学习（采一条路径、算个收益、丢弃、不累积任何 regret/σ）。
    /// 下列计数实测浪费比例：`traverser_steps` = 真跑 CFR 的 solve 的 Σ`iterations`；`wasted_steps`
    /// = 其中 traverser 落在弃牌/all-in 座的步数；`effective_seats_sum`/`solves_measured` = 算均
    /// 有效座位数（= 子树根仍 `Active` 的座数 = 有决策节点的充要集）。
    pub traverser_steps: AtomicU64,
    pub wasted_steps: AtomicU64,
    pub effective_seats_sum: AtomicU64,
    pub solves_measured: AtomicU64,
}

impl SearchObserver {
    /// 记一次真跑 CFR（[`subgame_search`] 返回 `Ok`）的 subgame solve 的 traverser 浪费。
    /// `active[seat]` = 该座在子树根仍 `Active`（= 子树里有 ≥1 决策节点的充要条件：`Active` 座
    /// 必在本街某线行动，`Folded`/`AllIn` 座永不行动）。ES traverser 调度从 `update_count == 0`
    /// 起 = 第 `t` 步 traverser `= t % n`，故座 `s` 摊到 `iterations/n (+1 if s < iterations%n)` 步。
    /// `live_traversers` = solve 已开 `SubgameSearchConfig::live_traversers`（traverser 只轮
    /// Active 座，缺口①续修复）→ 浪费恒 0（effective seats 照记，作修复前后对照）。
    fn record_solve_waste(
        &self,
        iterations: u64,
        n: usize,
        active: &[bool],
        live_traversers: bool,
    ) {
        let mut wasted = 0u64;
        let mut eff = 0u64;
        for (s, &act) in active.iter().enumerate().take(n) {
            if act {
                eff += 1;
            } else if !live_traversers {
                wasted += iterations / n as u64 + u64::from((s as u64) < iterations % n as u64);
            }
        }
        self.traverser_steps
            .fetch_add(iterations, Ordering::Relaxed);
        self.wasted_steps.fetch_add(wasted, Ordering::Relaxed);
        self.effective_seats_sum.fetch_add(eff, Ordering::Relaxed);
        self.solves_measured.fetch_add(1, Ordering::Relaxed);
    }
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
    search_obs: Option<&SearchObserver>,
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

    // round-start 快照（§6 #1 RoundStart 重解用）：每街首决策点 = 该街 betting-round 起点
    // （postflop 首个行动者面对无本街下注）。在 apply 之前于 loop 顶 snapshot → 必是轮起点。
    // round_within = 当前街 round-start 以来的真实动作序 (动作, 是否令行动者 all-in)——
    // deep_menu mid-round 在子树上重放导航用（subgame_search within_round_real，缺口③细化）；
    // 街变清空（收街动作属上一街，与 openpoker_advisor::build_real_auth 同口径）。
    let mut round_start: Option<GameState> = None;
    let mut round_start_street: Option<Street> = None;
    let mut round_within: Vec<(Action, bool)> = Vec::new();

    for decision_ordinal in 0..max_actions {
        if auth.is_terminal() {
            return finalize_payoffs(&auth, n);
        }
        let Some(actor) = auth.current_player() else {
            return finalize_payoffs(&auth, n);
        };
        let actor_idx = actor.0 as usize;
        let bp_idx = seat_blueprint[actor_idx];

        // 维护 round-start 快照：每街首决策点 = 该街 betting-round 起点（loop 顶、apply 之前
        // snapshot → 本街尚无下注 = 轮起点）。街变即重 snapshot；同街内复用同一快照 → 同一轮
        // 多决策的 RoundStart 重解共享 byte-identical 输入（§6 #2 一致性）。
        if round_start_street != Some(auth.street()) {
            round_start = Some(auth.clone());
            round_start_street = Some(auth.street());
            round_within.clear();
        }

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
        // S6 实时搜索插桩点（设计 §4.1）：该 actor 配了 search 且命中触发面
        // （[`should_search`]）→ subgame re-solve 出分布；否则、或搜索失败 → 回落 blueprint
        // average strategy。`outgoing_action`（L下方）与影子推进**完全不变**（低侵入关键）。
        // `search_obs` 计搜索 attempts / successes（探针读 fallback 率 = 1 - succ/att，判
        // 搜索是否真在跑还是大多回落——是解读不退化 CI 的关键，见 subgame.rs 顶部 confound）。
        let blueprint_dist =
            || strategy_distribution(&info, &legal_abs, contestants[bp_idx].strategy);
        let dist = match &contestants[bp_idx].search {
            Some(scfg) if should_search(&auth, scfg.trigger) => {
                if let Some(o) = search_obs {
                    o.attempts.fetch_add(1, Ordering::Relaxed);
                }
                // RoundStart（默认，§6 #1）：从本街 round-start 快照为根重解；CurrentDecision
                // （A/B）：从当前权威态为根。round_start 由 loop 顶 snapshot 保证为本街轮起点。
                let root_state: &GameState = match scfg.resolve_root {
                    ResolveRoot::RoundStart => round_start.as_ref().unwrap_or(&auth),
                    ResolveRoot::CurrentDecision => &auth,
                };
                match subgame_search(
                    &auth,
                    root_state,
                    contestants[bp_idx].game,
                    &legal_abs,
                    node_id,
                    contestants[bp_idx].strategy,
                    scfg,
                    contestants[bp_idx].leaf_values.as_ref(),
                    Some(&round_within),
                    hand_seed,
                    decision_ordinal as u64,
                ) {
                    Ok(d) => {
                        if let Some(o) = search_obs {
                            o.successes.fetch_add(1, Ordering::Relaxed);
                            // 实测 traverser 浪费：子树根 = root_state（CurrentDecision = auth /
                            // RoundStart = round_start）；有决策节点的座 = 根仍 Active 的座。
                            let active: Vec<bool> = root_state
                                .players()
                                .iter()
                                .map(|p| matches!(p.status, PlayerStatus::Active))
                                .collect();
                            o.record_solve_waste(scfg.iterations, n, &active, scfg.live_traversers);
                        }
                        d
                    }
                    Err(_) => blueprint_dist(),
                }
            }
            _ => blueprint_dist(),
        };
        let chosen = sample_discrete(&dist, sample_rng);

        // outgoing：真实 pot 算尺寸 → apply 权威局。
        let applied = outgoing_action(&auth, contestants[bp_idx].game.abstraction(), chosen)
            .map_err(HandError::Illegal)?;
        auth.apply(applied)
            .map_err(|e| HandError::Illegal(format!("权威 apply({applied:?}) 非法: {e:?}")))?;
        let applied_is_all_in = auth.players()[actor_idx].status == PlayerStatus::AllIn;
        // 收街动作属上一街（street 已变 → 不入序；loop 顶将重 snapshot + 清空）。
        if round_start_street == Some(auth.street()) {
            round_within.push((applied, applied_is_all_in));
        }

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
    /// S6 实时搜索调用统计（[`SearchObserver`]）：`search_attempts` = 命中触发面的决策点数，
    /// `search_successes` = 其中真用了搜索分布（[`subgame_search`] 返回 `Ok`）的数。fallback 率
    /// = 1 - successes/attempts。两者均为 0 = 无参赛者配 search（纯 blueprint，旧行为）。
    pub search_attempts: u64,
    pub search_successes: u64,
    /// 审核遥测（[`SearchObserver`]）：subgame CFR traverser 浪费——`search_traverser_steps` =
    /// 真跑 solve 的 Σ`iterations`；`search_wasted_steps` = 其中 traverser 落弃牌/all-in 座的零
    /// 学习步；`search_effective_seats_sum`/`search_solves_measured` = 算均有效座位数。浪费比
    /// = `wasted/traverser_steps`，修复（traverser 只轮 live 座）的潜在 effective-iters 加速 =
    /// `1/(1−浪费比)`。
    pub search_traverser_steps: u64,
    pub search_wasted_steps: u64,
    pub search_effective_seats_sum: u64,
    pub search_solves_measured: u64,
    /// 逐手 hero 净 PnL（chips），**对齐完整 task 列表**（`hero_seat × hand_idx`，长
    /// `hands_attempted`）：`Some(pnl)` = 计入手，`None` = desync/illegal 排除。两次同 `seed`/
    /// `hands_per_seat` 的 h2h 的本向量**逐下标同手**（同发牌+同 sample rng 流，仅搜索配置不同）→
    /// 探针据此算**配对差 CI**（臂间差方差远低于 marginal，§11.5 统计注意）。
    pub per_hand_pnl: Vec<Option<f64>>,
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
            search: hero.search,
            leaf_values: hero.leaf_values.clone(),
        },
        Contestant {
            game: field.game,
            strategy: field.strategy,
            label: field.label.clone(),
            search: field.search,
            leaf_values: field.leaf_values.clone(),
        },
    ];

    let mut hero_pnls: Vec<f64> = Vec::with_capacity(h2h_config.hands_per_seat as usize * n);
    let mut per_pos_sum = vec![0.0_f64; n];
    let mut per_pos_hands = vec![0u64; n];
    let mut desync_hands = 0u64;
    let mut illegal_hands = 0u64;
    // 实时搜索调用计数（跨 rayon 线程共享原子累加；search-off 两边时恒 0）。
    let search_obs = SearchObserver::default();

    // 每手独立（off-tree 自对弈无跨手状态）→ rayon 并行所有 (hero_seat, hand_idx)。
    // 每手 seed 由 (seed, hero_seat, hand) 确定派生 → 结果与线程调度无关、可复现。
    enum Outcome {
        Pnl { offset: usize, pnl: f64 },
        Desync,
        Illegal,
    }
    let tasks: Vec<(usize, u64)> = (0..n)
        .flat_map(|hero_seat| (0..h2h_config.hands_per_seat).map(move |h| (hero_seat, h)))
        .collect();
    let outcomes: Vec<Outcome> = tasks
        .par_iter()
        .map(|&(hero_seat, hand_idx)| {
            // hero 坐 hero_seat，其余座是 field。
            let mut seat_bp = vec![1usize; n];
            seat_bp[hero_seat] = 0;
            let offset = (hero_seat + n - button) % n;
            let hand_seed = mix3(h2h_config.seed, hero_seat as u64, hand_idx);
            let mut sample_rng = ChaCha20Rng::from_seed(mix3(hand_seed, 0xA5A5_A5A5, 1));
            match play_cross_abstraction_hand(
                &contestants,
                &seat_bp,
                config,
                hand_seed,
                &mut sample_rng,
                h2h_config.max_actions_per_hand,
                Some(&search_obs),
            ) {
                Ok(pnls) => Outcome::Pnl {
                    offset,
                    pnl: pnls[hero_seat],
                },
                Err(HandError::Desync(_)) | Err(HandError::NonTerminal) => Outcome::Desync,
                Err(HandError::Illegal(_)) => Outcome::Illegal,
            }
        })
        .collect();
    let attempted = outcomes.len() as u64;
    // 串行 reduce（collect 保 task 顺序 → f64 加法顺序确定、可复现）。per_hand_pnl 对齐**完整
    // task 列表**（Some=计入 / None=desync/illegal）→ 跨两臂逐下标同手，供探针配对差。
    let mut per_hand_pnl: Vec<Option<f64>> = Vec::with_capacity(outcomes.len());
    for o in &outcomes {
        match o {
            Outcome::Pnl { offset, pnl } => {
                hero_pnls.push(*pnl);
                per_pos_sum[*offset] += *pnl;
                per_pos_hands[*offset] += 1;
                per_hand_pnl.push(Some(*pnl));
            }
            Outcome::Desync => {
                desync_hands += 1;
                per_hand_pnl.push(None);
            }
            Outcome::Illegal => {
                illegal_hands += 1;
                per_hand_pnl.push(None);
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
        search_attempts: search_obs.attempts.load(Ordering::Relaxed),
        search_successes: search_obs.successes.load(Ordering::Relaxed),
        search_traverser_steps: search_obs.traverser_steps.load(Ordering::Relaxed),
        search_wasted_steps: search_obs.wasted_steps.load(Ordering::Relaxed),
        search_effective_seats_sum: search_obs.effective_seats_sum.load(Ordering::Relaxed),
        search_solves_measured: search_obs.solves_measured.load(Ordering::Relaxed),
        per_hand_pnl,
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
    use crate::abstraction::action::BetRatio;
    use crate::abstraction::bucket_table::{BucketConfig, BucketTable};
    use crate::training::nlhe_betting_tree::{first_small_6max, first_small_preopen_6max};
    use crate::training::subgame::SearchTrigger;
    use std::sync::Arc;

    fn stub_table() -> Arc<BucketTable> {
        Arc::new(BucketTable::stub_for_postflop(
            BucketConfig::default_500_500_500(),
        ))
    }

    // 机制测试统一用 **N=2 redirect**（debug 建树 78,852 节点、快）；结构性质（limp gap /
    // 开池尺寸差异）N=2 与 N=3 等价。生产 h2h run 用真训练的 N=3 ckpt（release）。
    fn nolimp_game() -> SimplifiedNlheGame {
        let (a, mut r) = first_small_6max(2);
        r.no_open_limp = true;
        SimplifiedNlheGame::new_with_abstraction(
            stub_table(),
            TableConfig::default_6max_100bb(),
            a,
            r,
        )
        .expect("nolimp game")
    }
    fn preopen_game() -> SimplifiedNlheGame {
        let (a, r) = first_small_preopen_6max(2);
        SimplifiedNlheGame::new_with_abstraction(
            stub_table(),
            TableConfig::default_6max_100bb(),
            a,
            r,
        )
        .expect("preopen game")
    }
    fn baseline_game() -> SimplifiedNlheGame {
        let (a, r) = first_small_6max(2);
        SimplifiedNlheGame::new_with_abstraction(
            stub_table(),
            TableConfig::default_6max_100bb(),
            a,
            r,
        )
        .expect("baseline game")
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

    /// 纯**尺寸**差异（off-tree 该忠实处理）：把 preopen 的 0.5pot 小开池（2.25BB）的真实
    /// `to` 喂给 nolimp 影子（preflop 仅 {1.0} 档）→ [`advance_shadow_by_applied`] 返回 Ok
    /// 的 **Raise/AllIn**（aggressive，更大的开池），绝不报结构性 gap。证 size 差异不误判。
    #[test]
    fn advance_size_diff_maps_to_aggressive_not_error() {
        // 取 preopen 根（UTG 开池）的 0.5pot 开池真实 to。
        let preopen = preopen_game();
        let mut prng = ChaCha20Rng::from_seed(0x0151_2E01);
        let pre_root = preopen.root(&mut prng);
        let open_to = SimplifiedNlheGame::legal_actions(&pre_root)
            .iter()
            .find_map(|a| match a {
                AbstractAction::Raise { to, ratio_label } if *ratio_label == BetRatio::HALF_POT => {
                    Some(*to)
                }
                _ => None,
            })
            .expect("preopen 根应有 0.5pot(2.25BB) 开池档");

        let nolimp = nolimp_game();
        let mut nrng = ChaCha20Rng::from_seed(0x0151_2E02);
        let mut nl_root = nolimp.root(&mut nrng);
        let adv = advance_shadow_by_applied(
            &mut nl_root,
            Action::Raise { to: open_to },
            false,
            &mut nrng,
        )
        .expect("2.25BB 开池映进 nolimp 影子应成功（纯尺寸差异，非结构 gap）");
        assert!(
            matches!(
                AbstractActionTag::of(&adv),
                AbstractActionTag::Raise(_) | AbstractActionTag::AllIn
            ),
            "应映成 Raise/AllIn（aggressive，更大开池），实得 {:?}",
            AbstractActionTag::of(&adv)
        );
    }

    /// **结构性** gap（off-tree 无法忠实、必须显式报错）：nolimp 影子的 UTG 开池节点没有
    /// open-limp `Call`，对手一个**被动 limp**（`Action::Call`、非 all-in）喂进来 →
    /// [`advance_shadow_by_applied`] 返回 `Err`（不静默塌 AllIn 改 kind）。这是 S5 正确性
    /// 边界的钉子：passive→aggressive 的 kind 变会污染回合 / 价值，引擎拒绝它。
    #[test]
    fn advance_passive_limp_into_nolimp_errors() {
        let nolimp = nolimp_game();
        let mut nrng = ChaCha20Rng::from_seed(0x0151_2E03);
        let mut nl_root = nolimp.root(&mut nrng);
        // 前置确认：nolimp 根（UTG）确实无 open-limp Call。
        assert!(
            !SimplifiedNlheGame::legal_actions(&nl_root)
                .iter()
                .any(|a| matches!(a, AbstractAction::Call { .. })),
            "no_open_limp：nolimp 根（UTG）应无 open-limp Call"
        );
        let r = advance_shadow_by_applied(&mut nl_root, Action::Call, false, &mut nrng);
        assert!(
            r.is_err(),
            "open-limp 进 no-limp 影子应报结构性 gap Err，实得 {r:?}"
        );
    }

    /// nolimp×preopen 全程对局（uniform 策略、N=2、多 seed）：所有**完成**的手 6 座 PnL
    /// 守恒（Σ==0）—— 守恒能让任何回合漂移 / 记账错 fail（核心 lockstep 证明）。
    /// 不强约束 desync 计数：uniform 制造大量 all-in 战，开池尺寸差异在 all-in 边界**偶发**
    /// desync 属已知近似（被引擎检出并排除），与「结构性 gap」不同。
    #[test]
    fn nolimp_vs_preopen_completed_hands_conserve() {
        let nolimp = nolimp_game();
        let preopen = preopen_game();
        let uniform = |_info: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        let contestants = [
            Contestant {
                game: &nolimp,
                strategy: &uniform,
                label: "nolimp".into(),
                search: None,
                leaf_values: None,
            },
            Contestant {
                game: &preopen,
                strategy: &uniform,
                label: "preopen".into(),
                search: None,
                leaf_values: None,
            },
        ];
        let cfg = TableConfig::default_6max_100bb();
        let n = cfg.n_seats as usize;
        let mut completed = 0u64;
        for hero_seat in 0..n {
            let mut seat_bp = vec![1usize; n];
            seat_bp[hero_seat] = 0;
            for hand in 0..150u64 {
                let hand_seed = 0x0D15_EA5E ^ ((hero_seat as u64) << 32) ^ hand;
                let mut rng = ChaCha20Rng::from_seed(hand_seed ^ 0x9999);
                match play_cross_abstraction_hand(
                    &contestants,
                    &seat_bp,
                    &cfg,
                    hand_seed,
                    &mut rng,
                    512,
                    None,
                ) {
                    Ok(pnls) => {
                        completed += 1;
                        let sum: f64 = pnls.iter().sum();
                        assert!(sum.abs() < 1e-6, "完成的手须守恒 Σ==0，实得 {sum}");
                        assert_eq!(pnls.len(), n);
                    }
                    Err(HandError::Desync(_)) => {}
                    Err(e) => panic!("意外非 desync 失败: {e:?}"),
                }
            }
        }
        assert!(completed > 0, "应有手完成（不能全 desync）");
    }

    /// 端到端结构性 gap 检出：nolimp 坐 1 座 hero、baseline（含 open-limp）占其余 field →
    /// 任一 baseline field 座 open-limp，nolimp hero 影子推进该 limp 时撞 gap → desync。
    /// 验证引擎不静默吞掉（desync>0），即 S5 baseline 类对比的不可信性显式可见。
    #[test]
    fn baseline_field_open_limp_detected_as_desync() {
        let nolimp = nolimp_game();
        let baseline = baseline_game();
        let uniform = |_info: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        // 0 = nolimp(hero)、1 = baseline(field)。
        let contestants = [
            Contestant {
                game: &nolimp,
                strategy: &uniform,
                label: "nolimp".into(),
                search: None,
                leaf_values: None,
            },
            Contestant {
                game: &baseline,
                strategy: &uniform,
                label: "baseline".into(),
                search: None,
                leaf_values: None,
            },
        ];
        let cfg = TableConfig::default_6max_100bb();
        let n = cfg.n_seats as usize;
        let mut seat_bp = vec![1usize; n]; // 全 baseline field…
        seat_bp[0] = 0; // …除 seat0 = nolimp hero。
        let mut desyncs = 0u64;
        for hand in 0..400u64 {
            let hand_seed = 0x0BAD_5EED ^ hand;
            let mut rng = ChaCha20Rng::from_seed(hand_seed ^ 0x3333);
            if let Err(HandError::Desync(_)) = play_cross_abstraction_hand(
                &contestants,
                &seat_bp,
                &cfg,
                hand_seed,
                &mut rng,
                512,
                None,
            ) {
                desyncs += 1;
            }
        }
        assert!(
            desyncs > 0,
            "baseline field open-limp 应被 nolimp hero 影子检出为结构性 gap（desync>0），实得 {desyncs}"
        );
    }

    /// S6 实时搜索集成安全：seat0 search-on（[`SubgameSearchConfig`]）、其余 search-off，
    /// 两边**同一** nolimp game（自对弈、同抽象 → 无结构 desync）。搜索只改决策分布、不碰
    /// 筹码记账：①完成的手仍 6 座守恒（Σ==0，能让任何回合漂移 / 记账错 fail）；②同 seed
    /// 两次跑逐手 PnL byte-equal（搜索 RNG 由 (hand_seed, decision_ordinal) 确定派生）。
    /// 注：search 触发面 + 出合法分布由 `subgame::tests` 钉死；本测试钉的是**接进 live 决策
    /// 环后不破对局正确性 + 可复现**。
    #[test]
    fn search_on_conserves_and_reproducible() {
        let game = nolimp_game();
        let uniform = |_info: &InfoSetId, n: usize| vec![1.0 / n as f64; n];
        // AllPostflop + RoundStart（默认）：exercise §6 #1 round-start 重解全路径（within-round
        // 导航 + round-stable seed），在 6-max 自对弈下钉「接 live 后不破守恒 + 可复现」。
        let scfg = SubgameSearchConfig {
            iterations: 300,
            max_subtree_nodes: 8000,
            seed: 0x5EA2_C400_5EA2_C400,
            use_blueprint_range: true,
            trigger: SearchTrigger::AllPostflop,
            resolve_root: ResolveRoot::RoundStart,
            depth_limit: false,
            biased_leaf: false,
            lcfr: false,
            time_budget: None,
            deep_menu: false,
            live_traversers: false,
        };
        let cfg = TableConfig::default_6max_100bb();
        let n = cfg.n_seats as usize;

        let run = || {
            let contestants = [
                Contestant {
                    game: &game,
                    strategy: &uniform,
                    label: "search-on".into(),
                    search: Some(scfg),
                    leaf_values: None,
                },
                Contestant {
                    game: &game,
                    strategy: &uniform,
                    label: "search-off".into(),
                    search: None,
                    leaf_values: None,
                },
            ];
            let mut seat_bp = vec![1usize; n];
            seat_bp[0] = 0; // seat0 = search-on hero；其余 search-off。
            let obs = SearchObserver::default(); // 顺带 exercise Some(&obs) 计数路径。
            let mut completed = 0u64;
            let mut pnls_all: Vec<Vec<f64>> = Vec::new();
            for hand in 0..80u64 {
                let hand_seed = 0x5EA2_C400 ^ hand;
                let mut rng = ChaCha20Rng::from_seed(hand_seed ^ 0x55);
                match play_cross_abstraction_hand(
                    &contestants,
                    &seat_bp,
                    &cfg,
                    hand_seed,
                    &mut rng,
                    512,
                    Some(&obs),
                ) {
                    Ok(pnls) => {
                        completed += 1;
                        let sum: f64 = pnls.iter().sum();
                        assert!(
                            sum.abs() < 1e-6,
                            "search-on 完成的手须守恒 Σ==0，实得 {sum}"
                        );
                        assert_eq!(pnls.len(), n);
                        pnls_all.push(pnls);
                    }
                    Err(HandError::Desync(_)) => {}
                    Err(e) => panic!("意外非 desync 失败: {e:?}"),
                }
            }
            let att = obs.attempts.load(Ordering::Relaxed);
            let succ = obs.successes.load(Ordering::Relaxed);
            assert!(succ <= att, "successes ({succ}) 不能超 attempts ({att})");
            (completed, pnls_all, att, succ)
        };

        let (c1, p1, a1, s1) = run();
        let (c2, p2, a2, s2) = run();
        assert!(c1 > 0, "应有手完成（不能全 desync）");
        assert_eq!(c1, c2, "可复现：两次完成手数一致");
        assert_eq!(p1, p2, "可复现：search-on 逐手 PnL byte-equal");
        // 可复现：同 seed 两次跑搜索调用计数也一致（plumbing 确定性）。
        assert_eq!((a1, s1), (a2, s2), "可复现：search attempts/successes 一致");
    }
}
