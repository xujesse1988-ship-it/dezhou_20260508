//! Baseline opponents + 1M-hand sanity evaluation（API-480..API-484 / D-480..D-489）。
//!
//! stage 4 验收四锚点之一（D-480 字面 1M 手 vs 3 类 baseline 击败：
//! random ≥ +500 / call-station ≥ +200 / TAG ≥ +50 mbb/g + 95% CI 下界 > 0）。
//!
//! **F2 \[实现\] 状态**（2026-05-15）：3 baseline `act` + [`evaluate_vs_baseline`]
//! 全部落地。
//!
//! **3 类 baseline**（D-480 字面）：
//! - `RandomOpponent`: legal action 等概率随机（baseline minimum sanity）
//! - `CallStationOpponent`: 99% call/check + 1% random（aggression baseline）
//! - `TagOpponent`: preflop 20% top range raise + postflop 70% c-bet（tight-
//!   aggressive 真实人类风格 baseline）

use crate::abstraction::action_pluribus::PluribusAction;
use crate::abstraction::preflop::PreflopLossless169;
use crate::core::rng::{ChaCha20Rng, RngSource};
use crate::core::Street;
use crate::error::TrainerError;
use crate::rules::state::GameState;
use crate::training::game::{Game, NodeKind, PlayerId};
use crate::training::nlhe_6max::{NlheGame6, NlheGame6State};
use crate::training::trainer::Trainer;

/// stage 4 D-483 — 6-player NLHE opponent trait。
///
/// 3 baseline impl + 未来 stage 4-5 实时 search opponent 共用。`act` 在 decision
/// point 上选 1 个 [`PluribusAction`]；`rng` 显式注入（D-027 / D-050 字面继承
/// `RngSource`）。
pub trait Opponent6Max {
    /// stage 4 D-483 — 在 `state` 决策点上从 `legal_actions` 选 1 个 action。
    fn act(
        &mut self,
        state: &GameState,
        legal_actions: &[PluribusAction],
        rng: &mut dyn RngSource,
    ) -> PluribusAction;

    /// baseline 名称（eval result 输出用）。
    fn name(&self) -> &'static str;
}

/// stage 4 D-480 ① — random baseline（legal action 等概率随机）。
///
/// 最弱 baseline；blueprint 击败 random ≥ +500 mbb/g 是最低 sanity 要求。
#[derive(Clone, Copy, Debug, Default)]
pub struct RandomOpponent;

impl Opponent6Max for RandomOpponent {
    fn act(
        &mut self,
        _state: &GameState,
        legal_actions: &[PluribusAction],
        rng: &mut dyn RngSource,
    ) -> PluribusAction {
        debug_assert!(
            !legal_actions.is_empty(),
            "RandomOpponent::act called with empty legal_actions"
        );
        let idx = (rng.next_u64() as usize) % legal_actions.len();
        legal_actions[idx]
    }

    fn name(&self) -> &'static str {
        "random"
    }
}

/// stage 4 D-480 ② — call-station baseline（99% call/check + 1% random）。
///
/// 中等 baseline；过 passive；blueprint 击败 call_station ≥ +200 mbb/g 是
/// baseline sanity 第二档（D-480 字面）。1% random 是为避免死局（永远 call
/// 在某些 corner case 触发 stuck loop）。
#[derive(Clone, Copy, Debug, Default)]
pub struct CallStationOpponent;

impl Opponent6Max for CallStationOpponent {
    fn act(
        &mut self,
        _state: &GameState,
        legal_actions: &[PluribusAction],
        rng: &mut dyn RngSource,
    ) -> PluribusAction {
        debug_assert!(
            !legal_actions.is_empty(),
            "CallStationOpponent::act called with empty legal_actions"
        );
        // D-480 ②：99% call/check + 1% random（avoid 死局）。
        let dice = rng.next_u64() % 100;
        if dice < 1 {
            let idx = (rng.next_u64() as usize) % legal_actions.len();
            return legal_actions[idx];
        }
        if legal_actions.contains(&PluribusAction::Call) {
            return PluribusAction::Call;
        }
        if legal_actions.contains(&PluribusAction::Check) {
            return PluribusAction::Check;
        }
        if legal_actions.contains(&PluribusAction::Fold) {
            return PluribusAction::Fold;
        }
        legal_actions[0]
    }

    fn name(&self) -> &'static str {
        "call_station"
    }
}

