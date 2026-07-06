//! 启发式 9-max 自对弈仿真（离线迭代回归门）。
//!
//! 用权威规则引擎 [`GameState`]（N-generic，side pot/showdown 已验证）发牌、结算。hero（座 0）
//! = 启发式引擎，其余座 = baseline 原型（nit/station/maniac/tag/random）混合场。每手轮转按钮、
//! 重置起始码深（`--start-bb`，默认 100bb；200/400 = 深码回归区），累计 hero 净盈亏 → mbb/g + CI95。
//!
//! **关键**：hero 的决策走**真实** advisor 路径（GameState → Request → [`HeuristicBot::respond`]
//! → Action），因此 sim 同时**验证下注重建**——把 [`GameView`] 重建的 pot/to_call 与权威
//! GameState 对照，统计漂移率（重建 bug 在这里现形）。
//!
//! `--exploit` 开对手画像（与 live driver 同语义：每手给每座稳定名 `p{seat}`、按 board 长度
//! 标街、按 SB/BB 座标 forced_blind，手末 `observe_hand` 喂画像）——让剥削层在 sim 里可测/可门控。
//! 默认关：`exploit=false` 时 `names` 空、不 observe，行为与历史基线 byte 级一致。
//!
//! 用法：
//! ```text
//! heuristic9_sim [--hands N] [--seats K] [--bb 20] [--start-bb 100] [--samples S] [--seed X]
//!                [--field mixed|nit|station|maniac|tag|random|limpy|pairedtrap|3bp|flushtrap|3bpfold] [--mirror]
//!                [--exploit] [--config cfg.json] [--trace-summary]
//! ```
//! `--config` 覆盖 HeuristicConfig（partial JSON，缺字段走默认）——无需重编即可扫旋钮。
//! `--trace-summary` 结束时追加打印 Phase A 节点热度表（叶节点 × 命中次数 + legalize 降级数，
//! `docs/scenario_heuristic_design.md` §6）；默认关，关时输出与历史基线逐字节一致。

use poker::core::{Card, Rank, Suit};
use poker::heuristic::features::{draws, made_class, monster_non_nut, nonnut_flush, MadeClass};
use poker::heuristic::view::{GameView, HistAction, Request, ValidActions};
use poker::heuristic::{EquityEngine, HeuristicBot, HeuristicConfig, ObservedAction};
use poker::rules::action::{Action, LegalActionSet};
use poker::rules::config::TableConfig;
use poker::rules::state::GameState;
use poker::ChipAmount;

// ===========================================================================
// baseline 原型
// ===========================================================================

#[derive(Copy, Clone, Debug, PartialEq)]
enum Archetype {
    Nit,
    Station,
    Maniac,
    Tag,
    Random,
    /// 被动开池跛入者 / 跟注站混合：first-in 用宽 range **跛入（仅跟 BB，不加注）**，
    /// 制造多人 limped 底池——这是 hero `limp_behind` 路径的测试载荷。
    Limper,
    /// 对子面陷阱：松被动入池见多翻牌；**仅在成对面持 trips+ 时才升级加注战**（大注/再加注），
    /// 否则被动 check/call、绝不诈唬加注；非对子面打法同 Tag。这是 hero 「对子面非坚果价值再加注」
    /// 漏洞的测试载荷（run24 −7031 复盘）——专门惩罚把两对/顶对加进 trips/boat 深战。
    PairedTrap,
    /// 3bet 池造靶（P1-5，Phase C 第一轮）之一：**宽开池 + 被再加注后爱跟**的开池跟注机。
    /// first-in 极宽 raise-or-fold 开池（制造大量单加注池底料）；被 3bet 后宽跟、几乎不 4bet；
    /// 翻后站式粘跟。让 hero 高频作为 3bet 者进翻后（喂 `post.unopened.{value_bet,cbet_bluff}.3bp`）。
    WideOpener,
    /// 3bet 池造靶（P1-5）之二：**高频 3bet 单次加注**的 3bet 机。面对 open 用宽价值域 +
    /// 低频空气混合再加注；被 4bet 后仅顶级牌继续；翻后作为进攻方高频 c-bet（含空气）。
    /// 喂 hero 的 `pre.facing_raise.*.vs_3bet` 与 hero 作为 caller 的 3bet 池。
    ThreeBettor,
    /// 成花/成顺湿板非坚果 made commit 造靶（Phase C 第二轮，run33 −285BB 复盘）：**松被动
    /// 入池见多翻牌**（宽跟制造大量到河的成花机会）；成花/成顺板上**仅持坚果同类**（nut flush /
    /// boat via `monster_non_nut==false`）时才升级下注/加注战（大注 / 池量再加注），把 hero 的
    /// **非坚果同花/顺子** value_raise 全下钓进 drawing-dead；非坚果同类被动 check/call（让 hero
    /// 的价值注被更差的牌支付、把漏洞隔离在 re-raise 上）；非成花/成顺板打法同 Tag（不污染
    /// 「正常价值加注」对照）。这是 hero 「成花板非坚果 stack-off」漏洞的测试载荷。
    FlushTrap,
    /// 3bp c-bet 频率跨靶复核造靶（Phase C 第二轮候选②）：**翻后爱弃**的 3bet 池跟注机。
    /// 翻前**逐字节同 `WideOpener`**（宽开 + 被 3bet 后跟——喂 hero 作 3bettor 进翻后、保节点密度），
    /// 翻后**高弃**：面对 c-bet 仅 TopPair+ / 好赔率强听牌继续，其余全弃 → 拉高 hero c-bet 的
    /// fold-equity。与 `WideOpener`（站式粘跟 = 低 fold-equity）构成 `cbet_bluff.3bp` 频率最优值的
    /// 对靶：若最优频率随对手翻后弃率反号（如 float 先例），此靶应偏好**高** c-bet 频率。
    ThreebetFolder,
    Hero, // 仅 mirror 模式
}

/// board 是否成花可能（任一花色 ≥3 张）——顺子被压 / 同花在场的判据。
fn board_flushy(board: &[Card]) -> bool {
    let mut cnt = [0u8; 4];
    for c in board {
        cnt[(c.to_u8() % 4) as usize] += 1;
    }
    cnt.iter().any(|&c| c >= 3)
}

