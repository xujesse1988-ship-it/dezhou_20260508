//! B2b 因果 m_prior 跨语言字节级对拍（docs/opponent_range_ml_design.md M3 / §6）。
//!
//! 读 `artifacts/range_ml/golden_mprior.json`（Python `range_golden_dump.py` 导出的
//! oracle）+ `strength169.json`，用 Rust 侧 [`RangeModel`] 的移植因果链重算每个街末
//! 快照的 log m_prior，比对：
//!   - `p = softmax(logprior_rust + log alive，死类掩 −inf)` 与 golden `p`
//!     的 `max|Δ| < 1e-4`；
//!   - top-5 类（按 (−p, idx) 破平）逐街一致；
//!   - 诊断：`max|logprior_rust − logprior_golden|`（alive 类上）。
//!
//! golden 缺席（无 venv / CI）→ 打印并 return（不 fail），镜像 optional-test 约定。

use std::path::PathBuf;

use poker::heuristic::equity::EquityEngine;
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
    #[serde(default)]
    streets: std::collections::BTreeMap<String, StreetRec>,
}

#[derive(serde::Deserialize)]
struct StreetRec {
    #[allow(dead_code)]
    snap: i64,
    logprior: Vec<f64>,
    alive: Vec<i64>,
    p: Vec<f64>,
}

/// B2b 预测：softmax(logprior + log alive)，alive==0 掩 −1e30（复刻 range_golden_dump.b2b_p）。
fn b2b_p(logprior: &[f64; 169], alive: &[i64]) -> [f64; 169] {
    let mut lp = [0.0f64; 169];
    for (l, (&lpv, &av)) in lp.iter_mut().zip(logprior.iter().zip(alive.iter())) {
        *l = if av == 0 {
            -1e30
        } else {
            lpv + (av as f64).max(1e-9).ln()
        };
    }
    let maxlp = lp.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let mut p = [0.0f64; 169];
    let mut sum = 0.0;
    for (pi, &lpi) in p.iter_mut().zip(lp.iter()) {
        *pi = (lpi - maxlp).exp();
        sum += *pi;
    }
    for pi in p.iter_mut() {
        *pi /= sum;
    }
    p
}

/// 失败快照记录：(max|Δp|, hand_idx, street, rust_top5, golden_top5)。
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
fn heuristic_range_parity() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let golden_path = root.join("artifacts/range_ml/golden_mprior.json");
    let strength_path = root.join("artifacts/range_ml/strength169.json");

    if !golden_path.exists() {
        eprintln!(
            "[range-parity] golden 缺席 ({}) → skipped",
            golden_path.display()
        );
        return;
    }
    if !strength_path.exists() {
        eprintln!(
            "[range-parity] strength169.json 缺席 ({}) → skipped",
            strength_path.display()
        );
        return;
    }

    // strength169 载入 → EquityEngine::from_strength → RangeModel。
    let sf: StrengthFile =
        serde_json::from_slice(&std::fs::read(&strength_path).expect("read strength169"))
            .expect("parse strength169");
    assert_eq!(sf.strength.len(), 169, "strength169 长度须 169");
    let mut strength = [0.0f64; 169];
    strength.copy_from_slice(&sf.strength);
    let engine = EquityEngine::from_strength(strength);
    let model = RangeModel::new(&engine);

    let golden: Golden = serde_json::from_slice(&std::fs::read(&golden_path).expect("read golden"))
        .expect("parse golden");

    let mut n_snaps = 0usize;
    let mut n_pass = 0usize;
    let mut max_dp_all = 0.0f64;
    let mut max_dlp_all = 0.0f64;
    let mut top5_mismatch = 0usize;
    // 最差样本记录：(dp, hand_idx, street, rust_top5, golden_top5)。
    let mut worst: Vec<WorstRec> = Vec::new();

    for (hi, hand) in golden.hands.iter().enumerate() {
        let res = model.causal_logpriors(&hand.s);
        for (skey, st) in &hand.streets {
            let s_idx: usize = skey.parse().expect("street key int");
            n_snaps += 1;
            let lp_rust = res.street_logprior[s_idx]
                .unwrap_or_else(|| panic!("hand {hi} street {s_idx}: Rust 无快照但 golden 有"));
            assert_eq!(st.logprior.len(), 169);
            assert_eq!(st.alive.len(), 169);
            assert_eq!(st.p.len(), 169);

            let p_rust = b2b_p(&lp_rust, &st.alive);

            // max|Δp|。
            let mut max_dp = 0.0f64;
            for (pr, pg) in p_rust.iter().zip(st.p.iter()) {
                max_dp = max_dp.max((pr - pg).abs());
            }
            // 诊断：max|Δlogprior|（alive 类上）。
            let mut max_dlp = 0.0f64;
            for ((lr, lg), &av) in lp_rust.iter().zip(st.logprior.iter()).zip(st.alive.iter()) {
                if av > 0 {
                    max_dlp = max_dlp.max((lr - lg).abs());
                }
            }
            let t5_rust = top5(&p_rust);
            let t5_gold = top5(&st.p);

            max_dp_all = max_dp_all.max(max_dp);
            max_dlp_all = max_dlp_all.max(max_dlp);
            let t5_ok = t5_rust == t5_gold;
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
        eprintln!("[range-parity] {} 个失败快照，最差 10：", worst.len());
        for (dp, hi, s_idx, rt, gt) in worst.iter().take(10) {
            eprintln!(
                "  hand {hi} street {s_idx}: max|Δp|={dp:.3e}  rust_top5={rt:?}  gold_top5={gt:?}"
            );
        }
    }

    eprintln!(
        "[range-parity] snaps={n_snaps} pass={n_pass} \
         max|Δp|={max_dp_all:.3e} max|Δlogprior|={max_dlp_all:.3e} top5_mismatch={top5_mismatch}"
    );

    assert!(n_snaps > 0, "golden 无快照");
    assert_eq!(
        n_pass, n_snaps,
        "并非所有街末快照通过（max|Δp|<1e-4 且 top-5 一致）"
    );
    assert!(max_dp_all < 1e-4, "max|Δp|={max_dp_all:.3e} 超阈值 1e-4");
    assert_eq!(top5_mismatch, 0, "top-5 不一致快照数={top5_mismatch}");
}
