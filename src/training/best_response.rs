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

// ===========================================================================
// Deal-integrated Monte-Carlo best response / exploitability
// （S6 subgame 收敛诊断，临时 2026-06-16）。
//
// 既有 `exploitability::<G,BR>` 走 in-tree chance 枚举，对 Kuhn/Leduc 精确。但
// `SubgameNlheGame` 把整条 board（含 river）+ 双方底牌一次性发在 `root()` 里、树内
// 无 chance 节点（`chance_distribution` panic），既有枚举路径不可用——单次 walk 只
// 评估一个**已知 runout** = "开天眼" best response，跨 deal 平均得 E[max]（上界，有
// Jensen gap），不是 exploitability。本节按 deal 采样积分：采 K 个 `root()`（= K 个
// (双方底牌, runout)），把 per-(infoset,action) cfv 累进**同一张** bucket-key 表，
// policy iteration 收敛到 deal-积分 best response（每桶提交单一动作，非开天眼）；再在
// **独立 eval deal** 上评估值（去 in-sample 过拟合）。
//
// `exploitability(σ) = Σ_i [ BR_i(σ_{-i}) − u_i(σ) ] ≥ 0`，NE 处 → 0。用 `(BR − u)`
// 差值而非 `(BR0+BR1)/2`，故对**非零和**（子博弈含弃牌座 dead money = 常和）也成立。
// in-tree-chance 的游戏 `root()` 不消费 rng → 任何 K 精确（单测对
// `exploitability::<KuhnGame,KuhnBestResponse>` 钉死，差值只来自 zero-sum 下 u0+u1=0
// → 本式 = 2×(BR0+BR1)/2）。
// ===========================================================================

/// profile σ（所有 actor 同一 `strat` 闭包）下 `player` 视角 EV。chance 枚举。
/// 用于算 exploitability 基线 `u_player(σ)`（profile 下该玩家收益）。
fn value_under_profile<G: Game>(
    state: &G::State,
    player: PlayerId,
    strat: &dyn Fn(&G::InfoSet, usize) -> Vec<f64>,
) -> f64 {
    match G::current(state) {
        NodeKind::Terminal => G::payoff(state, player),
        NodeKind::Chance => {
            let mut rng = ChaCha20Rng::from_seed(0);
            G::chance_distribution(state)
                .into_iter()
                .map(|(a, p)| {
                    let next = G::next(state.clone(), a, &mut rng);
                    p * value_under_profile::<G>(&next, player, strat)
                })
                .sum()
        }
        NodeKind::Player(actor) => {
            let info = G::info_set(state, actor);
            let actions = G::legal_actions(state);
            let sigma = strat(&info, actions.len());
            let mut rng = ChaCha20Rng::from_seed(0);
            actions
                .iter()
                .enumerate()
                .map(|(i, a)| {
                    let next = G::next(state.clone(), *a, &mut rng);
                    sigma[i] * value_under_profile::<G>(&next, player, strat)
                })
                .sum()
        }
    }
}

