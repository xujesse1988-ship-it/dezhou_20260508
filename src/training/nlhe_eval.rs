//! H3 简化 heads-up NLHE blueprint 评测与近似 LBR proxy。
//!
//! 本模块只复用现有 [`crate::training::nlhe::SimplifiedNlheGame`] /
//! [`crate::training::trainer::EsMccfrTrainer`] 查询面，不改变
//! game trait、checkpoint schema 或 bucket table schema。H3 的目标是把
//! blueprint-only 策略闭环跑通：固定 seed 可复现评测、三类基础 baseline 对战，
//! 以及一个工程用 local best-response proxy 趋势指标。

use serde::{Deserialize, Serialize};

use crate::core::rng::{ChaCha20Rng, RngSource};
use crate::core::{Card, Rank, SeatId, Street};
use crate::error::NlheEvaluationError;
use crate::eval::{HandCategory, HandEvaluator, NaiveHandEvaluator};
use crate::training::game::{Game, NodeKind, PlayerId};
use crate::training::nlhe::{SimplifiedNlheAction, SimplifiedNlheGame, SimplifiedNlheInfoSet};
use crate::training::sampling::sample_discrete;

/// H3 baseline policy 集合。
#[derive(Clone, Copy, Eq, PartialEq, Hash, Debug, Serialize, Deserialize)]
pub enum NlheBaselinePolicy {
    /// 当前抽象 legal actions 上均匀随机。
    Random,
    /// 可 check 则 check；面对下注优先 call，否则 fold；不主动 bet/raise。
    CallStation,
    /// Preflop 只继续 TT+ / AK / AQ / AJs / KQs；postflop 面对下注仅 made hand
    /// one-pair+ 继续；不主动 bet/raise。
    OverlyTight,
}

impl NlheBaselinePolicy {
    pub fn label(self) -> &'static str {
        match self {
            NlheBaselinePolicy::Random => "random",
            NlheBaselinePolicy::CallStation => "call-station",
            NlheBaselinePolicy::OverlyTight => "overly-tight",
        }
    }

    /// 在当前 state 的 legal abstract actions 中选择一个动作。
    ///
    /// 该方法公开给 H3 tests / tools 做 policy sanity；返回值始终来自 `actions`
    /// 切片本身，保证可直接传给 [`SimplifiedNlheGame::next`]。
    pub fn select_action(
        self,
        state: &crate::training::nlhe::SimplifiedNlheState,
        actions: &[SimplifiedNlheAction],
        rng: &mut dyn RngSource,
    ) -> Result<SimplifiedNlheAction, NlheEvaluationError> {
        if actions.is_empty() {
            return Err(NlheEvaluationError::EmptyLegalActions {
                state: format!("{:?}", SimplifiedNlheGame::current(state)),
            });
        }
        let chosen = match self {
            NlheBaselinePolicy::Random => {
                let idx = (rng.next_u64() as usize) % actions.len();
                actions[idx]
            }
            NlheBaselinePolicy::CallStation => passive_action(actions, true),
            NlheBaselinePolicy::OverlyTight => tight_action(state, actions)?,
        };
        debug_assert!(actions.contains(&chosen));
        Ok(chosen)
    }
}

/// Blueprint vs baseline 评测配置。
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct NlheEvaluationConfig {
    /// 每个 blueprint 座位跑多少手。总手数 = `hands_per_seat * 2`。
    pub hands_per_seat: u64,
    /// master seed；每手通过 deterministic mix 派生独立 seed。
    pub seed: u64,
    /// 单手最多执行多少个 player/chance transition，防止评测 bug 死循环。
    pub max_actions_per_hand: usize,
}

impl Default for NlheEvaluationConfig {
    fn default() -> Self {
        Self {
            hands_per_seat: 1_000,
            seed: 0x4833_4556_414c_0001,
            max_actions_per_hand: 512,
        }
    }
}

/// Blueprint vs baseline 评测结果。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NlheEvaluationReport {
    pub baseline: NlheBaselinePolicy,
    pub hands: u64,
    pub hands_per_seat: u64,
    pub seed: u64,
    pub blueprint_total_chips: f64,
    pub mbb_per_game: f64,
    pub standard_error_mbb_per_game: f64,
    pub ci95_low_mbb_per_game: f64,
    pub ci95_high_mbb_per_game: f64,
    pub sb_mbb_per_game: f64,
    pub bb_mbb_per_game: f64,
}

/// 近似 local best-response proxy 配置。
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct NlheLbrConfig {
    /// 采样多少个 target-player decision probe。
    pub probes: u64,
    /// 每个候选动作用多少条 blueprint rollout 估计 EV。
    pub rollouts_per_action: u64,
    pub seed: u64,
    pub max_actions_per_probe: usize,
    pub max_actions_per_rollout: usize,
}

