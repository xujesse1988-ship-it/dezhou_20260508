//! H3 简化 heads-up NLHE blueprint 评测与近似 LBR proxy。
//!
//! 本模块只复用现有 [`crate::training::nlhe::SimplifiedNlheGame`] /
//! [`crate::training::trainer::EsMccfrTrainer`] 查询面，不改变
//! game trait、checkpoint schema 或 bucket table schema。H3 的目标是把
//! blueprint-only 策略闭环跑通：固定 seed 可复现评测、多类基础 baseline 对战，
//! 以及一个工程用 local best-response proxy 趋势指标。

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::abstraction::equity::{EquityCalculator, MonteCarloEquity};
use crate::core::rng::{ChaCha20Rng, RngSource};
use crate::core::{Card, Rank, SeatId, Street};
use crate::error::NlheEvaluationError;
use crate::eval::{HandCategory, HandEvaluator, NaiveHandEvaluator};
use crate::training::game::{Game, NodeKind, PlayerId};
use crate::training::lbr::{estimate_lbr, estimate_lbr_filtered, LbrConfig, LbrReport};
use crate::training::nlhe::{SimplifiedNlheAction, SimplifiedNlheGame, SimplifiedNlheInfoSet};
use crate::training::sampling::sample_discrete;

const EQUITY_EV_BASELINE_ITER: u32 = 512;

/// H3 baseline policy 集合。
#[derive(Clone, Copy, Eq, PartialEq, Hash, Debug, Serialize, Deserialize)]
pub enum NlheBaselinePolicy {
    /// 当前抽象 legal actions 上均匀随机。
    Random,
    /// 在非 fold legal actions 上均匀随机；只在无非 fold 动作时退回全量随机。
    RandomNoFold,
    /// 可 check 则 check；面对下注优先 call，否则 fold；不主动 bet/raise。
    CallStation,
    /// Preflop 只继续 TT+ / AK / AQ / AJs / KQs；postflop 面对下注仅 made hand
    /// one-pair+ 继续；不主动 bet/raise。
    OverlyTight,
    /// 用手牌 + 当前公共牌估计对随机对手胜率，按正 EV 边界选择接近下注额。
    EquityEv,
}

impl NlheBaselinePolicy {
    pub fn label(self) -> &'static str {
        match self {
            NlheBaselinePolicy::Random => "random",
            NlheBaselinePolicy::RandomNoFold => "random-no-fold",
            NlheBaselinePolicy::CallStation => "call-station",
            NlheBaselinePolicy::OverlyTight => "overly-tight",
            NlheBaselinePolicy::EquityEv => "equity-ev",
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
            NlheBaselinePolicy::RandomNoFold => random_no_fold_action(actions, rng),
            NlheBaselinePolicy::CallStation => passive_action(actions, true),
            NlheBaselinePolicy::OverlyTight => tight_action(state, actions)?,
            NlheBaselinePolicy::EquityEv => equity_ev_action(state, actions, rng)?,
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

/// 6-max（多人）blueprint vs baseline 评测结果（S4 gate）。
///
/// 与 HU [`NlheEvaluationReport`] 的区别：blueprint 轮遍全部 `n_players` 座（每座
/// `hands_per_seat` 手）vs **其余 N-1 座全打同一 baseline**，并按**相对按钮的位置**
/// （offset 0 = 按钮 BTN、1 = SB、2 = BB、3 = UTG、4 = HJ/MP、5 = CO）拆收益——
/// 6-max 位置差异巨大，总均值会掩盖。门槛（S4）：1,000,000 手稳定击败 random /
/// call-station / tight-aggressive（必要非充分）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NlheMultiwayEvalReport {
    pub baseline: NlheBaselinePolicy,
    pub n_players: usize,
    /// 总手数 = `hands_per_seat * n_players`。
    pub hands: u64,
    pub hands_per_seat: u64,
    pub seed: u64,
    pub blueprint_total_chips: f64,
    pub mbb_per_game: f64,
    pub standard_error_mbb_per_game: f64,
    pub ci95_low_mbb_per_game: f64,
    pub ci95_high_mbb_per_game: f64,
    /// 按相对按钮位置 offset 拆的 mbb/g（下标 = `(seat - button) mod n_players`；
    /// 0 = BTN、1 = SB、2 = BB、3 = UTG、...）。长度 = `n_players`。
    pub per_position_mbb_per_game: Vec<f64>,
}

/// 6-max blueprint A vs blueprint B 互评结果（S5①，**同 betting tree**）。
///
/// 与 [`NlheMultiwayEvalReport`] 的唯一区别：对手不是启发式 baseline 而是另一个
/// blueprint 策略，故无 `baseline` 字段。mbb/g 从 **hero(A)** 视角，正数 = A 净赢 B。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NlheBlueprintH2hReport {
    pub n_players: usize,
    /// 总手数 = `hands_per_seat * n_players`。
    pub hands: u64,
    pub hands_per_seat: u64,
    pub seed: u64,
    pub hero_total_chips: f64,
    pub mbb_per_game: f64,
    pub standard_error_mbb_per_game: f64,
    pub ci95_low_mbb_per_game: f64,
    pub ci95_high_mbb_per_game: f64,
    /// 按相对按钮位置 offset 拆的 mbb/g（0 = BTN、1 = SB、2 = BB、3 = UTG、...）。长度 = `n_players`。
    pub per_position_mbb_per_game: Vec<f64>,
}

