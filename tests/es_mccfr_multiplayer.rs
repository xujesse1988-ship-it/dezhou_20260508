//! 多人（n ≥ 3）ES-MCCFR 平均策略 NashConv 门槛
//! （`docs/temp/es_mccfr_leduc_convergence_issue_2026_10_25.md` §9）。
//!
//! 平均策略在所有非遍历者节点按权重 1 累积（[`EsMccfrTrainer::average_at_opponent_nodes`]）。
//! 本文件自带 N 人 Kuhn / Leduc（[`MultiPoker`]，规则同 §9 探针）和按公共历史展开的精确最优反应。
//! 多人下 CFR 不保证收敛到 Nash，但这两个小游戏上 NashConv 实测随迭代下降；门槛取新旧写法之间，
//! 旧写法（遍历者节点按自身 reach 累积）的数字是同一测试强制走旧路径实测的。
//!
//! - `multi_poker_rules_sanity`：信息集数（3 人 Kuhn 48、两人 Leduc 288）+ 每个终局零和。
//! - `three_player_average_accumulates_at_non_traverser_nodes`：一次更新后 strategy_sum 里没有
//!   遍历者自己的信息集。
//! - `kuhn_3p_es_mccfr_300k_nash_conv_below_0_03`（默认套件，粗门槛）。
//! - `kuhn_3p_es_mccfr_3m_nash_conv_below_0_01`、`leduc_3p_es_mccfr_3m_mean_nash_conv_below_0_14`
//!   （`--ignored`，release）。

use std::collections::HashMap;

use poker::error::GameVariant;
use poker::training::game::{Game, NodeKind, PlayerId};
use poker::training::{EsMccfrTrainer, Trainer};
use poker::{ChaCha20Rng, RngSource};

/// (actor, private rank, public rank or `NO_PUBLIC`, public action history).
type InfoKey = (u8, u8, u8, Vec<u8>);

const NO_PUBLIC: u8 = u8::MAX;
/// History marker between Leduc's two betting rounds.
const ROUND_SEP: u8 = 9;

/// N-player Kuhn (n + 1 cards, ante 1, one bet of 1) or N-player Leduc (`ranks` × 2 cards, ante 1,
/// raise 2 / 4, at most `max_raises` raises per round, one public card).
#[derive(Clone, Copy)]
struct MultiPoker {
    n: usize,
    leduc: bool,
    ranks: usize,
    max_raises: usize,
}

#[derive(Clone)]
struct St {
    n: usize,
    leduc: bool,
    max_raises: usize,
    used: Vec<bool>,
    private: Vec<usize>,
    public: Option<usize>,
    round: usize,
    hist: Vec<u8>,
    folded: Vec<bool>,
    contrib: Vec<f64>,
    round_in: Vec<f64>,
    level: f64,
    raises: usize,
    need: Vec<bool>,
    last: Option<usize>,
    kuhn_bet: Option<usize>,
}

impl St {
    fn rank(&self, card: usize) -> usize {
        if self.leduc {
            card / 2
        } else {
            card
        }
    }

    fn free_cards(&self) -> Vec<usize> {
        (0..self.used.len()).filter(|&c| !self.used[c]).collect()
    }

    fn next_actor(&self) -> Option<usize> {
        let start = self.last.map_or(0, |l| l + 1);
        (0..self.n)
            .map(|k| (start + k) % self.n)
            .find(|&p| !self.folded[p] && self.need[p])
    }

    fn kind(&self) -> NodeKind {
        if self.private.len() < self.n {
            return NodeKind::Chance;
        }
        if self.leduc {
            if self.folded.iter().filter(|f| !**f).count() == 1 {
                return NodeKind::Terminal;
            }
            match self.next_actor() {
                Some(p) => NodeKind::Player(p as PlayerId),
                None if self.public.is_none() => NodeKind::Chance,
                None => NodeKind::Terminal,
            }
        } else {
            match self.kuhn_bet {
                None if self.hist.len() < self.n => NodeKind::Player(self.hist.len() as PlayerId),
                Some(b) if self.hist.len() - b - 1 < self.n - 1 => {
                    NodeKind::Player(((self.hist.len()) % self.n) as PlayerId)
                }
                _ => NodeKind::Terminal,
            }
        }
    }

