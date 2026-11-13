//! Pool model on the solver side: the Rust reader of `opponent_analysis/pool_model.py`.
//!
//! The Python package fits how the opponents of one platform play on average (frequencies with
//! counts, per situation) from hand logs and writes `pool_model.json`. This module loads that file,
//! answers the same queries with the same back-off arithmetic ([`PoolModel::predict`] mirrors
//! `pool_model.predict`), and classifies a preflop decision into the model's situation and cell
//! ([`PreflopTracker`] mirrors the preflop half of `pool_model.hand_obs`).
//!
//! The two sides must agree decision by decision, otherwise frequencies measured in one situation
//! get applied in another. `tests/pool_model_vectors.rs` replays real preflop action sequences
//! exported by `tools/pool_model_vectors.py` through [`PreflopTracker`] and compares every
//! situation, cell and prediction with what Python produced.
//!
//! Sizes are pot fractions, the unit of the betting tree:
//! (chips raised above the call) / (pot before the action + the call).

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

/// `pool_model.VERSION` this reader understands.
pub const POOL_MODEL_VERSION: u64 = 1;

/// Upper edges of the size buckets (`pool_model.SIZE_EDGES`).
pub const SIZE_EDGES: [f64; 5] = [0.42, 0.62, 0.87, 1.2, 2.0];
/// Labels of the size buckets (`pool_model.SIZE_LABELS`); a bet / raise that puts the player
/// all-in is counted under [`SIZE_ALLIN`] instead.
pub const SIZE_LABELS: [&str; 6] = [
    "<=0.42",
    "0.42-0.62",
    "0.62-0.87",
    "0.87-1.2",
    "1.2-2.0",
    ">2.0",
];
pub const SIZE_ALLIN: &str = "allin";

/// Size bucket of a pot fraction (`pool_model.size_bucket`).
pub fn size_bucket(frac: f64) -> &'static str {
    for (edge, label) in SIZE_EDGES.iter().zip(SIZE_LABELS) {
        if frac <= *edge {
            return label;
        }
    }
    SIZE_LABELS[SIZE_LABELS.len() - 1]
}

/// Pot fraction of a bet / raise (`pool_model.raise_fraction`).
pub fn raise_fraction(to_total: u64, current_bet: u64, pot_before: u64, to_call: u64) -> f64 {
    let base = pot_before + to_call;
    if base == 0 {
        0.0
    } else {
        (to_total - current_bet) as f64 / base as f64
    }
}

#[derive(Deserialize, Debug, Clone)]
struct FamilyJson {
    dims: Vec<String>,
    cells: BTreeMap<String, BTreeMap<String, u64>>,
    size_dims: Vec<String>,
    size_cells: BTreeMap<String, BTreeMap<String, u64>>,
}

#[derive(Deserialize, Debug, Clone)]
struct ModelJson {
    version: u64,
    k: f64,
    size_edges: Vec<f64>,
    families: BTreeMap<String, FamilyJson>,
}

/// One answer of the model: what the pool does in a cell.
#[derive(Debug, Clone, PartialEq)]
pub struct PoolPrediction {
    /// Outcome (`fold` / `call` / `raise`, or `check` / `raise` for the option, `check` / `bet`
    /// postflop) -> probability. Empty when the family has no observations at all.
    pub probs: BTreeMap<String, f64>,
    /// Observations in the finest cell asked for (0 = answered entirely by coarser cells).
    pub n: u64,
    /// Size label -> probability, given a bet / raise.
    pub sizes: BTreeMap<String, f64>,
    pub size_n: u64,
}

/// Loaded `pool_model.json`.
#[derive(Debug, Clone)]
pub struct PoolModel {
    k: f64,
    families: BTreeMap<String, FamilyJson>,
}

impl PoolModel {
    pub fn from_json(text: &str) -> Result<PoolModel, String> {
        let m: ModelJson = serde_json::from_str(text).map_err(|e| format!("pool model: {e}"))?;
        if m.version != POOL_MODEL_VERSION {
            return Err(format!(
                "pool model version {} != {POOL_MODEL_VERSION}",
                m.version
            ));
        }
        if m.size_edges != SIZE_EDGES {
            return Err(format!(
                "pool model size edges {:?} != {SIZE_EDGES:?}",
                m.size_edges
            ));
        }
        if !(m.k.is_finite() && m.k > 0.0) {
            return Err(format!("pool model k {} must be positive", m.k));
        }
        Ok(PoolModel {
            k: m.k,
            families: m.families,
        })
    }

