//! The pool's preflop strategy on a betting tree: for every preflop decision node and each of
//! the 169 starting-hand classes, how the average opponent acts there, and how far that is trusted.
//!
//! [`crate::training::pool_model`] knows frequencies ("in this situation the pool folds 71%, calls
//! 22%, raises 7%, and raises mostly to about one pot"), measured from public actions only. A
//! solver needs more: a probability for every hand. This module fills that in.
//!
//! Per node, in tree order:
//! 1. **Situation**: [`PreflopTracker`] names the situation of the acting seat from the betting so
//!    far, [`PoolModel::predict`] gives its frequencies and the number of observations `n`.
//! 2. **Onto the node's actions**: fold -> `Fold`, call / check -> `Call` / `Check`, the raise
//!    mass split by the pool's size distribution, each size going to the legal raise (or all-in)
//!    whose pot fraction is nearest in log space. What the node does not offer moves to the
//!    neighbour: no raise -> call, no call (the width gate closed the pot) -> fold, no fold (free
//!    check) -> check.
//! 3. **Onto hands**: hands are ranked by all-in equity against a random hand
//!    ([`PREFLOP_EQUITY_VS_RANDOM`]); a hand's score `s` is the log-odds of its strength percentile,
//!    `ln(q / (1 - q))` ([`class_scores`]). Actions are in the tree's order, which is also by
//!    aggression: fold, call, raises small to large, all-in. For each cut j between them,
//!    `P(action at or above j | hand) = 1 / (1 + exp(-(s - t_j) / noise))`, and the threshold `t_j`
//!    is solved so that, summed over the hands the acting seat still holds, the frequency equals the
//!    target. `noise` -> 0 is a hard cut (the strongest x% raise); larger values let strong hands
//!    call and weak hands raise some of the time. The log-odds scale keeps the very best hands
//!    apart from the merely good ones: with the percentile itself as the score, aces and nines
//!    differ by 0.07 and any blur wide enough to matter in the middle makes aces fold to a raise
//!    half the time.
//!    "The hands the seat still holds" = combinations per class times the probability the seat's
//!    own earlier actions in this hand had under this same strategy (other seats' cards are ignored).
//! 4. **Trust**: `confidence = min(conf_max, conf_scale * n / (n + conf_k))`, the probability with
//!    which training makes an opponent at this node play the pool's action instead of its own.
//!
//! Only the frequencies come from data. Which hands take which action (step 3) is an assumption:
//! shown hands agree with its direction (re-raises are mostly premiums, limps mostly weak) but
//! cannot calibrate it, because only hands that reach showdown are seen. `noise` exists so that
//! results can be checked for sensitivity to it.

use std::collections::BTreeMap;

use crate::abstraction::action::AbstractAction;
use crate::abstraction::preflop::PreflopLossless169;
use crate::core::rng::ChaCha20Rng;
use crate::core::Street;
use crate::training::game::{Game, NodeKind};
use crate::training::nlhe::{SimplifiedNlheGame, SimplifiedNlheState};
use crate::training::nlhe_betting_tree::NodeId;
use crate::training::pool_model::{
    position_labels, PoolModel, PreflopTracker, SIZE_ALLIN, SIZE_LABELS,
};

/// Number of preflop hand classes.
pub const CLASSES: usize = 169;