    /// 0 = fold (Leduc, facing a bet) / pass-or-fold (Kuhn), 1 = check/call (Leduc) / bet-or-call
    /// (Kuhn), 2 = raise (Leduc).
    fn legal(&self) -> Vec<u8> {
        let NodeKind::Player(p) = self.kind() else {
            return vec![];
        };
        if !self.leduc {
            return vec![0, 1];
        }
        let mut acts = vec![];
        if self.level > self.round_in[p as usize] {
            acts.push(0);
        }
        acts.push(1);
        if self.raises < self.max_raises {
            acts.push(2);
        }
        acts
    }

    fn deal(&mut self, c: usize) {
        assert!(!self.used[c], "card {c} dealt twice");
        self.used[c] = true;
        if self.private.len() < self.n {
            self.private.push(c);
        } else {
            self.public = Some(c);
            self.round = 1;
            self.hist.push(ROUND_SEP);
            self.round_in = vec![0.0; self.n];
            self.level = 0.0;
            self.raises = 0;
            self.need = self.folded.iter().map(|f| !*f).collect();
            self.last = None;
        }
    }

    fn act(&mut self, p: usize, a: u8) {
        self.hist.push(a);
        if self.leduc {
            match a {
                0 => {
                    self.folded[p] = true;
                    self.need[p] = false;
                }
                1 => {
                    let d = self.level - self.round_in[p];
                    self.round_in[p] += d;
                    self.contrib[p] += d;
                    self.need[p] = false;
                }
                _ => {
                    self.level += if self.round == 0 { 2.0 } else { 4.0 };
                    let d = self.level - self.round_in[p];
                    self.round_in[p] += d;
                    self.contrib[p] += d;
                    self.raises += 1;
                    for q in 0..self.n {
                        self.need[q] = q != p && !self.folded[q];
                    }
                }
            }
            self.last = Some(p);
        } else {
            match (self.kuhn_bet, a) {
                (None, 1) => {
                    self.kuhn_bet = Some(self.hist.len() - 1);
                    self.contrib[p] += 1.0;
                }
                (Some(_), 1) => self.contrib[p] += 1.0,
                (Some(_), 0) => self.folded[p] = true,
                _ => {}
            }
        }
    }

    fn strength(&self, p: usize) -> usize {
        let r = self.rank(self.private[p]);
        match self.public {
            Some(pc) if self.rank(pc) == r => 1000 + r,
            _ => r,
        }
    }

    fn payoff(&self, p: usize) -> f64 {
        let pot: f64 = self.contrib.iter().sum();
        let alive: Vec<usize> = (0..self.n).filter(|&q| !self.folded[q]).collect();
        let best = alive.iter().map(|&q| self.strength(q)).max().unwrap();
        let winners = alive.iter().filter(|&&q| self.strength(q) == best).count();
        let won = if !self.folded[p] && self.strength(p) == best {
            pot / winners as f64
        } else {
            0.0
        };
        won - self.contrib[p]
    }

    fn info_key(&self, p: usize) -> InfoKey {
        (
            p as u8,
            self.rank(self.private[p]) as u8,
            self.public.map_or(NO_PUBLIC, |c| self.rank(c) as u8),
            self.hist.clone(),
        )
    }
}

impl MultiPoker {
    fn kuhn(n: usize) -> Self {
        Self {
            n,
            leduc: false,
            ranks: n + 1,
            max_raises: 0,
        }
    }

    fn leduc(n: usize, ranks: usize, max_raises: usize) -> Self {
        Self {
            n,
            leduc: true,
            ranks,
            max_raises,
        }
    }