/// stage 4 D-480 ③ — TAG baseline（tight-aggressive 真实人类风格）。
///
/// preflop 20% top range raise / 80% fold + postflop 70% c-bet + 其他 fold；
/// 最强 baseline；blueprint 击败 TAG ≥ +50 mbb/g 是 baseline sanity 最高档
/// （但仍是 baseline 而非真正强对手；真正评测走 Slumbot 100K 手）。
pub struct TagOpponent {
    /// D-480 ③ 默认 20% top range（hand_class 169 中前 33 个 = ~19.5%）。
    pub preflop_top_range_pct: u8,
    /// D-480 ③ 默认 70% c-bet rate。
    pub postflop_cbet_pct: u8,
}

impl Default for TagOpponent {
    fn default() -> Self {
        Self {
            preflop_top_range_pct: 20,
            postflop_cbet_pct: 70,
        }
    }
}

impl TagOpponent {
    /// D-480 ③ — 当前 actor preflop 是否在 top range（hand_class < threshold）。
    fn preflop_in_top_range(&self, state: &GameState) -> bool {
        let actor = match state.current_player() {
            Some(seat) => seat,
            None => return false,
        };
        let hole = match state.players()[actor.0 as usize].hole_cards {
            Some(h) => h,
            None => return false,
        };
        let preflop = PreflopLossless169::new();
        let hand_class = preflop.hand_class(hole);
        let threshold = (169u16 * u16::from(self.preflop_top_range_pct)) / 100;
        u16::from(hand_class) < threshold
    }
}

impl Opponent6Max for TagOpponent {
    fn act(
        &mut self,
        state: &GameState,
        legal_actions: &[PluribusAction],
        rng: &mut dyn RngSource,
    ) -> PluribusAction {
        debug_assert!(
            !legal_actions.is_empty(),
            "TagOpponent::act called with empty legal_actions"
        );
        let is_preflop = state.street() == Street::Preflop;
        if is_preflop {
            if self.preflop_in_top_range(state) {
                for raise in [
                    PluribusAction::Raise1Pot,
                    PluribusAction::Raise075Pot,
                    PluribusAction::Raise2Pot,
                    PluribusAction::Raise05Pot,
                    PluribusAction::Raise3Pot,
                ] {
                    if legal_actions.contains(&raise) {
                        return raise;
                    }
                }
                if legal_actions.contains(&PluribusAction::Call) {
                    return PluribusAction::Call;
                }
                if legal_actions.contains(&PluribusAction::Check) {
                    return PluribusAction::Check;
                }
                if legal_actions.contains(&PluribusAction::AllIn) {
                    return PluribusAction::AllIn;
                }
                return legal_actions[0];
            }
            if legal_actions.contains(&PluribusAction::Check) {
                return PluribusAction::Check;
            }
            if legal_actions.contains(&PluribusAction::Fold) {
                return PluribusAction::Fold;
            }
            return legal_actions[0];
        }
        // Postflop：70% c-bet (1pot raise / bet) / 否则 check-fold
        let dice = rng.next_u64() % 100;
        let cbet = dice < u64::from(self.postflop_cbet_pct);
        if cbet {
            for raise in [
                PluribusAction::Raise075Pot,
                PluribusAction::Raise1Pot,
                PluribusAction::Raise05Pot,
                PluribusAction::Raise15Pot,
            ] {
                if legal_actions.contains(&raise) {
                    return raise;
                }
            }
        }
        if legal_actions.contains(&PluribusAction::Check) {
            return PluribusAction::Check;
        }
        if legal_actions.contains(&PluribusAction::Fold) {
            return PluribusAction::Fold;
        }
        legal_actions[0]
    }

    fn name(&self) -> &'static str {
        "tag"
    }
}

/// stage 4 D-481 / API-484 — baseline 评测结果。
#[derive(Clone, Debug)]
pub struct BaselineEvalResult {
    pub mean_mbbg: f64,
    pub standard_error_mbbg: f64,
    pub n_hands: u64,
    pub opponent_name: String,
    /// blueprint 占用 seat（D-481 字面 4 或 5 seats / 1M 手 baseline 占
    /// 1 或 2 seats）。
    pub blueprint_seats: Vec<usize>,
    pub opponent_seats: Vec<usize>,
}