/// All-in equity of each preflop class against one random hand, indexed by
/// [`PreflopLossless169::hand_class`] (pairs 22..AA, then suited, then offsuit). Monte Carlo,
/// 2,000,000 boards per class (`examples/tmp_preflop_equity.rs`); standard error about 0.0003.
#[rustfmt::skip]
pub const PREFLOP_EQUITY_VS_RANDOM: [f32; 169] = [
    0.5035, 0.5372, 0.5702, 0.6030, 0.6331, 0.6619, 0.6916, 0.7200,
    0.7500, 0.7751, 0.7990, 0.8242, 0.8522, 0.3598, 0.3684, 0.3867,
    0.3784, 0.3968, 0.4142, 0.3767, 0.3960, 0.4134, 0.4307, 0.3813,
    0.3998, 0.4190, 0.4370, 0.4536, 0.4030, 0.4088, 0.4273, 0.4454,
    0.4620, 0.4792, 0.4241, 0.4321, 0.4393, 0.4581, 0.4744, 0.4909,
    0.5088, 0.4485, 0.4571, 0.4654, 0.4724, 0.4890, 0.5062, 0.5229,
    0.5400, 0.4736, 0.4815, 0.4905, 0.5001, 0.5064, 0.5228, 0.5404,
    0.5564, 0.5750, 0.5015, 0.5104, 0.5188, 0.5275, 0.5360, 0.5432,
    0.5601, 0.5765, 0.5949, 0.6027, 0.5328, 0.5400, 0.5493, 0.5574,
    0.5664, 0.5753, 0.5831, 0.6000, 0.6175, 0.6257, 0.6334, 0.5733,
    0.5824, 0.5904, 0.5992, 0.5989, 0.6102, 0.6191, 0.6274, 0.6464,
    0.6539, 0.6622, 0.6702, 0.3232, 0.3317, 0.3516, 0.3423, 0.3627,
    0.3820, 0.3408, 0.3605, 0.3800, 0.3998, 0.3461, 0.3662, 0.3853,
    0.4047, 0.4231, 0.3682, 0.3746, 0.3938, 0.4147, 0.4325, 0.4504,
    0.3907, 0.4003, 0.4059, 0.4267, 0.4443, 0.4636, 0.4810, 0.4167,
    0.4263, 0.4350, 0.4427, 0.4608, 0.4793, 0.4975, 0.5154, 0.4430,
    0.4527, 0.4620, 0.4720, 0.4785, 0.4970, 0.5157, 0.5324, 0.5524,
    0.4731, 0.4819, 0.4914, 0.5003, 0.5098, 0.5178, 0.5357, 0.5538,
    0.5725, 0.5814, 0.5045, 0.5145, 0.5233, 0.5329, 0.5422, 0.5519,
    0.5601, 0.5780, 0.5973, 0.6061, 0.6149, 0.5497, 0.5587, 0.5671,
    0.5773, 0.5769, 0.5883, 0.5988, 0.6079, 0.6271, 0.6359, 0.6442,
    0.6527,
];

/// Representative pot fraction of each size bucket of the pool model ([`SIZE_LABELS`]).
const SIZE_REPRESENTATIVE: [f64; 6] = [1.0 / 3.0, 0.5, 0.75, 1.0, 1.5, 2.5];

/// How the pool's frequencies are turned into a per-hand strategy and how far it is trusted.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PoolStrategyParams {
    /// Blur of the strength cut, in units of the score (log-odds of the strength percentile):
    /// two hands `noise` apart differ by a factor e in their odds of taking the stronger action.
    /// Must be at least 0.01.
    pub noise: f64,
    /// Observations at which a cell is trusted halfway (before `conf_scale`).
    pub conf_k: f64,
    /// Multiplies every confidence; 0 switches the pool off, 1 uses the counts as they are.
    pub conf_scale: f64,
    /// Upper bound of the confidence; below 1 so that every seat keeps playing its own strategy
    /// some of the time everywhere.
    pub conf_max: f64,
}

impl Default for PoolStrategyParams {
    fn default() -> Self {
        PoolStrategyParams {
            // Shown hands bracket it between about 0.5 (opens and re-raises: a quarter and a half
            // premiums) and 1 to 1.5 (limps: some premiums); see the module docs on what shown
            // hands can and cannot tell.
            noise: 0.75,
            conf_k: 50.0,
            conf_scale: 1.0,
            conf_max: 0.9,
        }
    }
}

