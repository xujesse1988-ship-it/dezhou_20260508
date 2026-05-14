//! Baseline opponents + 1M-hand sanity evaluation（API-480..API-484 / D-480..D-489）。
//!
//! stage 4 验收四锚点之一（D-480 字面 1M 手 vs 3 类 baseline 击败：
//! random ≥ +500 / call-station ≥ +200 / TAG ≥ +50 mbb/g + 95% CI 下界 > 0）。
//!
//! **A1 \[实现\] 状态**：[`Opponent6Max`] trait + [`RandomOpponent`] /
//! [`CallStationOpponent`] / [`TagOpponent`] 3 impl + [`BaselineEvalResult`]
//! struct + [`evaluate_vs_baseline`] free function 全 signature 锁；方法体
//! `unimplemented!()`，F2 \[实现\] 落地。
//!
//! **3 类 baseline**（D-480 字面）：
//! - `RandomOpponent`: legal action 等概率随机（baseline minimum sanity）
//! - `CallStationOpponent`: 99% call/check + 1% random（aggression baseline）
//! - `TagOpponent`: preflop 20% top range raise + postflop 70% c-bet（tight-
//!   aggressive 真实人类风格 baseline）

use crate::core::rng::RngSource;
use crate::error::TrainerError;
use crate::rules::state::GameState;

use crate::abstraction::action_pluribus::PluribusAction;
use crate::training::game::Game;
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
        state: &GameState,
        legal_actions: &[PluribusAction],
        rng: &mut dyn RngSource,
    ) -> PluribusAction {
        let _ = (state, legal_actions, rng);
        unimplemented!("stage 4 A1 [实现] scaffold: RandomOpponent::act 落地 F2 [实现] D-480")
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
        state: &GameState,
        legal_actions: &[PluribusAction],
        rng: &mut dyn RngSource,
    ) -> PluribusAction {
        let _ = (state, legal_actions, rng);
        unimplemented!("stage 4 A1 [实现] scaffold: CallStationOpponent::act 落地 F2 [实现] D-480")
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

impl Opponent6Max for TagOpponent {
    fn act(
        &mut self,
        state: &GameState,
        legal_actions: &[PluribusAction],
        rng: &mut dyn RngSource,
    ) -> PluribusAction {
        let _ = (state, legal_actions, rng);
        unimplemented!("stage 4 A1 [实现] scaffold: TagOpponent::act 落地 F2 [实现] D-480")
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
/// `blueprint` 占 4 或 5 seats / `opponent` 占其余 seats，1M 手 deal → 计算
/// blueprint 视角 mean mbb/g + 95% CI。
///
/// **A1 \[实现\] 状态**：方法体 `unimplemented!()`，F2 \[实现\] 落地。
pub fn evaluate_vs_baseline<G, T, O>(
    blueprint: &T,
    opponent: &mut O,
    n_hands: u64,
    master_seed: u64,
    rng: &mut dyn RngSource,
) -> Result<BaselineEvalResult, TrainerError>
where
    G: Game,
    T: Trainer<G>,
    O: Opponent6Max,
{
    let _ = (blueprint, opponent, n_hands, master_seed, rng);
    unimplemented!("stage 4 A1 [实现] scaffold: evaluate_vs_baseline 落地 F2 [实现] D-481")
}