/// stage 4 D-481 / API-484 — 1M 手 baseline sanity 评测入口。
///
/// `blueprint` 占 n_players-1 seats / `opponent` 占 1 seat（D-481 字面），1M 手
/// deal → 计算 blueprint 视角 mean mbb/g + SE（CI 调用方算）。
///
/// **F2 \[实现\]**：6-max 仿真主循环。每 hand 选 1 个 opponent seat (seat 0)，
/// 其余 seats 走 blueprint average strategy sample；blueprint 视角 chip 净收益
/// 累积。
///
/// blueprint 视角 mbb/g = `(sum_{blueprint_seats} chip_pnl) / (n_blueprint_seats
/// × BB) × 1000`（按"每 blueprint seat 平均 mbb/g" 标定）。
///
/// **当前限定**：generic G: Game 接口允许任意 game，但实际 stage 4 baseline 主路径
/// 锁 NlheGame6（test signature `evaluate_vs_baseline::<NlheGame6, _, _>`）。
/// 通过 [`std::any::Any`] downcast 在 NlheGame6 路径上接 [`Opponent6Max::act`]
/// `GameState` 引用；其它 G 路径调用 panic（baseline 主路径设计要求）。
pub fn evaluate_vs_baseline<G, T, O>(
    blueprint: &T,
    opponent: &mut O,
    n_hands: u64,
    master_seed: u64,
    _rng: &mut dyn RngSource,
) -> Result<BaselineEvalResult, TrainerError>
where
    G: Game + 'static,
    G::State: 'static,
    G::Action: 'static,
    T: Trainer<G>,
    O: Opponent6Max,
{
    let game = blueprint.game_ref();
    let n_players = game.n_players();
    if n_players < 2 {
        return Err(TrainerError::UnsupportedBucketTable {
            expected: 0,
            got: 0,
        });
    }

    let start_wall = std::time::Instant::now();

    let mut sum_blueprint_mbb: f64 = 0.0;
    let mut sum_sq_blueprint_mbb: f64 = 0.0;
    let mut completed_hands: u64 = 0;

    // 固定 seat 配置：opponent 占 seat 0；blueprint 占 [1, n_players)。
    // n_players=6 → blueprint=5 / opponent=1（D-481 字面）；n_players=2 → 1/1
    // （HU 退化）。
    let opponent_seats: Vec<usize> = vec![0];
    let blueprint_seats: Vec<usize> = (1..n_players).collect();

    let big_blind = nlhe_big_blind::<G>(game).unwrap_or(100.0);

    for hand_id in 0..n_hands {
        let hand_seed =
            splitmix64(master_seed ^ splitmix64(hand_id.wrapping_add(0x9E37_79B9_7F4A_7C15)));
        let mut hand_rng = ChaCha20Rng::from_seed(hand_seed);

        let mut state = game.root(&mut hand_rng);
        let mut steps: u64 = 0;
        const MAX_STEPS: u64 = 1024;
        loop {
            if steps > MAX_STEPS {
                // 防御性：state machine 不应在 1024 step 内不进入 Terminal
                // （stage 1 D-260 6-max NLHE 99 分位 < 32 actions）。
                break;
            }
            steps += 1;
            match G::current(&state) {
                NodeKind::Terminal => break,
                NodeKind::Chance => {
                    let dist = G::chance_distribution(&state);
                    let action = sample_discrete(&dist, &mut hand_rng);
                    state = G::next(state, action, &mut hand_rng);
                }
                NodeKind::Player(actor) => {
                    let actor_usize = actor as usize;
                    let is_opponent = actor_usize == opponent_seats[0];
                    let actions = G::legal_actions(&state);
                    if actions.is_empty() {
                        break;
                    }

                    let chosen = if is_opponent {
                        let pl_actions = actions_to_pluribus::<G>(&actions);
                        let game_state_ref = nlhe_game_state_ref::<G>(&state)
                            .expect("evaluate_vs_baseline: opponent must run on NlheGame6");
                        let pluribus_choice =
                            opponent.act(game_state_ref, &pl_actions, &mut hand_rng);
                        let mut picked: Option<G::Action> = None;
                        for a in actions.iter().copied() {
                            if pluribus_action_eq::<G::Action>(&a, pluribus_choice) {
                                picked = Some(a);
                                break;
                            }
                        }
                        picked.unwrap_or(actions[0])
                    } else {
                        let info = G::info_set(&state, actor);
                        let avg = blueprint.average_strategy_for_traverser(actor, &info);
                        sample_blueprint_action(&actions, &avg, &mut hand_rng)
                    };

                    state = G::next(state, chosen, &mut hand_rng);
                }
            }
        }

        // terminal：累 blueprint 视角 chip pnl
        if matches!(G::current(&state), NodeKind::Terminal) {
            let mut hand_blueprint_chips: f64 = 0.0;
            for &seat in &blueprint_seats {
                let chip_pnl = G::payoff(&state, seat as PlayerId);
                hand_blueprint_chips += chip_pnl;
            }
            let mbb_per_seat =
                hand_blueprint_chips / big_blind * 1000.0 / blueprint_seats.len() as f64;
            sum_blueprint_mbb += mbb_per_seat;
            sum_sq_blueprint_mbb += mbb_per_seat * mbb_per_seat;
            completed_hands += 1;
        }
    }

    let n = completed_hands as f64;
    let mean = if completed_hands > 0 {
        sum_blueprint_mbb / n
    } else {
        0.0
    };
    let variance = if completed_hands > 1 {
        let m2 = sum_sq_blueprint_mbb / n - mean * mean;
        m2.max(0.0) * n / (n - 1.0)
    } else {
        0.0
    };
    let standard_error = if completed_hands > 0 {
        (variance / n).sqrt()
    } else {
        0.0
    };

    let _wall = start_wall.elapsed().as_secs_f64();

    Ok(BaselineEvalResult {
        mean_mbbg: mean,
        standard_error_mbbg: standard_error,
        n_hands: completed_hands,
        opponent_name: opponent.name().to_string(),
        blueprint_seats,
        opponent_seats,
    })
}

