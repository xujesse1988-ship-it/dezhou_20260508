//! AIVAT 估计器核心（通用 over [`Game`]）+ 小游戏精确值函数 + full-tree 期望枚举。
//!
//! 见 `docs/aivat_eval.md`。AIVAT 估计量：
//! ```text
//! AIVAT(z) = U − Σ_{t ∈ chance ∪ eval决策} c_t ，  c_t = V(实际孩子) − E_{x~p_t}[V(孩子(x))]
//! ```
//! 每个 `c_t` 条件均值零（`p_t` = 该转移真实分布），故 `E[AIVAT] = E[U]` = 真值，对
//! **任意**固定值函数 `V` 成立。对手（未知策略）的决策节点**不**修正。
//!
//! 本模块提供：
//! - [`exact_state_value`]：小游戏（Kuhn/Leduc）full-tree 精确值函数，作 AIVAT 基线。
//! - [`enumerate_aivat_moments`]：在真实数据分布下 full-tree 枚举出 raw 与 AIVAT 的各阶
//!   矩（无 MC 噪声），用于无偏 / 降方差的确定性校验。
//!
//! 简化 NLHE 的 board 在 root 预发、无 chance 节点（见 [`crate::training::nlhe`]），其
//! 发牌修正在估计器里按已知牌 + 值表显式处理，不走本模块的 chance-node 枚举；但修正公式
//! 与此处一致。

use crate::core::rng::ChaCha20Rng;
use crate::training::game::{Game, NodeKind, PlayerId};

/// 决策 / chance transition 不消费 RNG（[`Game::next`] 约定：传入显式 action）；提供
/// 一个一次性 dummy 满足签名。
fn dummy_rng() -> ChaCha20Rng {
    ChaCha20Rng::from_seed(0)
}

/// 精确状态值函数：`V(state) = E[U_eval | state]`，`eval` 用 `sigma_eval`、对手用
/// `sigma_opp_model`。full-tree 递归，适用小游戏（Kuhn/Leduc）。AIVAT 的控制变量基线。
///
/// `sigma_*(info, n)` 必须返回长度 `n`、非负、和为 1 的分布（`n` = 该节点合法 action 数）。
pub fn exact_state_value<G: Game>(
    state: &G::State,
    eval: PlayerId,
    sigma_eval: &dyn Fn(&G::InfoSet, usize) -> Vec<f64>,
    sigma_opp_model: &dyn Fn(&G::InfoSet, usize) -> Vec<f64>,
) -> f64 {
    match G::current(state) {
        NodeKind::Terminal => G::payoff(state, eval),
        NodeKind::Chance => {
            let dist = G::chance_distribution(state);
            let mut rng = dummy_rng();
            let mut v = 0.0;
            for (a, p) in dist {
                let child = G::next(state.clone(), a, &mut rng);
                v += p * exact_state_value::<G>(&child, eval, sigma_eval, sigma_opp_model);
            }
            v
        }
        NodeKind::Player(actor) => {
            let info = G::info_set(state, actor);
            let actions = G::legal_actions(state);
            let n = actions.len();
            let sigma = if actor == eval {
                sigma_eval(&info, n)
            } else {
                sigma_opp_model(&info, n)
            };
            assert_eq!(
                sigma.len(),
                n,
                "strategy length must match legal action count"
            );
            let mut rng = dummy_rng();
            let mut v = 0.0;
            for (i, a) in actions.iter().enumerate() {
                let child = G::next(state.clone(), *a, &mut rng);
                v += sigma[i] * exact_state_value::<G>(&child, eval, sigma_eval, sigma_opp_model);
            }
            v
        }
    }
}

/// raw 与 AIVAT 在真实数据分布下的精确各阶矩（reach 加权，无 MC 噪声）。
#[derive(Clone, Copy, Debug)]
pub struct AivatMoments {
    /// `E[raw]` = `E[U_eval]`（真值）。
    pub e_raw: f64,
    /// `E[AIVAT]`。无偏时应 == `e_raw`。
    pub e_aivat: f64,
    /// `E[raw²]`。
    pub e_raw_sq: f64,
    /// `E[AIVAT²]`。
    pub e_aivat_sq: f64,
    /// 枚举到的 reach 概率总和（应 == 1，作健全性校验）。
    pub total_reach: f64,
}

impl AivatMoments {
    /// `Var[raw]`。
    pub fn var_raw(&self) -> f64 {
        self.e_raw_sq - self.e_raw * self.e_raw
    }
    /// `Var[AIVAT]`。
    pub fn var_aivat(&self) -> f64 {
        self.e_aivat_sq - self.e_aivat * self.e_aivat
    }
}