    pub fn load(path: &Path) -> Result<PoolModel, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("pool model {}: {e}", path.display()))?;
        Self::from_json(&text)
    }

    /// Back-off strength the file was written with.
    pub fn k(&self) -> f64 {
        self.k
    }

    /// Action and size probabilities of one cell, backed off from fine to coarse exactly as
    /// `pool_model.predict`: with the first j dimensions fixed,
    /// `p_j(a) = (count_j(a) + k * p_{j-1}(a)) / (n_j + k)`, `p_0` = family totals.
    /// `dims` must hold every dimension the family (and its size table) is split on.
    pub fn predict(&self, spot: &PoolSpot) -> Result<PoolPrediction, String> {
        let fam = self
            .families
            .get(spot.family)
            .ok_or_else(|| format!("pool model has no family {}", spot.family))?;
        let (probs, n) = smooth(&fam.cells, &fam.dims, spot, self.k)?;
        let (sizes, size_n) = smooth(&fam.size_cells, &fam.size_dims, spot, self.k)?;
        Ok(PoolPrediction {
            probs,
            n,
            sizes,
            size_n,
        })
    }
}

fn smooth(
    cells: &BTreeMap<String, BTreeMap<String, u64>>,
    names: &[String],
    spot: &PoolSpot,
    k: f64,
) -> Result<(BTreeMap<String, f64>, u64), String> {
    let empty = BTreeMap::new();
    let root = cells.get("").unwrap_or(&empty);
    let total: u64 = root.values().sum();
    if total == 0 {
        return Ok((BTreeMap::new(), 0));
    }
    let mut probs: BTreeMap<String, f64> = root
        .iter()
        .map(|(a, c)| (a.clone(), *c as f64 / total as f64))
        .collect();
    let mut n = total;
    let mut key = String::new();
    for (j, name) in names.iter().enumerate() {
        let value = spot
            .dim(name)
            .ok_or_else(|| format!("{}: dimension {name} missing", spot.family))?;
        if j > 0 {
            key.push('|');
        }
        key.push_str(value);
        let cell = cells.get(&key).unwrap_or(&empty);
        n = cell.values().sum();
        for (a, p) in probs.iter_mut() {
            *p = (cell.get(a).copied().unwrap_or(0) as f64 + k * *p) / (n as f64 + k);
        }
    }
    Ok((probs, n))
}

/// A decision's place in the model: situation ("family") plus the features cells are split on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolSpot {
    pub family: &'static str,
    /// (dimension, value); holds every feature Python records for the family, used or not.
    pub dims: Vec<(&'static str, &'static str)>,
}

impl PoolSpot {
    pub fn dim(&self, name: &str) -> Option<&'static str> {
        self.dims.iter().find(|(d, _)| *d == name).map(|(_, v)| *v)
    }
}

/// Position labels as `opponent_analysis.replay.assign_positions` gives them, for seats listed
/// clockwise starting at the button. Three or more seats (heads-up is labelled differently per
/// platform and is not part of the pool model's data).
pub fn position_labels(n: usize) -> Vec<&'static str> {
    assert!(
        (3..=10).contains(&n),
        "position_labels: 3..=10 seats, got {n}"
    );
    const UTG_N: [&str; 5] = ["UTG", "UTG1", "UTG2", "UTG3", "UTG4"];
    let mut out = vec!["BTN", "SB", "BB"];
    let k = n - 3;
    for i in 0..k {
        let dist = k - i; // 1 = the seat right of the button
        out.push(if dist == 1 {
            "CO"
        } else if i == 0 {
            "UTG"
        } else if dist == 2 {
            "HJ"
        } else if dist == 3 {
            "LJ"
        } else {
            UTG_N.get(i).copied().expect("at most 10 seats")
        });
    }
    out
}

/// `pool_model._pos_group`.
fn pos_group(label: &str) -> &'static str {
    match label {
        "BTN" | "BTN_SB" => "BTN",
        "SB" => "SB",
        "BB" | "BTN_BB" => "BB",
        "CO" => "CO",
        "HJ" | "LJ" => "MP",
        _ => "EP",
    }
}

/// `pool_model._seat3`.
fn seat3(label: &str) -> &'static str {
    match label {
        "SB" | "BTN_SB" => "SB",
        "BB" | "BTN_BB" => "BB",
        _ => "field",
    }
}