/// D-335 SplitMix64 finalizer（hand-level seed 派生）。
fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// sample 1 个 action 按离散分布。
fn sample_discrete<A: Copy>(dist: &[(A, f64)], rng: &mut dyn RngSource) -> A {
    debug_assert!(!dist.is_empty(), "sample_discrete: empty distribution");
    let r = (rng.next_u64() as f64) / (u64::MAX as f64);
    let mut cumulative = 0.0;
    for (action, prob) in dist {
        cumulative += *prob;
        if r <= cumulative {
            return *action;
        }
    }
    dist[dist.len() - 1].0
}

/// blueprint sample（同 lbr.rs::sample_blueprint_action 政策）。
fn sample_blueprint_action<A: Copy>(actions: &[A], avg: &[f64], rng: &mut dyn RngSource) -> A {
    debug_assert!(
        !actions.is_empty(),
        "sample_blueprint_action: empty actions"
    );
    if avg.len() != actions.len() {
        let idx = (rng.next_u64() as usize) % actions.len();
        return actions[idx];
    }
    let r = (rng.next_u64() as f64) / (u64::MAX as f64);
    let mut cumulative = 0.0;
    for (i, p) in avg.iter().enumerate() {
        cumulative += *p;
        if r <= cumulative {
            return actions[i];
        }
    }
    actions[actions.len() - 1]
}

/// G::Action → PluribusAction slice（NlheGame6 路径下走 Any downcast）。
fn actions_to_pluribus<G: Game>(actions: &[G::Action]) -> Vec<PluribusAction>
where
    G::Action: 'static,
{
    actions
        .iter()
        .map(|a| {
            let any_ref: &dyn std::any::Any = a;
            *any_ref.downcast_ref::<PluribusAction>().expect(
                "evaluate_vs_baseline: G::Action must be PluribusAction (stage 4 \
                         baseline 主路径锁 NlheGame6)",
            )
        })
        .collect()
}

fn pluribus_action_eq<A: 'static>(a: &A, target: PluribusAction) -> bool {
    let any_ref: &dyn std::any::Any = a;
    any_ref
        .downcast_ref::<PluribusAction>()
        .map(|p| *p == target)
        .unwrap_or(false)
}

/// 取 game state 内 GameState 引用（NlheGame6State 路径下走 game_state 字段）。
fn nlhe_game_state_ref<G: Game>(state: &G::State) -> Option<&GameState>
where
    G::State: 'static,
{
    let any_ref: &dyn std::any::Any = state;
    any_ref
        .downcast_ref::<NlheGame6State>()
        .map(|s| &s.game_state)
}

/// 取 NlheGame6 的 big_blind chip 数（mbb/g 单位换算分母）。
fn nlhe_big_blind<G: Game + 'static>(game: &G) -> Option<f64> {
    let any_ref: &dyn std::any::Any = game;
    any_ref
        .downcast_ref::<NlheGame6>()
        .map(|g| g.config().big_blind.as_u64() as f64)
}