/// full-tree 枚举：在真实数据分布（`eval` 打 `sigma_eval`、对手打 `sigma_opp_real`、
/// chance 按 [`Game::chance_distribution`]）下，精确算出 raw 与 AIVAT 的各阶矩。
///
/// `value` 是 AIVAT 的值函数（任意固定函数，通常 =
/// `|s| exact_state_value(s, eval, sigma_eval, sigma_opp_model)`；`sigma_opp_model` 可
/// **不等于** `sigma_opp_real`，模拟"对手策略未知"——AIVAT 仍无偏）。
pub fn enumerate_aivat_moments<G: Game>(
    game: &G,
    eval: PlayerId,
    sigma_eval: &dyn Fn(&G::InfoSet, usize) -> Vec<f64>,
    sigma_opp_real: &dyn Fn(&G::InfoSet, usize) -> Vec<f64>,
    value: &dyn Fn(&G::State) -> f64,
) -> AivatMoments {
    let mut rng = dummy_rng();
    let root = game.root(&mut rng);
    let mut acc = AivatMoments {
        e_raw: 0.0,
        e_aivat: 0.0,
        e_raw_sq: 0.0,
        e_aivat_sq: 0.0,
        total_reach: 0.0,
    };
    recurse_moments::<G>(
        &root,
        eval,
        1.0,
        0.0,
        sigma_eval,
        sigma_opp_real,
        value,
        &mut acc,
    );
    acc
}

