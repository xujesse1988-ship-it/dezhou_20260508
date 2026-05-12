//! `BestResponse` trait + `KuhnBestResponse` + `LeducBestResponse` + `exploitability`
//! 辅助函数（API-340..API-343 / D-340 / D-341 / D-344）。
//!
//! BestResponse 通过 full-tree backward induction 计算：给定对手策略 `σ_opp`，求
//! `target_player` 视角下最大化 EV 的 one-hot 策略（D-344 输出 `(strategy,
//! value)`）。
//!
//! 实现路径（policy iteration，K/L 共用）：
//! - 初始 `target_strategy = uniform`（HashMap 中未命中的 InfoSet 在 walk 中按
//!   `1 / n_actions` 退化分布消费）。
//! - 每轮：在 `target_strategy` 下 DFS 整棵 tree，per `(I, a)` 累积 `cfv[I][a] +=
//!   opp_reach × subtree_EV`；同时返回当前 strategy 下的 `EV(target)`。
//! - 派生 `new_strategy = one-hot argmax cfv[I][*]`；若 `new_strategy ==
//!   target_strategy` 收敛，返回最后一次 `EV` 即 BR value；否则 swap 续轮。
//! - 收敛条件保证：① BR 总满足 `BR_value ≥ EV(σ_opp, σ_opp)`（zero-sum），
//!   policy iteration 单调非降；② Kuhn 12 InfoSet / Leduc ~288 InfoSet 收敛量级
//!   `≤ 50` 轮（实测 5-10 轮即停）。
//!
//! `exploitability` = `(BR_0(σ_1) + BR_1(σ_0)) / 2`（D-340 / D-341），严格非负
//! （zero-sum + 收敛 BR 数学不变量）。

use std::collections::HashMap;
use std::hash::Hash;

use crate::core::rng::ChaCha20Rng;
use crate::training::game::{Game, NodeKind, PlayerId};
use crate::training::kuhn::{KuhnGame, KuhnInfoSet};
use crate::training::leduc::{LeducGame, LeducInfoSet};

/// Best response 计算 trait（API-340 / D-344）。
pub trait BestResponse<G: Game> {
    fn compute(
        game: &G,
        opponent_strategy: &dyn Fn(&G::InfoSet, usize) -> Vec<f64>,
        target_player: PlayerId,
    ) -> (HashMap<G::InfoSet, Vec<f64>>, f64);
}

/// Kuhn full-tree backward induction BR（API-341 / D-340）。
#[derive(Clone, Copy, Debug, Default)]
pub struct KuhnBestResponse;

impl BestResponse<KuhnGame> for KuhnBestResponse {
    fn compute(
        game: &KuhnGame,
        opponent_strategy: &dyn Fn(&KuhnInfoSet, usize) -> Vec<f64>,
        target_player: PlayerId,
    ) -> (HashMap<KuhnInfoSet, Vec<f64>>, f64) {
        compute_br_generic::<KuhnGame>(game, opponent_strategy, target_player)
    }
}

/// Leduc full-tree backward induction BR（API-342 / D-341）。
#[derive(Clone, Copy, Debug, Default)]
pub struct LeducBestResponse;

impl BestResponse<LeducGame> for LeducBestResponse {
    fn compute(
        game: &LeducGame,
        opponent_strategy: &dyn Fn(&LeducInfoSet, usize) -> Vec<f64>,
        target_player: PlayerId,
    ) -> (HashMap<LeducInfoSet, Vec<f64>>, f64) {
        compute_br_generic::<LeducGame>(game, opponent_strategy, target_player)
    }
}