    fn initial(&self) -> St {
        let n_cards = if self.leduc {
            self.ranks * 2
        } else {
            self.n + 1
        };
        St {
            n: self.n,
            leduc: self.leduc,
            max_raises: self.max_raises,
            used: vec![false; n_cards],
            private: vec![],
            public: None,
            round: 0,
            hist: vec![],
            folded: vec![false; self.n],
            contrib: vec![1.0; self.n],
            round_in: vec![0.0; self.n],
            level: 0.0,
            raises: 0,
            need: vec![true; self.n],
            last: None,
            kuhn_bet: None,
        }
    }
}

impl Game for MultiPoker {
    type State = St;
    type Action = u8;
    type InfoSet = InfoKey;

    const VARIANT: GameVariant = GameVariant::Kuhn;

    fn n_players(&self) -> usize {
        self.n
    }

    fn root(&self, _rng: &mut dyn RngSource) -> St {
        self.initial()
    }

    fn current(state: &St) -> NodeKind {
        state.kind()
    }

    fn info_set(state: &St, actor: PlayerId) -> InfoKey {
        state.info_key(actor as usize)
    }

    fn legal_actions(state: &St) -> Vec<u8> {
        state.legal()
    }

    fn next(mut state: St, action: u8, _rng: &mut dyn RngSource) -> St {
        match state.kind() {
            NodeKind::Chance => state.deal(action as usize),
            NodeKind::Player(p) => state.act(p as usize, action),
            NodeKind::Terminal => panic!("next on terminal"),
        }
        state
    }

    fn chance_distribution(state: &St) -> Vec<(u8, f64)> {
        let free = state.free_cards();
        let p = 1.0 / free.len() as f64;
        free.into_iter().map(|c| (c as u8, p)).collect()
    }

    fn payoff(state: &St, player: PlayerId) -> f64 {
        state.payoff(player as usize)
    }
}

// ---------------------------------------------------------------------------------------------
// Exact NashConv: walk the public tree carrying every private deal consistent with it.
// ---------------------------------------------------------------------------------------------

/// Per-state value for `p` below `states` (all at the same public history; `w[i]` = chance ×
/// other players' reach of state `i`). `br == true`: `p` best-responds (one action per own
/// infoset, chosen by `Σ w·v` over the states in it); otherwise `p` plays `strat` too.
fn walk(
    states: &[St],
    w: &[f64],
    p: usize,
    br: bool,
    strat: &dyn Fn(&InfoKey, usize) -> Vec<f64>,
) -> Vec<f64> {
    match states[0].kind() {
        NodeKind::Terminal => states.iter().map(|s| s.payoff(p)).collect(),
        NodeKind::Chance => {
            // Public card: branch on it; states holding it privately get probability 0.
            let mut v = vec![0.0; states.len()];
            for c in 0..states[0].used.len() {
                let idx: Vec<usize> = (0..states.len()).filter(|&i| !states[i].used[c]).collect();
                if idx.is_empty() {
                    continue;
                }
                let mut kids = Vec::with_capacity(idx.len());
                let mut kw = Vec::with_capacity(idx.len());
                let mut pc = Vec::with_capacity(idx.len());
                for &i in &idx {
                    let prob = 1.0 / states[i].free_cards().len() as f64;
                    let mut s = states[i].clone();
                    s.deal(c);
                    kids.push(s);
                    kw.push(w[i] * prob);
                    pc.push(prob);
                }
                let kv = walk(&kids, &kw, p, br, strat);
                for (k, &i) in idx.iter().enumerate() {
                    v[i] += pc[k] * kv[k];
                }
            }
            v
        }
        NodeKind::Player(actor) => {
            let actor = actor as usize;
            let acts = states[0].legal();
            let sigma: Vec<Vec<f64>> = states
                .iter()
                .map(|s| strat(&s.info_key(actor), acts.len()))
                .collect();
            let child_vals: Vec<Vec<f64>> = acts
                .iter()
                .enumerate()
                .map(|(ai, &a)| {
                    let kids: Vec<St> = states
                        .iter()
                        .map(|s| {
                            let mut s = s.clone();
                            s.act(actor, a);
                            s
                        })
                        .collect();
                    let kw: Vec<f64> = if actor == p {
                        w.to_vec()
                    } else {
                        w.iter().zip(&sigma).map(|(x, sg)| x * sg[ai]).collect()
                    };
                    walk(&kids, &kw, p, br, strat)
                })
                .collect();
            if actor == p && br {
                // Group states by p's private rank (= p's infoset at this public history).
                let mut best: HashMap<usize, usize> = HashMap::new();
                let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
                for (i, s) in states.iter().enumerate() {
                    groups.entry(s.rank(s.private[p])).or_default().push(i);
                }
                for (r, idx) in &groups {
                    let score =
                        |ai: usize| idx.iter().map(|&i| w[i] * child_vals[ai][i]).sum::<f64>();
                    let a_best = (0..acts.len())
                        .max_by(|&x, &y| score(x).total_cmp(&score(y)))
                        .unwrap();
                    best.insert(*r, a_best);
                }
                (0..states.len())
                    .map(|i| child_vals[best[&states[i].rank(states[i].private[p])]][i])
                    .collect()
            } else {
                (0..states.len())
                    .map(|i| {
                        (0..acts.len())
                            .map(|ai| sigma[i][ai] * child_vals[ai][i])
                            .sum()
                    })
                    .collect()
            }
        }
    }
}