/// 历史名称，等价于 [`LbrConfig`]。LBR proxy 已 game-generic 化，新代码请直接
/// 用 [`LbrConfig`]；此 alias 仅为保持公开 API 稳定（`NlheLbrConfig::default()` /
/// 字段访问全部继承自 generic）。
pub type NlheLbrConfig = LbrConfig;

/// 历史名称，等价于 [`LbrReport`]。
pub type NlheLbrReport = LbrReport;

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

/// 6-max（多人）blueprint vs baseline 评测（S4 gate）。blueprint 依次坐遍全部
/// `n_players` 座（每座 `hands_per_seat` 手），**其余 N-1 座全打同一 `baseline`**；
/// 复用 [`rollout_blueprint_vs_baseline`]（已 N-generic：actor == blueprint_seat 走
/// blueprint，否则走 baseline）。按相对按钮位置拆收益。
///
/// 每座、每手用 `mix3(seed, seat, hand)` 派生独立 rng → 固定 seed 可复现（S5 要求）。
/// n_players == 2 时与 [`evaluate_blueprint_vs_baseline`] 同口径（轮遍 2 座 vs 1 对手），
/// 只是报告形态换成 per-position（HU 那版保留 SB/BB 命名，不动）。
pub fn evaluate_blueprint_vs_baseline_multiway(
    game: &SimplifiedNlheGame,
    blueprint_strategy: &dyn Fn(&SimplifiedNlheInfoSet, usize) -> Vec<f64>,
    baseline: NlheBaselinePolicy,
    config: &NlheEvaluationConfig,
) -> Result<NlheMultiwayEvalReport, NlheEvaluationError> {
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

    let n_players = game.n_players();
    let button = game.config.button_seat.0 as usize;
    let mut all_pnl_chips: Vec<f64> =
        Vec::with_capacity(config.hands_per_seat as usize * n_players);
    let mut per_pos_sum = vec![0.0_f64; n_players];

    for seat_idx in 0..n_players {
        let blueprint_seat = SeatId(seat_idx as u8);
        // 相对按钮的位置 offset（0 = BTN、1 = SB、2 = BB、...）。
        let offset = (seat_idx + n_players - button) % n_players;
        for hand_idx in 0..config.hands_per_seat {
            let seed = mix3(config.seed, seat_idx as u64, hand_idx);
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
            per_pos_sum[offset] += pnl;
            all_pnl_chips.push(pnl);
        }
    }

    let hands = all_pnl_chips.len() as u64;
    let bb_chips = game.config.big_blind.as_u64() as f64;
    let scale = 1000.0 / bb_chips;
    let stats = sample_stats(&all_pnl_chips);
    let mean_mbb = stats.mean * scale;
    let se_mbb = stats.standard_error * scale;
    let per_position_mbb_per_game = per_pos_sum
        .iter()
        .map(|s| (s / config.hands_per_seat as f64) * scale)
        .collect();
    Ok(NlheMultiwayEvalReport {
        baseline,
        n_players,
        hands,
        hands_per_seat: config.hands_per_seat,
        seed: config.seed,
        blueprint_total_chips: all_pnl_chips.iter().sum(),
        mbb_per_game: mean_mbb,
        standard_error_mbb_per_game: se_mbb,
        ci95_low_mbb_per_game: mean_mbb - 1.96 * se_mbb,
        ci95_high_mbb_per_game: mean_mbb + 1.96 * se_mbb,
        per_position_mbb_per_game,
    })
}

