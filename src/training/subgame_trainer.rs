//! Array-backed ES-MCCFR trainer for real-time search subgames ([`SubgameNlheGame`]).
//!
//! Same algorithm as [`crate::training::trainer::EsMccfrTrainer`] (single-thread `step`,
//! deterministic `step_parallel`, LCFR period rescale, traverser rotation, visit counts); only the
//! regret / strategy-sum storage changes from `HashMap<InfoSetId, Vec<f64>>` to arrays indexed by
//! the subtree node id and hand bucket ([`SubgameTables`]). Every infoset sees the same f64
//! operation sequence as on the HashMap path, so results are bit-identical (including LCFR, which
//! rescales eagerly here, unlike the dense blueprint table's lazy global scale).
//!
//! Why not reuse [`crate::training::nlhe_dense_trainer::DenseNlheEsMccfrTrainer`]: it is tied to
//! `SimplifiedNlheGame`, pre-allocates the full `nodes × buckets × actions` table (a 200k-node
//! search subtree would be ~5 GiB per table) and lacks rotation / visit counts. Here a node's
//! row-index block is allocated the first time the node is touched and values are stored only
//! for touched rows, so memory follows what the solve actually visits.

use rayon::prelude::*;
use smallvec::SmallVec;

use crate::abstraction::info::{InfoSetId, StreetTag};
use crate::core::rng::RngSource;
use crate::error::TrainerError;
use crate::training::game::{Game, NodeKind, PlayerId};
use crate::training::nlhe::NLHE_V2_NODE_ID_SHIFT;
use crate::training::regret::{LocalRegretDelta, LocalStrategyDelta, SigmaVec};
use crate::training::sampling::sample_discrete;
use crate::training::subgame::SubgameNlheGame;

type ShortVec<T> = SmallVec<[T; 8]>;

/// Sentinel for "no block / no row".
const NONE: u32 = u32::MAX;
/// Row flag: regret entry exists (HashMap path: key present in the regret table).
const REGRET_PRESENT: u8 = 1;
/// Row flag: strategy-sum entry exists (HashMap path: key present in the strategy accumulator).
const STRATEGY_PRESENT: u8 = 2;

/// Regret + strategy-sum storage for one subgame solve.
///
/// Lookup is `node_block[node] + bucket → row_ids → row`, then `row_off[row]` locates the row's
/// `row_len[row]` contiguous values in `regret` / `strategy`. Both value arrays share offsets.
/// `flags` keeps the HashMap path's key-presence semantics (an infoset can have a regret entry
/// without a strategy entry), which the readout gates rely on.
pub struct SubgameTables {
    /// Hand-bucket count per street (`StreetTag as usize`), from the subgame's bucket table.
    bucket_count_by_street: [u32; 4],
    /// Per subtree node: start of its block in `row_ids`, or [`NONE`] if never touched.
    node_block: Vec<u32>,
    /// Per node: block length (= bucket count of the node's street); 0 if untouched.
    node_buckets: Vec<u32>,
    /// Per touched node, one slot per bucket: row id or [`NONE`].
    row_ids: Vec<u32>,
    row_off: Vec<u32>,
    row_len: Vec<u8>,
    flags: Vec<u8>,
    visits: Vec<u32>,
    regret: Vec<f64>,
    strategy: Vec<f64>,
}

impl SubgameTables {
    fn new(n_nodes: usize, bucket_count_by_street: [u32; 4]) -> Self {
        Self {
            bucket_count_by_street,
            node_block: vec![NONE; n_nodes],
            node_buckets: vec![0; n_nodes],
            row_ids: Vec::new(),
            row_off: Vec::new(),
            row_len: Vec::new(),
            flags: Vec::new(),
            visits: Vec::new(),
            regret: Vec::new(),
            strategy: Vec::new(),
        }
    }

    #[inline]
    fn node_and_bucket(info: InfoSetId) -> (usize, usize) {
        (
            (info.raw() >> NLHE_V2_NODE_ID_SHIFT) as usize,
            info.bucket_id() as usize,
        )
    }

    /// Row of `info`, if one was created.
    #[inline]
    fn find(&self, info: InfoSetId) -> Option<usize> {
        let (node, bucket) = Self::node_and_bucket(info);
        let base = self.node_block[node];
        if base == NONE {
            return None;
        }
        assert!(
            bucket < self.node_buckets[node] as usize,
            "bucket {bucket} out of range for node {node} ({} buckets)",
            self.node_buckets[node]
        );
        let row = self.row_ids[base as usize + bucket];
        (row != NONE).then_some(row as usize)
    }

    /// Row of `info`, creating it (zero values, no flags) on first use. Panics if `n_actions`
    /// differs from the stored row length (D-324, same trip-wire as `RegretTable::get_or_init`).
    fn find_or_insert(&mut self, info: InfoSetId, n_actions: usize) -> usize {
        if let Some(row) = self.find(info) {
            assert_eq!(
                self.row_len[row] as usize, n_actions,
                "SubgameTables action_count mismatch: stored {}, requested {n_actions} (D-324)",
                self.row_len[row]
            );
            return row;
        }
        let (node, bucket) = Self::node_and_bucket(info);
        if self.node_block[node] == NONE {
            let buckets = self.bucket_count_by_street[street_index(info.street_tag())];
            self.node_block[node] =
                u32::try_from(self.row_ids.len()).expect("row-id slots exceed u32");
            self.node_buckets[node] = buckets;
            self.row_ids
                .resize(self.row_ids.len() + buckets as usize, NONE);
        }
        assert!(
            bucket < self.node_buckets[node] as usize,
            "bucket {bucket} out of range for node {node} ({} buckets)",
            self.node_buckets[node]
        );
        let row = self.row_off.len();
        let off = self.regret.len();
        self.row_ids[self.node_block[node] as usize + bucket] =
            u32::try_from(row).expect("rows exceed u32");
        self.row_off
            .push(u32::try_from(off).expect("value slots exceed u32"));
        self.row_len
            .push(u8::try_from(n_actions).expect("action count exceeds u8"));
        self.flags.push(0);
        self.visits.push(0);
        self.regret.resize(off + n_actions, 0.0);
        self.strategy.resize(off + n_actions, 0.0);
        row
    }