impl PoolStrategyParams {
    pub fn validate(&self) -> Result<(), String> {
        let ok = self.noise.is_finite()
            && self.noise >= 0.01
            && self.conf_k.is_finite()
            && self.conf_k > 0.0
            && self.conf_scale.is_finite()
            && (0.0..=1.0).contains(&self.conf_scale)
            && (0.0..1.0).contains(&self.conf_max);
        if ok {
            Ok(())
        } else {
            Err(format!(
                "pool strategy params out of range (noise >= 0.01, conf_k > 0, 0 <= conf_scale <= 1, \
                 0 <= conf_max < 1): {self:?}"
            ))
        }
    }
}

/// Strength percentile of each class: share of all 1326 starting hands that are weaker, plus half
/// of the class's own share.
pub fn class_percentiles() -> [f64; CLASSES] {
    let mut order: Vec<usize> = (0..CLASSES).collect();
    order.sort_by(|a, b| {
        PREFLOP_EQUITY_VS_RANDOM[*a]
            .partial_cmp(&PREFLOP_EQUITY_VS_RANDOM[*b])
            .expect("finite")
            .then(a.cmp(b))
    });
    let mut out = [0.0; CLASSES];
    let mut below = 0.0;
    for c in order {
        let w = f64::from(PreflopLossless169::hole_count_in_class(c as u8)) / 1326.0;
        out[c] = below + w / 2.0;
        below += w;
    }
    out
}

/// Score of each class for the strength cut: log-odds of [`class_percentiles`].
pub fn class_scores() -> [f64; CLASSES] {
    class_percentiles().map(|q| (q / (1.0 - q)).ln())
}

/// What the pool does at one node, summed over hands: for diagnostics and tests.
#[derive(Debug, Clone)]
pub struct PoolNodeInfo {
    pub node: NodeId,
    pub seat: u8,
    pub family: &'static str,
    /// Observations in the model's finest cell for this node.
    pub n: u64,
    pub confidence: f64,
    /// Target frequency of each legal action (tree order).
    pub target: Vec<f64>,
    /// Probability that the acting seat's own earlier actions reach this node under the pool
    /// strategy, averaged over hands.
    pub own_reach: f64,
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    confidence: f32,
    offset: u32,
    n_actions: u8,
}

/// The pool's preflop strategy on one betting tree. See the module docs.
#[derive(Debug, Clone)]
pub struct PoolPreflopStrategy {
    /// Tree node id -> index into `entries`; `u32::MAX` for nodes that are not preflop decisions.
    index: Vec<u32>,
    entries: Vec<Entry>,
    /// `probs[offset + class * n_actions + action]`.
    probs: Vec<f32>,
    info: Vec<PoolNodeInfo>,
    hash: [u8; 32],
}

impl PoolPreflopStrategy {
    /// Walks every preflop decision node of `game`'s tree. Errors when the table is not one the
    /// model's situations cover (fewer than 3 seats, a straddle) or the model lacks a family.
    pub fn build(
        game: &SimplifiedNlheGame,
        model: &PoolModel,
        params: PoolStrategyParams,
    ) -> Result<PoolPreflopStrategy, String> {
        params.validate()?;
        let cfg = game.table_config();
        let n_seats = cfg.n_seats as usize;
        if n_seats < 3 {
            return Err("pool strategy needs at least 3 seats".into());
        }
        if cfg.straddle.as_u64() != 0 {
            return Err("pool strategy does not cover straddle tables".into());
        }
        // Cards do not matter for the betting geometry; the seed only feeds the deal.
        let mut rng = ChaCha20Rng::from_seed(0x504F_4F4C_5354_5241);
        let root = game.root(&mut rng);
        let players = root.game_state.players();
        let tracker = PreflopTracker::new(
            position_labels(n_seats),
            players
                .iter()
                .map(|p| p.committed_this_round.as_u64())
                .collect(),
            players.iter().map(|p| p.committed_total.as_u64()).sum(),
        );
        let mut b = Builder {
            model,
            params,
            score: class_scores(),
            prior: std::array::from_fn(|c| {
                f64::from(PreflopLossless169::hole_count_in_class(c as u8)) / 1326.0
            }),
            out: PoolPreflopStrategy {
                index: vec![u32::MAX; game.tree().num_nodes()],
                entries: Vec::new(),
                probs: Vec::new(),
                info: Vec::new(),
                hash: [0; 32],
            },
            rng,
        };
        let reach = vec![[1.0f64; CLASSES]; n_seats];
        b.walk(root, tracker, reach)?;
        let mut out = b.out;
        let mut h = blake3::Hasher::new();
        h.update(b"pool-preflop-strategy-v1");
        for x in [
            params.noise,
            params.conf_k,
            params.conf_scale,
            params.conf_max,
        ] {
            h.update(&x.to_le_bytes());
        }
        for e in &out.entries {
            h.update(&e.confidence.to_le_bytes());
            h.update(&[e.n_actions]);
        }
        for i in &out.index {
            h.update(&i.to_le_bytes());
        }
        for p in &out.probs {
            h.update(&p.to_le_bytes());
        }
        out.hash = *h.finalize().as_bytes();
        Ok(out)
    }

