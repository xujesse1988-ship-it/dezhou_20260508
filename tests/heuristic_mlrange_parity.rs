//! M3 Rust 前向：端到端 ML range 预测的跨语言字节级对拍
//! （docs/opponent_range_ml_design.md M3 / §6 神经残差侧）。
//!
//! 完整链路 = Rust 因果 m_prior（[`RangeModel::causal_logpriors`]）+ Rust 手写 GRU 残差
//! 前向（[`mlrange::GruForward`]）+ 牌面移除（[`mlrange::predict`]，rscale=0.8）。对每个街末
//! 快照复算 `pred`，比对 golden（Python `range_shadow` 前向，rscale=0.8）：
//!   - `max|pred_rust − pred_golden| < 1e-4`；
//!   - top-5 类（按 (−p, idx) 破平）逐街一致。
//!
//! golden / 权重缺席（无 venv / CI）→ 打印并 return（不 fail），镜像 optional-test 约定。

use std::path::PathBuf;

use poker::heuristic::equity::EquityEngine;
use poker::heuristic::mlrange::{self, GruForward};
use poker::heuristic::range::{RangeModel, Sample};

#[derive(serde::Deserialize)]
struct StrengthFile {
    strength: Vec<f64>,
}

#[derive(serde::Deserialize)]
struct Golden {
    hands: Vec<HandRec>,
}

#[derive(serde::Deserialize)]
struct HandRec {
    s: Sample,
    #[allow(dead_code)]
    y: i64,
    fdim: usize,
    /// token 特征 flat `T*fdim` row-major f64。
    feat: Vec<f64>,
    #[serde(default)]
    streets: std::collections::BTreeMap<String, StreetRec>,
}

#[derive(serde::Deserialize)]
struct StreetRec {
    snap: i64,
    alive: Vec<i64>,
    /// Python ML 预测（rscale=0.8）= 对拍目标。
    pred: Vec<f64>,
}

/// 失败快照记录：(max|Δpred|, hand_idx, street, rust_top5, golden_top5)。
type WorstRec = (f64, usize, usize, [usize; 5], [usize; 5]);

/// top-5 类索引（p 降序，(-p, idx) 破平；等价 np.lexsort((arange, -p))[:5]）。
fn top5(p: &[f64]) -> [usize; 5] {
    let mut idx: Vec<usize> = (0..p.len()).collect();
    idx.sort_by(|&a, &b| match p[b].partial_cmp(&p[a]).unwrap() {
        std::cmp::Ordering::Equal => a.cmp(&b),
        o => o,
    });
    [idx[0], idx[1], idx[2], idx[3], idx[4]]
}

#[test]
fn heuristic_mlrange_parity() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let golden_path = root.join("artifacts/range_ml/golden_mprior.json");
    let strength_path = root.join("artifacts/range_ml/strength169.json");
    let weights_path = root.join("artifacts/range_ml/range_v1_gru_weights.json");

    for (label, p) in [
        ("golden_mprior.json", &golden_path),
        ("strength169.json", &strength_path),
        ("range_v1_gru_weights.json", &weights_path),
    ] {
        if !p.exists() {
            eprintln!("[mlrange-parity] {label} 缺席 ({}) → skipped", p.display());
            return;
        }
    }

    // strength169 → EquityEngine::from_strength → RangeModel（同 range parity 加载口径）。
    let sf: StrengthFile =
        serde_json::from_slice(&std::fs::read(&strength_path).expect("read strength169"))
            .expect("parse strength169");
    assert_eq!(sf.strength.len(), 169, "strength169 长度须 169");
    let mut strength = [0.0f64; 169];
    strength.copy_from_slice(&sf.strength);
    let engine = EquityEngine::from_strength(strength);
    let model = RangeModel::new(&engine);

    let gru = GruForward::from_json(&weights_path).expect("load gru weights");
    assert_eq!(gru.fdim(), 72, "fdim 须 72");

    let golden: Golden = serde_json::from_slice(&std::fs::read(&golden_path).expect("read golden"))
        .expect("parse golden");

    let rscale = 0.8;
    let mut n_snaps = 0usize;
    let mut n_pass = 0usize;
    let mut max_dpred_all = 0.0f64;
    let mut top5_mismatch = 0usize;
    let mut worst: Vec<WorstRec> = Vec::new();

    for (hi, hand) in golden.hands.iter().enumerate() {
        assert_eq!(hand.fdim, gru.fdim(), "hand {hi}: fdim 不匹配");
        assert_eq!(
            hand.feat.len() % hand.fdim,
            0,
            "hand {hi}: feat 非 fdim 整数倍"
        );
        let t_len = hand.feat.len() / hand.fdim;
        let res = gru.residual(&hand.feat, t_len); // [T][169]
        let logpriors = model.causal_logpriors(&hand.s);

        for (skey, st) in &hand.streets {
            let s_idx: usize = skey.parse().expect("street key int");
            n_snaps += 1;
            assert_eq!(st.alive.len(), 169);
            assert_eq!(st.pred.len(), 169);
            let snap = st.snap;
            assert!(
                snap >= 0 && (snap as usize) < t_len,
                "hand {hi} 街 {s_idx}: snap 越界"
            );
            let snap = snap as usize;

            let lp = logpriors.street_logprior[s_idx]
                .unwrap_or_else(|| panic!("hand {hi} 街 {s_idx}: Rust 无快照但 golden 有"));
            let pred_rust = mlrange::predict(&lp, &res[snap], &st.alive, rscale);

            let mut max_dp = 0.0f64;
            for (pr, pg) in pred_rust.iter().zip(st.pred.iter()) {
                max_dp = max_dp.max((pr - pg).abs());
            }
            let t5_rust = top5(&pred_rust);
            let t5_gold = top5(&st.pred);
            let t5_ok = t5_rust == t5_gold;

            max_dpred_all = max_dpred_all.max(max_dp);
            if !t5_ok {
                top5_mismatch += 1;
            }
            if max_dp < 1e-4 && t5_ok {
                n_pass += 1;
            } else {
                worst.push((max_dp, hi, s_idx, t5_rust, t5_gold));
            }
        }
    }

    worst.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    if !worst.is_empty() {
        eprintln!("[mlrange-parity] {} 个失败快照，最差 10：", worst.len());
        for (dp, hi, s_idx, rt, gt) in worst.iter().take(10) {
            eprintln!(
                "  hand {hi} street {s_idx}: max|Δpred|={dp:.3e}  rust_top5={rt:?}  gold_top5={gt:?}"
            );
        }
    }

    eprintln!(
        "[mlrange-parity] snaps={n_snaps} pass={n_pass} \
         max|Δpred|={max_dpred_all:.3e} top5_mismatch={top5_mismatch}"
    );

    assert!(n_snaps > 0, "golden 无快照");
    assert_eq!(
        n_pass, n_snaps,
        "并非所有街末快照通过（max|Δpred|<1e-4 且 top-5 一致）"
    );
    assert!(
        max_dpred_all < 1e-4,
        "max|Δpred|={max_dpred_all:.3e} 超阈值 1e-4"
    );
    assert_eq!(top5_mismatch, 0, "top-5 不一致快照数={top5_mismatch}");
}