/// 6-max blueprint A vs blueprint B 互评（S5①「相对强度」，**要求 A/B 共用同一 `game`**
/// = 同 betting tree + 同 bucket）。hero(A) 依次坐遍全部 `n_players` 座（每座
/// `hands_per_seat` 手），其余 N-1 座全用 `opponent_strategy`(B)；按相对按钮位置拆收益。
///
/// 复用 [`sample_blueprint_action`]（与 baseline 版同一查询面）：actor == hero_seat 走
/// `hero_strategy`，否则走 `opponent_strategy`。每座、每手用 `mix3(seed, seat, hand)`
/// 派生独立 rng → 固定 seed 可复现。
///
/// **不能用于不同抽象**（baseline/nolimp/preopen 是不同 betting tree）——那需要
/// off-tree advisor 引擎（单一权威 `GameState` + 每方抽象影子），见
/// `docs/six_max_nlhe_target.md` S5 §6 / `docs/temp/openpoker_client_design_2026_06_02.md` §6。
pub fn evaluate_blueprint_vs_blueprint_multiway(
    game: &SimplifiedNlheGame,
    hero_strategy: &dyn Fn(&SimplifiedNlheInfoSet, usize) -> Vec<f64>,
    opponent_strategy: &dyn Fn(&SimplifiedNlheInfoSet, usize) -> Vec<f64>,
    config: &NlheEvaluationConfig,
) -> Result<NlheBlueprintH2hReport, NlheEvaluationError> {
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

    let n_players = game.n_players();
    let button = game.config.button_seat.0 as usize;
    let mut all_pnl_chips: Vec<f64> =
        Vec::with_capacity(config.hands_per_seat as usize * n_players);
    let mut per_pos_sum = vec![0.0_f64; n_players];

    for seat_idx in 0..n_players {
        let hero_seat = SeatId(seat_idx as u8);
        let offset = (seat_idx + n_players - button) % n_players;
        for hand_idx in 0..config.hands_per_seat {
            let seed = mix3(config.seed, seat_idx as u64, hand_idx);
            let mut rng = ChaCha20Rng::from_seed(seed);
            let root = game.root(&mut rng);
            let terminal = rollout_blueprint_vs_blueprint(
                root,
                hero_seat,
                hero_strategy,
                opponent_strategy,
                &mut rng,
                config.max_actions_per_hand,
            )?;
            let pnl = payoff_for_seat(&terminal, hero_seat)?;
            per_pos_sum[offset] += pnl;
            all_pnl_chips.push(pnl);
        }
    }

    let hands = all_pnl_chips.len() as u64;
    let bb_chips = game.config.big_blind.as_u64() as f64;
    let scale = 1000.0 / bb_chips;
    let stats = sample_stats(&all_pnl_chips);
    let mean_mbb = stats.mean * scale;
    let se_mbb = stats.standard_error * scale;
    let per_position_mbb_per_game = per_pos_sum
        .iter()
        .map(|s| (s / config.hands_per_seat as f64) * scale)
        .collect();
    Ok(NlheBlueprintH2hReport {
        n_players,
        hands,
        hands_per_seat: config.hands_per_seat,
        seed: config.seed,
        hero_total_chips: all_pnl_chips.iter().sum(),
        mbb_per_game: mean_mbb,
        standard_error_mbb_per_game: se_mbb,
        ci95_low_mbb_per_game: mean_mbb - 1.96 * se_mbb,
        ci95_high_mbb_per_game: mean_mbb + 1.96 * se_mbb,
        per_position_mbb_per_game,
    })
}

/// 估计 H3 工程用 local best-response proxy。薄壳调用 game-generic
/// [`estimate_lbr`]，保持公开 API 稳定。
pub fn estimate_simplified_nlhe_lbr(
    game: &SimplifiedNlheGame,
    blueprint_strategy: &dyn Fn(&SimplifiedNlheInfoSet, usize) -> Vec<f64>,
    config: &NlheLbrConfig,
) -> Result<NlheLbrReport, NlheEvaluationError> {
    estimate_lbr::<SimplifiedNlheGame>(game, blueprint_strategy, config)
}