/// `acc_corr` = 沿当前路径累计的 `Σ c_t`；到 terminal 时 `AIVAT = U − acc_corr`。
#[allow(clippy::too_many_arguments)]
fn recurse_moments<G: Game>(
    state: &G::State,
    eval: PlayerId,
    reach: f64,
    acc_corr: f64,
    sigma_eval: &dyn Fn(&G::InfoSet, usize) -> Vec<f64>,
    sigma_opp_real: &dyn Fn(&G::InfoSet, usize) -> Vec<f64>,
    value: &dyn Fn(&G::State) -> f64,
    acc: &mut AivatMoments,
) {
    match G::current(state) {
        NodeKind::Terminal => {
            let raw = G::payoff(state, eval);
            let aivat = raw - acc_corr;
            acc.e_raw += reach * raw;
            acc.e_aivat += reach * aivat;
            acc.e_raw_sq += reach * raw * raw;
            acc.e_aivat_sq += reach * aivat * aivat;
            acc.total_reach += reach;
        }
        NodeKind::Chance => {
            // chance 转移：correction = V(实际孩子) − E_{x~p}[V(孩子(x))]
            let dist = G::chance_distribution(state);
            let mut rng = dummy_rng();
            let mut children: Vec<(G::State, f64, f64)> = Vec::with_capacity(dist.len());
            let mut baseline = 0.0;
            for (a, p) in &dist {
                let child = G::next(state.clone(), *a, &mut rng);
                let vc = value(&child);
                baseline += p * vc;
                children.push((child, *p, vc));
            }
            for (child, p, vc) in children {
                recurse_moments::<G>(
                    &child,
                    eval,
                    reach * p,
                    acc_corr + (vc - baseline),
                    sigma_eval,
                    sigma_opp_real,
                    value,
                    acc,
                );
            }
        }
        NodeKind::Player(actor) => {
            let info = G::info_set(state, actor);
            let actions = G::legal_actions(state);
            let n = actions.len();
            let mut rng = dummy_rng();
            if actor == eval {
                // 我方决策：correction = V(实际孩子) − Σ_a σ(a)·V(孩子(a))；reach 用 σ_eval。
                let sigma = sigma_eval(&info, n);
                assert_eq!(sigma.len(), n, "sigma_eval length must match action count");
                let mut children: Vec<(G::State, f64, f64)> = Vec::with_capacity(n);
                let mut baseline = 0.0;
                for (i, a) in actions.iter().enumerate() {
                    let child = G::next(state.clone(), *a, &mut rng);
                    let vc = value(&child);
                    baseline += sigma[i] * vc;
                    children.push((child, sigma[i], vc));
                }
                for (child, p, vc) in children {
                    recurse_moments::<G>(
                        &child,
                        eval,
                        reach * p,
                        acc_corr + (vc - baseline),
                        sigma_eval,
                        sigma_opp_real,
                        value,
                        acc,
                    );
                }
            } else {
                // 对手决策：**不修正**；数据分布按 σ_opp_real。
                let sigma = sigma_opp_real(&info, n);
                assert_eq!(
                    sigma.len(),
                    n,
                    "sigma_opp_real length must match action count"
                );
                for (i, a) in actions.iter().enumerate() {
                    let child = G::next(state.clone(), *a, &mut rng);
                    recurse_moments::<G>(
                        &child,
                        eval,
                        reach * sigma[i],
                        acc_corr,
                        sigma_eval,
                        sigma_opp_real,
                        value,
                        acc,
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::training::kuhn::KuhnGame;
    use crate::training::leduc::LeducGame;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// 确定性、合法（长度 n、非负、和 1）的"策略"，由 `salt` + infoset Debug 串派生；
    /// 不同 `salt` 给不同分布。用于校验 AIVAT 对任意策略组合都无偏。
    fn strat<I: std::fmt::Debug>(salt: u64) -> impl Fn(&I, usize) -> Vec<f64> {
        move |info, n| {
            // FNV-ish hash of (salt, debug(info))
            let mut h = salt
                .wrapping_mul(1099511628211)
                .wrapping_add(0x9e3779b97f4a7c15);
            for b in format!("{info:?}").bytes() {
                h = h.wrapping_mul(1099511628211).wrapping_add(b as u64);
            }
            let mut w = vec![0.0f64; n];
            let mut s = 0.0;
            for (i, wi) in w.iter_mut().enumerate() {
                // 每个 action 一个正权重（1..=8），保证严格 > 0 → 全 support。
                let bits = (h >> ((i * 11) % 53)) & 0x7;
                *wi = 1.0 + bits as f64;
                s += *wi;
            }
            for wi in w.iter_mut() {
                *wi /= s;
            }
            w
        }
    }

    /// 用持久 memo 包出值函数闭包，把 enumerate 的双重递归降到 O(N)。
    fn memoized_value<'a, G: Game>(
        eval: PlayerId,
        sigma_eval: &'a dyn Fn(&G::InfoSet, usize) -> Vec<f64>,
        sigma_opp_model: &'a dyn Fn(&G::InfoSet, usize) -> Vec<f64>,
        memo: &'a RefCell<HashMap<String, f64>>,
    ) -> impl Fn(&G::State) -> f64 + 'a
    where
        G::State: std::fmt::Debug,
    {
        move |state: &G::State| {
            let key = format!("{state:?}");
            if let Some(&v) = memo.borrow().get(&key) {
                return v;
            }
            let v = exact_state_value::<G>(state, eval, sigma_eval, sigma_opp_model);
            memo.borrow_mut().insert(key, v);
            v
        }
    }

    /// 对一个 game 跑全套断言：无偏（任意 opp model）+ 降方差，两个位置都测。
    fn assert_aivat_unbiased_and_reduces<G: Game + Default>()
    where
        G::State: std::fmt::Debug,
    {
        let game = G::default();
        let bp = strat::<G::InfoSet>(1); // 我方 blueprint
        let opp_real = strat::<G::InfoSet>(2); // 对手真实策略
        let opp_wrong = strat::<G::InfoSet>(7); // 给值函数用的"错误"对手模型

        for eval in [0u8, 1u8] {
            // 真值 = E[U_eval] under (blueprint vs 真实对手)
            let truth = {
                let mut rng = dummy_rng();
                let root = game.root(&mut rng);
                exact_state_value::<G>(&root, eval, &bp, &opp_real)
            };

            // (a) 值函数用 **错误** 对手模型（opp_wrong ≠ opp_real）—— 模拟对手策略未知。
            {
                let memo = RefCell::new(HashMap::new());
                let value = memoized_value::<G>(eval, &bp, &opp_wrong, &memo);
                let m = enumerate_aivat_moments::<G>(&game, eval, &bp, &opp_real, &value);
                assert!(
                    (m.total_reach - 1.0).abs() < 1e-9,
                    "reach sum {} != 1",
                    m.total_reach
                );
                assert!(
                    (m.e_raw - truth).abs() < 1e-9,
                    "eval{eval} wrong-model: E[raw] {} != truth {truth}",
                    m.e_raw
                );
                assert!(
                    (m.e_aivat - truth).abs() < 1e-9,
                    "eval{eval} wrong-model: E[AIVAT] {} != truth {truth} (BIASED!)",
                    m.e_aivat
                );
                assert!(
                    m.var_aivat() < m.var_raw(),
                    "eval{eval} wrong-model: var_aivat {} !< var_raw {}",
                    m.var_aivat(),
                    m.var_raw()
                );
            }

            // (b) 值函数用 **自对弈** 模型（opp_model == opp_real）—— 降方差应更强。
            {
                let memo = RefCell::new(HashMap::new());
                let value = memoized_value::<G>(eval, &bp, &opp_real, &memo);
                let m = enumerate_aivat_moments::<G>(&game, eval, &bp, &opp_real, &value);
                assert!(
                    (m.e_aivat - truth).abs() < 1e-9,
                    "eval{eval} self-play: E[AIVAT] {} != truth {truth} (BIASED!)",
                    m.e_aivat
                );
                assert!(
                    m.var_aivat() < m.var_raw(),
                    "eval{eval} self-play: var_aivat {} !< var_raw {}",
                    m.var_aivat(),
                    m.var_raw()
                );
            }
        }
    }

    #[test]
    fn aivat_unbiased_on_kuhn() {
        assert_aivat_unbiased_and_reduces::<KuhnGame>();
    }

    #[test]
    fn aivat_unbiased_on_leduc() {
        assert_aivat_unbiased_and_reduces::<LeducGame>();
    }
}
