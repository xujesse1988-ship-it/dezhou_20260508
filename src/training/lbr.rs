//! Game-generic local best-response (LBR) proxy。
//!
//! LBR proxy 流程：每个 probe 沿 blueprint self-play 走到目标玩家 (`probe_idx %
//! 2`) 的一个决策点，枚举该点 legal actions；每个 action 后续按 blueprint rollout，
//! 取最高 EV。Target 之后的未来决策也回到 blueprint，所以这是 *local* best
//! response，不是真 BR；它只用于比较同一评测配置下不同 checkpoint 的趋势。
//!
//! 实现按 [`Game`] trait 通用化，所以同一份代码同时跑 Leduc / Kuhn / 简化
//! NLHE。Leduc 上有精确 [`crate::training::exploitability`] 真值，本模块的输出可以
//! 直接与真值对照，作为 LBR proxy 方法学的校准证据（CLAUDE.md "在已知 Nash
//! 解的小博弈上对照"）。
//!
//! RNG seed 推导 (`mix3` / `mix4`) 严格匹配原 NLHE 实现，保证简化 NLHE 的
//! BLAKE3 snapshot 不漂。

use serde::{Deserialize, Serialize};

use crate::core::rng::{ChaCha20Rng, RngSource};
use crate::error::NlheEvaluationError;
use crate::training::game::{Game, NodeKind, PlayerId};
use crate::training::sampling::sample_discrete;

/// Local best-response proxy 配置。
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct LbrConfig {
    /// 采样多少个 target-player decision probe。
    pub probes: u64,
    /// 每个候选动作用多少条 blueprint rollout 估计 EV。
    pub rollouts_per_action: u64,
    pub seed: u64,
    pub max_actions_per_probe: usize,
    pub max_actions_per_rollout: usize,
}

impl Default for LbrConfig {
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

/// LBR proxy 输出。`mean_best_response_chips` 单位是 chip / probe，等价于
/// 在 probe 分布下 target 走 local-BR 的人均收益。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LbrReport {
    pub probes_requested: u64,
    pub probes_used: u64,
    pub terminal_or_unreached_probes: u64,
    /// 被 `probe_accept` filter 拒绝的 probe 数（[`estimate_lbr_filtered`] 路径专用；
    /// 默认 wrapper 走 `|_| true` 永远是 0）。
    #[serde(default)]
    pub filtered_probes: u64,
    pub rollouts_per_action: u64,
    pub seed: u64,
    pub mean_best_response_chips: f64,
    pub standard_error_chips: f64,
    pub target0_mean_chips: f64,
    pub target1_mean_chips: f64,
}

/// 估计 game 上的 LBR proxy。`blueprint_strategy` 返回空向量时按 uniform fallback；
/// 返回非空但长度与当前 legal action 数不同则报
/// [`NlheEvaluationError::StrategyLengthMismatch`]。
pub fn estimate_lbr<G: Game>(
    game: &G,
    blueprint_strategy: &dyn Fn(&G::InfoSet, usize) -> Vec<f64>,
    config: &LbrConfig,
) -> Result<LbrReport, NlheEvaluationError> {
    estimate_lbr_filtered(game, blueprint_strategy, &|_| true, config)
}