    /// Confidence and per-action probabilities of the pool at `node` holding `class`; `None` when
    /// the node is not a preflop decision.
    #[inline]
    pub fn lookup(&self, node: NodeId, class: u32) -> Option<(f32, &[f32])> {
        let i = *self.index.get(node as usize)?;
        if i == u32::MAX {
            return None;
        }
        let e = self.entries[i as usize];
        let start = e.offset as usize + class as usize * e.n_actions as usize;
        Some((
            e.confidence,
            &self.probs[start..start + e.n_actions as usize],
        ))
    }

    /// One record per preflop decision node, in tree order.
    pub fn nodes(&self) -> &[PoolNodeInfo] {
        &self.info
    }

    /// Identifies the table: parameters, confidences and every probability.
    pub fn hash(&self) -> [u8; 32] {
        self.hash
    }

    /// Frequency of each action at `node` over the hands with weights `weight[class]` (need not
    /// be normalized). `None` when the node is not a preflop decision.
    pub fn frequencies(&self, node: NodeId, weight: &[f64; CLASSES]) -> Option<Vec<f64>> {
        let (_, first) = self.lookup(node, 0)?;
        let mut out = vec![0.0; first.len()];
        let total: f64 = weight.iter().sum();
        for (c, w) in weight.iter().enumerate() {
            let (_, p) = self.lookup(node, c as u32)?;
            for (o, x) in out.iter_mut().zip(p) {
                *o += w / total * f64::from(*x);
            }
        }
        Some(out)
    }
}

struct Builder<'a> {
    model: &'a PoolModel,
    params: PoolStrategyParams,
    score: [f64; CLASSES],
    prior: [f64; CLASSES],
    out: PoolPreflopStrategy,
    rng: ChaCha20Rng,
}