    #[inline]
    fn range(&self, row: usize) -> std::ops::Range<usize> {
        let off = self.row_off[row] as usize;
        off..off + self.row_len[row] as usize
    }

    /// Regret matching (same R⁺ sum + division sequence as
    /// `RegretTable::current_strategy_smallvec`; a missing entry or `Σ R⁺ == 0` → uniform).
    fn current_strategy_smallvec(&self, info: InfoSetId, n_actions: usize) -> SigmaVec {
        let uniform = || SigmaVec::from_elem(1.0 / n_actions as f64, n_actions);
        let Some(row) = self
            .find(info)
            .filter(|&r| self.flags[r] & REGRET_PRESENT != 0)
        else {
            return uniform();
        };
        assert_eq!(
            self.row_len[row] as usize, n_actions,
            "SubgameTables::current_strategy action_count mismatch (D-324)"
        );
        let mut positives: SigmaVec = SigmaVec::with_capacity(n_actions);
        let mut sum = 0.0_f64;
        for &r in &self.regret[self.range(row)] {
            let r_plus = if r > 0.0 { r } else { 0.0 };
            positives.push(r_plus);
            sum += r_plus;
        }
        if sum > 0.0 {
            for p in &mut positives {
                *p /= sum;
            }
            positives
        } else {
            uniform()
        }
    }

    /// `RegretTable::get_or_init`: make sure the regret entry exists.
    pub(crate) fn init_regret(&mut self, info: InfoSetId, n_actions: usize) {
        let row = self.find_or_insert(info, n_actions);
        self.flags[row] |= REGRET_PRESENT;
    }

    /// `RegretTable::accumulate`. Returns the row (visit counting on the merge path).
    pub(crate) fn accumulate_regret(&mut self, info: InfoSetId, delta: &[f64]) -> usize {
        let row = self.find_or_insert(info, delta.len());
        self.flags[row] |= REGRET_PRESENT;
        let range = self.range(row);
        for (slot, &d) in self.regret[range].iter_mut().zip(delta) {
            *slot += d;
        }
        row
    }

    /// `StrategyAccumulator::accumulate`.
    pub(crate) fn accumulate_strategy(&mut self, info: InfoSetId, weighted: &[f64]) -> usize {
        let row = self.find_or_insert(info, weighted.len());
        self.flags[row] |= STRATEGY_PRESENT;
        let range = self.range(row);
        for (slot, &w) in self.strategy[range].iter_mut().zip(weighted) {
            *slot += w;
        }
        row
    }

    fn add_visit(&mut self, row: usize) {
        self.visits[row] = self.visits[row].saturating_add(1);
    }

    /// LCFR rescale. Values of rows without the matching flag are 0 and stay 0, so this equals
    /// the HashMap path's per-entry `*v *= factor`.
    fn rescale(&mut self, factor: f64, regret: bool) {
        if regret {
            for v in &mut self.regret {
                *v *= factor;
            }
        }
        for v in &mut self.strategy {
            *v *= factor;
        }
    }

    /// Strategy-sum row of `info` (`StrategyAccumulator::inner().get`); `None` if absent.
    pub fn strategy_sum_row(&self, info: InfoSetId) -> Option<&[f64]> {
        let row = self.find(info)?;
        (self.flags[row] & STRATEGY_PRESENT != 0).then(|| &self.strategy[self.range(row)])
    }

    /// Regret row of `info` (`RegretTable::inner().get`); `None` if absent.
    pub fn regret_row(&self, info: InfoSetId) -> Option<&[f64]> {
        let row = self.find(info)?;
        (self.flags[row] & REGRET_PRESENT != 0).then(|| &self.regret[self.range(row)])
    }

    /// Infosets with a regret entry (`RegretTable::len`).
    pub fn regret_len(&self) -> usize {
        self.flags
            .iter()
            .filter(|&&f| f & REGRET_PRESENT != 0)
            .count()
    }

    /// Infosets with a strategy-sum entry (`StrategyAccumulator::len`).
    pub fn strategy_len(&self) -> usize {
        self.flags
            .iter()
            .filter(|&&f| f & STRATEGY_PRESENT != 0)
            .count()
    }
}

fn street_index(street: StreetTag) -> usize {
    street as usize
}

/// ES-MCCFR trainer over [`SubgameTables`]; API mirrors the parts of `EsMccfrTrainer` that the
/// real-time search uses.
pub struct SubgameTrainer {
    game: SubgameNlheGame,
    tables: SubgameTables,
    update_count: u64,
    lcfr_period_size: Option<u64>,
    lcfr_periods_completed: u64,
    traverser_rotation: Option<Vec<PlayerId>>,
    track_visits: bool,
}

