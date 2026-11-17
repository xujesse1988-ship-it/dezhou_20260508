//! Opponents of the real-time search that act with the pool's frequencies.
//!
//! The pool model ([`PoolModel`]) says how often the platform's players check / bet and
//! fold / call / raise in each postflop situation. It does not say with which hands. The search
//! therefore lets its own solve decide the hands and takes only the frequencies from the pool:
//!
//! At an opponent decision node the solve keeps one regret row per hand bucket, as always. The
//! "free" strategy of a bucket is regret matching on that row. The "pool-type" strategy of the same
//! bucket looks at the row's average regret per update (how many chips each action gains over
//! what the bucket plays, on average), adds an offset per action class (fold / check-or-call /
//! bet-or-raise) that is the same for every bucket of the node, and takes each action with
//! probability proportional to `exp(value / temp)`. The offsets are fitted so that the node's
//! overall frequencies, weighted by how often each bucket gets there, equal the pool's. A positive
//! offset on "call" turns the folds that cost the fewest chips into calls first; a negative one
//! drops the calls that gain the least. So the pool-type player is "mostly rational with a taste":
//! he goes for his winnings plus a fixed bonus in chips per kind of action, the bonus being
//! whatever makes his frequencies come out as observed.
//!
//! `temp` (chips; `PoolSearchParams::temp` times the pot at the node) says how sharply hands are
//! told apart: two actions whose values differ by `temp` are taken e : 1. A hard maximum (temp →
//! 0) cannot produce a frequency between "this bucket does it" and "it does not" at nodes where a
//! few buckets carry the weight, and it flips on the sampling noise of the averages; a pool is
//! also many players, whose thresholds differ. The data does not pin `temp` down.
//!
//! How far the pool is trusted follows the data-biased response of Johanson & Bowling (2009), as in
//! the pool-aware blueprint training ([`crate::training::pool_strategy`]): with probability
//! `confidence = min(conf_scale * n / (n + conf_k), conf_max)` (`n` = observations in the model's
//! cell) the opponent plays the pool-type strategy, otherwise the free one. The free strategy keeps
//! its regrets against what is actually played, so it stays a best reply; hero's strategy is a best
//! reply to the mixture.
//!
//! Only hero's opponents are constrained, only on postflop streets, and only in subtrees whose
//! root is postflop (the preflop pool frequencies are part of the blueprint). Bet sizes are not
//! constrained: within "bet-or-raise" the solve picks among the sizes of its own menu.
//!
//! The bucket weights of a node are counted during the solve: every time the node is reached with
//! its actor not being the traverser, the actor's hand was drawn from its range and its earlier
//! actions were sampled from its played strategy, so the visits follow its reach.
//!
//! Offsets are in chips per update, the unit of the average regret, so they do not have to follow
//! the growth of the regret sums; fits happen every eighth of the updates done so far and start
//! from the previous answer.

use std::path::Path;
use std::sync::OnceLock;

use smallvec::SmallVec;

use crate::abstraction::action::AbstractAction;
use crate::core::{PlayerStatus, Street};
use crate::rules::action::Action;
use crate::rules::state::GameState;
use crate::training::game::PlayerId;
use crate::training::nlhe::SimplifiedNlheState;
use crate::training::nlhe_betting_tree::NodeId;
use crate::training::pool_model::{PoolModel, PoolSpot, PostflopTracker, POOL_MAX_SEATS};
use crate::training::regret::SigmaVec;

/// Action classes the pool's frequencies are given in.
pub const CLASS_FOLD: usize = 0;
/// Check or call (an all-in that does not exceed the bet included).
pub const CLASS_PASSIVE: usize = 1;
/// Bet or raise of any size (an all-in above the bet included).
pub const CLASS_AGGRESSIVE: usize = 2;
pub const N_CLASSES: usize = 3;

/// A node's offsets are fitted once its counted weight reaches this.
const MIN_FIT_WEIGHT: f64 = 30.0;
/// ... and again whenever the weight has grown by this factor since the last fit (a node visited
/// ten times as often is refitted about ten more times, so the fits cost a bounded share of the
/// solve; the last fit before reading covers every node).
const REFIT_GROWTH: f64 = 1.25;
/// Frequencies are matched to within this.
const FIT_TOLERANCE: f64 = 0.002;
/// A target frequency is kept this far from 0 and 1 (an offset would have to be infinite).
const TARGET_FLOOR: f64 = 0.002;

/// How far the pool's frequencies are trusted in the search.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PoolSearchParams {
    /// Observations at which a cell is trusted halfway (before `conf_scale`).
    pub conf_k: f64,
    /// Multiplies every confidence; 0 switches the pool off.
    pub conf_scale: f64,
    /// Upper bound of the confidence.
    pub conf_max: f64,
    /// How sharply the pool-type strategy tells hands apart, as a fraction of the pot at the node
    /// (module docs).
    pub temp: f64,
}