/// board 是否成对（任一 rank 出现 ≥2）。
fn board_paired(board: &[Card]) -> bool {
    let mut cnt = [0u8; 13];
    for c in board {
        cnt[(c.to_u8() / 4) as usize] += 1;
    }
    cnt.iter().any(|&c| c >= 2)
}

/// 成对面上我方是否持「三条」（某 hole 张匹配 board 成对 rank → trips/quads）。葫芦/四条已归
/// Monster，此处只补 trips；与 `made==Monster` 合用即覆盖 trips+。用于 `PairedTrap` 仅坚果建战。
fn holds_trips_on_paired(hole: [Card; 2], board: &[Card]) -> bool {
    let mut cnt = [0u8; 13];
    for c in board {
        cnt[(c.to_u8() / 4) as usize] += 1;
    }
    let h0 = (hole[0].to_u8() / 4) as usize;
    let h1 = (hole[1].to_u8() / 4) as usize;
    cnt[h0] >= 2 || cnt[h1] >= 2
}

struct SimRng(u64);
impl SimRng {
    fn new(seed: u64) -> SimRng {
        SimRng(seed ^ 0x2545_F491_4F6C_DD1D)
    }
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0xa076_1d64_78bd_642f);
        let s = self.0;
        let t = (s as u128).wrapping_mul((s ^ 0xe703_7ed1_a0b4_28db) as u128);
        (t as u64) ^ ((t >> 64) as u64)
    }
    fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