impl SubgameTrainer {
    pub fn new(game: SubgameNlheGame) -> Self {
        let table = game.bucket_table();
        // hero 真实手牌单独信息集：专用桶号 = 该街桶数，每街多留一格（`SubgameNlheGame::with_real_hand`）。
        let extra = u32::from(game.has_real_hand());
        let bucket_count_by_street = [
            table.bucket_count(StreetTag::Preflop) + extra,
            table.bucket_count(StreetTag::Flop) + extra,
            table.bucket_count(StreetTag::Turn) + extra,
            table.bucket_count(StreetTag::River) + extra,
        ];
        let tables = SubgameTables::new(game.subtree().num_nodes(), bucket_count_by_street);
        Self {
            game,
            tables,
            update_count: 0,
            lcfr_period_size: None,
            lcfr_periods_completed: 0,
            traverser_rotation: None,
            track_visits: false,
        }
    }

    /// Same contract as `EsMccfrTrainer::with_traverser_rotation`.
    pub fn with_traverser_rotation(mut self, seats: Vec<PlayerId>) -> Self {
        assert!(!seats.is_empty(), "traverser rotation must be non-empty");
        let n = self.game.n_players();
        assert!(
            seats.iter().all(|&s| (s as usize) < n),
            "traverser rotation seat out of range (n_players = {n})"
        );
        assert_eq!(
            self.update_count, 0,
            "traverser rotation must be configured before any step"
        );
        self.traverser_rotation = Some(seats);
        self
    }

    /// Same contract as `EsMccfrTrainer::with_visit_counts`.
    pub fn with_visit_counts(mut self) -> Self {
        assert_eq!(
            self.update_count, 0,
            "visit counts must be enabled before any step"
        );
        self.track_visits = true;
        self
    }

    /// Same contract as `EsMccfrTrainer::with_lcfr_period` (regret and strategy sum rescaled).
    pub fn with_lcfr_period(mut self, period_size: u64) -> Self {
        assert!(period_size > 0, "LCFR period_size must be > 0");
        assert_eq!(
            self.update_count, 0,
            "LCFR period_size must be configured before any step"
        );
        self.lcfr_period_size = Some(period_size);
        self
    }

    fn traverser_at(&self, t: u64) -> PlayerId {
        match &self.traverser_rotation {
            Some(rot) => rot[(t % rot.len() as u64) as usize],
            None => (t % self.game.n_players() as u64) as PlayerId,
        }
    }

    fn maybe_lcfr_rescale(&mut self) {
        let Some(period_size) = self.lcfr_period_size else {
            return;
        };
        let target = self.update_count / period_size;
        while self.lcfr_periods_completed < target {
            let n = self.lcfr_periods_completed + 1;
            let factor = (n as f64) / ((n + 1) as f64);
            self.tables.rescale(factor, true);
            self.lcfr_periods_completed = n;
        }
    }

    /// One update (`EsMccfrTrainer::step`).
    pub fn step(&mut self, rng: &mut dyn RngSource) -> Result<(), TrainerError> {
        let traverser = self.traverser_at(self.update_count);
        let root = self.game.root_for_traverser(traverser, rng);
        let track = self.track_visits;
        recurse(root, traverser, 1.0, &mut self.tables, track, rng);
        self.update_count += 1;
        self.maybe_lcfr_rescale();
        Ok(())
    }

    /// `EsMccfrTrainer::step_parallel`: workers read σ from the pre-dispatch tables and push
    /// deltas locally; the main thread merges them in tid order × push order.
    pub fn step_parallel(
        &mut self,
        rng_pool: &mut [Box<dyn RngSource>],
        n_threads: usize,
        batch_per_worker: usize,
    ) -> Result<(), TrainerError> {
        let n_active = n_threads.min(rng_pool.len());
        if n_active == 0 || batch_per_worker == 0 {
            return Ok(());
        }
        let base = self.update_count;
        let game = &self.game;
        let shared = &self.tables;
        let rotation = self.traverser_rotation.as_deref();
        let n_players = game.n_players() as u64;
        #[allow(clippy::type_complexity)]
        let deltas: Vec<(LocalRegretDelta<InfoSetId>, LocalStrategyDelta<InfoSetId>)> = rng_pool
            [..n_active]
            .par_iter_mut()
            .enumerate()
            .map(|(tid, rng_slot)| {
                let mut local_regret = LocalRegretDelta::new();
                let mut local_strategy = LocalStrategyDelta::new();
                let rng = rng_slot.as_mut();
                for batch_idx in 0..batch_per_worker {
                    let t = base + batch_idx as u64 * n_active as u64 + tid as u64;
                    let traverser = match rotation {
                        Some(rot) => rot[(t % rot.len() as u64) as usize],
                        None => (t % n_players) as PlayerId,
                    };
                    let root = game.root_for_traverser(traverser, rng);
                    recurse_parallel(
                        root,
                        traverser,
                        1.0,
                        shared,
                        &mut local_regret,
                        &mut local_strategy,
                        rng,
                    );
                }
                (local_regret, local_strategy)
            })
            .collect();
        for (local_regret, local_strategy) in deltas {
            for (info, delta) in local_regret.into_entries() {
                // One regret push = one traverser visit.
                let row = self.tables.accumulate_regret(info, &delta);
                if self.track_visits {
                    self.tables.add_visit(row);
                }
            }
            for (info, weighted) in local_strategy.into_entries() {
                self.tables.accumulate_strategy(info, &weighted);
            }
        }
        self.update_count += (n_active as u64) * (batch_per_worker as u64);
        self.maybe_lcfr_rescale();
        Ok(())
    }