impl Default for PoolSearchParams {
    /// The values the pool-aware blueprint was trained with.
    fn default() -> Self {
        PoolSearchParams {
            conf_k: 50.0,
            conf_scale: 0.5,
            conf_max: 0.9,
            temp: 0.05,
        }
    }
}

impl PoolSearchParams {
    pub fn validate(&self) -> Result<(), String> {
        let ok = self.conf_k.is_finite()
            && self.conf_k > 0.0
            && self.conf_scale.is_finite()
            && (0.0..=1.0).contains(&self.conf_scale)
            && (0.0..=1.0).contains(&self.conf_max)
            && self.temp.is_finite()
            && self.temp >= 1e-3;
        if ok {
            Ok(())
        } else {
            Err(format!(
                "pool search params out of range (conf_k > 0, 0 <= conf_scale <= 1, \
                 0 <= conf_max <= 1, temp >= 0.001): {self:?}"
            ))
        }
    }

    /// Probability that an opponent plays the pool-type strategy in a cell seen `n` times.
    pub fn confidence(&self, n: u64) -> f64 {
        (self.conf_scale * n as f64 / (n as f64 + self.conf_k)).min(self.conf_max)
    }
}

/// The pool model plus how far to trust it; loaded once per process.
pub struct PoolSearch {
    model: PoolModel,
    params: PoolSearchParams,
    hash: [u8; 32],
}

impl std::fmt::Debug for PoolSearch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "PoolSearch {{ hash: {}, params: {:?} }}",
            self.hash_hex(),
            self.params
        )
    }
}

impl PoolSearch {
    pub fn from_json(text: &str, params: PoolSearchParams) -> Result<PoolSearch, String> {
        params.validate()?;
        let model = PoolModel::from_json(text)?;
        let mut h = blake3::Hasher::new();
        h.update(b"pool-search-v1");
        for x in [
            params.conf_k,
            params.conf_scale,
            params.conf_max,
            params.temp,
        ] {
            h.update(&x.to_le_bytes());
        }
        h.update(text.as_bytes());
        Ok(PoolSearch {
            model,
            params,
            hash: *h.finalize().as_bytes(),
        })
    }

    pub fn load(path: &Path, params: PoolSearchParams) -> Result<PoolSearch, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("pool model {}: {e}", path.display()))?;
        Self::from_json(&text, params)
    }

    /// Identifies model + params (part of the solve cache key).
    pub fn hash(&self) -> &[u8; 32] {
        &self.hash
    }

    /// First 8 bytes of [`Self::hash`] in hex.
    pub fn hash_hex(&self) -> String {
        self.hash[..8].iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn params(&self) -> PoolSearchParams {
        self.params
    }
}

/// What the pool does at one opponent node.
#[derive(Debug, Clone)]
pub struct NodeConstraint {
    /// Probability of playing the pool-type strategy here.
    pub confidence: f64,
    /// Pool frequency of each class; classes the node does not have are 0.
    pub target: [f64; N_CLASSES],
    pub present: [bool; N_CLASSES],
    /// Class of each legal action (tree order).
    pub class: SmallVec<[u8; 8]>,
    pub spot: PoolSpot,
    /// Observations in the model's finest cell.
    pub n: u64,
    /// Temperature of the pool-type strategy in chips ([`offset_strategy`]).
    pub temp: f64,
}

/// Per-node state: where the betting stands when the node's actor is about to act, and the
/// constraint if the actor is a constrained opponent.
#[derive(Debug, Clone)]
pub struct PoolNode {
    tracker: PostflopTracker,
    pub constraint: Option<NodeConstraint>,
}

/// The step from a node to one of its children, for deriving the child's [`PoolNode`].
#[derive(Clone, Copy)]
pub struct PoolParent<'a> {
    node: &'a PoolNode,
    actor: usize,
    fold: bool,
}