/// 带 probe filter 的 LBR proxy。薄壳调用 game-generic [`estimate_lbr_filtered`]。
/// `probe_accept(state, target_info)` 在 target player 决策点上被调用，可以
/// 同时查 state（如 `state.game_state.street()`）和 trainer 表。
pub fn estimate_simplified_nlhe_lbr_filtered(
    game: &SimplifiedNlheGame,
    blueprint_strategy: &dyn Fn(&SimplifiedNlheInfoSet, usize) -> Vec<f64>,
    probe_accept: &dyn Fn(
        &crate::training::nlhe::SimplifiedNlheState,
        &SimplifiedNlheInfoSet,
    ) -> bool,
    config: &NlheLbrConfig,
) -> Result<NlheLbrReport, NlheEvaluationError> {
    estimate_lbr_filtered::<SimplifiedNlheGame>(game, blueprint_strategy, probe_accept, config)
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

/// 与 [`rollout_blueprint_vs_baseline`] 同形，但非 hero 座位走第二个 blueprint 策略
/// 而非启发式 baseline（要求两策略来自同一 `game` 的 infoset 空间）。
fn rollout_blueprint_vs_blueprint(
    mut state: crate::training::nlhe::SimplifiedNlheState,
    hero_seat: SeatId,
    hero_strategy: &dyn Fn(&SimplifiedNlheInfoSet, usize) -> Vec<f64>,
    opponent_strategy: &dyn Fn(&SimplifiedNlheInfoSet, usize) -> Vec<f64>,
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
                let strategy = if SeatId(actor) == hero_seat {
                    hero_strategy
                } else {
                    opponent_strategy
                };
                let action = sample_blueprint_action(&state, actor, strategy, rng)?;
                state = SimplifiedNlheGame::next(state, action, rng);
            }
        }
    }
    Err(NlheEvaluationError::NonTerminalRollout { max_actions })
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

fn random_no_fold_action(
    actions: &[SimplifiedNlheAction],
    rng: &mut dyn RngSource,
) -> SimplifiedNlheAction {
    let non_fold_count = actions
        .iter()
        .filter(|a| !matches!(a, SimplifiedNlheAction::Fold))
        .count();
    if non_fold_count == 0 {
        let idx = (rng.next_u64() as usize) % actions.len();
        return actions[idx];
    }

    let target = (rng.next_u64() as usize) % non_fold_count;
    actions
        .iter()
        .copied()
        .filter(|a| !matches!(a, SimplifiedNlheAction::Fold))
        .nth(target)
        .expect("target index is bounded by non_fold_count")
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

fn equity_ev_action(
    state: &crate::training::nlhe::SimplifiedNlheState,
    actions: &[SimplifiedNlheAction],
    rng: &mut dyn RngSource,
) -> Result<SimplifiedNlheAction, NlheEvaluationError> {
    let actor = state
        .game_state
        .current_player()
        .expect("equity_ev_action only called on player node");
    let player = &state.game_state.players()[actor.0 as usize];
    let hole = player
        .hole_cards
        .ok_or(NlheEvaluationError::MissingHoleCards { seat: actor })?;
    let equity = estimate_direct_equity(hole, state.game_state.board(), rng)?;
    let pot = state.game_state.pot().as_u64() as f64;

    let mut best: Option<(SimplifiedNlheAction, u64)> = None;
    for &action in actions {
        let Some(risk_chips) = action_incremental_risk_chips(action, player.committed_this_round)
        else {
            continue;
        };
        if risk_chips == 0 {
            continue;
        }
        let risk = risk_chips as f64;
        let ev = equity * pot - (1.0 - equity) * risk;
        if ev <= 0.0 {
            continue;
        }

        // Positive EV implies risk is below the break-even boundary; the largest
        // legal risk is therefore the closest abstract size to that boundary.
        if best
            .map(|(_, best_risk)| risk_chips > best_risk)
            .unwrap_or(true)
        {
            best = Some((action, risk_chips));
        }
    }

    if let Some((action, _)) = best {
        Ok(action)
    } else {
        Ok(passive_action(actions, false))
    }
}

fn estimate_direct_equity(
    hole: [Card; 2],
    board: &[Card],
    rng: &mut dyn RngSource,
) -> Result<f64, NlheEvaluationError> {
    let evaluator: Arc<dyn HandEvaluator> = Arc::new(NaiveHandEvaluator);
    let calc = MonteCarloEquity::new(evaluator)
        .with_iter(EQUITY_EV_BASELINE_ITER)
        .with_river_exact(true);
    calc.equity(hole, board, rng)
        .map_err(|e| NlheEvaluationError::InvalidConfig {
            reason: format!("equity-ev baseline equity failed: {e}"),
        })
}

fn action_incremental_risk_chips(
    action: SimplifiedNlheAction,
    committed_this_round: crate::core::ChipAmount,
) -> Option<u64> {
    let to = match action {
        SimplifiedNlheAction::Fold | SimplifiedNlheAction::Check => return None,
        SimplifiedNlheAction::Call { to }
        | SimplifiedNlheAction::Bet { to, .. }
        | SimplifiedNlheAction::Raise { to, .. }
        | SimplifiedNlheAction::AllIn { to } => to,
    };
    Some(to.as_u64().saturating_sub(committed_this_round.as_u64()))
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

fn mix64(mut x: u64) -> u64 {
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}