impl Builder<'_> {
    fn walk(
        &mut self,
        state: SimplifiedNlheState,
        tracker: PreflopTracker,
        reach: Vec<[f64; CLASSES]>,
    ) -> Result<(), String> {
        let actor = match SimplifiedNlheGame::current(&state) {
            NodeKind::Player(a) if state.game_state.street() == Street::Preflop => a as usize,
            _ => return Ok(()),
        };
        let node = state.current_node_id;
        let actions = SimplifiedNlheGame::legal_actions(&state);
        let spot = tracker.spot(actor);
        let pred = self.model.predict(&spot)?;
        if pred.probs.is_empty() {
            return Err(format!(
                "pool model has no observations for {}",
                spot.family
            ));
        }
        let target = target_frequencies(&actions, actor, &tracker, &pred.probs, &pred.sizes);
        let mut beta: [f64; CLASSES] = std::array::from_fn(|c| self.prior[c] * reach[actor][c]);
        let mut total: f64 = beta.iter().sum();
        if total <= 1e-300 || !total.is_finite() {
            beta = self.prior;
            total = 1.0;
        }
        let own_reach = total;
        for x in beta.iter_mut() {
            *x /= total;
        }
        let rows = soft_threshold_strategy(&target, &beta, &self.score, self.params.noise);
        let n = pred.n;
        let confidence = (self.params.conf_scale * n as f64 / (n as f64 + self.params.conf_k))
            .min(self.params.conf_max);

        self.out.index[node as usize] = self.out.entries.len() as u32;
        self.out.entries.push(Entry {
            confidence: confidence as f32,
            offset: u32::try_from(self.out.probs.len())
                .map_err(|_| "pool strategy table too large".to_string())?,
            n_actions: actions.len() as u8,
        });
        for row in &rows {
            self.out.probs.extend(row.iter().map(|p| *p as f32));
        }
        self.out.info.push(PoolNodeInfo {
            node,
            seat: actor as u8,
            family: spot.family,
            n,
            confidence,
            target: target.clone(),
            own_reach,
        });

        for (i, action) in actions.iter().enumerate() {
            let next = SimplifiedNlheGame::next(state.clone(), *action, &mut self.rng);
            let still_preflop = matches!(SimplifiedNlheGame::current(&next), NodeKind::Player(_))
                && next.game_state.street() == Street::Preflop;
            if !still_preflop {
                continue;
            }
            let mut t = tracker.clone();
            if matches!(action, AbstractAction::Fold) {
                t.fold(actor);
            } else {
                t.put(
                    actor,
                    next.game_state.players()[actor]
                        .committed_this_round
                        .as_u64(),
                );
            }
            let mut r = reach.clone();
            for (c, x) in r[actor].iter_mut().enumerate() {
                *x *= rows[c][i];
            }
            self.walk(next, t, r)?;
        }
        Ok(())
    }
}

/// Step 2 of the module docs: the pool's outcome frequencies onto the node's legal actions.
fn target_frequencies(
    actions: &[AbstractAction],
    actor: usize,
    tracker: &PreflopTracker,
    probs: &BTreeMap<String, f64>,
    sizes: &BTreeMap<String, f64>,
) -> Vec<f64> {
    let get = |k: &str| probs.get(k).copied().unwrap_or(0.0);
    let (mut fold_mass, mut passive_mass, raise_mass) =
        (get("fold"), get("call") + get("check"), get("raise"));

    let fold_idx = actions
        .iter()
        .position(|a| matches!(a, AbstractAction::Fold));
    // An all-in that does not exceed the bet is a call.
    let frac_of = |a: &AbstractAction| -> Option<f64> {
        match a {
            AbstractAction::Raise { to, .. }
            | AbstractAction::Bet { to, .. }
            | AbstractAction::AllIn { to } => tracker.fraction_if_raise(actor, to.as_u64()),
            _ => None,
        }
    };
    let passive_idx = actions.iter().position(|a| {
        matches!(a, AbstractAction::Check | AbstractAction::Call { .. })
            || (matches!(a, AbstractAction::AllIn { .. }) && frac_of(a).is_none())
    });
    let raises: Vec<(usize, f64, bool)> = actions
        .iter()
        .enumerate()
        .filter_map(|(i, a)| frac_of(a).map(|f| (i, f, matches!(a, AbstractAction::AllIn { .. }))))
        .collect();

    let mut out = vec![0.0; actions.len()];
    if raises.is_empty() {
        passive_mass += raise_mass;
    } else {
        let known: f64 = sizes.values().sum();
        let nearest = |rep: f64| -> usize {
            raises
                .iter()
                .min_by(|a, b| {
                    (a.1 / rep)
                        .ln()
                        .abs()
                        .partial_cmp(&(b.1 / rep).ln().abs())
                        .expect("finite")
                })
                .expect("non-empty")
                .0
        };
        if known <= 0.0 {
            // The pool was never seen raising here: put the (smoothed) raise mass on the smallest.
            out[raises[0].0] += raise_mass;
        } else {
            for (label, q) in sizes {
                let idx = if label == SIZE_ALLIN {
                    raises
                        .iter()
                        .find(|r| r.2)
                        .map(|r| r.0)
                        .unwrap_or(raises[raises.len() - 1].0)
                } else {
                    let k = SIZE_LABELS
                        .iter()
                        .position(|l| l == label)
                        .expect("size label of the pool model");
                    nearest(SIZE_REPRESENTATIVE[k])
                };
                out[idx] += raise_mass * q / known;
            }
        }
    }
    match (passive_idx, fold_idx) {
        (Some(p), Some(f)) => {
            out[p] += passive_mass;
            out[f] += fold_mass;
        }
        (Some(p), None) => out[p] += passive_mass + fold_mass,
        (None, Some(f)) => out[f] += passive_mass + fold_mass,
        (None, None) => {
            fold_mass += passive_mass;
            out[raises[0].0] += fold_mass;
        }
    }
    let total: f64 = out.iter().sum();
    for x in out.iter_mut() {
        *x /= total;
    }
    out
}

