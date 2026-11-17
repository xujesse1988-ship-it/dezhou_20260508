//! The Rust side of the pool model must name every decision exactly as the Python side that
//! fitted the frequencies (`opponent_analysis/pool_model.py`), and answer every query with the
//! same numbers. The vectors are real betting sequences exported by `tools/pool_model_vectors.py`
//! (no names, cards or hand ids): 125 hands with 1096 preflop decisions in 87 cells, and the 88 of
//! them that saw a flop with 615 postflop decisions in 69 cells.

use std::collections::BTreeMap;
use std::path::Path;

use poker::training::pool_model::{
    position_labels, PoolModel, PoolSpot, PostflopTracker, PreflopKind, PreflopTracker,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct Act {
    seat: usize,
    fold: bool,
    to: u64,
    family: String,
    dims: BTreeMap<String, String>,
    kind: String,
    frac: Option<f64>,
}

#[derive(Deserialize)]
struct Hand {
    labels: Vec<String>,
    contrib: Vec<u64>,
    pot: u64,
    actions: Vec<Act>,
}

#[derive(Deserialize)]
struct Pred {
    family: String,
    dims: BTreeMap<String, String>,
    probs: BTreeMap<String, f64>,
    n: u64,
    sizes: BTreeMap<String, f64>,
    size_n: u64,
}

#[derive(Deserialize)]
struct Vectors {
    hands: Vec<Hand>,
    predictions: Vec<Pred>,
}

fn load() -> Vectors {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/pool_model_preflop_vectors.json");
    serde_json::from_str(&std::fs::read_to_string(path).expect("vectors file"))
        .expect("vectors json")
}

#[derive(Deserialize)]
struct PreAct {
    seat: usize,
    fold: bool,
    raise: bool,
    all_in: bool,
}

#[derive(Deserialize)]
struct PostAct {
    street: String,
    seat: usize,
    fold: bool,
    to: u64,
    all_in: bool,
    /// Pot before the action.
    pot: u64,
    family: String,
    dims: BTreeMap<String, String>,
    kind: String,
    frac: Option<f64>,
}

#[derive(Deserialize)]
struct PostHand {
    n: usize,
    preflop: Vec<PreAct>,
    actions: Vec<PostAct>,
}

#[derive(Deserialize)]
struct PostVectors {
    hands: Vec<PostHand>,
    predictions: Vec<Pred>,
}

fn load_postflop() -> PostVectors {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/pool_model_postflop_vectors.json");
    serde_json::from_str(&std::fs::read_to_string(path).expect("vectors file"))
        .expect("vectors json")
}

fn dims_of(spot: &PoolSpot) -> BTreeMap<String, String> {
    spot.dims
        .iter()
        .map(|(d, v)| (d.to_string(), v.to_string()))
        .collect()
}

#[test]
fn preflop_situations_match_python() {
    let v = load();
    assert!(v.hands.len() >= 100);
    let mut decisions = 0usize;
    let mut families: BTreeMap<String, usize> = BTreeMap::new();
    for (hi, h) in v.hands.iter().enumerate() {
        let labels = position_labels(h.labels.len());
        assert_eq!(labels, h.labels, "hand {hi}: position labels");
        let mut t = PreflopTracker::new(labels, h.contrib.clone(), h.pot);
        for (ai, a) in h.actions.iter().enumerate() {
            let spot = t.spot(a.seat);
            assert_eq!(spot.family, a.family, "hand {hi} action {ai}: family");
            assert_eq!(dims_of(&spot), a.dims, "hand {hi} action {ai}: features");
            let kind = if a.fold {
                t.fold(a.seat)
            } else {
                t.put(a.seat, a.to)
            };
            match (kind, a.kind.as_str(), a.frac) {
                (PreflopKind::Fold, "fold", None)
                | (PreflopKind::Check, "check", None)
                | (PreflopKind::Call, "call", None) => {}
                (PreflopKind::Raise(f), "raise", Some(want)) => {
                    assert!(
                        (f - want).abs() < 1e-12,
                        "hand {hi} action {ai}: {f} vs {want}"
                    )
                }
                other => panic!("hand {hi} action {ai}: {other:?}"),
            }
            decisions += 1;
            *families.entry(a.family.clone()).or_default() += 1;
        }
    }
    assert!(decisions >= 1000, "{decisions} decisions");
    // Every preflop situation of the model occurs in the vectors.
    assert_eq!(families.len(), 8, "{families:?}");
}

#[test]
fn postflop_situations_match_python() {
    let v = load_postflop();
    assert!(v.hands.len() >= 80);
    let mut decisions = 0usize;
    let mut families: BTreeMap<String, usize> = BTreeMap::new();
    for (hi, h) in v.hands.iter().enumerate() {
        // Who folded / went all-in / raised last before the flop.
        let (mut folded, mut all_in, mut aggr) = (0u16, 0u16, None);
        for a in &h.preflop {
            if a.fold {
                folded |= 1 << a.seat;
            }
            if a.all_in {
                all_in |= 1 << a.seat;
            }
            if a.raise {
                aggr = Some(a.seat);
            }
        }
        let mut tracker: Option<PostflopTracker> = None;
        let mut street = String::new();
        for (ai, a) in h.actions.iter().enumerate() {
            if a.street != street {
                street = a.street.clone();
                match tracker.as_mut() {
                    None => {
                        let idx = match street.as_str() {
                            "flop" => 1,
                            "turn" => 2,
                            _ => 3,
                        };
                        assert_eq!(idx, 1, "hand {hi}: first postflop street");
                        tracker = Some(PostflopTracker::start(
                            h.n, idx, a.pot, folded, all_in, aggr,
                        ));
                    }
                    Some(t) => t.next_street(a.pot, folded, all_in),
                }
            }
            let t = tracker.as_mut().expect("tracker");
            let spot = t.spot(a.seat);
            assert_eq!(spot.family, a.family, "hand {hi} action {ai}: family");
            assert_eq!(dims_of(&spot), a.dims, "hand {hi} action {ai}: features");
            let kind = if a.fold {
                folded |= 1 << a.seat;
                t.fold(a.seat)
            } else {
                t.put(a.seat, a.to)
            };
            if a.all_in {
                all_in |= 1 << a.seat;
            }
            match (kind, a.kind.as_str(), a.frac) {
                (PreflopKind::Fold, "fold", None)
                | (PreflopKind::Check, "check", None)
                | (PreflopKind::Call, "call", None) => {}
                (PreflopKind::Raise(f), "bet" | "raise", Some(want)) => {
                    assert!(
                        (f - want).abs() < 1e-12,
                        "hand {hi} action {ai}: {f} vs {want}"
                    )
                }
                other => panic!("hand {hi} action {ai}: {other:?}"),
            }
            decisions += 1;
            *families.entry(a.family.clone()).or_default() += 1;
        }
    }
    assert!(decisions >= 600, "{decisions} decisions");
    // Every postflop situation of the model occurs in the vectors.
    assert_eq!(families.len(), 3, "{families:?}");
}

#[test]
fn predictions_match_python() {
    let v = load();
    let model = PoolModel::load(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/pool_model_small.json"),
    )
    .expect("model");
    assert!((model.k() - 10.0).abs() < 1e-12);
    assert!(v.predictions.len() >= 50);
    let close = |got: &BTreeMap<String, f64>, want: &BTreeMap<String, f64>, what: &str| {
        assert_eq!(
            got.keys().collect::<Vec<_>>(),
            want.keys().collect::<Vec<_>>(),
            "{what}"
        );
        for (k, g) in got {
            assert!(
                (g - want[k]).abs() < 1e-12,
                "{what} {k}: {g} vs {}",
                want[k]
            );
        }
    };
    // Rust needs 'static feature names and values: leak the handful of distinct strings.
    let leak = |s: &str| -> &'static str { Box::leak(s.to_string().into_boxed_str()) };
    let post = load_postflop();
    assert!(post.predictions.len() >= 50);
    for p in v.predictions.iter().chain(&post.predictions) {
        let spot = PoolSpot {
            family: leak(&p.family),
            dims: p.dims.iter().map(|(d, x)| (leak(d), leak(x))).collect(),
        };
        let got = model.predict(&spot).expect("cell");
        let what = format!("{} {:?}", p.family, p.dims);
        assert_eq!(got.n, p.n, "{what}");
        assert_eq!(got.size_n, p.size_n, "{what}");
        close(&got.probs, &p.probs, &what);
        close(&got.sizes, &p.sizes, &what);
        if !got.probs.is_empty() {
            assert!(
                (got.probs.values().sum::<f64>() - 1.0).abs() < 1e-9,
                "{what}"
            );
        }
    }
}