/// deal-积分 best response（对 `target`，对手按 `opp`）。在**固定** `deal_seeds` 上
/// policy iteration：每轮把所有 deal 的 cfv 累进同一张表 → per-infoset argmax 提交单
/// 动作（chance/range 已被 deal 采样积分掉，非开天眼）。返回收敛（或 max_iter 内 train
/// -EV 最优）的 one-hot 策略表。
/// `allow_br(info)==false` 的 infoset 不许 BR 偏离 → 强制打 `fallback`（= profile σ̄），
/// 用于按子集（如某条街）拆 exploitability 来源。`allow_br=|_|true` = 全树 BR。
#[allow(clippy::too_many_arguments)]
fn mc_br_strategy<G: Game>(
    game: &G,
    opp: &dyn Fn(&G::InfoSet, usize) -> Vec<f64>,
    target: PlayerId,
    deal_seeds: &[u64],
    allow_br: &dyn Fn(&G::InfoSet) -> bool,
    fallback: &dyn Fn(&G::InfoSet, usize) -> Vec<f64>,
) -> HashMap<G::InfoSet, Vec<f64>>
where
    G::InfoSet: Hash + Eq,
{
    let max_iter = 100;
    let mut target_strategy: HashMap<G::InfoSet, Vec<f64>> = HashMap::new();
    let mut best_strategy = target_strategy.clone();
    let mut best_value = f64::NEG_INFINITY;
    for _ in 0..max_iter {
        let mut cfv: HashMap<G::InfoSet, Vec<f64>> = HashMap::new();
        let mut train_value = 0.0;
        for &ds in deal_seeds {
            let mut rng = ChaCha20Rng::from_seed(ds);
            let root = game.root(&mut rng);
            train_value += walk_cfv::<G>(&root, 1.0, target, opp, &target_strategy, &mut cfv);
        }
        if train_value > best_value {
            best_value = train_value;
            best_strategy = target_strategy.clone();
        }
        let mut new_strategy: HashMap<G::InfoSet, Vec<f64>> = HashMap::new();
        for (info, vals) in cfv.iter() {
            let strat = if allow_br(info) {
                let mut best_idx = 0usize;
                let mut best = vals[0];
                for (i, &v) in vals.iter().enumerate().skip(1) {
                    if v > best {
                        best = v;
                        best_idx = i;
                    }
                }
                let mut one_hot = vec![0.0; vals.len()];
                one_hot[best_idx] = 1.0;
                one_hot
            } else {
                fallback(info, vals.len())
            };
            new_strategy.insert(info.clone(), strat);
        }
        if new_strategy == target_strategy {
            return new_strategy;
        }
        target_strategy = new_strategy;
    }
    best_strategy
}

/// 在 `eval_seeds` 个独立 deal 上评估固定 `target_strategy`（对 `opp`）的 `target` EV。
fn mc_strategy_value<G: Game>(
    game: &G,
    opp: &dyn Fn(&G::InfoSet, usize) -> Vec<f64>,
    target: PlayerId,
    target_strategy: &HashMap<G::InfoSet, Vec<f64>>,
    eval_seeds: &[u64],
) -> f64
where
    G::InfoSet: Hash + Eq,
{
    let mut total = 0.0;
    for &ds in eval_seeds {
        let mut rng = ChaCha20Rng::from_seed(ds);
        let root = game.root(&mut rng);
        let mut cfv: HashMap<G::InfoSet, Vec<f64>> = HashMap::new();
        total += walk_cfv::<G>(&root, 1.0, target, opp, target_strategy, &mut cfv);
    }
    total / eval_seeds.len() as f64
}

/// 在 `eval_seeds` 个独立 deal 上评估 profile σ 下 `player` 的 EV（= `u_player(σ)`）。
fn mc_profile_value<G: Game>(
    game: &G,
    profile: &dyn Fn(&G::InfoSet, usize) -> Vec<f64>,
    player: PlayerId,
    eval_seeds: &[u64],
) -> f64 {
    let mut total = 0.0;
    for &ds in eval_seeds {
        let mut rng = ChaCha20Rng::from_seed(ds);
        let root = game.root(&mut rng);
        total += value_under_profile::<G>(&root, player, profile);
    }
    total / eval_seeds.len() as f64
}

/// deal-积分 MC exploitability of profile `avg`，对 `players` 求和：
/// `Σ_i [ BR_i(avg_{-i}) − u_i(avg) ] ≥ 0`，NE 处 → 0。`k_train` 个 deal 求 BR，
/// **独立** `k_eval` 个 deal 评估值（去 in-sample 过拟合）。返回
/// `(expl_sum, per_player[(player, br_value, profile_value)])`。
#[allow(clippy::too_many_arguments)]
pub fn mc_exploitability<G: Game>(
    game: &G,
    avg: &dyn Fn(&G::InfoSet, usize) -> Vec<f64>,
    players: &[PlayerId],
    k_train: usize,
    k_eval: usize,
    seed: u64,
) -> (f64, Vec<(PlayerId, f64, f64)>)
where
    G::InfoSet: Hash + Eq,
{
    mc_exploitability_restricted::<G>(game, avg, players, k_train, k_eval, seed, &|_| true)
}