impl Default for NlheLbrConfig {
    fn default() -> Self {
        Self {
            probes: 1_000,
            rollouts_per_action: 16,
            seed: 0x4833_4c42_5200_0001,
            max_actions_per_probe: 128,
            max_actions_per_rollout: 512,
        }
    }
}

/// H3 工程用 LBR proxy。它不是正式 exploitability，只用于比较同一评测配置下
/// checkpoint 之间的趋势。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NlheLbrReport {
    pub probes_requested: u64,
    pub probes_used: u64,
    pub terminal_or_unreached_probes: u64,
    pub rollouts_per_action: u64,
    pub seed: u64,
    pub mean_best_response_chips: f64,
    pub standard_error_chips: f64,
    pub target0_mean_chips: f64,
    pub target1_mean_chips: f64,
}

/// 评测 blueprint average strategy 对单个 baseline 的收益。
///
/// `blueprint_strategy` 返回空向量时按 uniform fallback；返回非空但长度与当前
/// legal action 数不同则报 [`NlheEvaluationError::StrategyLengthMismatch`]。
pub fn evaluate_blueprint_vs_baseline(
    game: &SimplifiedNlheGame,
    blueprint_strategy: &dyn Fn(&SimplifiedNlheInfoSet, usize) -> Vec<f64>,
    baseline: NlheBaselinePolicy,
    config: &NlheEvaluationConfig,
) -> Result<NlheEvaluationReport, NlheEvaluationError> {
    if config.hands_per_seat == 0 {
        return Err(NlheEvaluationError::InvalidConfig {
            reason: "hands_per_seat must be > 0".to_string(),
        });
    }
    if config.max_actions_per_hand == 0 {
        return Err(NlheEvaluationError::InvalidConfig {
            reason: "max_actions_per_hand must be > 0".to_string(),
        });
    }

    let mut all_pnl_chips: Vec<f64> = Vec::with_capacity((config.hands_per_seat * 2) as usize);
    let mut sb_pnl = 0.0;
    let mut bb_pnl = 0.0;

    for blueprint_seat in [SeatId(0), SeatId(1)] {
        for hand_idx in 0..config.hands_per_seat {
            let seed = mix3(config.seed, blueprint_seat.0 as u64, hand_idx);
            let mut rng = ChaCha20Rng::from_seed(seed);
            let root = game.root(&mut rng);
            let terminal = rollout_blueprint_vs_baseline(
                root,
                blueprint_seat,
                baseline,
                blueprint_strategy,
                &mut rng,
                config.max_actions_per_hand,
            )?;
            let pnl = payoff_for_seat(&terminal, blueprint_seat)?;
            if blueprint_seat == game.config.button_seat {
                sb_pnl += pnl;
            } else {
                bb_pnl += pnl;
            }
            all_pnl_chips.push(pnl);
        }
    }

    let hands = all_pnl_chips.len() as u64;
    let bb_chips = game.config.big_blind.as_u64() as f64;
    let stats = sample_stats(&all_pnl_chips);
    let scale = 1000.0 / bb_chips;
    let mean_mbb = stats.mean * scale;
    let se_mbb = stats.standard_error * scale;
    Ok(NlheEvaluationReport {
        baseline,
        hands,
        hands_per_seat: config.hands_per_seat,
        seed: config.seed,
        blueprint_total_chips: all_pnl_chips.iter().sum(),
        mbb_per_game: mean_mbb,
        standard_error_mbb_per_game: se_mbb,
        ci95_low_mbb_per_game: mean_mbb - 1.96 * se_mbb,
        ci95_high_mbb_per_game: mean_mbb + 1.96 * se_mbb,
        sb_mbb_per_game: (sb_pnl / config.hands_per_seat as f64) * scale,
        bb_mbb_per_game: (bb_pnl / config.hands_per_seat as f64) * scale,
    })
}