    /// Regret-matching strategy (`Trainer::current_strategy`): empty if `info` was never seen.
    pub fn current_strategy(&self, info: &InfoSetId) -> Vec<f64> {
        match self.tables.find(*info) {
            Some(row) if self.tables.flags[row] != 0 => self
                .tables
                .current_strategy_smallvec(*info, self.tables.row_len[row] as usize)
                .into_vec(),
            _ => Vec::new(),
        }
    }

    /// Average strategy (`Trainer::average_strategy`): empty if `info` was never seen; uniform if
    /// it only has a regret entry or a zero strategy sum.
    pub fn average_strategy(&self, info: &InfoSetId) -> Vec<f64> {
        let Some(row) = self
            .tables
            .find(*info)
            .filter(|&r| self.tables.flags[r] != 0)
        else {
            return Vec::new();
        };
        let n = self.tables.row_len[row] as usize;
        let uniform = || vec![1.0 / n as f64; n];
        let Some(sums) = self.tables.strategy_sum_row(*info) else {
            return uniform();
        };
        let total: f64 = sums.iter().sum();
        if total > 0.0 {
            sums.iter().map(|s| s / total).collect()
        } else {
            uniform()
        }
    }

    /// Traverser visits of `info` (`EsMccfrTrainer::visit_count`): `None` when not tracked.
    pub fn visit_count(&self, info: &InfoSetId) -> Option<u32> {
        if !self.track_visits {
            return None;
        }
        Some(
            self.tables
                .find(*info)
                .map_or(0, |row| self.tables.visits[row]),
        )
    }

    pub fn tables(&self) -> &SubgameTables {
        &self.tables
    }

    /// Test hook: edit the tables directly (readout-gate tests stage specific table states).
    #[cfg(test)]
    pub(crate) fn tables_mut(&mut self) -> &mut SubgameTables {
        &mut self.tables
    }

    /// Test hook: overwrite the traverser visit count of `info` (creates the row if needed).
    #[cfg(test)]
    pub(crate) fn set_visit_count(&mut self, info: InfoSetId, n_actions: usize, visits: u32) {
        let row = self.tables.find_or_insert(info, n_actions);
        self.tables.visits[row] = visits;
    }

    pub fn game(&self) -> &SubgameNlheGame {
        &self.game
    }

    pub fn update_count(&self) -> u64 {
        self.update_count
    }
}

/// Single-thread ES-MCCFR recursion; line for line `trainer::recurse_es` (subgames have no
/// in-game chance nodes).
fn recurse(
    state: <SubgameNlheGame as Game>::State,
    traverser: PlayerId,
    pi_trav: f64,
    tables: &mut SubgameTables,
    track_visits: bool,
    rng: &mut dyn RngSource,
) -> f64 {
    match SubgameNlheGame::current(&state) {
        NodeKind::Terminal => SubgameNlheGame::payoff(&state, traverser),
        NodeKind::Chance => unreachable!("subgame has no in-game chance nodes"),
        NodeKind::Player(actor) => {
            let info = SubgameNlheGame::info_set(&state, actor);
            let actions = SubgameNlheGame::legal_actions(&state);
            let n = actions.len();
            tables.init_regret(info, n);
            let sigma = tables.current_strategy_smallvec(info, n);

            if actor == traverser {
                let weighted: ShortVec<f64> = sigma.iter().map(|s| pi_trav * s).collect();
                let row = tables.accumulate_strategy(info, &weighted);
                if track_visits {
                    tables.add_visit(row);
                }
                let mut cfvs: ShortVec<f64> = ShortVec::with_capacity(n);
                for (i, action) in actions.iter().enumerate() {
                    let next_state = SubgameNlheGame::next(state.clone(), *action, rng);
                    let cfv = recurse(
                        next_state,
                        traverser,
                        pi_trav * sigma[i],
                        tables,
                        track_visits,
                        rng,
                    );
                    cfvs.push(cfv);
                }
                let sigma_value: f64 = sigma.iter().zip(&cfvs).map(|(s, c)| s * c).sum();
                let delta: ShortVec<f64> = cfvs.iter().map(|c| c - sigma_value).collect();
                tables.accumulate_regret(info, &delta);
                sigma_value
            } else {
                let nonzero_dist: ShortVec<(<SubgameNlheGame as Game>::Action, f64)> = actions
                    .iter()
                    .copied()
                    .zip(sigma.iter().copied())
                    .filter(|(_, p)| *p > 0.0)
                    .collect();
                debug_assert!(!nonzero_dist.is_empty(), "non-traverser σ all-zero");
                let sampled = sample_discrete(&nonzero_dist, rng);
                let next_state = SubgameNlheGame::next(state, sampled, rng);
                recurse(next_state, traverser, pi_trav, tables, track_visits, rng)
            }
        }
    }
}

