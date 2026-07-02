//! 每 token 72 维特征跨语言字节级对拍（docs/opponent_range_ml_design.md M3 / §4.1）。
//!
//! 读 `artifacts/range_ml/golden_mprior.json`（Python `range_golden_dump.py` 导出）的
//! 每手 `feat`（flat `T*72` row-major f64，Python 侧 float32 计算），用 Rust 侧
//! [`tokenizer::tokenize`] 逐样本重算 token 序列，比对：
//!   - token 数 `T` 一致（`len(feat)/72`）；
//!   - 每 token 每维 `max|Δfeat| < 1e-5`（Python float32 舍入误差 ≪ 阈值）。
//!
//! golden 缺席（无 venv / CI）→ 打印并 return（不 fail），镜像 optional-test 约定。

use std::path::PathBuf;

use poker::heuristic::range::Sample;
use poker::heuristic::tokenizer::{tokenize, FDIM};

#[derive(serde::Deserialize)]
struct Golden {
    hands: Vec<HandRec>,
}

#[derive(serde::Deserialize)]
struct HandRec {
    s: Sample,
    fdim: usize,
    /// token 特征 flat `T*fdim` row-major f64。
    feat: Vec<f64>,
}

#[test]
fn heuristic_tokenizer_parity() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let golden_path = root.join("artifacts/range_ml/golden_mprior.json");

    if !golden_path.exists() {
        eprintln!(
            "[tokenizer-parity] golden 缺席 ({}) → skipped",
            golden_path.display()
        );
        return;
    }

    let golden: Golden = serde_json::from_slice(&std::fs::read(&golden_path).expect("read golden"))
        .expect("parse golden");

    let mut total_tokens = 0usize;
    let mut max_dfeat = 0.0f64;
    let mut t_mismatch = 0usize;
    // 首个 |Δ|>=1e-5 的失配定位：(hand, tok, feat, rust, gold)。
    let mut first_bad: Option<(usize, usize, usize, f64, f64)> = None;

    for (hi, hand) in golden.hands.iter().enumerate() {
        assert_eq!(hand.fdim, FDIM, "hand {hi}: fdim={} != 72", hand.fdim);
        assert_eq!(
            hand.feat.len() % FDIM,
            0,
            "hand {hi}: feat 长度非 {FDIM} 整数倍"
        );
        let t_gold = hand.feat.len() / FDIM;

        let toks = tokenize(&hand.s);
        if toks.len() != t_gold {
            t_mismatch += 1;
            eprintln!(
                "[tokenizer-parity] hand {hi}: T 失配 rust={} gold={t_gold}",
                toks.len()
            );
            continue;
        }
        total_tokens += toks.len();

        for (ti, tok) in toks.iter().enumerate() {
            for (fi, &rv) in tok.iter().enumerate() {
                let gv = hand.feat[ti * FDIM + fi];
                let d = (rv - gv).abs();
                if d > max_dfeat {
                    max_dfeat = d;
                }
                if d >= 1e-5 && first_bad.is_none() {
                    first_bad = Some((hi, ti, fi, rv, gv));
                }
            }
        }
    }

    if let Some((hi, ti, fi, rv, gv)) = first_bad {
        eprintln!(
            "[tokenizer-parity] 首个失配 hand {hi} tok {ti} feat {fi}: rust={rv} gold={gv} Δ={:.3e}",
            (rv - gv).abs()
        );
    }
    eprintln!(
        "[tokenizer-parity] total_tokens={total_tokens} max|Δfeat|={max_dfeat:.3e} T_mismatch={t_mismatch}"
    );

    assert_eq!(t_mismatch, 0, "T 失配手数={t_mismatch}");
    assert!(total_tokens > 0, "无 token 参与比对（golden 空？）");
    assert!(max_dfeat < 1e-5, "max|Δfeat|={max_dfeat:.3e} 超阈值 1e-5");
}
