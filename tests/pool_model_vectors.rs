//! The Rust side of the pool model must name every preflop decision exactly as the Python side
//! that fitted the frequencies (`opponent_analysis/pool_model.py`), and answer every query with
//! the same numbers. The vectors are real preflop betting sequences exported by
//! `tools/pool_model_vectors.py` (no names, cards or hand ids): 125 hands, 1096 decisions, 87 cells.

use std::collections::BTreeMap;
use std::path::Path;

use poker::training::pool_model::{
    position_labels, PoolModel, PoolSpot, PreflopKind, PreflopTracker,
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
    for p in &v.predictions {
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