/// Parallel worker recursion; line for line `trainer::recurse_es_parallel` (σ from the shared
/// pre-dispatch tables, deltas pushed locally).
fn recurse_parallel(
    state: <SubgameNlheGame as Game>::State,
    traverser: PlayerId,
    pi_trav: f64,
    shared: &SubgameTables,
    local_regret: &mut LocalRegretDelta<InfoSetId>,
    local_strategy: &mut LocalStrategyDelta<InfoSetId>,
    rng: &mut dyn RngSource,
) -> f64 {
    match SubgameNlheGame::current(&state) {
        NodeKind::Terminal => SubgameNlheGame::payoff(&state, traverser),
        NodeKind::Chance => unreachable!("subgame has no in-game chance nodes"),
        NodeKind::Player(actor) => {
            let info = SubgameNlheGame::info_set(&state, actor);
            let actions = SubgameNlheGame::legal_actions(&state);
            let n = actions.len();
            let sigma = shared.current_strategy_smallvec(info, n);

            if actor == traverser {
                debug_assert!(n > 0, "traverser node must have a legal action");
                let weighted: SigmaVec = sigma.iter().map(|s| pi_trav * s).collect();
                local_strategy.push(info, weighted);

                let mut cfvs: ShortVec<f64> = ShortVec::with_capacity(n);
                let last_idx = n - 1;
                for i in 0..last_idx {
                    let next_state = SubgameNlheGame::next(state.clone(), actions[i], rng);
                    let cfv = recurse_parallel(
                        next_state,
                        traverser,
                        pi_trav * sigma[i],
                        shared,
                        local_regret,
                        local_strategy,
                        rng,
                    );
                    cfvs.push(cfv);
                }
                let last_state = SubgameNlheGame::next(state, actions[last_idx], rng);
                let cfv_last = recurse_parallel(
                    last_state,
                    traverser,
                    pi_trav * sigma[last_idx],
                    shared,
                    local_regret,
                    local_strategy,
                    rng,
                );
                cfvs.push(cfv_last);
                let sigma_value: f64 = sigma.iter().zip(&cfvs).map(|(s, c)| s * c).sum();
                let delta: SigmaVec = cfvs.iter().map(|c| c - sigma_value).collect();
                local_regret.push(info, delta);
                sigma_value
            } else {
                let nonzero_dist: ShortVec<(<SubgameNlheGame as Game>::Action, f64)> = actions
                    .iter()
                    .copied()
                    .zip(sigma.iter().copied())
                    .filter(|(_, p)| *p > 0.0)
                    .collect();
                debug_assert!(!nonzero_dist.is_empty(), "non-traverser σ all-zero");
                let sampled = sample_discrete(&nonzero_dist, rng);
                let next_state = SubgameNlheGame::next(state, sampled, rng);
                recurse_parallel(
                    next_state,
                    traverser,
                    pi_trav,
                    shared,
                    local_regret,
                    local_strategy,
                    rng,
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::abstraction::bucket_table::{BucketConfig, BucketTable};
    use crate::core::rng::ChaCha20Rng;
    use crate::core::{Card, PlayerStatus};
    use crate::rules::action::Action;
    use crate::rules::config::TableConfig;
    use crate::rules::state::GameState;
    use crate::training::nlhe::{pack_info_set_v2, SimplifiedNlheState};
    use crate::training::nlhe_betting_tree::deep_menu_for;
    use crate::training::regret::{RegretTable, StrategyAccumulator};
    use crate::training::trainer::{EsMccfrTrainer, Trainer};

    fn bits(v: &[f64]) -> Vec<u64> {
        v.iter().map(|x| x.to_bits()).collect()
    }

    /// Index math against a HashMap reference: random ops over infosets spread across nodes,
    /// streets and buckets (incl. the last bucket of a block) must leave every row bit-identical,
    /// with the same key-presence sets and no cross-talk between neighbouring blocks.
    #[test]
    fn tables_match_hashmap_reference() {
        let buckets = [169u32, 7, 5, 3];
        let n_nodes = 40;
        let mut tables = SubgameTables::new(n_nodes, buckets);
        let mut regret: RegretTable<InfoSetId> = RegretTable::new();
        let mut strategy: StrategyAccumulator<InfoSetId> = StrategyAccumulator::new();
        let streets = [
            StreetTag::Preflop,
            StreetTag::Flop,
            StreetTag::Turn,
            StreetTag::River,
        ];
        // Node k: street k % 4, action count 1 + k % 5 (fixed per node, as in a real tree).
        let info_of =
            |node: u32, bucket: u32| pack_info_set_v2(bucket, node, streets[node as usize % 4]);
        let n_of = |node: u32| 1 + node as usize % 5;
        let mut rng = ChaCha20Rng::from_seed(0x7AB1E5);
        for step in 0..20_000u32 {
            let node = (rng.next_u64() % n_nodes as u64) as u32;
            let b = buckets[node as usize % 4];
            let bucket = if step % 7 == 0 {
                b - 1
            } else {
                (rng.next_u64() % u64::from(b)) as u32
            };
            let info = info_of(node, bucket);
            let n = n_of(node);
            let delta: Vec<f64> = (0..n)
                .map(|_| (rng.next_u64() % 2001) as f64 / 7.0 - 140.0)
                .collect();
            match rng.next_u64() % 4 {
                0 => {
                    regret.get_or_init(info, n);
                    tables.init_regret(info, n);
                }
                1 => {
                    regret.accumulate(info, &delta);
                    tables.accumulate_regret(info, &delta);
                }
                2 => {
                    strategy.accumulate(info, &delta);
                    tables.accumulate_strategy(info, &delta);
                }
                _ => {
                    if step % 1000 == 3 {
                        regret.rescale_all(0.75);
                        strategy.rescale_all(0.75);
                        tables.rescale(0.75, true);
                    }
                }
            }
            assert_eq!(
                bits(&regret.current_strategy(&info, n)),
                bits(&tables.current_strategy_smallvec(info, n)),
                "σ at step {step}"
            );
        }
        assert_eq!(regret.len(), tables.regret_len());
        assert_eq!(strategy.len(), tables.strategy_len());
        for (k, v) in regret.inner() {
            assert_eq!(bits(v), bits(tables.regret_row(*k).expect("regret row")));
        }
        for (k, v) in strategy.inner() {
            assert_eq!(
                bits(v),
                bits(tables.strategy_sum_row(*k).expect("strategy row"))
            );
        }
        // Keys the reference never saw stay absent.
        for node in 0..n_nodes as u32 {
            for bucket in 0..buckets[node as usize % 4] {
                let info = info_of(node, bucket);
                assert_eq!(
                    regret.inner().contains_key(&info),
                    tables.regret_row(info).is_some()
                );
                assert_eq!(
                    strategy.inner().contains_key(&info),
                    tables.strategy_sum_row(info).is_some()
                );
            }
        }
    }

    /// 6-max limped pot, three live on the flop (BTN / SB / BB).
    fn three_way_flop_sub() -> SubgameNlheGame {
        let cfg = TableConfig::six_max_at_bb(100);
        let mut st = GameState::new(&cfg, 0x3_3A7);
        for _ in 0..3 {
            st.apply(Action::Fold).expect("UTG/HJ/CO fold");
        }
        st.apply(Action::Call).expect("BTN limp");
        st.apply(Action::Call).expect("SB complete");
        st.apply(Action::Check).expect("BB check");
        assert_eq!(st.board().len(), 3, "flop");
        let (abs, rules) = deep_menu_for(&st);
        let entrants = st
            .players()
            .iter()
            .enumerate()
            .filter(|(_, p)| matches!(p.status, PlayerStatus::Active | PlayerStatus::AllIn))
            .fold(0u16, |e, (i, _)| e | (1 << i));
        let table = Arc::new(BucketTable::stub_for_postflop(
            BucketConfig::default_500_500_500(),
        ));
        SubgameNlheGame::new(table, cfg, abs, rules, st, entrants, 0)
    }

    /// Same limped pot checked down to the river (three live, first to act decides).
    fn three_way_river_sub() -> SubgameNlheGame {
        let cfg = TableConfig::six_max_at_bb(100);
        let mut st = GameState::new(&cfg, 0x3_3A7);
        for _ in 0..3 {
            st.apply(Action::Fold).expect("UTG/HJ/CO fold");
        }
        st.apply(Action::Call).expect("BTN limp");
        st.apply(Action::Call).expect("SB complete");
        st.apply(Action::Check).expect("BB check");
        for _ in 0..6 {
            st.apply(Action::Check).expect("check flop / turn");
        }
        assert_eq!(st.board().len(), 5, "river");
        let (abs, rules) = deep_menu_for(&st);
        let entrants = st
            .players()
            .iter()
            .enumerate()
            .filter(|(_, p)| matches!(p.status, PlayerStatus::Active | PlayerStatus::AllIn))
            .fold(0u16, |e, (i, _)| e | (1 << i));
        let table = Arc::new(BucketTable::stub_for_postflop(
            BucketConfig::default_500_500_500(),
        ));
        SubgameNlheGame::new(table, cfg, abs, rules, st, entrants, 0)
    }

    fn assert_same(a: &EsMccfrTrainer<SubgameNlheGame>, b: &SubgameTrainer, label: &str) {
        assert_eq!(a.update_count(), b.update_count(), "{label}");
        assert_eq!(a.regret_table().len(), b.tables().regret_len(), "{label}");
        assert_eq!(a.strategy_sum().len(), b.tables().strategy_len(), "{label}");
        assert!(
            a.regret_table().len() > 100,
            "{label}: too few infosets to mean anything"
        );
        for (k, v) in a.regret_table().inner() {
            assert_eq!(bits(v), bits(b.tables().regret_row(*k).unwrap()), "{label}");
            assert_eq!(a.visit_count(k), b.visit_count(k), "{label}");
            assert_eq!(a.average_strategy(k), b.average_strategy(k), "{label}");
            assert_eq!(a.current_strategy(k), b.current_strategy(k), "{label}");
        }
        for (k, v) in a.strategy_sum().inner() {
            assert_eq!(
                bits(v),
                bits(b.tables().strategy_sum_row(*k).unwrap()),
                "{label}"
            );
        }
    }

    /// Hero (first to act on the flop) and its real hole cards in [`three_way_flop_sub`].
    fn hero_and_hole(sub: &SubgameNlheGame) -> (PlayerId, [Card; 2]) {
        let t = sub.template();
        let hero = t.current_player().expect("flop decision").0 as PlayerId;
        (
            hero,
            t.players()[hero as usize].hole_cards.expect("hero hole"),
        )
    }

    fn with_real(sub: SubgameNlheGame, q: f64) -> SubgameNlheGame {
        let (hero, hole) = hero_and_hole(&sub);
        sub.with_real_hand(hero, hole, q)
    }

    /// Bit-identical to `EsMccfrTrainer<SubgameNlheGame>`: single-thread `step` and
    /// `step_parallel`, each with / without LCFR, with live-seat rotation and visit counts; also with
    /// the real-hand infoset + forced deal on (both trainers go through `root_for_traverser`).
    #[test]
    fn trainer_matches_hashmap_trainer() {
        let live: Vec<PlayerId> = vec![0, 1, 2];
        for (label, parallel, lcfr, real) in [
            ("step", false, false, false),
            ("step+lcfr", false, true, false),
            ("parallel", true, false, false),
            ("parallel+lcfr", true, true, false),
            ("step+real", false, false, true),
            ("parallel+lcfr+real", true, true, true),
        ] {
            let sub = || {
                if real {
                    with_real(three_way_flop_sub(), 0.5)
                } else {
                    three_way_flop_sub()
                }
            };
            let mut a = EsMccfrTrainer::new(sub(), 1)
                .with_visit_counts()
                .with_traverser_rotation(live.clone());
            let mut b = SubgameTrainer::new(sub())
                .with_visit_counts()
                .with_traverser_rotation(live.clone());
            if lcfr {
                a = a.with_lcfr_period(50);
                b = b.with_lcfr_period(50);
            }
            if parallel {
                let pool = || -> Vec<Box<dyn RngSource>> {
                    (0..3u64)
                        .map(|t| Box::new(ChaCha20Rng::from_seed(0x9A + t)) as Box<dyn RngSource>)
                        .collect()
                };
                let (mut pa, mut pb) = (pool(), pool());
                for _ in 0..20 {
                    a.step_parallel(&mut pa, 3, 4).unwrap();
                    b.step_parallel(&mut pb, 3, 4).unwrap();
                }
            } else {
                let mut ra = ChaCha20Rng::from_seed(0x51);
                let mut rb = ChaCha20Rng::from_seed(0x51);
                for _ in 0..240 {
                    a.step(&mut ra).unwrap();
                    b.step(&mut rb).unwrap();
                }
            }
            assert_same(&a, &b, label);
        }
        // Visit tracking off → `None`, like the HashMap trainer.
        let b = SubgameTrainer::new(three_way_flop_sub());
        let info = pack_info_set_v2(0, 0, StreetTag::Flop);
        assert_eq!(b.visit_count(&info), None);
        assert!(b.average_strategy(&info).is_empty());
    }

    fn holes_and_board(s: &SimplifiedNlheState) -> (Vec<Option<[Card; 2]>>, Vec<Card>) {
        let g = &s.game_state;
        (
            g.players().iter().map(|p| p.hole_cards).collect(),
            g.board().to_vec(),
        )
    }

    /// Off, or on but the traverser is not hero (or q = 0): `root_for_traverser` deals exactly what
    /// `root` deals and leaves the rng at the same point.
    #[test]
    fn root_for_traverser_without_forced_deal_matches_root() {
        let plain = three_way_flop_sub();
        let (hero, _) = hero_and_hole(&plain);
        let other = (0..3).find(|&s| s != hero).unwrap();
        for (sub, traverser) in [
            (three_way_flop_sub(), hero),
            (with_real(three_way_flop_sub(), 0.5), other),
            (with_real(three_way_flop_sub(), 0.0), hero),
        ] {
            let mut ra = ChaCha20Rng::from_seed(0x77);
            let mut rb = ChaCha20Rng::from_seed(0x77);
            for _ in 0..50 {
                let a = holes_and_board(&plain.root(&mut ra));
                let b = holes_and_board(&sub.root_for_traverser(traverser, &mut rb));
                assert_eq!(a, b);
            }
            assert_eq!(ra.next_u64(), rb.next_u64(), "rng streams diverged");
        }
    }

    /// q = 1: every hero traversal deals the real hole, no card collides, the visible board is kept.
    /// q = 0.5: about half of hero traversals are forced (plus the rare natural draw).
    #[test]
    fn forced_deal_gives_hero_real_hole() {
        let sub = with_real(three_way_flop_sub(), 1.0);
        let (hero, hole) = hero_and_hole(&sub);
        let flop = sub.template().board().to_vec();
        let mut rng = ChaCha20Rng::from_seed(0xF0);
        for _ in 0..300 {
            let (holes, board) = holes_and_board(&sub.root_for_traverser(hero, &mut rng));
            assert_eq!(holes[hero as usize], Some(hole));
            assert_eq!(&board[..3], &flop[..]);
            let mut seen = 0u64;
            for c in holes.iter().flatten().flatten().chain(board.iter()) {
                let bit = 1u64 << c.to_u8();
                assert_eq!(seen & bit, 0, "card {c:?} dealt twice");
                seen |= bit;
            }
            for (s, h) in holes.iter().enumerate() {
                let live = sub.template().players()[s].hole_cards.is_some();
                assert_eq!(h.is_some(), live, "seat {s}");
            }
        }
        let sub = with_real(three_way_flop_sub(), 0.5);
        let n = 4000;
        let hits = (0..n)
            .filter(|_| {
                holes_and_board(&sub.root_for_traverser(hero, &mut rng)).0[hero as usize]
                    == Some(hole)
            })
            .count();
        let rate = hits as f64 / n as f64;
        assert!((0.46..0.54).contains(&rate), "forced rate {rate}");
    }

    /// Real hole at a hero node → the street's dedicated slot (= bucket count); any other hole, or
    /// another seat, keeps its normal bucket. `query_at` and `info_set_for_cards` agree.
    #[test]
    fn real_hand_infoset_uses_dedicated_slot() {
        let sub = with_real(three_way_flop_sub(), 0.5);
        let (hero, hole) = hero_and_hole(&sub);
        let root = sub.subtree().root_id();
        let flop_buckets = sub.bucket_table().bucket_count(StreetTag::Flop);
        let (info, _) = sub.query_at(root, sub.template());
        assert_eq!(info.bucket_id(), flop_buckets);
        assert_eq!(info.street_tag(), StreetTag::Flop);
        let board = sub.template().board().to_vec();
        assert_eq!(sub.info_set_for_cards(root, hole, &board), info);
        assert_eq!(
            sub.info_set_for_cards(root, [hole[1], hole[0]], &board),
            info
        );
        // Same node, a different hole → a normal bucket, identical to the feature-off game.
        let plain = three_way_flop_sub();
        let mut rng = ChaCha20Rng::from_seed(0x1F);
        let mut checked = 0;
        for _ in 0..200 {
            let s = sub.root_for_traverser((hero + 1) % 3, &mut rng);
            let h = s.game_state.players()[hero as usize].hole_cards.unwrap();
            let got = SubgameNlheGame::info_set(&s, hero);
            if h == hole || h == [hole[1], hole[0]] {
                assert_eq!(got.bucket_id(), flop_buckets);
                continue;
            }
            assert!(got.bucket_id() < flop_buckets);
            assert_eq!(got, plain.info_set_for_cards(root, h, &board));
            checked += 1;
        }
        assert!(checked > 150);
        // Feature off: the real hole maps to its ordinary bucket.
        let (plain_info, _) = plain.query_at(root, plain.template());
        assert!(plain_info.bucket_id() < flop_buckets);
    }

    /// V3 (design doc §3): forced deal is unbiased for the real-hand infoset. Same subgame, same total
    /// iterations (so opponents get the same training), only hero's sampling differs: slot-only
    /// (real hole reached by natural draws, ~1/1000 of hero traversals) vs q = 0.5. The stub bucket
    /// table pools every other hand into one bucket per node — a coarse but well-defined game, small
    /// enough that natural draws alone reach thousands of visits. Across seeds the cross-scheme L1
    /// distance of the real-hand average strategy must stay within 1.5× the within-scheme spread.
    #[test]
    #[ignore = "release/--ignored opt-in（2 × 4 seeds × RH_V3_ITERS updates, 8 threads）"]
    fn forced_deal_matches_natural_draw_reference() {
        let iters: u64 = std::env::var("RH_V3_ITERS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(24_000_000);
        let solve = |q: f64, seed: u64| -> (Vec<f64>, u32) {
            let sub = with_real(three_way_river_sub(), q);
            let root = sub.subtree().root_id();
            let (info, _) = sub.query_at(root, sub.template());
            let mut t = SubgameTrainer::new(sub)
                .with_visit_counts()
                .with_traverser_rotation(vec![0, 1, 2]);
            let mut pool: Vec<Box<dyn RngSource>> = (0..8u64)
                .map(|k| Box::new(ChaCha20Rng::from_seed(seed * 100 + k)) as Box<dyn RngSource>)
                .collect();
            let batch = 256;
            for _ in 0..iters / (8 * batch as u64) {
                t.step_parallel(&mut pool, 8, batch).unwrap();
            }
            (t.average_strategy(&info), t.visit_count(&info).unwrap())
        };
        let l1 = |a: &[f64], b: &[f64]| a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum::<f64>();
        let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
        let seeds = [1u64, 2, 3, 4];
        let natural: Vec<_> = seeds.iter().map(|&s| solve(0.0, s)).collect();
        let forced: Vec<_> = seeds.iter().map(|&s| solve(0.5, s)).collect();
        let pairs = |v: &[(Vec<f64>, u32)]| -> Vec<f64> {
            let mut d = Vec::new();
            for i in 0..v.len() {
                for j in i + 1..v.len() {
                    d.push(l1(&v[i].0, &v[j].0));
                }
            }
            d
        };
        let d_nat = mean(&pairs(&natural));
        let d_forced = mean(&pairs(&forced));
        let cross: Vec<f64> = natural
            .iter()
            .flat_map(|a| forced.iter().map(move |b| l1(&a.0, &b.0)))
            .collect();
        let d_cross = mean(&cross);
        for (label, v) in [("natural", &natural), ("forced", &forced)] {
            for (s, (sigma, visits)) in seeds.iter().zip(v.iter()) {
                eprintln!("{label} seed={s} visits={visits} sigma={sigma:.4?}");
            }
        }
        eprintln!("iters={iters} D_cross={d_cross:.4} D_natural={d_nat:.4} D_forced={d_forced:.4}");
        assert!(
            natural.iter().all(|(_, v)| *v >= 2_000),
            "natural reference too thin to mean anything"
        );
        let within = (d_nat + d_forced) / 2.0;
        assert!(
            d_cross <= 1.5 * within.max(0.01),
            "forced deal shifts the real-hand strategy: D_cross {d_cross:.4} vs within {within:.4}"
        );
    }

    /// The point of the feature: the hero infoset read at the decision gets far more traverser visits.
    /// (The stub bucket table maps every hand to bucket 0, so the feature-off read here sees every
    /// hero traversal; the meaningful baseline is slot-only, where the real hole needs a natural draw.)
    #[test]
    fn forced_deal_raises_hero_visits() {
        let visits = |sub: SubgameNlheGame| {
            let root = sub.subtree().root_id();
            let (info, _) = sub.query_at(root, sub.template());
            let mut t = SubgameTrainer::new(sub)
                .with_visit_counts()
                .with_traverser_rotation(vec![0, 1, 2]);
            let mut rng = ChaCha20Rng::from_seed(0x5EED);
            for _ in 0..3000 {
                t.step(&mut rng).unwrap();
            }
            t.visit_count(&info).unwrap()
        };
        let slot_only = visits(with_real(three_way_flop_sub(), 0.0));
        let forced = visits(with_real(three_way_flop_sub(), 0.5));
        // 1000 hero traversals, half forced → ~500; slot-only needs the exact combo drawn (~1/1000).
        assert!(forced >= 400, "forced visits {forced}");
        assert!(
            forced > 20 * slot_only.max(1),
            "forced {forced} vs slot-only {slot_only}"
        );
    }
}