fn card_str(c: Card) -> String {
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

fn valid_from_legal(legal: &LegalActionSet) -> ValidActions {
    let (mut min_raise, mut max_raise) = (None, None);
    if let Some((mn, mx)) = legal.bet_range {
        min_raise = Some(mn.0);
        max_raise = Some(mx.0);
    } else if let Some((mn, mx)) = legal.raise_range {
        min_raise = Some(mn.0);
        max_raise = Some(mx.0);
    }
    ValidActions {
        can_check: legal.check,
        can_call: legal.call.is_some(),
        can_raise: legal.bet_range.is_some() || legal.raise_range.is_some(),
        min_raise,
        max_raise,
    }
}

/// 把启发式 Response（action/amount）映射回规则引擎 Action（合法、never illegal）。
fn response_to_action(legal: &LegalActionSet, action: &str, amount: Option<u64>) -> Action {
    match action {
        "fold" => {
            if legal.fold {
                Action::Fold
            } else if legal.check {
                Action::Check
            } else {
                Action::Call
            }
        }
        "check" => {
            if legal.check {
                Action::Check
            } else if legal.call.is_some() {
                Action::Call
            } else {
                Action::Fold
            }
        }
        "call" => {
            if legal.call.is_some() {
                Action::Call
            } else if legal.check {
                Action::Check
            } else {
                Action::Fold
            }
        }
        "raise" => {
            let to = amount.unwrap_or(0);
            if let Some((mn, mx)) = legal.bet_range {
                let t = to.clamp(mn.0, mx.0);
                Action::Bet { to: ChipAmount(t) }
            } else if let Some((mn, mx)) = legal.raise_range {
                let t = to.clamp(mn.0, mx.0);
                Action::Raise { to: ChipAmount(t) }
            } else if legal.call.is_some() {
                Action::Call
            } else if legal.check {
                Action::Check
            } else {
                Action::Fold
            }
        }
        _ => {
            if legal.check {
                Action::Check
            } else if legal.call.is_some() {
                Action::Call
            } else {
                Action::Fold
            }
        }
    }
}

// ===========================================================================
// baseline 策略
// ===========================================================================

/// baseline 决策（廉价：翻前查强度表，翻后查 made/draw）。
#[allow(clippy::too_many_arguments)]
fn baseline_action(
    arch: Archetype,
    state: &GameState,
    seat: usize,
    eq: &EquityEngine,
    rng: &mut SimRng,
) -> Action {
    let legal = state.legal_actions();
    let players = state.players();
    let hole = match players[seat].hole_cards {
        Some(h) => h,
        None => return safe_action(&legal),
    };
    let board = state.board();
    let strength = eq.hand_strength(hole[0], hole[1]);
    let pot = state.pot().0.max(1);
    let to_call = match legal.call {
        Some(c) => c.0.saturating_sub(players[seat].committed_this_round.0),
        None => 0,
    };
    let pot_odds = if to_call > 0 {
        to_call as f64 / (pot + to_call) as f64
    } else {
        0.0
    };
    let preflop = board.is_empty();

    // 下注尺寸 helper。
    let make_bet = |frac: f64, legal: &LegalActionSet| -> Option<Action> {
        let amt = (frac * pot as f64).round() as u64;
        if let Some((mn, mx)) = legal.bet_range {
            let me = players[seat].committed_this_round.0;
            let to = (me + amt).clamp(mn.0, mx.0);
            Some(Action::Bet { to: ChipAmount(to) })
        } else if let Some((mn, mx)) = legal.raise_range {
            let to = (state_high(state) + amt).clamp(mn.0, mx.0);
            Some(Action::Raise { to: ChipAmount(to) })
        } else {
            None
        }
    };

    if preflop {
        // Limper：被动开池跛入者。first-in（仅面对 BB）= 用宽 range 跛入（call BB，不加注），
        // 偶尔只用顶级强牌加注；面对跛入者继续跛入；面对加注用宽 range 跟注（站式），几乎不 3bet。
        if arch == Archetype::Limper {
            let facing = to_call > 0;
            // 是否仅面对 BB（即 first-in / 跛入底池，尚无人加注）。high bet == BB 的整数倍判定不可靠，
            // 改用「最高本街投入 <= BB」近似 first-in（盲注/limper 的投入都 == BB）。
            let high = state_high(state);
            let only_blinds = high <= state.config().big_blind.0;
            if !facing {
                // 无人下注（盲位 check option 或 BB）：用宽 range 偶尔加注顶级牌，否则过牌。
                if strength >= 0.72 {
                    if let Some(a) = make_bet(0.12, &legal) {
                        return a;
                    }
                }
                return safe_action(&legal);
            }
            if only_blinds {
                // first-in / 面对跛入者：宽 range（约 top 50%）跛入 = 只跟 BB，不加注。
                // 仅极顶级牌（~AA/KK/AKs 量级）才偶尔 iso-raise，频率低。
                if strength >= 0.74 && legal_can_raise(&legal) && rng.unit() < 0.30 {
                    if let Some(a) = make_bet(0.15, &legal) {
                        return a;
                    }
                }
                if strength >= 0.50 && legal.call.is_some() {
                    return Action::Call; // open-limp / limp-along
                }
                return safe_action(&legal);
            }
            // 面对真正的加注：站式宽跟，几乎不 3bet。
            if strength >= 0.46 && legal.call.is_some() {
                return Action::Call;
            }
            if pot_odds < 0.22 && legal.call.is_some() {
                return Action::Call; // 好赔率也跟
            }
            return safe_action(&legal);
        }
        // WideOpener：3bet 池造靶的「宽开池 + 被再加注后爱跟」一侧。
        // ThreebetFolder 翻前逐字节共享此分支（唯一差异在翻后弃率，见 postflop 分支）。
        if arch == Archetype::WideOpener || arch == Archetype::ThreebetFolder {
            let high = state_high(state);
            let only_blinds = high <= state.config().big_blind.0;
            if to_call == 0 {
                // 无人下注（BB check option）：仅顶级牌加注，否则过牌。
                if strength >= 0.62 {
                    if let Some(a) = make_bet(0.15, &legal) {
                        return a;
                    }
                }
                return safe_action(&legal);
            }
            if only_blinds {
                // first-in / 面对 limper：极宽 raise-or-fold 开池（单加注池底料）。
                if strength >= 0.40 {
                    if let Some(a) = make_bet(0.12, &legal) {
                        return a;
                    }
                }
                return safe_action(&legal);
            }
            // 面对再加注（含自己开池被 3bet）：爱跟——宽强度域 + 好赔率都跟，低频顶级 4bet。
            if strength >= 0.87 && legal_can_raise(&legal) && rng.unit() < 0.5 {
                if let Some(a) = make_bet(0.8, &legal) {
                    return a;
                }
            }
            if (strength >= 0.42 || pot_odds < 0.28) && legal.call.is_some() {
                return Action::Call;
            }
            return safe_action(&legal);
        }
        // FlushTrap：成花板非坚果 stack-off 造靶。松被动入池见多翻牌（宽跟制造到河成花机会）。
        if arch == Archetype::FlushTrap {
            let high = state_high(state);
            let only_blinds = high <= state.config().big_blind.0;
            if to_call == 0 {
                // BB check option / 无人下注：过牌见翻牌（便宜），仅顶级牌偶尔小注。
                if strength >= 0.80 {
                    if let Some(a) = make_bet(0.10, &legal) {
                        return a;
                    }
                }
                return safe_action(&legal);
            }
            if only_blinds {
                // first-in / 面对跛入者：极宽跟注跛入见翻牌；仅坚果牌低频 iso。
                if strength >= 0.85 && legal_can_raise(&legal) && rng.unit() < 0.20 {
                    if let Some(a) = make_bet(0.12, &legal) {
                        return a;
                    }
                }
                if strength >= 0.30 && legal.call.is_some() {
                    return Action::Call;
                }
                return safe_action(&legal);
            }
            // 面对加注：站式宽跟（见翻牌），几乎不 3bet。
            if (strength >= 0.42 || pot_odds < 0.30) && legal.call.is_some() {
                return Action::Call;
            }
            return safe_action(&legal);
        }
        // ThreeBettor：3bet 池造靶的「高频 3bet 单次加注」一侧。
        if arch == Archetype::ThreeBettor {
            let bb_amt = state.config().big_blind.0;
            let high = state_high(state);
            let only_blinds = high <= bb_amt;
            if to_call == 0 {
                if strength >= 0.60 {
                    if let Some(a) = make_bet(0.15, &legal) {
                        return a;
                    }
                }
                return safe_action(&legal);
            }
            if only_blinds {
                // first-in：正常 TAG 开池（本型的重点在面对 open 的再加注）。
                if strength >= 0.55 {
                    if let Some(a) = make_bet(0.12, &legal) {
                        return a;
                    }
                }
                return safe_action(&legal);
            }
            if high <= bb_amt * 9 / 2 {
                // 面对单次加注（open ≤ 4.5bb）：高频 3bet——宽价值域 + 低频空气混合。
                if legal_can_raise(&legal) && (strength >= 0.55 || rng.unit() < 0.10) {
                    if let Some(a) = make_bet(1.0, &legal) {
                        return a;
                    }
                }
                if strength >= 0.48 && legal.call.is_some() {
                    return Action::Call;
                }
                return safe_action(&legal);
            }
            // 面对更深加注（被 4bet / 冷面对 3bet+）：仅顶级牌继续。
            if strength >= 0.85 && legal_can_raise(&legal) {
                if let Some(a) = make_bet(1.2, &legal) {
                    return a;
                }
            }
            if strength >= 0.72 && legal.call.is_some() {
                return Action::Call;
            }
            return safe_action(&legal);
        }
        let (open_thr, call_thr, threebet, aggro) = match arch {
            Archetype::Nit => (0.62, 0.60, 0.70, 0.0),
            Archetype::Station => (0.50, 0.42, 0.72, 0.05),
            Archetype::Maniac => (0.42, 0.40, 0.52, 0.55),
            Archetype::Tag => (0.55, 0.50, 0.64, 0.15),
            // 松被动入池见多翻牌；几乎只用顶级牌 3bet（建池靠跟注而非翻前加注）。
            Archetype::PairedTrap => (0.52, 0.42, 0.85, 0.0),
            Archetype::Random => return random_action(&legal, state, seat, rng),
            Archetype::Limper => unreachable!("Limper handled above"),
            Archetype::FlushTrap => unreachable!("FlushTrap handled above"),
            Archetype::WideOpener | Archetype::ThreeBettor | Archetype::ThreebetFolder => {
                unreachable!("3bp archetypes handled above")
            }
            Archetype::Hero => (0.55, 0.50, 0.64, 0.15),
        };
        let facing = to_call > 0;
        if !facing {
            if strength >= open_thr || rng.unit() < aggro {
                if let Some(a) = make_bet(0.12, &legal) {
                    return a;
                }
            }
            return safe_action(&legal);
        }
        // 面对加注。
        if strength >= threebet && legal_can_raise(&legal) {
            if let Some(a) = make_bet(0.55, &legal) {
                return a;
            }
        }
        if strength >= call_thr && legal.call.is_some() {
            // station 跟更宽（看赔率）。
            return Action::Call;
        }
        if arch == Archetype::Station && pot_odds < 0.25 && legal.call.is_some() {
            return Action::Call;
        }
        return safe_action(&legal);
    }

    // 翻后。
    let made = made_class(hole, board);
    let dr = draws(hole, board);
    let facing = to_call > 0;
    let r = made.rank();

    match arch {
        Archetype::Random => random_action(&legal, state, seat, rng),
        Archetype::Nit => {
            if !facing {
                if r >= MadeClass::TopPair.rank() {
                    make_bet(0.6, &legal).unwrap_or_else(|| safe_action(&legal))
                } else {
                    safe_action(&legal)
                }
            } else if r >= MadeClass::TopPair.rank() {
                Action::Call
            } else {
                safe_action(&legal)
            }
        }
        Archetype::Station => {
            if !facing {
                if r >= MadeClass::Strong.rank() {
                    make_bet(0.5, &legal).unwrap_or_else(|| safe_action(&legal))
                } else {
                    safe_action(&legal)
                }
            } else if r >= MadeClass::WeakPair.rank() || dr.has_meaningful_draw() || pot_odds < 0.2
            {
                Action::Call // 跟注站：任何对子/听牌/好赔率都跟
            } else {
                safe_action(&legal)
            }
        }
        Archetype::WideOpener => {
            // 被 3bet 后爱跟的翻后延续：站式粘跟——只用强成手下注，任何对子/听牌/好赔率都跟。
            if !facing {
                if r >= MadeClass::Strong.rank() {
                    make_bet(0.5, &legal).unwrap_or_else(|| safe_action(&legal))
                } else {
                    safe_action(&legal)
                }
            } else if r >= MadeClass::WeakPair.rank() || dr.has_meaningful_draw() || pot_odds < 0.22
            {
                Action::Call
            } else {
                safe_action(&legal)
            }
        }
        Archetype::ThreebetFolder => {
            // 3bp c-bet 频率跨靶复核：翻后**爱弃**的 3bet 池跟注机（翻前同 WideOpener）。
            // 面对 hero 的 c-bet 只用顶对+ / 好赔率强听牌继续，其余全弃 → 拉高 fold-equity，
            // 与 WideOpener 的站式粘跟构成频率最优值对靶（诊断 (a)）。
            if !facing {
                // 不主动进攻：仅强成手价值注，其余过牌（被动，不 stab/领打）。
                if r >= MadeClass::Strong.rank() {
                    make_bet(0.5, &legal).unwrap_or_else(|| safe_action(&legal))
                } else {
                    safe_action(&legal)
                }
            } else if r >= MadeClass::Strong.rank() && legal_can_raise(&legal) {
                // 坚果+：加注（不做纯站，避免 hero 价值注被无脑支付污染 c-bet 频率信号）。
                make_bet(0.8, &legal).unwrap_or(Action::Call)
            } else if r >= MadeClass::TopPair.rank()
                || (dr.has_meaningful_draw() && pot_odds < 0.25)
            {
                // 顶对/超对跟；强听牌仅好赔率跟。
                Action::Call
            } else {
                safe_action(&legal) // 空气 / 弱对中对 / 边缘听牌 → 弃（爱弃的核心）
            }
        }
        Archetype::ThreeBettor => {
            // 3bet 池进攻方样板：高频 c-bet（含空气），面对反抗按成手/赔率继续。
            if !facing {
                if r >= MadeClass::TopPair.rank() {
                    make_bet(0.66, &legal).unwrap_or_else(|| safe_action(&legal))
                } else if dr.has_meaningful_draw() || rng.unit() < 0.50 {
                    make_bet(0.45, &legal).unwrap_or_else(|| safe_action(&legal))
                } else {
                    safe_action(&legal)
                }
            } else if r >= MadeClass::Strong.rank() && legal_can_raise(&legal) {
                make_bet(0.8, &legal).unwrap_or(Action::Call)
            } else if (r >= MadeClass::MidPair.rank() && pot_odds < 0.38)
                || (dr.has_meaningful_draw() && pot_odds < 0.33)
            {
                Action::Call
            } else {
                safe_action(&legal)
            }
        }
        Archetype::Limper => {
            // 极被动：check/call 为主，仅强 made-hand 低频下注，几乎不加注。
            if !facing {
                if r >= MadeClass::Strong.rank() && rng.unit() < 0.5 {
                    make_bet(0.4, &legal).unwrap_or_else(|| safe_action(&legal))
                } else {
                    safe_action(&legal)
                }
            } else if r >= MadeClass::WeakPair.rank() || dr.has_meaningful_draw() || pot_odds < 0.22
            {
                Action::Call // 跟注站式：任何对子/听牌/好赔率都跟，不加注
            } else {
                safe_action(&legal)
            }
        }
        Archetype::PairedTrap => {
            let paired = board_paired(board);
            // 坚果建战判据：成对面持 trips+（trips via 匹配 rank，或 boat/quads via Monster）。
            let danger =
                made == MadeClass::Monster || (paired && holds_trips_on_paired(hole, board));
            if paired {
                if danger {
                    // 仅此处升级加注战：大注/池量再加注，把 hero 的非坚果价值再加注钓进深战。
                    if facing {
                        if legal_can_raise(&legal) {
                            make_bet(1.0, &legal).unwrap_or(Action::Call)
                        } else {
                            Action::Call
                        }
                    } else {
                        make_bet(0.8, &legal).unwrap_or_else(|| safe_action(&legal))
                    }
                } else {
                    // 成对面无坚果：被动，绝不诈唬加注（不喂 hero 的抓诈）。
                    if !facing {
                        if r >= MadeClass::TopPair.rank() && rng.unit() < 0.4 {
                            make_bet(0.4, &legal).unwrap_or_else(|| safe_action(&legal))
                        } else {
                            safe_action(&legal)
                        }
                    } else if (r >= MadeClass::MidPair.rank() && pot_odds < 0.30) || pot_odds < 0.18
                    {
                        Action::Call // 便宜跟成手，深注弃
                    } else {
                        safe_action(&legal)
                    }
                }
            } else {
                // 非对子面：直球价值，打法同 Tag（不限深）——确保「非对子面加注战」对照不被污染。
                if !facing {
                    if r >= MadeClass::TopPair.rank() {
                        make_bet(0.6, &legal).unwrap_or_else(|| safe_action(&legal))
                    } else if dr.has_meaningful_draw() && rng.unit() < 0.6 {
                        make_bet(0.55, &legal).unwrap_or_else(|| safe_action(&legal))
                    } else {
                        safe_action(&legal)
                    }
                } else if r >= MadeClass::Strong.rank() && legal_can_raise(&legal) {
                    make_bet(0.7, &legal).unwrap_or(Action::Call)
                } else if (r >= MadeClass::MidPair.rank() && pot_odds < 0.4)
                    || (dr.has_meaningful_draw() && pot_odds < 0.33)
                {
                    Action::Call
                } else {
                    safe_action(&legal)
                }
            }
        }
        Archetype::FlushTrap => {
            let flushy = board_flushy(board);
            // danger = 成花/成顺板持坚果同类（nut flush / boat，`monster_non_nut==false`）→ 升级
            // 加注战，把 hero 非坚果同花/顺子 stack-off 钓进 drawing-dead。
            let danger = flushy && made == MadeClass::Monster && !monster_non_nut(hole, board);
            if flushy {
                if danger {
                    // 升级：能加就池量下注/加注（不管 facing）。
                    if legal_can_raise(&legal) {
                        make_bet(1.0, &legal).unwrap_or(Action::Call)
                    } else if facing {
                        Action::Call
                    } else {
                        safe_action(&legal)
                    }
                } else {
                    // 非坚果同类 / 无同花：被动——让 hero 下注、便宜跟成手（付 hero 价值注）、大注弃。
                    if !facing {
                        safe_action(&legal)
                    } else if (r >= MadeClass::MidPair.rank() && pot_odds < 0.35) || pot_odds < 0.18
                    {
                        Action::Call
                    } else {
                        safe_action(&legal)
                    }
                }
            } else {
                // 非成花/成顺板：打法同 Tag（不污染「正常价值加注」对照）。
                if !facing {
                    if r >= MadeClass::TopPair.rank() {
                        make_bet(0.6, &legal).unwrap_or_else(|| safe_action(&legal))
                    } else if dr.has_meaningful_draw() && rng.unit() < 0.6 {
                        make_bet(0.55, &legal).unwrap_or_else(|| safe_action(&legal))
                    } else {
                        safe_action(&legal)
                    }
                } else if r >= MadeClass::Strong.rank() && legal_can_raise(&legal) {
                    make_bet(0.7, &legal).unwrap_or(Action::Call)
                } else if (r >= MadeClass::MidPair.rank() && pot_odds < 0.4)
                    || (dr.has_meaningful_draw() && pot_odds < 0.33)
                {
                    Action::Call
                } else {
                    safe_action(&legal)
                }
            }
        }
        Archetype::Maniac => {
            let aggro = rng.unit() < 0.55;
            if !facing {
                if r >= MadeClass::MidPair.rank() || dr.has_meaningful_draw() || aggro {
                    make_bet(0.75, &legal).unwrap_or_else(|| safe_action(&legal))
                } else {
                    safe_action(&legal)
                }
            } else if r >= MadeClass::Strong.rank() && legal_can_raise(&legal) {
                make_bet(0.9, &legal).unwrap_or(Action::Call)
            } else if r >= MadeClass::WeakPair.rank() || dr.has_meaningful_draw() || aggro {
                Action::Call
            } else {
                safe_action(&legal)
            }
        }
        Archetype::Tag | Archetype::Hero => {
            if !facing {
                if r >= MadeClass::TopPair.rank() {
                    make_bet(0.6, &legal).unwrap_or_else(|| safe_action(&legal))
                } else if dr.has_meaningful_draw() && rng.unit() < 0.6 {
                    make_bet(0.55, &legal).unwrap_or_else(|| safe_action(&legal))
                } else {
                    safe_action(&legal)
                }
            } else if r >= MadeClass::Strong.rank() && legal_can_raise(&legal) {
                make_bet(0.7, &legal).unwrap_or(Action::Call)
            } else if (r >= MadeClass::MidPair.rank() && pot_odds < 0.4)
                || (dr.has_meaningful_draw() && pot_odds < 0.33)
            {
                Action::Call
            } else {
                safe_action(&legal)
            }
        }
    }
}

fn state_high(state: &GameState) -> u64 {
    state
        .players()
        .iter()
        .map(|p| p.committed_this_round.0)
        .max()
        .unwrap_or(0)
}

fn legal_can_raise(legal: &LegalActionSet) -> bool {
    legal.bet_range.is_some() || legal.raise_range.is_some()
}

fn safe_action(legal: &LegalActionSet) -> Action {
    if legal.check {
        Action::Check
    } else if legal.fold {
        Action::Fold
    } else if legal.call.is_some() {
        Action::Call
    } else {
        Action::Fold
    }
}

fn random_action(
    legal: &LegalActionSet,
    state: &GameState,
    seat: usize,
    rng: &mut SimRng,
) -> Action {
    let mut opts: Vec<Action> = Vec::new();
    if legal.check {
        opts.push(Action::Check);
    }
    if legal.call.is_some() {
        opts.push(Action::Call);
    }
    if legal.fold {
        opts.push(Action::Fold);
    }
    if let Some((mn, mx)) = legal.bet_range {
        let me = state.players()[seat].committed_this_round.0;
        let span = mx.0.saturating_sub(mn.0);
        let to = mn.0
            + if span > 0 {
                rng.next_u64() % (span + 1)
            } else {
                0
            };
        let _ = me;
        opts.push(Action::Bet { to: ChipAmount(to) });
    } else if let Some((mn, mx)) = legal.raise_range {
        let span = mx.0.saturating_sub(mn.0);
        let to = mn.0
            + if span > 0 {
                rng.next_u64() % (span + 1)
            } else {
                0
            };
        opts.push(Action::Raise { to: ChipAmount(to) });
    }
    if opts.is_empty() {
        return safe_action(legal);
    }
    opts[rng.below(opts.len())]
}

// ===========================================================================
// Request 构建（GameState → advisor 协议）
// ===========================================================================

/// 稳定座位名（observe/names 用）。座位身份跨手固定 = 真实对手保号。
fn seat_name(seat: usize) -> String {
    format!("p{seat}")
}

fn build_request(
    state: &GameState,
    seat: usize,
    hist: &[HistAction],
    cfg: &TableConfig,
    exploit: bool,
) -> Request {
    let players = state.players();
    let hole = players[seat]
        .hole_cards
        .map(|h| vec![card_str(h[0]), card_str(h[1])])
        .unwrap_or_default();
    let board: Vec<String> = state.board().iter().map(|c| card_str(*c)).collect();
    let stacks: Vec<u64> = cfg.starting_stacks.iter().map(|s| s.0).collect();
    Request {
        hole,
        board,
        button_seat: cfg.button_seat.0,
        my_seat: seat as u8,
        num_seats: cfg.n_seats,
        small_blind: cfg.small_blind.0,
        big_blind: cfg.big_blind.0,
        actions: hist.to_vec(),
        valid: valid_from_legal(&state.legal_actions()),
        stacks,
        names: if exploit {
            (0..cfg.n_seats)
                .map(|s| (s, seat_name(s as usize)))
                .collect()
        } else {
            std::collections::BTreeMap::new()
        },
        // sim 走启发式重建路径（不送权威盲注座/街标签）→ from_request 走与历史一致的物理+启发式
        // 分段 → 重建 byte-for-byte 不变（漂移恒 0.000% by construction）。
        sb_seat: None,
        bb_seat: None,
    }
}

/// 记录一个已应用动作到 OpenPoker 风格历史。返回动作标签（observe 复用）。
fn record_action(
    state_after: &GameState,
    seat: usize,
    action: Action,
    hist: &mut Vec<HistAction>,
) -> &'static str {
    let (label, to) = match action {
        Action::Fold => ("fold", None),
        Action::Check => ("check", None),
        Action::Call => ("call", None),
        Action::Bet { to } => ("raise", Some(to.0)),
        Action::Raise { to } => ("raise", Some(to.0)),
        Action::AllIn => {
            // 归一化后读 committed_this_round 判 raise/call。
            let cr = state_after.players()[seat].committed_this_round.0;
            let high = state_after
                .players()
                .iter()
                .enumerate()
                .filter(|(s, _)| *s != seat)
                .map(|(_, p)| p.committed_this_round.0)
                .max()
                .unwrap_or(0);
            if cr > high {
                ("raise", Some(cr))
            } else {
                ("call", None)
            }
        }
    };
    hist.push(HistAction {
        seat: seat as u8,
        action: label.to_string(),
        to,
        // street: None → 任一动作缺街标签 → from_request 整手走启发式分段（零漂移 by construction）。
        street: None,
    });
    label
}