/// What one preflop action did, as [`PreflopTracker::act`] classified it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PreflopKind {
    Fold,
    Check,
    Call,
    /// Bet / raise with its pot fraction.
    Raise(f64),
}

/// Follows the preflop betting of one hand and names the situation of the seat about to act
/// (the preflop half of `pool_model.hand_obs`). Seats are indices into the list given to
/// [`PreflopTracker::new`]; chips are in any one unit.
#[derive(Debug, Clone)]
pub struct PreflopTracker {
    labels: Vec<&'static str>,
    contrib: Vec<u64>,
    pot: u64,
    current_bet: u64,
    raises: u32,
    callers_since_raise: u32,
    acted: Vec<bool>,
    limped_pot: bool,
    raisers: Vec<usize>,
    last_frac: f64,
}

impl PreflopTracker {
    /// `labels[i]` = position label of seat i; `contrib[i]` = what seat i has in for this street
    /// after the blinds (and any extra live blind) are posted; `pot` = all chips in the middle
    /// including antes.
    pub fn new(labels: Vec<&'static str>, contrib: Vec<u64>, pot: u64) -> PreflopTracker {
        assert_eq!(labels.len(), contrib.len());
        let n = labels.len();
        PreflopTracker {
            labels,
            current_bet: contrib.iter().copied().max().unwrap_or(0),
            contrib,
            pot,
            raises: 0,
            callers_since_raise: 0,
            acted: vec![false; n],
            limped_pot: false,
            raisers: Vec::new(),
            last_frac: 0.0,
        }
    }

    /// Situation and features of `seat`, who is about to act.
    pub fn spot(&self, seat: usize) -> PoolSpot {
        let pos = self.labels[seat];
        let to_call = self.current_bet - self.contrib[seat];
        let callers = if self.callers_since_raise == 0 {
            "0"
        } else {
            "1+"
        };
        let size = size_bucket(self.last_frac);
        let yes_no = |b: bool| if b { "yes" } else { "no" };
        let (family, dims) = match self.raises {
            0 if to_call == 0 => (
                "pf_option",
                vec![(
                    "limpers",
                    if self.callers_since_raise <= 1 {
                        "0-1"
                    } else {
                        "2+"
                    },
                )],
            ),
            0 if self.callers_since_raise == 0 => ("pf_unopened", vec![("pos", pos_group(pos))]),
            0 => (
                "pf_vs_limp",
                vec![
                    ("pos", pos_group(pos)),
                    (
                        "limpers",
                        if self.callers_since_raise == 1 {
                            "1"
                        } else {
                            "2+"
                        },
                    ),
                ],
            ),
            1 if self.acted[seat] => (
                "pf_limper_vs_raise",
                vec![("size", size), ("callers", callers)],
            ),
            1 => (
                "pf_vs_raise",
                vec![
                    ("size", size),
                    ("seat", seat3(pos)),
                    ("callers", callers),
                    ("raise_kind", if self.limped_pot { "iso" } else { "open" }),
                ],
            ),
            2 if self.raisers.first() == Some(&seat) => (
                "pf_raiser_vs_3bet",
                vec![("size", size), ("callers", callers)],
            ),
            2 => (
                "pf_other_vs_3bet",
                vec![("entered", yes_no(self.acted[seat]))],
            ),
            _ => (
                "pf_vs_4bet",
                vec![("raised", yes_no(self.raisers.contains(&seat)))],
            ),
        };
        PoolSpot { family, dims }
    }

    /// Pot fraction of `seat` raising to the street total `to_total`; `None` when that total does
    /// not exceed the current bet (a call, a check, or an all-in for less).
    pub fn fraction_if_raise(&self, seat: usize, to_total: u64) -> Option<f64> {
        (to_total > self.current_bet).then(|| {
            raise_fraction(
                to_total,
                self.current_bet,
                self.pot,
                self.current_bet - self.contrib[seat],
            )
        })
    }

    /// Record `seat` folding.
    pub fn fold(&mut self, seat: usize) -> PreflopKind {
        self.acted[seat] = true;
        PreflopKind::Fold
    }