/// Policy iteration BR（K/L 共用，避免重复）。
fn compute_br_generic<G: Game>(
    game: &G,
    opponent_strategy: &dyn Fn(&G::InfoSet, usize) -> Vec<f64>,
    target_player: PlayerId,
) -> (HashMap<G::InfoSet, Vec<f64>>, f64)
where
    G::InfoSet: Hash + Eq,
{
    let max_iter = 100;
    let mut target_strategy: HashMap<G::InfoSet, Vec<f64>> = HashMap::new();
    let mut best_strategy: HashMap<G::InfoSet, Vec<f64>> = HashMap::new();
    let mut best_value = f64::NEG_INFINITY;
    let mut stagnant = 0;
    let mut last_value_seen = f64::NEG_INFINITY;

    for _ in 0..max_iter {
        let mut cfv: HashMap<G::InfoSet, Vec<f64>> = HashMap::new();
        let mut rng = ChaCha20Rng::from_seed(0);
        let root = game.root(&mut rng);
        let cur_value = walk_cfv::<G>(
            &root,
            1.0,
            target_player,
            opponent_strategy,
            &target_strategy,
            &mut cfv,
        );

        // 跟踪 PI 过程中遇到的最大 EV，避免 PI cycle 时返回 sub-optimal target_strategy。
        // policy improvement 数学上单调非降，但浮点 tie-breaking + argmax 不确定性
        // 可能让 strategy 在等价 EV 之间震荡；max-tracking 等价稳态。
        if cur_value > best_value {
            best_value = cur_value;
            best_strategy = target_strategy.clone();
        }

        // Derive new one-hot strategy from argmax of cfv per info_set。
        let mut new_strategy: HashMap<G::InfoSet, Vec<f64>> = HashMap::new();
        for (info, vals) in cfv.iter() {
            let mut best_idx = 0usize;
            let mut best_val = vals[0];
            for (i, &v) in vals.iter().enumerate().skip(1) {
                if v > best_val {
                    best_val = v;
                    best_idx = i;
                }
            }
            let mut one_hot = vec![0.0; vals.len()];
            one_hot[best_idx] = 1.0;
            new_strategy.insert(info.clone(), one_hot);
        }

        if new_strategy == target_strategy {
            let final_value = best_value.max(cur_value);
            let final_strategy = if cur_value >= best_value {
                target_strategy
            } else {
                best_strategy
            };
            return (final_strategy, final_value);
        }

        // 早退：value stagnation（连续 5 轮无 strict improvement）
        let stagnant_now = cur_value <= last_value_seen + 1e-12;
        if stagnant_now {
            stagnant += 1;
        } else {
            stagnant = 0;
        }
        last_value_seen = cur_value;
        target_strategy = new_strategy;
        if stagnant_now && stagnant >= 5 {
            break;
        }
    }

    // 未在 max_iter 内收敛：返回 best-seen（最大值对应的 strategy）。
    (best_strategy, best_value)
}

/// DFS 走树：在 `target_strategy` + `opp_strategy` 固定下，per `(I, a)` 累积
/// `cfv[I][a] += opp_reach × subtree_EV`；返回当前 strategy 组合下 target 视角
/// `EV = opp_reach × E[utility(target)]`（root call 时 opp_reach = 1.0 即 EV 本身）。
fn walk_cfv<G: Game>(
    state: &G::State,
    opp_reach: f64,
    target: PlayerId,
    opp_strategy: &dyn Fn(&G::InfoSet, usize) -> Vec<f64>,
    target_strategy: &HashMap<G::InfoSet, Vec<f64>>,
    cfv: &mut HashMap<G::InfoSet, Vec<f64>>,
) -> f64 {
    match G::current(state) {
        NodeKind::Terminal => opp_reach * G::payoff(state, target),
        NodeKind::Chance => {
            let dist = G::chance_distribution(state);
            let mut v = 0.0;
            let mut rng = ChaCha20Rng::from_seed(0);
            for (action, p) in dist {
                let next = G::next(state.clone(), action, &mut rng);
                v += walk_cfv::<G>(
                    &next,
                    opp_reach * p,
                    target,
                    opp_strategy,
                    target_strategy,
                    cfv,
                );
            }
            v
        }
        NodeKind::Player(actor) => {
            let info = G::info_set(state, actor);
            let actions = G::legal_actions(state);
            let n = actions.len();
            let mut rng = ChaCha20Rng::from_seed(0);
            if actor == target {
                // target node：先 recurse 每个 action 拿到 children_value，再用
                // target_strategy 加权回返；同时 cfv[I][a] += children_value[a]。
                let sigma = target_strategy
                    .get(&info)
                    .cloned()
                    .unwrap_or_else(|| vec![1.0 / n as f64; n]);
                let mut children = Vec::with_capacity(n);
                for action in actions.iter() {
                    let next = G::next(state.clone(), *action, &mut rng);
                    let v_a =
                        walk_cfv::<G>(&next, opp_reach, target, opp_strategy, target_strategy, cfv);
                    children.push(v_a);
                }
                let entry = cfv.entry(info).or_insert_with(|| vec![0.0; n]);
                for (i, &v) in children.iter().enumerate() {
                    entry[i] += v;
                }
                sigma.iter().zip(&children).map(|(s, c)| s * c).sum()
            } else {
                // opp 节点：按 σ_opp 加权累计（reach probability 折进递归 opp_reach）
                let sigma = opp_strategy(&info, n);
                let mut v = 0.0;
                for (i, action) in actions.iter().enumerate() {
                    let next = G::next(state.clone(), *action, &mut rng);
                    v += walk_cfv::<G>(
                        &next,
                        opp_reach * sigma[i],
                        target,
                        opp_strategy,
                        target_strategy,
                        cfv,
                    );
                }
                v
            }
        }
    }
}

/// 计算 game 上的 exploitability（API-343 / D-340 / D-341）。
///
/// `exploitability(game, σ) = (BR_0(σ_1) + BR_1(σ_0)) / 2`，单位 chip/game。
pub fn exploitability<G, BR>(game: &G, strategy: &dyn Fn(&G::InfoSet, usize) -> Vec<f64>) -> f64
where
    G: Game,
    BR: BestResponse<G>,
{
    let (_, br_p0) = BR::compute(game, strategy, 0);
    let (_, br_p1) = BR::compute(game, strategy, 1);
    (br_p0 + br_p1) / 2.0
}