/// 同 [`mc_exploitability`]，但 best response 只允许在 `allow_br(info)==true` 的 infoset
/// 偏离（其余 infoset 强制打 profile σ̄）→ 拆「只在某子集（如某条街）纠偏能榨多少」=
/// exploitability 的来源归因。`allow_br=|_|true` 时与 [`mc_exploitability`] 完全等价。
#[allow(clippy::too_many_arguments)]
pub fn mc_exploitability_restricted<G: Game>(
    game: &G,
    avg: &dyn Fn(&G::InfoSet, usize) -> Vec<f64>,
    players: &[PlayerId],
    k_train: usize,
    k_eval: usize,
    seed: u64,
    allow_br: &dyn Fn(&G::InfoSet) -> bool,
) -> (f64, Vec<(PlayerId, f64, f64)>)
where
    G::InfoSet: Hash + Eq,
{
    let mix = |salt: u64, i: u64| {
        seed.wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ salt.wrapping_mul(0xD1B5_4A32_D192_ED03)
            ^ i.wrapping_mul(0x2545_F491_4F6C_DD1D)
    };
    let train: Vec<u64> = (0..k_train as u64).map(|i| mix(0xA1, i)).collect();
    let eval: Vec<u64> = (0..k_eval as u64).map(|i| mix(0xE2, i)).collect();

    let mut per_player = Vec::with_capacity(players.len());
    let mut expl = 0.0;
    for &p in players {
        let br = mc_br_strategy::<G>(game, avg, p, &train, allow_br, avg);
        let br_val = mc_strategy_value::<G>(game, avg, p, &br, &eval);
        let u_val = mc_profile_value::<G>(game, avg, p, &eval);
        expl += br_val - u_val;
        per_player.push((p, br_val, u_val));
    }
    (expl, per_player)
}

#[cfg(test)]
mod mc_tests {
    use super::*;
    use crate::core::rng::ChaCha20Rng;
    use crate::training::kuhn::KuhnGame;
    use crate::training::trainer::{EsMccfrTrainer, Trainer};

    /// 对 in-tree-chance 的 Kuhn，`root()` 不消费 rng → deal 由 chance 枚举，任何 K 精确。
    /// 故 deal-积分 MC exploitability（K=1）必须逐数匹配既有精确
    /// `exploitability::<KuhnGame,KuhnBestResponse>`：zero-sum 下 u0+u1=0，本式 = BR0+BR1
    /// = 2×参考值，故比较 `mc/2 ≈ exact`。钉死 BR/exploitability 逻辑正确（再用到子博弈）。
    #[test]
    fn mc_exploitability_matches_exact_kuhn() {
        let game = KuhnGame;

        // (a) uniform profile
        let uniform = |_: &<KuhnGame as Game>::InfoSet, n: usize| vec![1.0 / n as f64; n];
        let exact = exploitability::<KuhnGame, KuhnBestResponse>(&game, &uniform);
        let (mc, _) = mc_exploitability::<KuhnGame>(&game, &uniform, &[0, 1], 1, 1, 12345);
        assert!(
            (mc / 2.0 - exact).abs() < 1e-9,
            "uniform: mc/2={} exact={}",
            mc / 2.0,
            exact
        );

        // (b) CFR-trained profile（应接近 NE → exploitability 小）
        let mut t = EsMccfrTrainer::new(KuhnGame, 0xABCD);
        let mut rng = ChaCha20Rng::from_seed(0x1234);
        for _ in 0..50_000 {
            t.step(&mut rng).unwrap();
        }
        let avg = |info: &<KuhnGame as Game>::InfoSet, n: usize| {
            let v = t.average_strategy(info);
            if v.len() == n {
                v
            } else {
                vec![1.0 / n as f64; n]
            }
        };
        let exact2 = exploitability::<KuhnGame, KuhnBestResponse>(&game, &avg);
        let (mc2, _) = mc_exploitability::<KuhnGame>(&game, &avg, &[0, 1], 1, 1, 999);
        assert!(
            (mc2 / 2.0 - exact2).abs() < 1e-9,
            "trained: mc/2={} exact={}",
            mc2 / 2.0,
            exact2
        );
        assert!(
            exact2 < 0.05,
            "trained Kuhn exploitability should be small: {exact2}"
        );
    }
}