    /// Record `seat` putting chips in until its street total is `to_total` (unchanged total =
    /// check). Above the current bet it is a raise, whatever its size; at or below, a call.
    pub fn put(&mut self, seat: usize, to_total: u64) -> PreflopKind {
        let before = self.contrib[seat];
        assert!(to_total >= before, "street total cannot go down");
        let to_call = self.current_bet - before;
        let kind = if to_total > self.current_bet {
            let frac = raise_fraction(to_total, self.current_bet, self.pot, to_call);
            self.current_bet = to_total;
            self.raises += 1;
            self.callers_since_raise = 0;
            self.raisers.push(seat);
            self.last_frac = frac;
            PreflopKind::Raise(frac)
        } else if to_total > before {
            self.callers_since_raise += 1;
            if self.raises == 0 {
                self.limped_pot = true;
            }
            PreflopKind::Call
        } else {
            PreflopKind::Check
        };
        self.pot += to_total - before;
        self.contrib[seat] = to_total;
        self.acted[seat] = true;
        kind
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_buckets_and_fraction() {
        let got: Vec<&str> = [0.33, 0.42, 0.5, 0.75, 1.0, 1.5, 3.0]
            .iter()
            .map(|x| size_bucket(*x))
            .collect();
        assert_eq!(
            got,
            vec![
                "<=0.42",
                "<=0.42",
                "0.42-0.62",
                "0.62-0.87",
                "0.87-1.2",
                "1.2-2.0",
                ">2.0"
            ]
        );
        // 8 seats, 1 BB ante, blinds 50 / 100: pot 950 before the first action.
        assert!((raise_fraction(450, 100, 950, 100) - 1.0 / 3.0).abs() < 1e-12);
        assert!((raise_fraction(1150, 100, 950, 100) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn labels_match_python_for_common_table_sizes() {
        assert_eq!(position_labels(3), vec!["BTN", "SB", "BB"]);
        assert_eq!(
            position_labels(6),
            vec!["BTN", "SB", "BB", "UTG", "HJ", "CO"]
        );
        assert_eq!(
            position_labels(8),
            vec!["BTN", "SB", "BB", "UTG", "UTG1", "LJ", "HJ", "CO"]
        );
        assert_eq!(
            position_labels(9),
            vec!["BTN", "SB", "BB", "UTG", "UTG1", "UTG2", "LJ", "HJ", "CO"]
        );
    }

    /// 6 seats (button 0), blinds 10 / 20, no ante. Seat 3 limps, seat 4 raises to 100, seats 5 / 0 /
    /// 1 fold, seat 2 calls, seat 3 raises to 400, seat 4 folds... each seat's situation on the way.
    #[test]
    fn tracker_names_each_situation() {
        let mut t = PreflopTracker::new(position_labels(6), vec![0, 10, 20, 0, 0, 0], 30);
        assert_eq!(t.spot(3).family, "pf_unopened");
        assert_eq!(t.spot(3).dim("pos"), Some("EP"));
        assert_eq!(t.put(3, 20), PreflopKind::Call);
        let s = t.spot(4);
        assert_eq!(
            (s.family, s.dim("pos"), s.dim("limpers")),
            ("pf_vs_limp", Some("MP"), Some("1"))
        );
        // Raise to 100 over a 20 limp: 80 above the call, pot 50 + the 20 call.
        match t.put(4, 100) {
            PreflopKind::Raise(f) => assert!((f - 80.0 / 70.0).abs() < 1e-12),
            k => panic!("{k:?}"),
        }
        let s = t.spot(5);
        assert_eq!(s.family, "pf_vs_raise");
        assert_eq!(
            (
                s.dim("size"),
                s.dim("seat"),
                s.dim("callers"),
                s.dim("raise_kind")
            ),
            (Some("0.87-1.2"), Some("field"), Some("0"), Some("iso"))
        );
        t.fold(5);
        t.fold(0);
        assert_eq!(t.spot(1).dim("seat"), Some("SB"));
        t.fold(1);
        assert_eq!(t.spot(2).dim("seat"), Some("BB"));
        assert_eq!(t.put(2, 100), PreflopKind::Call);
        let s = t.spot(3);
        assert_eq!(
            (s.family, s.dim("callers")),
            ("pf_limper_vs_raise", Some("1+"))
        );
        assert!(matches!(t.put(3, 400), PreflopKind::Raise(_)));
        // Seat 4 made the first raise and now faces the second.
        assert_eq!(t.spot(4).family, "pf_raiser_vs_3bet");
        assert!(matches!(t.put(4, 1200), PreflopKind::Raise(_)));
        // Seat 2 called the first raise, never raised; seat 3 raised.
        let s = t.spot(2);
        assert_eq!((s.family, s.dim("raised")), ("pf_vs_4bet", Some("no")));
        t.fold(2);
        assert_eq!(t.spot(3).dim("raised"), Some("yes"));
    }

    #[test]
    fn option_and_three_bet_roles() {
        // Small blind completes, big blind has the option.
        let mut t = PreflopTracker::new(position_labels(4), vec![0, 10, 20, 0], 30);
        t.fold(3);
        t.fold(0);
        assert_eq!(t.spot(1).family, "pf_unopened");
        assert_eq!(t.put(1, 20), PreflopKind::Call);
        let s = t.spot(2);
        assert_eq!((s.family, s.dim("limpers")), ("pf_option", Some("0-1")));
        assert_eq!(t.put(2, 20), PreflopKind::Check);

        // Open, cold seat facing a 3bet vs a caller facing it.
        let mut t = PreflopTracker::new(position_labels(6), vec![0, 10, 20, 0, 0, 0], 30);
        t.put(3, 60);
        t.put(4, 60);
        t.put(5, 200);
        let s = t.spot(0);
        assert_eq!(
            (s.family, s.dim("entered")),
            ("pf_other_vs_3bet", Some("no"))
        );
        t.fold(0);
        t.fold(1);
        t.fold(2);
        assert_eq!(t.spot(3).family, "pf_raiser_vs_3bet");
        t.fold(3);
        let s = t.spot(4);
        assert_eq!(
            (s.family, s.dim("entered")),
            ("pf_other_vs_3bet", Some("yes"))
        );
    }

    #[test]
    fn predict_backs_off_like_python() {
        // Same numbers as opponent_analysis/tests/test_pool_model.py FitPredict.
        let json = r#"{"version":1,"k":10.0,"size_edges":[0.42,0.62,0.87,1.2,2.0],"families":{
          "pf_vs_raise":{"dims":["seat","size"],"size_dims":["seat"],
            "cells":{"":{"fold":31,"call":6,"raise":3},
                     "BB":{"fold":11,"call":6,"raise":3},"SB":{"fold":20},
                     "BB|<=0.42":{"fold":2,"call":6},"BB|0.87-1.2":{"fold":9,"raise":3},
                     "SB|0.87-1.2":{"fold":20}},
            "size_cells":{"":{"0.87-1.2":3},"BB":{"0.87-1.2":3}}}}}"#;
        let m = PoolModel::from_json(json).expect("valid");
        let spot = |seat: &'static str, size: &'static str| PoolSpot {
            family: "pf_vs_raise",
            dims: vec![("seat", seat), ("size", size)],
        };
        let p = m.predict(&spot("BB", "<=0.42")).expect("cell");
        let root = [31.0 / 40.0, 6.0 / 40.0, 3.0 / 40.0];
        let bb = [
            (11.0 + 10.0 * root[0]) / 30.0,
            (6.0 + 10.0 * root[1]) / 30.0,
            (3.0 + 10.0 * root[2]) / 30.0,
        ];
        assert_eq!(p.n, 8);
        assert!((p.probs["fold"] - (2.0 + 10.0 * bb[0]) / 18.0).abs() < 1e-12);
        assert!((p.probs["call"] - (6.0 + 10.0 * bb[1]) / 18.0).abs() < 1e-12);
        assert!((p.probs["raise"] - 10.0 * bb[2] / 18.0).abs() < 1e-12);
        assert!((p.probs.values().sum::<f64>() - 1.0).abs() < 1e-12);
        assert!((p.sizes["0.87-1.2"] - 1.0).abs() < 1e-12);
        // A size nobody was seen facing in this seat: the seat's own frequencies, n = 0.
        let unseen = m.predict(&spot("SB", ">2.0")).expect("cell");
        assert_eq!(unseen.n, 0);
        assert!((unseen.probs["fold"] - (20.0 + 10.0 * root[0]) / 30.0).abs() < 1e-12);

        assert!(PoolModel::from_json(&json.replace("\"version\":1", "\"version\":2")).is_err());
        assert!(PoolModel::from_json(&json.replace("0.42,0.62", "0.4,0.62")).is_err());
        assert!(m
            .predict(&PoolSpot {
                family: "pf_vs_raise",
                dims: vec![("seat", "BB")],
            })
            .is_err());
    }
}