/// 带 probe filter 的 LBR proxy。`probe_accept(target_info)` 返回 false 时本次
/// probe 被丢弃（既不进 mean BR 估值，也不计入 `probes_used`，而是计入
/// [`LbrReport::filtered_probes`]）。用于回答"如果只在 blueprint 真实学过的
/// infoset 上 probe，LBR 会是多少"这类对照实验。
pub fn estimate_lbr_filtered<G: Game>(
    game: &G,
    blueprint_strategy: &dyn Fn(&G::InfoSet, usize) -> Vec<f64>,
    probe_accept: &dyn Fn(&G::InfoSet) -> bool,
    config: &LbrConfig,
) -> Result<LbrReport, NlheEvaluationError> {
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
    let mut filtered = 0u64;

    for probe_idx in 0..config.probes {
        let target = (probe_idx % 2) as PlayerId;
        let mut rng = ChaCha20Rng::from_seed(mix3(config.seed, 0x9b52, probe_idx));
        let mut state = game.root(&mut rng);
        let mut reached = false;

        for _ in 0..config.max_actions_per_probe {
            match G::current(&state) {
                NodeKind::Terminal => break,
                NodeKind::Chance => {
                    let dist = G::chance_distribution(&state);
                    let action = sample_discrete(&dist, &mut rng);
                    state = G::next(state, action, &mut rng);
                }
                NodeKind::Player(actor) if actor == target => {
                    let target_info = G::info_set(&state, actor);
                    if !probe_accept(&target_info) {
                        filtered += 1;
                        reached = true;
                        break;
                    }
                    let best = estimate_best_action_value::<G>(
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
                        sample_blueprint_action::<G>(&state, actor, blueprint_strategy, &mut rng)?;
                    state = G::next(state, action, &mut rng);
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
    Ok(LbrReport {
        probes_requested: config.probes,
        probes_used: values.len() as u64,
        terminal_or_unreached_probes: skipped,
        filtered_probes: filtered,
        rollouts_per_action: config.rollouts_per_action,
        seed: config.seed,
        mean_best_response_chips: stats.mean,
        standard_error_chips: stats.standard_error,
        target0_mean_chips: sample_stats(&values_p0).mean,
        target1_mean_chips: sample_stats(&values_p1).mean,
    })
}

fn estimate_best_action_value<G: Game>(
    state: &G::State,
    target: PlayerId,
    blueprint_strategy: &dyn Fn(&G::InfoSet, usize) -> Vec<f64>,
    config: &LbrConfig,
    probe_idx: u64,
) -> Result<f64, NlheEvaluationError> {
    let actions = G::legal_actions(state);
    if actions.is_empty() {
        return Err(NlheEvaluationError::EmptyLegalActions {
            state: format!("{:?}", G::current(state)),
        });
    }
    let mut best = f64::NEG_INFINITY;
    for (action_idx, action) in actions.iter().enumerate() {
        let mut sum = 0.0;
        for rollout_idx in 0..config.rollouts_per_action {
            let seed = mix4(config.seed, probe_idx, action_idx as u64, rollout_idx);
            let mut rng = ChaCha20Rng::from_seed(seed);
            let next = G::next(state.clone(), *action, &mut rng);
            let terminal = finish_blueprint_rollout::<G>(
                next,
                blueprint_strategy,
                &mut rng,
                config.max_actions_per_rollout,
            )?;
            sum += G::payoff(&terminal, target);
        }
        let mean = sum / config.rollouts_per_action as f64;
        best = best.max(mean);
    }
    Ok(best)
}

fn finish_blueprint_rollout<G: Game>(
    mut state: G::State,
    blueprint_strategy: &dyn Fn(&G::InfoSet, usize) -> Vec<f64>,
    rng: &mut dyn RngSource,
    max_actions: usize,
) -> Result<G::State, NlheEvaluationError> {
    for _ in 0..max_actions {
        match G::current(&state) {
            NodeKind::Terminal => return Ok(state),
            NodeKind::Chance => {
                let dist = G::chance_distribution(&state);
                let action = sample_discrete(&dist, rng);
                state = G::next(state, action, rng);
            }
            NodeKind::Player(actor) => {
                let action = sample_blueprint_action::<G>(&state, actor, blueprint_strategy, rng)?;
                state = G::next(state, action, rng);
            }
        }
    }
    Err(NlheEvaluationError::NonTerminalRollout { max_actions })
}

fn sample_blueprint_action<G: Game>(
    state: &G::State,
    actor: PlayerId,
    blueprint_strategy: &dyn Fn(&G::InfoSet, usize) -> Vec<f64>,
    rng: &mut dyn RngSource,
) -> Result<G::Action, NlheEvaluationError> {
    let actions = G::legal_actions(state);
    if actions.is_empty() {
        return Err(NlheEvaluationError::EmptyLegalActions {
            state: format!("{:?}", G::current(state)),
        });
    }
    let info = G::info_set(state, actor);
    let dist = strategy_distribution::<G>(&info, &actions, blueprint_strategy)?;
    Ok(sample_discrete(&dist, rng))
}

fn strategy_distribution<G: Game>(
    info: &G::InfoSet,
    actions: &[G::Action],
    strategy: &dyn Fn(&G::InfoSet, usize) -> Vec<f64>,
) -> Result<Vec<(G::Action, f64)>, NlheEvaluationError> {
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