/// 估计 H3 工程用 local best-response proxy。
///
/// 每个 probe 先沿 blueprint self-play 采样到 target player 的一个决策点，再枚举该
/// 点 legal actions；每个 action 后续按 blueprint rollout，取最高 EV。target
/// player 之后的未来决策也回到 blueprint，因此这是 local proxy，不是正式 BR。
pub fn estimate_simplified_nlhe_lbr(
    game: &SimplifiedNlheGame,
    blueprint_strategy: &dyn Fn(&SimplifiedNlheInfoSet, usize) -> Vec<f64>,
    config: &NlheLbrConfig,
) -> Result<NlheLbrReport, NlheEvaluationError> {
    if config.probes == 0 {
        return Err(NlheEvaluationError::InvalidConfig {
            reason: "probes must be > 0".to_string(),
        });
    }
    if config.rollouts_per_action == 0 {
        return Err(NlheEvaluationError::InvalidConfig {
            reason: "rollouts_per_action must be > 0".to_string(),
        });
    }

    let mut values: Vec<f64> = Vec::with_capacity(config.probes as usize);
    let mut values_p0 = Vec::new();
    let mut values_p1 = Vec::new();
    let mut skipped = 0u64;

    for probe_idx in 0..config.probes {
        let target = (probe_idx % 2) as PlayerId;
        let mut rng = ChaCha20Rng::from_seed(mix3(config.seed, 0x9b52, probe_idx));
        let mut state = game.root(&mut rng);
        let mut reached = false;

        for _ in 0..config.max_actions_per_probe {
            match SimplifiedNlheGame::current(&state) {
                NodeKind::Terminal => break,
                NodeKind::Chance => {
                    let dist = SimplifiedNlheGame::chance_distribution(&state);
                    let action = sample_discrete(&dist, &mut rng);
                    state = SimplifiedNlheGame::next(state, action, &mut rng);
                }
                NodeKind::Player(actor) if actor == target => {
                    let best = estimate_best_action_value(
                        game,
                        &state,
                        target,
                        blueprint_strategy,
                        config,
                        probe_idx,
                    )?;
                    values.push(best);
                    if target == 0 {
                        values_p0.push(best);
                    } else {
                        values_p1.push(best);
                    }
                    reached = true;
                    break;
                }
                NodeKind::Player(actor) => {
                    let action =
                        sample_blueprint_action(&state, actor, blueprint_strategy, &mut rng)?;
                    state = SimplifiedNlheGame::next(state, action, &mut rng);
                }
            }
        }
        if !reached {
            skipped += 1;
        }
    }

    if values.is_empty() {
        return Err(NlheEvaluationError::InvalidConfig {
            reason: "no LBR probes reached a target-player decision".to_string(),
        });
    }

    let stats = sample_stats(&values);
    Ok(NlheLbrReport {
        probes_requested: config.probes,
        probes_used: values.len() as u64,
        terminal_or_unreached_probes: skipped,
        rollouts_per_action: config.rollouts_per_action,
        seed: config.seed,
        mean_best_response_chips: stats.mean,
        standard_error_chips: stats.standard_error,
        target0_mean_chips: sample_stats(&values_p0).mean,
        target1_mean_chips: sample_stats(&values_p1).mean,
    })
}

fn rollout_blueprint_vs_baseline(
    mut state: crate::training::nlhe::SimplifiedNlheState,
    blueprint_seat: SeatId,
    baseline: NlheBaselinePolicy,
    blueprint_strategy: &dyn Fn(&SimplifiedNlheInfoSet, usize) -> Vec<f64>,
    rng: &mut dyn RngSource,
    max_actions: usize,
) -> Result<crate::training::nlhe::SimplifiedNlheState, NlheEvaluationError> {
    for _ in 0..max_actions {
        match SimplifiedNlheGame::current(&state) {
            NodeKind::Terminal => return Ok(state),
            NodeKind::Chance => {
                let dist = SimplifiedNlheGame::chance_distribution(&state);
                let action = sample_discrete(&dist, rng);
                state = SimplifiedNlheGame::next(state, action, rng);
            }
            NodeKind::Player(actor) => {
                let actions = SimplifiedNlheGame::legal_actions(&state);
                if actions.is_empty() {
                    return Err(NlheEvaluationError::EmptyLegalActions {
                        state: format!("{:?}", SimplifiedNlheGame::current(&state)),
                    });
                }
                let action = if SeatId(actor) == blueprint_seat {
                    sample_blueprint_action(&state, actor, blueprint_strategy, rng)?
                } else {
                    baseline.select_action(&state, &actions, rng)?
                };
                state = SimplifiedNlheGame::next(state, action, rng);
            }
        }
    }
    Err(NlheEvaluationError::NonTerminalRollout { max_actions })
}