// ===========================================================================
// 主循环
// ===========================================================================

struct SimStats {
    hero_chips: i64,
    hands: u64,
    per_hand: Vec<f64>, // bb/hand（算 CI）
    fallback: u64,
    decisions: u64,
    pot_mismatch: u64,
    tocall_mismatch: u64,
    reconstruct_checks: u64,
    /// `--trace-summary`：收集 Phase A 节点热度（叶节点 → 命中次数）。默认关（零收集）。
    collect_trace: bool,
    node_counts: std::collections::BTreeMap<String, u64>,
    legalize_downgrades: u64,
    /// 造靶度量（仅 collect_trace）：hero 在 `post.facing.*` 节点持**非坚果 Monster**
    /// （成花/成顺可被压制）的机会数 = FlushTrap 靶的分母 n；其中落 `value_raise`（漏洞点火）的次数。
    nonnut_facing: u64,
    nonnut_value_raise: u64,
}

#[allow(clippy::too_many_arguments)]
fn play_hand(
    bot: &mut HeuristicBot,
    eq: &EquityEngine,
    field: &[Archetype],
    cfg: &TableConfig,
    hand_seed: u64,
    stats: &mut SimStats,
    exploit: bool,
) {
    let mut state = GameState::new(cfg, hand_seed);
    let mut hist: Vec<HistAction> = Vec::new();
    // observe 累积：(seat, street, action_label)；街由翻牌前/后 board 长度判定。
    let mut obs: Vec<(u8, u8, &'static str)> = Vec::new();
    let mut guard = 0;
    while !state.is_terminal() && guard < 400 {
        guard += 1;
        let seat = match state.current_player() {
            Some(s) => s.0 as usize,
            None => break,
        };
        let legal = state.legal_actions();
        // 街 = 动作发生时的 board 长度（apply 前捕获，apply 可能翻新街）。
        let street_idx: u8 = match state.board().len() {
            0 => 0,
            3 => 1,
            4 => 2,
            _ => 3,
        };
        let action = if field[seat] == Archetype::Hero {
            let req = build_request(&state, seat, &hist, cfg, exploit);
            // 重建对照（验证 GameView）。
            let view = GameView::from_request(&req);
            let real_pot = state.pot().0;
            let real_to_call = match legal.call {
                Some(c) => {
                    c.0.saturating_sub(state.players()[seat].committed_this_round.0)
                }
                None => 0,
            };
            stats.reconstruct_checks += 1;
            if view.pot_chips() != real_pot {
                stats.pot_mismatch += 1;
            }
            if view.to_call_chips() != real_to_call {
                stats.tocall_mismatch += 1;
            }
            let resp = bot.respond(&req);
            stats.decisions += 1;
            if resp.source.starts_with("fallback") {
                stats.fallback += 1;
            }
            // Phase A 节点热度（仅 --trace-summary 时收集；trace 只写不读，不回馈决策）。
            if stats.collect_trace {
                if let Some(t) = &resp.trace {
                    if let Some(leaf) = t.path.last() {
                        *stats.node_counts.entry(leaf.clone()).or_insert(0) += 1;
                    }
                    if t.legalized.is_some() {
                        stats.legalize_downgrades += 1;
                    }
                    // 造靶度量：hero 在面对下注节点持非坚果 Monster 的机会 / 其中 value_raise 点火。
                    let leaf = t.path.last().map(|s| s.as_str()).unwrap_or("");
                    if leaf.starts_with("post.facing") {
                        if let Some(h) = state.players()[seat].hole_cards {
                            let b = state.board();
                            // 造靶焦点 = 非坚果**同花**面对下注（成花板 stack-off 漏洞的 n 分母）。
                            if nonnut_flush(h, b) {
                                stats.nonnut_facing += 1;
                                if leaf == "post.facing.value_raise" {
                                    stats.nonnut_value_raise += 1;
                                }
                            }
                        }
                    }
                }
            }
            response_to_action(&legal, &resp.action, resp.amount)
        } else {
            let mut brng = SimRng::new(hand_seed ^ ((seat as u64) << 32) ^ (guard as u64));
            baseline_action(field[seat], &state, seat, eq, &mut brng)
        };
        if state.apply(action).is_err() {
            // 不应发生（映射保证合法）；保险退出。
            break;
        }
        let label = record_action(&state, seat, action, &mut hist);
        if exploit {
            obs.push((seat as u8, street_idx, label));
        }
    }
    if let Some(payouts) = state.payouts() {
        // hero 座位 = field 中 Hero 的下标。
        let hero_seat = field
            .iter()
            .position(|a| *a == Archetype::Hero)
            .unwrap_or(0);
        let net = payouts
            .iter()
            .find(|(s, _)| s.0 as usize == hero_seat)
            .map(|(_, v)| *v)
            .unwrap_or(0);
        stats.hero_chips += net;
        stats.hands += 1;
        stats.per_hand.push(net as f64 / cfg.big_blind.0 as f64);
    }
    // 喂对手画像（与 live advisor feed_observe 同语义：盲位翻前动作标 forced）。
    if exploit {
        let n = cfg.n_seats;
        let btn = cfg.button_seat.0 % n;
        let (sb_seat, bb_seat) = if n == 2 {
            (btn, (btn + 1) % n)
        } else {
            ((btn + 1) % n, (btn + 2) % n)
        };
        let names: Vec<String> = (0..n as usize).map(seat_name).collect();
        let observed: Vec<ObservedAction> = obs
            .iter()
            .map(|(seat, st, act)| ObservedAction {
                name: &names[*seat as usize],
                street: *st,
                action: act,
                forced_blind: *st == 0 && (*seat == sb_seat || *seat == bb_seat),
            })
            .collect();
        bot.observe_hand(&observed, &names);
    }
}

fn main() {
    let mut hands = 5000u64;
    let mut seats = 9u8;
    let mut bb = 20u64;
    let mut start_bb = 100u64;
    let mut samples = 300u32;
    let mut seed = 12345u64;
    let mut field_kind = String::from("mixed");
    let mut mirror = false;
    let mut exploit = false;
    let mut trace_summary = false;
    let mut config_path: Option<String> = None;

    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--hands" => {
                hands = argv
                    .get(i + 1)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(hands);
                i += 2;
            }
            "--seats" => {
                seats = argv
                    .get(i + 1)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(seats);
                i += 2;
            }
            "--bb" => {
                bb = argv.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(bb);
                i += 2;
            }
            "--start-bb" => {
                start_bb = argv
                    .get(i + 1)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(start_bb);
                i += 2;
            }
            "--samples" => {
                samples = argv
                    .get(i + 1)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(samples);
                i += 2;
            }
            "--seed" => {
                seed = argv.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(seed);
                i += 2;
            }
            "--field" => {
                field_kind = argv.get(i + 1).cloned().unwrap_or(field_kind);
                i += 2;
            }
            "--mirror" => {
                mirror = true;
                i += 1;
            }
            "--exploit" => {
                exploit = true;
                i += 1;
            }
            "--trace-summary" => {
                trace_summary = true;
                i += 1;
            }
            "--config" => {
                config_path = argv.get(i + 1).cloned();
                i += 2;
            }
            _ => i += 1,
        }
    }
    let seats = seats.clamp(2, 9);

    // config：JSON 覆盖（缺字段走默认，serde(default)）→ 再压实 sim 提速用 equity 样本。
    let mut config = match &config_path {
        Some(p) => match std::fs::read_to_string(p) {
            Ok(s) => HeuristicConfig::from_json_str(&s).unwrap_or_else(|e| {
                eprintln!("[sim] config 解析失败（{e}），用默认。");
                HeuristicConfig::default()
            }),
            Err(e) => {
                eprintln!("[sim] config 读取失败（{e}），用默认。");
                HeuristicConfig::default()
            }
        },
        None => HeuristicConfig::default(),
    };
    config.equity.samples = samples;
    // sim 建表用稍小样本提速。
    config.equity.preflop_table_samples = 2000;
    let mut bot = HeuristicBot::new(config);
    let eq = EquityEngine::new(2000, 999);

    // 组场：座 0 = Hero，其余按 field_kind。
    let mut field: Vec<Archetype> = Vec::with_capacity(seats as usize);
    field.push(Archetype::Hero);
    let pool = [
        Archetype::Tag,
        Archetype::Station,
        Archetype::Maniac,
        Archetype::Nit,
        Archetype::Random,
        Archetype::Limper,
    ];
    for k in 1..seats as usize {
        let a = if mirror {
            Archetype::Hero
        } else {
            match field_kind.as_str() {
                "nit" => Archetype::Nit,
                "station" => Archetype::Station,
                "maniac" => Archetype::Maniac,
                "tag" => Archetype::Tag,
                "random" => Archetype::Random,
                "limpy" => Archetype::Limper,
                "pairedtrap" | "trap" => Archetype::PairedTrap,
                // 成花板非坚果 stack-off 造靶（Phase C 第二轮，run33 复盘）：全座 FlushTrap，
                // hero 高频在成花/成顺板对上「仅坚果建战」的对手。不入 mixed pool（默认关，
                // follow PairedTrap 先例：不动既有配置逐字节输出）。2-max = 最高密度通道。
                "flushtrap" | "flush" => Archetype::FlushTrap,
                // 3bet 池造靶（P1-5）：奇数座宽开跟注机 / 偶数座 3bet 机交替——hero 作为
                // 3bet 者与被 3bet 者的两条通道都要有。不入 mixed pool（默认关，
                // follow PairedTrap 先例：不动既有配置输出）。
                "3bp" | "threebet" => {
                    if k % 2 == 1 {
                        Archetype::WideOpener
                    } else {
                        Archetype::ThreeBettor
                    }
                }
                // 纯场变体：2-max vs WideOpener = hero 作为 3bet 者的最高密度通道
                // （对手几乎每手宽开、被 3bet 后爱跟）；vs ThreeBettor = hero 被 3bet
                // 的最高密度通道。
                "wideopen" | "wideopener" => Archetype::WideOpener,
                "3bettor" | "threebettor" => Archetype::ThreeBettor,
                // 3bp c-bet 频率跨靶复核造靶（Phase C 第二轮候选②）：全座 ThreebetFolder
                // ——翻前同 WideOpener（hero 作 3bettor 进翻后）、翻后爱弃。2-max = 最高密度。
                // 不入 mixed pool（默认关，follow 先例：不动既有 --field 逐字节输出）。
                "3bpfold" | "threebetfolder" => Archetype::ThreebetFolder,
                _ => pool[(k - 1) % pool.len()],
            }
        };
        field.push(a);
    }

    let bb_chips = ChipAmount(bb);
    let sb_chips = ChipAmount(bb / 2);
    // 起始码深（BB 数）：默认 100；200/400 = 深码回归区。下限 2BB（须够发盲）。
    let start_bb = start_bb.max(2);
    let stack = ChipAmount(bb * start_bb);

    let mut stats = SimStats {
        hero_chips: 0,
        hands: 0,
        per_hand: Vec::with_capacity(hands as usize),
        fallback: 0,
        decisions: 0,
        pot_mismatch: 0,
        tocall_mismatch: 0,
        reconstruct_checks: 0,
        collect_trace: trace_summary,
        node_counts: std::collections::BTreeMap::new(),
        legalize_downgrades: 0,
        nonnut_facing: 0,
        nonnut_value_raise: 0,
    };

    for h in 0..hands {
        let button = (h % seats as u64) as u8;
        let config = TableConfig {
            n_seats: seats,
            starting_stacks: vec![stack; seats as usize],
            small_blind: sb_chips,
            big_blind: bb_chips,
            ante: ChipAmount(0),
            button_seat: poker::SeatId(button),
        };
        let hand_seed = seed
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(h)
            .wrapping_add(0x1234_5678);
        play_hand(
            &mut bot, &eq, &field, &config, hand_seed, &mut stats, exploit,
        );
    }

    // 统计。
    let n = stats.per_hand.len().max(1) as f64;
    let mean_bb = stats.per_hand.iter().sum::<f64>() / n;
    let var = stats
        .per_hand
        .iter()
        .map(|x| (x - mean_bb).powi(2))
        .sum::<f64>()
        / n;
    let se = (var / n).sqrt();
    let mbb_per_g = mean_bb * 1000.0;
    let ci95 = 1.96 * se * 1000.0;

    println!("=== heuristic9 自对弈仿真 ===");
    println!(
        "场: seats={seats} start_bb={start_bb} field={} hands={} samples={samples} exploit={exploit}",
        if mirror {
            "mirror(hero×N)".into()
        } else {
            field_kind.clone()
        },
        stats.hands
    );
    let names: Vec<String> = field.iter().map(|a| format!("{a:?}")).collect();
    println!("座位: {}", names.join(","));
    println!(
        "hero 净盈亏: {} chips  |  win-rate: {:.1} ± {:.1} mbb/g  (CI95)",
        stats.hero_chips, mbb_per_g, ci95
    );
    println!(
        "决策 {} 次, fallback {} ({:.2}%)",
        stats.decisions,
        stats.fallback,
        100.0 * stats.fallback as f64 / stats.decisions.max(1) as f64
    );
    println!(
        "下注重建对照: pot 漂移 {}/{} ({:.3}%), to_call 漂移 {}/{} ({:.3}%)",
        stats.pot_mismatch,
        stats.reconstruct_checks,
        100.0 * stats.pot_mismatch as f64 / stats.reconstruct_checks.max(1) as f64,
        stats.tocall_mismatch,
        stats.reconstruct_checks,
        100.0 * stats.tocall_mismatch as f64 / stats.reconstruct_checks.max(1) as f64,
    );
    // Phase A 节点热度表（opt-in；默认关时上面输出与历史基线逐字节一致）。
    if trace_summary {
        println!("=== Phase A 节点热度 (--trace-summary) ===");
        let mut rows: Vec<(&String, &u64)> = stats.node_counts.iter().collect();
        rows.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        for (node, cnt) in rows {
            println!(
                "{cnt:8}  {:5.1}%  {node}",
                100.0 * *cnt as f64 / stats.decisions.max(1) as f64
            );
        }
        println!(
            "legalize 降级: {} ({:.2}%)",
            stats.legalize_downgrades,
            100.0 * stats.legalize_downgrades as f64 / stats.decisions.max(1) as f64
        );
        println!(
            "非坚果 Monster 面对下注机会 n={} (其中 value_raise 点火 {})",
            stats.nonnut_facing, stats.nonnut_value_raise
        );
    }
}