impl<'a> PoolParent<'a> {
    pub fn new(node: &'a PoolNode, actor: PlayerId, action: &AbstractAction) -> PoolParent<'a> {
        PoolParent {
            node,
            actor: actor as usize,
            fold: matches!(action, AbstractAction::Fold),
        }
    }
}

/// One line of [`PoolConstraint::report`].
#[derive(Debug, Clone)]
pub struct PoolNodeReport {
    pub node: NodeId,
    pub seat: PlayerId,
    pub spot: PoolSpot,
    pub n: u64,
    pub confidence: f64,
    /// Counted visits of the node with its actor not traversing.
    pub weight: f64,
    pub target: [f64; N_CLASSES],
    /// Class frequencies of the free strategy (regret matching alone).
    pub free: [f64; N_CLASSES],
    /// Class frequencies of what is played (the mixture).
    pub played: [f64; N_CLASSES],
}

/// Pool constraints of one solve. Node entries are filled in the first time a traversal reaches
/// the node (they need the chips of a real state); offsets are refitted between update batches.
pub struct PoolConstraint {
    pool: &'static PoolSearch,
    hero: PlayerId,
    root: PostflopTracker,
    /// Boxed: most nodes of a large subtree are never reached.
    nodes: Vec<OnceLock<Box<PoolNode>>>,
    /// Per node: offset of each class, in chips (the unit of the average regret). All zero until
    /// the first fit.
    offsets: Vec<[f64; N_CLASSES]>,
    /// Per node: counted weight at the last fit; 0 = not fitted yet (the free strategy is played).
    fitted_weight: Vec<f32>,
}

fn status_masks(state: &GameState) -> (u16, u16) {
    let (mut folded, mut all_in) = (0u16, 0u16);
    for (i, p) in state.players().iter().enumerate() {
        match p.status {
            PlayerStatus::Folded | PlayerStatus::SittingOut => folded |= 1 << i,
            PlayerStatus::AllIn => all_in |= 1 << i,
            PlayerStatus::Active => {}
        }
    }
    (folded, all_in)
}

fn postflop_index(street: Street) -> Option<u8> {
    match street {
        Street::Flop => Some(1),
        Street::Turn => Some(2),
        Street::River => Some(3),
        Street::Preflop | Street::Showdown => None,
    }
}

/// Tracker for `state`, a postflop decision point whose betting history is recorded: the last
/// bettor of the street before, and this street's actions so far. `None` when the street is not
/// postflop, the table is too large, or the state carries no history.
pub fn tracker_from_history(state: &GameState) -> Option<PostflopTracker> {
    let street = state.street();
    let idx = postflop_index(street)?;
    let n = state.players().len();
    if !state.track_history() || !(2..=POOL_MAX_SEATS).contains(&n) {
        return None;
    }
    let actions = &state.hand_history().actions;
    let prev_street = match street {
        Street::Flop => Street::Preflop,
        Street::Turn => Street::Flop,
        _ => Street::Turn,
    };
    let is_aggr = |a: &Action| matches!(a, Action::Bet { .. } | Action::Raise { .. });
    let prev_aggr = actions
        .iter()
        .rev()
        .find(|a| a.street == prev_street && is_aggr(&a.action))
        .map(|a| a.seat.0 as usize);
    let here: Vec<_> = actions.iter().filter(|a| a.street == street).collect();
    // Status at the start of the street: undo what happened on it.
    let (mut folded, mut all_in) = status_masks(state);
    for a in &here {
        let bit = 1u16 << a.seat.0;
        if matches!(a.action, Action::Fold) {
            folded &= !bit;
        }
        all_in &= !bit;
    }
    let committed: u64 = state
        .players()
        .iter()
        .map(|p| p.committed_this_round.as_u64())
        .sum();
    let pot = state.pot().as_u64().checked_sub(committed)?;
    let mut t = PostflopTracker::start(n, idx, pot, folded, all_in, prev_aggr);
    for a in here {
        let seat = a.seat.0 as usize;
        if matches!(a.action, Action::Fold) {
            t.fold(seat);
        } else {
            t.put(seat, a.committed_after.as_u64());
        }
    }
    Some(t)
}

/// One bucket of a node for the fit: (weight, regret sums, 1 / number of regret updates; 0 when
/// the row has none).
pub(crate) type PoolRow<'a> = (f64, &'a [f64], f64);

/// Pool-type strategy of one bucket: each action is taken with probability proportional to
/// `exp((average regret + offset of the action's class) / temp)`. `regrets` = the row's regret
/// sums and 1 / its number of regret updates (`None` or a factor 0 = nothing known: offsets
/// alone). `temp` is in chips: two actions whose values differ by `temp` are taken e : 1.
pub fn offset_strategy(
    regrets: Option<(&[f64], f64)>,
    class: &[u8],
    offsets: &[f64; N_CLASSES],
    temp: f64,
) -> SigmaVec {
    let value = |a: usize| regrets.map_or(0.0, |(r, inv)| r[a] * inv) + offsets[class[a] as usize];
    let best = (0..class.len())
        .map(value)
        .fold(f64::NEG_INFINITY, f64::max);
    let mut out: SigmaVec = (0..class.len())
        .map(|a| ((value(a) - best) / temp).exp())
        .collect();
    let sum: f64 = out.iter().sum();
    for p in &mut out {
        *p /= sum;
    }
    out
}