fn finish_blueprint_rollout(
    mut state: crate::training::nlhe::SimplifiedNlheState,
    blueprint_strategy: &dyn Fn(&SimplifiedNlheInfoSet, usize) -> Vec<f64>,
    rng: &mut dyn RngSource,
    max_actions: usize,
) -> Result<crate::training::nlhe::SimplifiedNlheState, NlheEvaluationError> {
    for _ in 0..max_actions {
        match SimplifiedNlheGame::current(&state) {
            NodeKind::Terminal => return Ok(state),
            NodeKind::Chance => {
                let dist = SimplifiedNlheGame::chance_distribution(&state);
                let action = sample_discrete(&dist, rng);
                state = SimplifiedNlheGame::next(state, action, rng);
            }
            NodeKind::Player(actor) => {
                let action = sample_blueprint_action(&state, actor, blueprint_strategy, rng)?;
                state = SimplifiedNlheGame::next(state, action, rng);
            }
        }
    }
    Err(NlheEvaluationError::NonTerminalRollout { max_actions })
}

fn estimate_best_action_value(
    _game: &SimplifiedNlheGame,
    state: &crate::training::nlhe::SimplifiedNlheState,
    target: PlayerId,
    blueprint_strategy: &dyn Fn(&SimplifiedNlheInfoSet, usize) -> Vec<f64>,
    config: &NlheLbrConfig,
    probe_idx: u64,
) -> Result<f64, NlheEvaluationError> {
    let actions = SimplifiedNlheGame::legal_actions(state);
    if actions.is_empty() {
        return Err(NlheEvaluationError::EmptyLegalActions {
            state: format!("{:?}", SimplifiedNlheGame::current(state)),
        });
    }
    let mut best = f64::NEG_INFINITY;
    for (action_idx, action) in actions.iter().enumerate() {
        let mut sum = 0.0;
        for rollout_idx in 0..config.rollouts_per_action {
            let seed = mix4(config.seed, probe_idx, action_idx as u64, rollout_idx);
            let mut rng = ChaCha20Rng::from_seed(seed);
            let next = SimplifiedNlheGame::next(state.clone(), *action, &mut rng);
            let terminal = finish_blueprint_rollout(
                next,
                blueprint_strategy,
                &mut rng,
                config.max_actions_per_rollout,
            )?;
            sum += SimplifiedNlheGame::payoff(&terminal, target);
        }
        let mean = sum / config.rollouts_per_action as f64;
        best = best.max(mean);
    }
    Ok(best)
}

fn sample_blueprint_action(
    state: &crate::training::nlhe::SimplifiedNlheState,
    actor: PlayerId,
    blueprint_strategy: &dyn Fn(&SimplifiedNlheInfoSet, usize) -> Vec<f64>,
    rng: &mut dyn RngSource,
) -> Result<SimplifiedNlheAction, NlheEvaluationError> {
    let actions = SimplifiedNlheGame::legal_actions(state);
    if actions.is_empty() {
        return Err(NlheEvaluationError::EmptyLegalActions {
            state: format!("{:?}", SimplifiedNlheGame::current(state)),
        });
    }
    let info = SimplifiedNlheGame::info_set(state, actor);
    let dist = strategy_distribution(&info, &actions, blueprint_strategy)?;
    Ok(sample_discrete(&dist, rng))
}

fn strategy_distribution(
    info: &SimplifiedNlheInfoSet,
    actions: &[SimplifiedNlheAction],
    strategy: &dyn Fn(&SimplifiedNlheInfoSet, usize) -> Vec<f64>,
) -> Result<Vec<(SimplifiedNlheAction, f64)>, NlheEvaluationError> {
    let raw = strategy(info, actions.len());
    if raw.is_empty() {
        let p = 1.0 / actions.len() as f64;
        return Ok(actions.iter().copied().map(|a| (a, p)).collect());
    }
    if raw.len() != actions.len() {
        return Err(NlheEvaluationError::StrategyLengthMismatch {
            info_set: format!("{info:?}"),
            expected: actions.len(),
            got: raw.len(),
        });
    }
    let mut sum = 0.0;
    for (idx, &p) in raw.iter().enumerate() {
        if !p.is_finite() || p < 0.0 {
            return Err(NlheEvaluationError::InvalidStrategyProbability {
                index: idx,
                probability: p,
            });
        }
        sum += p;
    }
    if !sum.is_finite() || sum <= 0.0 {
        return Err(NlheEvaluationError::InvalidStrategySum { sum });
    }
    let mut out = Vec::with_capacity(actions.len());
    for (action, p) in actions.iter().copied().zip(raw) {
        if p > 0.0 {
            out.push((action, p / sum));
        }
    }
    Ok(out)
}

fn passive_action(
    actions: &[SimplifiedNlheAction],
    call_when_possible: bool,
) -> SimplifiedNlheAction {
    if let Some(a) = actions
        .iter()
        .copied()
        .find(|a| matches!(a, SimplifiedNlheAction::Check))
    {
        return a;
    }
    if call_when_possible {
        if let Some(a) = actions
            .iter()
            .copied()
            .find(|a| matches!(a, SimplifiedNlheAction::Call { .. }))
        {
            return a;
        }
    }
    if let Some(a) = actions
        .iter()
        .copied()
        .find(|a| matches!(a, SimplifiedNlheAction::Fold))
    {
        return a;
    }
    actions[0]
}