/// Every private deal (ordered, without replacement) with its chance probability.
fn private_deals(game: &MultiPoker) -> (Vec<St>, Vec<f64>) {
    let mut out = (vec![], vec![]);
    fn rec(s: St, pc: f64, out: &mut (Vec<St>, Vec<f64>)) {
        if s.kind() != NodeKind::Chance || s.private.len() == s.n {
            out.0.push(s);
            out.1.push(pc);
            return;
        }
        let free = s.free_cards();
        let p = 1.0 / free.len() as f64;
        for c in free {
            let mut k = s.clone();
            k.deal(c);
            rec(k, pc * p, out);
        }
    }
    rec(game.initial(), 1.0, &mut out);
    out
}

/// `Σ_p (best-response value − on-policy value)` of the profile `strat`.
fn nash_conv(game: &MultiPoker, strat: &dyn Fn(&InfoKey, usize) -> Vec<f64>) -> f64 {
    let (states, pc) = private_deals(game);
    (0..game.n)
        .map(|p| {
            let dot = |v: Vec<f64>| v.iter().zip(&pc).map(|(a, b)| a * b).sum::<f64>();
            dot(walk(&states, &pc, p, true, strat)) - dot(walk(&states, &pc, p, false, strat))
        })
        .sum()
}

fn train(game: MultiPoker, seed: u64, updates: u64) -> EsMccfrTrainer<MultiPoker> {
    let mut trainer = EsMccfrTrainer::new(game, seed);
    assert!(trainer.average_at_opponent_nodes());
    let mut rng = ChaCha20Rng::from_seed(seed);
    for _ in 0..updates {
        trainer.step(&mut rng).expect("step");
    }
    trainer
}

fn avg_nash_conv(game: &MultiPoker, trainer: &EsMccfrTrainer<MultiPoker>) -> f64 {
    let strat = |info: &InfoKey, n: usize| {
        let s = trainer.average_strategy(info);
        if s.len() == n {
            s
        } else {
            vec![1.0 / n as f64; n]
        }
    };
    nash_conv(game, &strat)
}

/// Rules sanity: infoset count (3-player Kuhn 48, as in OpenSpiel / §9 probe) and a zero-sum
/// payoff vector at every terminal.
#[test]
fn multi_poker_rules_sanity() {
    fn infosets(s: &St, seen: &mut std::collections::HashSet<InfoKey>) {
        match s.kind() {
            NodeKind::Terminal => {
                let total: f64 = (0..s.n).map(|p| s.payoff(p)).sum();
                assert!(total.abs() < 1e-12, "zero-sum: {total}");
            }
            NodeKind::Chance => {
                for c in s.free_cards() {
                    let mut k = s.clone();
                    k.deal(c);
                    infosets(&k, seen);
                }
            }
            NodeKind::Player(p) => {
                seen.insert(s.info_key(p as usize));
                for a in s.legal() {
                    let mut k = s.clone();
                    k.act(p as usize, a);
                    infosets(&k, seen);
                }
            }
        }
    }
    let mut seen = Default::default();
    infosets(&MultiPoker::kuhn(3).initial(), &mut seen);
    assert_eq!(seen.len(), 48, "3-player Kuhn infosets");
    let mut seen = Default::default();
    infosets(&MultiPoker::leduc(2, 3, 2).initial(), &mut seen);
    assert_eq!(seen.len(), 288, "2-player Leduc infosets");
}