/// Weighted class frequencies of [`offset_strategy`] over a node's buckets, and (when asked) the
/// weighted sum of `P Pᵀ` over buckets, `P` = a bucket's class probabilities (for the Newton step
/// of [`fit_offsets`]).
fn class_marginal(
    rows: &[PoolRow<'_>],
    class: &[u8],
    offsets: &[f64; N_CLASSES],
    temp: f64,
    mut outer: Option<&mut [[f64; N_CLASSES]; N_CLASSES]>,
) -> [f64; N_CLASSES] {
    let mut m = [0.0; N_CLASSES];
    let mut total = 0.0;
    if let Some(o) = outer.as_deref_mut() {
        *o = [[0.0; N_CLASSES]; N_CLASSES];
    }
    for &(w, regrets, inv) in rows {
        let s = offset_strategy(Some((regrets, inv)), class, offsets, temp);
        let mut p = [0.0; N_CLASSES];
        for (a, x) in s.iter().enumerate() {
            p[class[a] as usize] += x;
        }
        for c in 0..N_CLASSES {
            m[c] += w * p[c];
            if let Some(o) = outer.as_deref_mut() {
                for d in 0..N_CLASSES {
                    o[c][d] += w * p[c] * p[d];
                }
            }
        }
        total += w;
    }
    if total > 0.0 {
        for x in &mut m {
            *x /= total;
        }
        if let Some(o) = outer {
            for row in o.iter_mut() {
                for x in row.iter_mut() {
                    *x /= total;
                }
            }
        }
    }
    m
}

/// Offsets under which the weighted class frequencies of `rows` equal `target` (zero for classes
/// the node does not have, positive for the others). The first present class keeps offset 0.
///
/// The frequencies are those of an exponential family in the offsets, so the fit is a convex
/// problem with one solution. Each round takes a Newton step; when that does not bring the
/// frequencies closer it takes an iterative-scaling step instead (`offset += temp * ln(target /
/// frequency)` per class, which always converges). Steps are capped at `30 * temp`.
pub(crate) fn fit_offsets(
    rows: &[PoolRow<'_>],
    class: &[u8],
    present: &[bool; N_CLASSES],
    target: &[f64; N_CLASSES],
    temp: f64,
    start: [f64; N_CLASSES],
) -> [f64; N_CLASSES] {
    let base = (0..N_CLASSES)
        .find(|&c| present[c])
        .expect("a class is present");
    let free: SmallVec<[usize; 2]> = (0..N_CLASSES)
        .filter(|&c| present[c] && c != base)
        .collect();
    let rebase = |mut o: [f64; N_CLASSES]| {
        let shift = o[base];
        for c in 0..N_CLASSES {
            o[c] = if present[c] { o[c] - shift } else { 0.0 };
        }
        o
    };
    let miss_of = |m: &[f64; N_CLASSES]| {
        free.iter()
            .map(|&c| (m[c] - target[c]).abs())
            .fold(0.0, f64::max)
    };
    let mut offs = rebase(start);
    let mut outer = [[0.0; N_CLASSES]; N_CLASSES];
    let mut m = class_marginal(rows, class, &offs, temp, Some(&mut outer));
    // No step moves an offset by more than this: beyond it the odds are e^30 and a class can drop
    // to exactly zero, from where its derivative says nothing.
    let max_step = 30.0 * temp;
    for _ in 0..200 {
        let miss = miss_of(&m);
        if miss <= FIT_TOLERANCE {
            break;
        }
        // Newton: d(frequency_c) / d(offset_d) = (δ_cd m_c − Σ_b w_b P_bc P_bd) / temp.
        let jac = |c: usize, d: usize| (if c == d { m[c] } else { 0.0 } - outer[c][d]) / temp;
        let mut step = [0.0; N_CLASSES];
        let solved = match free.as_slice() {
            [c] => {
                let j = jac(*c, *c);
                (j > 1e-12).then(|| step[*c] = (target[*c] - m[*c]) / j)
            }
            [c, d] => {
                let (a11, a12, a21, a22) = (jac(*c, *c), jac(*c, *d), jac(*d, *c), jac(*d, *d));
                let det = a11 * a22 - a12 * a21;
                (det.abs() > 1e-12).then(|| {
                    let (r1, r2) = (target[*c] - m[*c], target[*d] - m[*d]);
                    step[*c] = (a22 * r1 - a12 * r2) / det;
                    step[*d] = (a11 * r2 - a21 * r1) / det;
                })
            }
            _ => None,
        };
        let mut next = offs;
        let mut improved = false;
        if solved.is_some() {
            for &c in &free {
                next[c] = offs[c] + step[c].clamp(-max_step, max_step);
            }
            let m_next = class_marginal(rows, class, &next, temp, Some(&mut outer));
            if miss_of(&m_next) < miss {
                m = m_next;
                improved = true;
            }
        }
        if !improved {
            // Iterative scaling over all present classes, then back to base = 0.
            next = offs;
            for c in 0..N_CLASSES {
                if present[c] {
                    next[c] +=
                        (temp * (target[c] / m[c].max(1e-300)).ln()).clamp(-max_step, max_step);
                }
            }
            next = rebase(next);
            m = class_marginal(rows, class, &next, temp, Some(&mut outer));
        }
        offs = next;
    }
    if miss_of(&m) > FIT_TOLERANCE {
        // Stiff case (value gaps of many `temp`s, a few buckets carrying the weight): both steps
        // above crawl. Solve one class at a time by bisection instead; a class's frequency rises
        // with its own offset.
        for _round in 0..30 {
            for &c in &free {
                let f0 = m[c] - target[c];
                if f0.abs() <= 0.5 * FIT_TOLERANCE {
                    continue;
                }
                let dir = if f0 < 0.0 { 1.0 } else { -1.0 };
                let (mut near, mut far) = (offs[c], offs[c]);
                let mut step = temp;
                let mut bracketed = false;
                for _ in 0..80 {
                    far = near + dir * step;
                    offs[c] = far;
                    let f = class_marginal(rows, class, &offs, temp, None)[c] - target[c];
                    if (f < 0.0) != (f0 < 0.0) {
                        bracketed = true;
                        break;
                    }
                    near = far;
                    step *= 2.0;
                }
                if bracketed {
                    for _ in 0..60 {
                        offs[c] = 0.5 * (near + far);
                        let f = class_marginal(rows, class, &offs, temp, None)[c] - target[c];
                        if f.abs() <= 0.5 * FIT_TOLERANCE {
                            break;
                        }
                        if (f < 0.0) == (f0 < 0.0) {
                            near = offs[c];
                        } else {
                            far = offs[c];
                        }
                    }
                }
                m = class_marginal(rows, class, &offs, temp, None);
            }
            if miss_of(&m) <= FIT_TOLERANCE {
                break;
            }
        }
    }
    offs
}

impl PoolConstraint {
    /// Constraints for a solve with `n_nodes` subtree nodes; `hero` is the seat the search decides
    /// for (never constrained); `root` = where the betting stands at the subtree root
    /// ([`tracker_from_history`], taken while the root state still has its history).
    pub fn new(
        pool: &'static PoolSearch,
        hero: PlayerId,
        root: PostflopTracker,
        n_nodes: usize,
    ) -> PoolConstraint {
        PoolConstraint {
            pool,
            hero,
            root,
            nodes: (0..n_nodes).map(|_| OnceLock::new()).collect(),
            offsets: vec![[0.0; N_CLASSES]; n_nodes],
            fitted_weight: vec![0.0; n_nodes],
        }
    }

    /// The node's entry, if a traversal has been there.
    #[inline]
    pub fn get(&self, node: NodeId) -> Option<&PoolNode> {
        self.nodes[node as usize].get().map(|b| &**b)
    }

    /// The entry of the decision node `state` is at, created on first use. `parent` = the step
    /// that led here (`None` at the subtree root).
    #[inline]
    pub fn node(
        &self,
        state: &SimplifiedNlheState,
        actor: PlayerId,
        actions: &[AbstractAction],
        parent: Option<PoolParent<'_>>,
    ) -> &PoolNode {
        self.nodes[state.current_node_id as usize]
            .get_or_init(|| Box::new(self.build_node(&state.game_state, actor, actions, parent)))
    }

    fn build_node(
        &self,
        state: &GameState,
        actor: PlayerId,
        actions: &[AbstractAction],
        parent: Option<PoolParent<'_>>,
    ) -> PoolNode {
        let tracker = match parent {
            None => self.root,
            Some(p) => {
                let mut t = p.node.tracker;
                match postflop_index(state.street()) {
                    Some(idx) if idx != t.street() => {
                        let (folded, all_in) = status_masks(state);
                        t.next_street(state.pot().as_u64(), folded, all_in);
                    }
                    _ if p.fold => {
                        t.fold(p.actor);
                    }
                    _ => {
                        t.put(
                            p.actor,
                            state.players()[p.actor].committed_this_round.as_u64(),
                        );
                    }
                }
                t
            }
        };
        let constraint = (actor != self.hero)
            .then(|| self.constraint_for(&tracker, actor, actions))
            .flatten();
        PoolNode {
            tracker,
            constraint,
        }
    }

    fn constraint_for(
        &self,
        tracker: &PostflopTracker,
        actor: PlayerId,
        actions: &[AbstractAction],
    ) -> Option<NodeConstraint> {
        let spot = tracker.spot(actor as usize);
        let pred = self.pool.model.predict(&spot).ok()?;
        if pred.probs.is_empty() {
            return None;
        }
        let confidence = self.pool.params.confidence(pred.n);
        if confidence <= 0.0 {
            return None;
        }
        let class: SmallVec<[u8; 8]> = actions
            .iter()
            .map(|a| match a {
                AbstractAction::Fold => CLASS_FOLD,
                AbstractAction::Check | AbstractAction::Call { .. } => CLASS_PASSIVE,
                AbstractAction::Bet { .. } | AbstractAction::Raise { .. } => CLASS_AGGRESSIVE,
                // An all-in that does not exceed the bet is a call.
                AbstractAction::AllIn { to } if to.as_u64() > tracker.current_bet() => {
                    CLASS_AGGRESSIVE
                }
                AbstractAction::AllIn { .. } => CLASS_PASSIVE,
            } as u8)
            .collect();
        let mut present = [false; N_CLASSES];
        for &c in &class {
            present[c as usize] = true;
        }
        if present.iter().filter(|&&p| p).count() < 2 || !present[CLASS_PASSIVE] {
            return None;
        }
        let get = |k: &str| pred.probs.get(k).copied().unwrap_or(0.0);
        let mut target = [
            get("fold"),
            get("call") + get("check"),
            get("raise") + get("bet"),
        ];
        // What the node cannot do goes to check / call.
        for c in [CLASS_FOLD, CLASS_AGGRESSIVE] {
            if !present[c] {
                target[CLASS_PASSIVE] += target[c];
                target[c] = 0.0;
            }
        }
        for c in 0..N_CLASSES {
            if present[c] {
                target[c] = target[c].max(TARGET_FLOOR);
            }
        }
        let total: f64 = target.iter().sum();
        for t in &mut target {
            *t /= total;
        }
        Some(NodeConstraint {
            confidence,
            target,
            present,
            class,
            spot,
            n: pred.n,
            temp: (self.pool.params.temp * tracker.pot() as f64).max(1.0),
        })
    }

    /// Number of nodes whose offsets have been fitted.
    pub fn fitted_nodes(&self) -> usize {
        self.fitted_weight.iter().filter(|&&w| w > 0.0).count()
    }

    /// Whether `node`'s offsets have been fitted. Before that the free strategy is played.
    #[inline]
    pub fn is_fitted(&self, node: NodeId) -> bool {
        self.fitted_weight[node as usize] > 0.0
    }

    /// The pool-type strategy at `node`. `regrets` as in [`offset_strategy`].
    #[inline]
    pub fn pool_strategy(
        &self,
        node: NodeId,
        c: &NodeConstraint,
        regrets: Option<(&[f64], f64)>,
    ) -> SigmaVec {
        offset_strategy(regrets, &c.class, &self.offsets[node as usize], c.temp)
    }

    /// What a constrained opponent plays at `node` on average: the pool-type strategy with
    /// probability `confidence`, the free one otherwise. `regrets` as in [`offset_strategy`].
    /// Before the node's first fit: the free strategy.
    #[inline]
    pub fn played(
        &self,
        node: NodeId,
        c: &NodeConstraint,
        regrets: Option<(&[f64], f64)>,
        free: &SigmaVec,
    ) -> SigmaVec {
        if !self.is_fitted(node) {
            return free.clone();
        }
        let pool = self.pool_strategy(node, c, regrets);
        pool.iter()
            .zip(free.iter())
            .map(|(p, f)| c.confidence * p + (1.0 - c.confidence) * f)
            .collect()
    }

    /// LCFR rescale of the pool weights: the weights at the last fit follow.
    pub(crate) fn rescale(&mut self, factor: f64) {
        for w in &mut self.fitted_weight {
            *w *= factor as f32;
        }
    }

    /// Refit the offsets of the constrained nodes whose counted weight is large enough and, unless
    /// `all`, has grown enough since their last fit. `rows_of(node)` = the node's buckets with
    /// positive weight.
    pub(crate) fn fit<'t, F>(&mut self, all: bool, rows_of: F)
    where
        F: Fn(NodeId) -> Vec<PoolRow<'t>> + Sync,
    {
        use rayon::prelude::*;
        let fitted: Vec<(usize, f64, [f64; N_CLASSES])> = (0..self.nodes.len())
            .into_par_iter()
            .filter_map(|i| {
                let c = self.nodes[i].get()?.constraint.as_ref()?;
                let rows = rows_of(i as NodeId);
                let weight: f64 = rows.iter().map(|(w, _, _)| w).sum();
                let last = f64::from(self.fitted_weight[i]);
                if weight < MIN_FIT_WEIGHT || (!all && weight < REFIT_GROWTH * last) {
                    return None;
                }
                let offs = fit_offsets(
                    &rows,
                    &c.class,
                    &c.present,
                    &c.target,
                    c.temp,
                    self.offsets[i],
                );
                Some((i, weight, offs))
            })
            .collect();
        for (i, weight, offs) in fitted {
            self.offsets[i] = offs;
            self.fitted_weight[i] = weight as f32;
        }
    }

    /// Fitted constrained nodes with what the free and the played strategies do there, heaviest
    /// first. `rows_of` as in [`Self::fit`]; `seat_of(node)` = the node's actor.
    pub(crate) fn report<'t, F, S>(&self, rows_of: F, seat_of: S) -> Vec<PoolNodeReport>
    where
        F: Fn(NodeId) -> Vec<PoolRow<'t>>,
        S: Fn(NodeId) -> PlayerId,
    {
        let mut out: Vec<PoolNodeReport> = Vec::new();
        for i in 0..self.nodes.len() {
            let Some(c) = self.nodes[i].get().and_then(|n| n.constraint.as_ref()) else {
                continue;
            };
            if self.fitted_weight[i] <= 0.0 {
                continue;
            }
            let rows = rows_of(i as NodeId);
            let weight: f64 = rows.iter().map(|(w, _, _)| w).sum();
            if weight <= 0.0 {
                continue;
            }
            // The free strategy is plain regret matching (uniform without a positive regret).
            let mut free_rm = [0.0; N_CLASSES];
            for &(w, r, _) in &rows {
                let pos: f64 = r.iter().map(|v| v.max(0.0)).sum();
                for (a, v) in r.iter().enumerate() {
                    let p = if pos > 0.0 {
                        v.max(0.0) / pos
                    } else {
                        1.0 / r.len() as f64
                    };
                    free_rm[c.class[a] as usize] += w * p / weight;
                }
            }
            let pool = class_marginal(&rows, &c.class, &self.offsets[i], c.temp, None);
            let played: [f64; N_CLASSES] =
                std::array::from_fn(|k| c.confidence * pool[k] + (1.0 - c.confidence) * free_rm[k]);
            out.push(PoolNodeReport {
                node: i as NodeId,
                seat: seat_of(i as NodeId),
                spot: c.spot.clone(),
                n: c.n,
                confidence: c.confidence,
                weight,
                target: c.target,
                free: free_rm,
                played,
            });
        }
        out.sort_by(|a, b| b.weight.total_cmp(&a.weight));
        out
    }
}

/// Test model: one cell per postflop family (no feature splits), each seen a million times.
/// `lead` = [check, bet], `vs_bet` / `vs_raise` = [fold, call, raise], in observations per 1000.
#[cfg(test)]
pub(crate) fn test_pool(
    lead: [u64; 2],
    vs_bet: [u64; 3],
    vs_raise: [u64; 3],
    params: PoolSearchParams,
) -> &'static PoolSearch {
    let fcr = |x: [u64; 3]| {
        format!(
            r#"{{"fold":{},"call":{},"raise":{}}}"#,
            x[0] * 1000,
            x[1] * 1000,
            x[2] * 1000
        )
    };
    let family = |cells: String| {
        format!(r#"{{"dims":[],"cells":{{"":{cells}}},"size_dims":[],"size_cells":{{}}}}"#)
    };
    let text = format!(
        r#"{{"version":1,"k":10.0,"size_edges":[0.42,0.62,0.87,1.2,2.0],"families":{{"po_lead":{},"po_vs_bet":{},"po_vs_raise":{}}}}}"#,
        family(format!(
            r#"{{"check":{},"bet":{}}}"#,
            lead[0] * 1000,
            lead[1] * 1000
        )),
        family(fcr(vs_bet)),
        family(fcr(vs_raise)),
    );
    Box::leak(Box::new(
        PoolSearch::from_json(&text, params).expect("test pool model"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offset_strategy_follows_value_and_offset() {
        let class = [0u8, 1, 2];
        let regrets = [2.0, 3.0, -10.0];
        let at = |x: f64, temp: f64| {
            offset_strategy(Some((&regrets, 1.0)), &class, &[0.0, x, 0.0], temp)
        };
        // Values 1 apart at temp 1: e : 1.
        let s = at(0.0, 1.0);
        assert!((s[1] / s[0] - 1f64.exp()).abs() < 1e-9);
        assert!((s.iter().sum::<f64>() - 1.0).abs() < 1e-12);
        // The call frequency rises with the call offset.
        let xs = [-20.0, -6.0, -3.0, -1.0, 0.0, 1.0, 10.0, 100.0];
        for w in xs.windows(2) {
            assert!(at(w[0], 1.0)[1] < at(w[1], 1.0)[1], "{w:?}");
        }
        // A small temp is close to the hard maximum.
        assert!(at(0.0, 0.01)[1] > 0.999);
        assert!(at(-2.0, 0.01)[0] > 0.999);
        // Regret sums over 10 updates: the same averages, the same answer.
        let sums = [20.0, 30.0, -100.0];
        let avg = offset_strategy(Some((&sums, 0.1)), &class, &[0.0, -1.0, 0.0], 1.0);
        assert!((avg[1] - at(-1.0, 1.0)[1]).abs() < 1e-12);
        // Nothing known about the bucket and no offsets: uniform.
        let u = offset_strategy(None, &class, &[0.0; 3], 1.0);
        assert!(u.iter().all(|p| (p - 1.0 / 3.0).abs() < 1e-12));
        // Huge gaps do not overflow.
        let far = offset_strategy(Some((&[1e9, -1e9, 0.0], 1.0)), &class, &[0.0; 3], 1e-3);
        assert_eq!(far[0], 1.0);
    }

    /// The fit reaches three-class and two-class targets, from zero and from a far-off start, with
    /// many buckets and with a handful of very unequal ones, soft and nearly hard.
    #[test]
    fn fit_reaches_the_target_frequencies() {
        let many: Vec<(f64, Vec<f64>)> = (0..200)
            .map(|i| {
                let x = i as f64 / 200.0;
                (
                    1.0 + (i % 7) as f64,
                    vec![
                        50.0 * (0.5 - x),
                        30.0 * (x - 0.3),
                        80.0 * (x - 0.8),
                        40.0 * (x - 0.9),
                    ],
                )
            })
            .collect();
        // Three buckets, one of them with nine tenths of the weight and every action a loser.
        let few: Vec<(f64, Vec<f64>)> = vec![
            (900.0, vec![-30.0, -400.0, -900.0, -2000.0]),
            (60.0, vec![-200.0, 50.0, 300.0, 100.0]),
            (40.0, vec![10.0, -5.0, -700.0, -800.0]),
        ];
        let class = [0u8, 1, 2, 2];
        let present = [true, true, true];
        for data in [&many, &few] {
            let rows: Vec<PoolRow<'_>> =
                data.iter().map(|(w, r)| (*w, r.as_slice(), 1.0)).collect();
            for temp in [20.0, 1.0] {
                for target in [[0.6, 0.3, 0.1], [0.15, 0.5, 0.35], [0.05, 0.9, 0.05]] {
                    for start in [[0.0; 3], [0.0, 500.0, -300.0]] {
                        let offs = fit_offsets(&rows, &class, &present, &target, temp, start);
                        let m = class_marginal(&rows, &class, &offs, temp, None);
                        for c in 0..3 {
                            assert!(
                                (m[c] - target[c]).abs() < 0.005,
                                "{} rows temp {temp} target {target:?} start {start:?}: got {m:?}",
                                rows.len()
                            );
                        }
                        assert_eq!(offs[0], 0.0);
                    }
                }
                // Check / bet node: no fold class.
                let class2 = [1u8, 2, 2, 2];
                let present2 = [false, true, true];
                let target2 = [0.0, 0.7, 0.3];
                let offs = fit_offsets(&rows, &class2, &present2, &target2, temp, [0.0; 3]);
                let m = class_marginal(&rows, &class2, &offs, temp, None);
                assert!(
                    (m[2] - 0.3).abs() < 0.005 && (m[1] - 0.7).abs() < 0.005,
                    "{} rows temp {temp}: {m:?}",
                    rows.len()
                );
                assert_eq!((offs[0], offs[1]), (0.0, 0.0));
            }
        }
    }

    #[test]
    fn confidence_follows_the_counts() {
        let p = PoolSearchParams::default();
        assert_eq!(p.confidence(0), 0.0);
        assert!((p.confidence(50) - 0.25).abs() < 1e-12);
        assert!(p.confidence(1_000_000) < 0.5 + 1e-9);
        let full = PoolSearchParams {
            conf_scale: 1.0,
            ..p
        };
        assert_eq!(full.confidence(1_000_000), 0.9);
        assert!(PoolSearchParams {
            conf_scale: 1.5,
            ..p
        }
        .validate()
        .is_err());
        assert!(PoolSearchParams { temp: 0.0, ..p }.validate().is_err());
    }
}