fn tight_action(
    state: &crate::training::nlhe::SimplifiedNlheState,
    actions: &[SimplifiedNlheAction],
) -> Result<SimplifiedNlheAction, NlheEvaluationError> {
    if state.game_state.street() == Street::Preflop {
        let actor = state
            .game_state
            .current_player()
            .expect("tight_action only called on player node");
        let hole = state.game_state.players()[actor.0 as usize]
            .hole_cards
            .ok_or(NlheEvaluationError::MissingHoleCards { seat: actor })?;
        let continue_hand = is_tight_preflop_continue(hole);
        Ok(passive_action(actions, continue_hand))
    } else {
        if let Some(a) = actions
            .iter()
            .copied()
            .find(|a| matches!(a, SimplifiedNlheAction::Check))
        {
            return Ok(a);
        }
        let actor = state
            .game_state
            .current_player()
            .expect("tight_action only called on player node");
        let made = has_one_pair_or_better(state, actor)?;
        Ok(passive_action(actions, made))
    }
}

fn is_tight_preflop_continue(hole: [Card; 2]) -> bool {
    let r0 = hole[0].rank();
    let r1 = hole[1].rank();
    let suited = hole[0].suit() == hole[1].suit();
    if r0 == r1 {
        return rank_value(r0) >= rank_value(Rank::Ten);
    }
    let high = r0.max(r1);
    let low = r0.min(r1);
    matches!(
        (high, low),
        (Rank::Ace, Rank::King) | (Rank::Ace, Rank::Queen)
    ) || (suited
        && matches!(
            (high, low),
            (Rank::Ace, Rank::Jack) | (Rank::King, Rank::Queen)
        ))
}

fn rank_value(rank: Rank) -> u8 {
    rank as u8
}

fn has_one_pair_or_better(
    state: &crate::training::nlhe::SimplifiedNlheState,
    actor: SeatId,
) -> Result<bool, NlheEvaluationError> {
    let hole = state.game_state.players()[actor.0 as usize]
        .hole_cards
        .ok_or(NlheEvaluationError::MissingHoleCards { seat: actor })?;
    let board = state.game_state.board();
    let ev = NaiveHandEvaluator;
    let category = match board.len() {
        3 => {
            let cards = [hole[0], hole[1], board[0], board[1], board[2]];
            ev.eval5(&cards).category()
        }
        4 => {
            let cards = [hole[0], hole[1], board[0], board[1], board[2], board[3]];
            ev.eval6(&cards).category()
        }
        5 => {
            let cards = [
                hole[0], hole[1], board[0], board[1], board[2], board[3], board[4],
            ];
            ev.eval7(&cards).category()
        }
        _ => HandCategory::HighCard,
    };
    Ok(!matches!(category, HandCategory::HighCard))
}

fn payoff_for_seat(
    state: &crate::training::nlhe::SimplifiedNlheState,
    seat: SeatId,
) -> Result<f64, NlheEvaluationError> {
    state
        .game_state
        .payouts()
        .and_then(|payouts| {
            payouts
                .into_iter()
                .find(|(s, _)| *s == seat)
                .map(|(_, pnl)| pnl as f64)
        })
        .ok_or_else(|| NlheEvaluationError::EmptyLegalActions {
            state: "terminal state without payout for requested seat".to_string(),
        })
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
    let n = xs.len() as f64;
    let mean = xs.iter().sum::<f64>() / n;
    if xs.len() == 1 {
        return Stats {
            mean,
            standard_error: 0.0,
        };
    }
    let var = xs
        .iter()
        .map(|x| {
            let d = x - mean;
            d * d
        })
        .sum::<f64>()
        / (n - 1.0);
    Stats {
        mean,
        standard_error: var.sqrt() / n.sqrt(),
    }
}

fn mix3(seed: u64, a: u64, b: u64) -> u64 {
    mix64(seed ^ a.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ b.wrapping_mul(0xBF58_476D_1CE4_E5B9))
}

fn mix4(seed: u64, a: u64, b: u64, c: u64) -> u64 {
    mix64(
        seed ^ a.wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ b.wrapping_mul(0xBF58_476D_1CE4_E5B9)
            ^ c.wrapping_mul(0x94D0_49BB_1331_11EB),
    )
}

fn mix64(mut x: u64) -> u64 {
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}