/// Step 3 of the module docs. `target[a]` over actions ordered by aggression, `beta[c]` the
/// normalized weight of each class, `score[c]` its strength score. Returns `rows[c][a]`;
/// `sum_c beta[c] * rows[c][a] == target[a]` up to the bisection tolerance.
pub fn soft_threshold_strategy(
    target: &[f64],
    beta: &[f64; CLASSES],
    score: &[f64; CLASSES],
    noise: f64,
) -> Vec<Vec<f64>> {
    let m = target.len();
    let sigmoid = |x: f64| 1.0 / (1.0 + (-x).exp());
    // at_or_above[j][c] = P(action index >= j | class c); j = 0 is certain, j = m impossible.
    let mut at_or_above = vec![[1.0f64; CLASSES]; m + 1];
    at_or_above[m] = [0.0; CLASSES];
    let mut tail = 1.0;
    for j in 1..m {
        tail -= target[j - 1];
        let want = tail.clamp(0.0, 1.0);
        let row = &mut at_or_above[j];
        if want <= 1e-12 {
            *row = [0.0; CLASSES];
            continue;
        }
        if want >= 1.0 - 1e-12 {
            continue;
        }
        let freq = |t: f64| -> f64 {
            beta.iter()
                .zip(score)
                .map(|(b, s)| b * sigmoid((s - t) / noise))
                .sum()
        };
        // freq is decreasing in t; scores are within about [-6, 7].
        let (mut lo, mut hi) = (-10.0 - 40.0 * noise, 10.0 + 40.0 * noise);
        for _ in 0..100 {
            let mid = 0.5 * (lo + hi);
            if freq(mid) > want {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let t = 0.5 * (lo + hi);
        for (r, s) in row.iter_mut().zip(score) {
            *r = sigmoid((s - t) / noise);
        }
    }
    (0..CLASSES)
        .map(|c| {
            let mut row: Vec<f64> = (0..m)
                .map(|j| (at_or_above[j][c] - at_or_above[j + 1][c]).max(0.0))
                .collect();
            let s: f64 = row.iter().sum();
            for x in row.iter_mut() {
                *x /= s;
            }
            row
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ChipAmount;
    use crate::rules::config::TableConfig;
    use crate::training::nlhe_betting_tree::{
        ante_flop2_3bet_o75_limp_6max, AbstractActionTag, Child,
    };
    use crate::{BucketConfig, BucketTable};
    use std::sync::Arc;

    fn small_model() -> PoolModel {
        PoolModel::from_json(include_str!("../../tests/data/pool_model_small.json")).expect("model")
    }

    /// 6 seats, 30 BB, 1 BB ante, limps allowed (limp lines 3 to the flop, others 4).
    fn limp_game() -> SimplifiedNlheGame {
        let table = Arc::new(BucketTable::stub_for_postflop(
            BucketConfig::default_500_500_500(),
        ));
        let cfg = TableConfig::n_max_at_bb(6, 30).with_ante(ChipAmount::new(100));
        let (abs, rules) = ante_flop2_3bet_o75_limp_6max(4);
        SimplifiedNlheGame::new_with_abstraction(table, cfg, abs, rules).expect("game")
    }

    #[test]
    fn percentiles_follow_equity() {
        let s = class_percentiles();
        let abs = PreflopLossless169::new();
        // Class ids: pairs 0..13 (22..AA); 32o is the first offsuit class.
        assert!(
            (s[12] - (1.0 - 3.0 / 1326.0)).abs() < 1e-12,
            "AA is the top class"
        );
        assert!(
            (s[91] - 6.0 / 1326.0).abs() < 1e-12,
            "32o is the bottom class"
        );
        let mean: f64 = (0..CLASSES)
            .map(|c| s[c] * f64::from(PreflopLossless169::hole_count_in_class(c as u8)) / 1326.0)
            .sum();
        assert!((mean - 0.5).abs() < 1e-12);
        for a in 0..CLASSES {
            for b in 0..CLASSES {
                if PREFLOP_EQUITY_VS_RANDOM[a] < PREFLOP_EQUITY_VS_RANDOM[b] {
                    assert!(s[a] < s[b]);
                }
            }
        }
        let _ = abs;
    }

    #[test]
    fn soft_threshold_hits_the_target_and_orders_hands() {
        let score = class_scores();
        let prior: [f64; CLASSES] = std::array::from_fn(|c| {
            f64::from(PreflopLossless169::hole_count_in_class(c as u8)) / 1326.0
        });
        let target = [0.6, 0.25, 0.1, 0.05];
        for noise in [0.05, 1.0, 3.0] {
            let rows = soft_threshold_strategy(&target, &prior, &score, noise);
            for (a, want) in target.iter().enumerate() {
                let got: f64 = (0..CLASSES).map(|c| prior[c] * rows[c][a]).sum();
                assert!((got - want).abs() < 1e-9, "noise {noise} action {a}: {got}");
            }
            for row in &rows {
                assert!((row.iter().sum::<f64>() - 1.0).abs() < 1e-12);
                assert!(row.iter().all(|p| *p >= 0.0));
            }
            // AA (12) folds less and takes the top action more than 32o (91).
            assert!(rows[12][0] < rows[91][0]);
            assert!(rows[12][3] > rows[91][3]);
        }
        // Almost no blur: the top 5% take the top action, the bottom 60% fold.
        let rows = soft_threshold_strategy(&target, &prior, &score, 0.05);
        assert!(rows[12][3] > 0.99 && rows[91][0] > 0.99);
        // Default blur: aces still take the top action most of the time and almost never fold.
        let rows = soft_threshold_strategy(&target, &prior, &score, 1.0);
        assert!(rows[12][3] > 0.6 && rows[12][0] < 0.01, "{:?}", rows[12]);
        // Degenerate targets.
        let rows = soft_threshold_strategy(&[0.0, 1.0], &prior, &score, 1.0);
        assert!(rows.iter().all(|r| r[0] == 0.0 && r[1] == 1.0));
    }

    #[test]
    fn build_covers_every_preflop_node_and_matches_frequencies() {
        let game = limp_game();
        let model = small_model();
        let params = PoolStrategyParams::default();
        let pool = PoolPreflopStrategy::build(&game, &model, params).expect("build");
        let tree = game.tree();
        let prior: [f64; CLASSES] = std::array::from_fn(|c| {
            f64::from(PreflopLossless169::hole_count_in_class(c as u8)) / 1326.0
        });

        let mut preflop = 0usize;
        for id in 0..tree.num_nodes() as NodeId {
            let n = tree.node(id);
            let is_pre = n.street == crate::abstraction::info::StreetTag::Preflop;
            match pool.lookup(id, 0) {
                Some((conf, p)) => {
                    assert!(is_pre, "node {id} is not preflop");
                    preflop += 1;
                    assert_eq!(p.len(), n.legal_actions.len());
                    assert!((0.0..=params.conf_max as f32).contains(&conf));
                    for c in 0..CLASSES as u32 {
                        let (_, row) = pool.lookup(id, c).expect("class row");
                        let s: f32 = row.iter().sum();
                        assert!((s - 1.0).abs() < 1e-5, "node {id} class {c} sums to {s}");
                    }
                }
                None => assert!(!is_pre, "preflop node {id} missing"),
            }
        }
        assert_eq!(preflop, pool.nodes().len());
        assert!(preflop > 1000);

        let step = |id: NodeId, tag: AbstractActionTag| -> NodeId {
            let n = tree.node(id);
            let i = n
                .legal_actions
                .iter()
                .position(|a| *a == tag)
                .expect("edge");
            match n.children[i] {
                Child::Decision(c) => c,
                Child::Terminal => panic!("terminal"),
            }
        };
        let info = |id: NodeId| pool.nodes().iter().find(|i| i.node == id).expect("info");

        // Root: UTG, nobody in. Legal = Fold / Call / 0.75 pot: the model's fold / call / raise.
        let root = tree.root_id();
        let ri = info(root);
        assert_eq!((ri.family, ri.seat), ("pf_unopened", 3));
        let want = model
            .predict(&crate::training::pool_model::PoolSpot {
                family: "pf_unopened",
                dims: vec![("pos", "EP")],
            })
            .expect("cell");
        assert_eq!(ri.n, want.n);
        for (got, key) in ri.target.iter().zip(["fold", "call", "raise"]) {
            assert!((got - want.probs[key]).abs() < 1e-12, "{key}");
        }
        // Over all starting hands the strategy reproduces the target.
        let freq = pool.frequencies(root, &prior).expect("root");
        for (g, w) in freq.iter().zip(&ri.target) {
            assert!((g - w).abs() < 1e-6, "{g} vs {w}");
        }
        assert!((ri.own_reach - 1.0).abs() < 1e-12);
        assert!((ri.confidence - (ri.n as f64 / (ri.n as f64 + 50.0)).min(0.9)).abs() < 1e-12);

        // Two limpers: the third seat cannot limp, so the pool's limpers there fold.
        let hj = step(root, AbstractActionTag::Call);
        let co = step(hj, AbstractActionTag::Call);
        let ci = info(co);
        assert_eq!(ci.family, "pf_vs_limp");
        assert_eq!(tree.node(co).legal_actions.len(), 2);
        let want = model
            .predict(&crate::training::pool_model::PoolSpot {
                family: "pf_vs_limp",
                dims: vec![("pos", "CO"), ("limpers", "2+")],
            })
            .expect("cell");
        assert!((ci.target[0] - (want.probs["fold"] + want.probs["call"])).abs() < 1e-12);
        assert!((ci.target[1] - want.probs["raise"]).abs() < 1e-12);

        // UTG limps, HJ raises, everyone folds back to UTG: he is "limper facing a raise", and
        // the chance his own play got him here is the chance he limped.
        let raise = *tree.node(hj).legal_actions.last().expect("raise");
        let mut id = step(hj, raise);
        while tree.node(id).player_acting != 3 {
            id = step(id, AbstractActionTag::Fold);
        }
        let ui = info(id);
        assert_eq!(ui.family, "pf_limper_vs_raise");
        assert!(
            (ui.own_reach - ri.target[1]).abs() < 1e-6,
            "{} vs {}",
            ui.own_reach,
            ri.target[1]
        );
        // Strong hands limp less often than they raise, so the limper's range is weaker than the
        // deck: AA's share shrinks.
        let (_, aa_root) = pool.lookup(root, 12).expect("AA");
        assert!(aa_root[2] > aa_root[1]);

        // Same inputs, same table; different blur, different table.
        let again = PoolPreflopStrategy::build(&game, &model, params).expect("build");
        assert_eq!(pool.hash(), again.hash());
        let other = PoolPreflopStrategy::build(
            &game,
            &model,
            PoolStrategyParams {
                noise: 0.3,
                ..params
            },
        )
        .expect("build");
        assert_ne!(pool.hash(), other.hash());
        assert!(PoolPreflopStrategy::build(
            &game,
            &model,
            PoolStrategyParams {
                noise: 0.0,
                ..params
            }
        )
        .is_err());
    }
}