/// n = 3 → average strategy at non-traverser nodes: after one update (seat 0 traverses) no
/// strategy_sum entry belongs to seat 0, and some belong to each of seats 1 and 2.
#[test]
fn three_player_average_accumulates_at_non_traverser_nodes() {
    let trainer = train(MultiPoker::kuhn(3), 0x3131, 1);
    let actors: Vec<u8> = trainer.strategy_sum().inner().keys().map(|k| k.0).collect();
    assert!(
        !actors.contains(&0),
        "座 0 当遍历者时不应累积自己的平均策略"
    );
    assert!(actors.contains(&1) && actors.contains(&2), "{actors:?}");
}

const SEEDS: [u64; 5] = [0x11, 0x22, 0x33, 0x44, 0x55];

/// Coarse smoke gate (does not separate old and new paths at this length: old 0.0076–0.0190, new
/// 0.0075–0.0153 over [`SEEDS`]); catches an average strategy that is never accumulated.
#[test]
fn kuhn_3p_es_mccfr_300k_nash_conv_below_0_03() {
    let game = MultiPoker::kuhn(3);
    let nc = avg_nash_conv(&game, &train(game, 0x33, 300_000));
    eprintln!("3-player Kuhn ES-MCCFR 300k seed 0x33: NashConv = {nc:.5}");
    assert!(nc < 0.03, "NashConv {nc:.5} ≥ 0.03");
}

/// 3M updates: new path 0.0026–0.0068 over [`SEEDS`]; old path 0.0072–0.110 (3 of 5 ≥ 0.01).
#[test]
#[ignore = "release/--ignored opt-in（5 seeds × 3M 3-player Kuhn ES-MCCFR + 精确 NashConv）"]
fn kuhn_3p_es_mccfr_3m_nash_conv_below_0_01() {
    let game = MultiPoker::kuhn(3);
    let nc: Vec<f64> = SEEDS
        .iter()
        .map(|&s| avg_nash_conv(&game, &train(game, s, 3_000_000)))
        .collect();
    eprintln!("3-player Kuhn ES-MCCFR 3M seeds {SEEDS:x?}: NashConv = {nc:.5?}");
    for (s, v) in SEEDS.iter().zip(&nc) {
        assert!(*v < 0.01, "seed {s:#x}: NashConv {v:.5} ≥ 0.01");
    }
}

/// 3-player Leduc (3 ranks, 1 raise per round), 3M updates: new path mean 0.110 (0.089–0.153)
/// over [`SEEDS`]; old path mean 0.180 (0.134–0.231). Seeds overlap, so the gate is on the mean.
#[test]
#[ignore = "release/--ignored opt-in（5 seeds × 3M 3-player Leduc ES-MCCFR + 精确 NashConv）"]
fn leduc_3p_es_mccfr_3m_mean_nash_conv_below_0_14() {
    let game = MultiPoker::leduc(3, 3, 1);
    let nc: Vec<f64> = SEEDS
        .iter()
        .map(|&s| avg_nash_conv(&game, &train(game, s, 3_000_000)))
        .collect();
    let mean = nc.iter().sum::<f64>() / nc.len() as f64;
    eprintln!("3-player Leduc ES-MCCFR 3M seeds {SEEDS:x?}: NashConv = {nc:.5?}, mean {mean:.5}");
    assert!(mean < 0.14, "mean NashConv {mean:.5} ≥ 0.14");
}
