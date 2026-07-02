//! `equity_weighted`（加权 range 消费面，走读见 `docs/heuristic9_strategy.md`「主货币」节）
//! 的**正确性**检验——注意是数值逼近，非 byte-equal（新旧路径 CDF
//! 二分 vs `rng.below` 消费 RNG 不同）。
//!
//! 主锚点（degenerate 自检）：`equity_weighted(&[RangeVec::from_threshold(thr)])` 应与
//! 标量 `equity(&[thr])` 在同输入上收敛到同一真实 equity → 统计容差 |Δ| < 0.02。
//!
//! **card-removal 分歧（`blocker_divergence_is_bounded`）**：当 hero+board 阻挡对手 range
//! 里的**强类**时，两者会系统性分歧 —— `from_threshold` 保留每类的满副牌质量
//! `mass ∝ combo_weight`，`equity_weighted` 再把它「平摊到存活 combo」（`mass/n_alive`，
//! §2.5：保类边际），而 `equity()` 是「存活 combo 均匀」（≈严格 blocker 条件化）。二者仅
//! 在 range 类被阻挡时分离，幅度随「阻挡强类 × 阈值紧度」增大（实测有界 <0.05）。故
//! degenerate 主检刻意用**不阻挡 range 强类的低牌 hero/board**，此处才真正收敛到 <0.02。

use poker::core::Card;
use poker::heuristic::equity::EquityEngine;
use poker::heuristic::range::RangeVec;

/// 卡串 → Card（复用 range.rs 的 `card_id`）。
fn c(s: &str) -> Card {
    Card::from_u8(poker::heuristic::range::card_id(s)).expect("valid card")
}

fn board(cards: &[&str]) -> Vec<Card> {
    cards.iter().map(|s| c(s)).collect()
}

/// 建一张 equity 强度表（表 samples 适中，够稳定阈值/强度）。
fn engine() -> EquityEngine {
    EquityEngine::new(4000, 0x1234_5678)
}

#[test]
fn degenerate_equity_weighted_matches_scalar_equity() {
    let eq = engine();
    // (hole, board, thr)：覆盖 preflop 空板 / flop / turn。刻意选**低牌 hero + 低牌 board**，
    // 不阻挡 thr 紧 range 的强类 → mass/n_alive ≡ mass/combo_weight ≡ 存活均匀 = equity() 分布，
    // 二者仅差 MC 噪声。thr = 0.3 / 0.5 / 0.7。
    let holes: Vec<([Card; 2], Vec<Card>, &str)> = vec![
        ([c("7s"), c("6s")], board(&[]), "preflop 76s"),
        ([c("5h"), c("4h")], board(&[]), "preflop 54s"),
        (
            [c("8d"), c("7c")],
            board(&["6s", "5h", "2c"]),
            "flop 87 on 652",
        ),
        (
            [c("9d"), c("8c")],
            board(&["6s", "5h", "2c", "4d"]),
            "turn 98 on 6524",
        ),
    ];
    let thrs = [0.3f64, 0.5, 0.7];
    let seeds = [1u64, 42, 12345];
    const N: u32 = 30_000;
    const TOL: f64 = 0.02;

    let mut max_abs = 0.0f64;
    for (hole, brd, label) in &holes {
        for &thr in &thrs {
            let rv = RangeVec::from_threshold(thr, &eq);
            for &seed in &seeds {
                let ew = eq.equity_weighted(*hole, brd, std::slice::from_ref(&rv), N, seed);
                let es = eq.equity(*hole, brd, &[thr], N, seed);
                let d = (ew - es).abs();
                if d > max_abs {
                    max_abs = d;
                }
                println!("[{label}] thr={thr:.1} seed={seed:<6} ew={ew:.4} e={es:.4} |Δ|={d:.4}");
                assert!((0.0..=1.0).contains(&ew), "ew out of [0,1]: {ew}");
                assert!(
                    d < TOL,
                    "degenerate gap too big for {label} thr={thr} seed={seed}: |Δ|={d:.4} >= {TOL}"
                );
            }
        }
    }
    println!("--- max |Δ| over all (unblocking) degenerate cases = {max_abs:.4} (tol {TOL}) ---");
}

#[test]
fn two_opponents_in_range_and_matches_scalar() {
    let eq = engine();
    // 低牌 hero + 低牌 board，两对手各 thr=0.45 range：合法 [0,1] 且逼近标量双对手 equity。
    let hole = [c("8d"), c("7c")];
    let brd = board(&["6s", "5h", "2c"]);
    let thr = 0.45f64;
    let rv = RangeVec::from_threshold(thr, &eq);
    let ranges = [rv.clone(), rv.clone()];
    const N: u32 = 30_000;
    let mut max_abs = 0.0f64;
    for seed in [5u64, 55, 555] {
        let ew = eq.equity_weighted(hole, &brd, &ranges, N, seed);
        let es = eq.equity(hole, &brd, &[thr, thr], N, seed);
        let d = (ew - es).abs();
        max_abs = max_abs.max(d);
        println!("2-opp seed={seed:<5} ew={ew:.4} e={es:.4} |Δ|={d:.4}");
        assert!((0.0..=1.0).contains(&ew), "ew out of [0,1]: {ew}");
        assert!(d < 0.02, "2-opp degenerate gap too big: |Δ|={d:.4}");
    }
    println!("--- 2-opp max |Δ| = {max_abs:.4} ---");
}

#[test]
fn blocker_divergence_is_bounded() {
    // 文档化 card-removal 分歧：hero/board 阻挡 range 强类时，equity_weighted（保类边际）
    // 系统性低于 equity()（存活均匀）。分歧真实存在但**有界**（<0.05）。这不是 bug，是
    // §2.5「mass/n_alive 保类边际」相对严格 blocker 条件化的建模差异。
    let eq = engine();
    let cases: Vec<([Card; 2], Vec<Card>, f64, &str)> = vec![
        (
            [c("Kh"), c("Qd")],
            board(&[]),
            0.70,
            "KQo pre vs tight (blocks K/Q)",
        ),
        (
            [c("Ah"), c("Kh")],
            board(&["Qs", "Jd", "2c"]),
            0.70,
            "AKh on QJ2 (blocks A/K/Q/J)",
        ),
        (
            [c("9s"), c("9d")],
            board(&["Ah", "7c", "2d"]),
            0.50,
            "99 on A72 (blocks A)",
        ),
    ];
    const N: u32 = 40_000;
    let seeds = [11u64, 222, 3333];
    for (hole, brd, thr, label) in &cases {
        let rv = RangeVec::from_threshold(*thr, &eq);
        let mut sw = 0.0f64;
        let mut ss = 0.0f64;
        for &seed in &seeds {
            sw += eq.equity_weighted(*hole, brd, std::slice::from_ref(&rv), N, seed);
            ss += eq.equity(*hole, brd, &[*thr], N, seed);
        }
        let ew = sw / seeds.len() as f64;
        let es = ss / seeds.len() as f64;
        let d = (ew - es).abs();
        println!("[blocker] {label:<34} ew={ew:.4} e={es:.4} |Δ|={d:.4}");
        assert!((0.0..=1.0).contains(&ew), "ew out of [0,1]: {ew}");
        assert!(
            d < 0.05,
            "blocker divergence should stay bounded (<0.05), got |Δ|={d:.4}"
        );
    }
}

#[test]
fn nut_hand_wins_every_sample() {
    let eq = engine();
    // 皇同顺（我方持 s 花色的 T..A）→ 任何对手、任何 runout 都赢、无从平分 → equity ≡ 1.0。
    let hole = [c("As"), c("Ks")];
    let brd = board(&["Qs", "Js", "Ts"]);
    let rv = RangeVec::from_threshold(0.3, &eq); // 任意 range 都无所谓
    for seed in [1u64, 7, 99] {
        let ew = eq.equity_weighted(hole, &brd, std::slice::from_ref(&rv), 5_000, seed);
        assert_eq!(ew, 1.0, "royal flush must win every sample, got {ew}");
    }
}

#[test]
fn tighter_range_lowers_hero_equity() {
    let eq = engine();
    // 固定中强手 TT（preflop）：对手 range 越紧（thr 越高，越是强牌）→ hero equity 越低。
    let hole = [c("Ts"), c("Td")];
    let brd = board(&[]);
    let loose = RangeVec::from_threshold(0.30, &eq); // 松：含大量弱手
    let tight = RangeVec::from_threshold(0.62, &eq); // 紧：只剩顶端强牌
    const N: u32 = 30_000;
    let mut acc_loose = 0.0f64;
    let mut acc_tight = 0.0f64;
    let seeds = [3u64, 17, 2024];
    for &seed in &seeds {
        acc_loose += eq.equity_weighted(hole, &brd, std::slice::from_ref(&loose), N, seed);
        acc_tight += eq.equity_weighted(hole, &brd, std::slice::from_ref(&tight), N, seed);
    }
    let el = acc_loose / seeds.len() as f64;
    let et = acc_tight / seeds.len() as f64;
    println!("TT vs loose={el:.4}  vs tight={et:.4}");
    assert!(
        et < el - 0.02,
        "tighter range should lower hero equity: tight={et:.4} loose={el:.4}"
    );
}

#[test]
fn empty_ranges_returns_one() {
    let eq = engine();
    let hole = [c("2c"), c("3d")];
    let ew = eq.equity_weighted(hole, &board(&[]), &[], 1000, 1);
    assert_eq!(ew, 1.0, "no opponents ⇒ equity 1.0 (same as equity())");
}

#[test]
fn from_threshold_boundary_semantics() {
    let eq = engine();

    // thr=0.0：所有 169 类均满足（强度 ∈ [0,1] 恒 ≥ 0）→ 全非零，归一 ∑≈1。
    let all = RangeVec::from_threshold(0.0, &eq);
    let nonzero = all.mass.iter().filter(|&&m| m > 0.0).count();
    let sum: f64 = all.mass.iter().sum();
    assert_eq!(
        nonzero, 169,
        "from_threshold(0.0) should keep all 169 classes"
    );
    assert!((sum - 1.0).abs() < 1e-12, "sum≈1, got {sum}");
    // 归一后仍保 combo_weight 相对比例：offsuit(12) 类 mass 应为 pair(6) 类的 2 倍。
    let idx_pair = 0usize; // 22
    let idx_off = 91usize; // 第一个 offsuit 类
    let ratio = all.mass[idx_off] / all.mass[idx_pair];
    assert!(
        (ratio - 2.0).abs() < 1e-9,
        "offsuit/pair mass ratio ≈ 2, got {ratio}"
    );

    // thr=1.0：无类满足（最大强度 < 1.0）→ 全零 → normalize 兜底成 169 类均匀分布。
    // （文档化退化语义：过紧阈值 ⇒ uniform fallback，对应 equity() 空 range「任意两张」。）
    let deg = RangeVec::from_threshold(1.0, &eq);
    let sum_deg: f64 = deg.mass.iter().sum();
    assert!(
        (sum_deg - 1.0).abs() < 1e-12,
        "degenerate sum≈1, got {sum_deg}"
    );
    let u = 1.0 / 169.0;
    for (i, &m) in deg.mass.iter().enumerate() {
        assert!(
            (m - u).abs() < 1e-12,
            "from_threshold(1.0) class {i} should be uniform {u}, got {m}"
        );
    }

    // normalize：全零直接兜底成合法均匀分布。
    let mut z = RangeVec { mass: [0.0; 169] };
    z.normalize();
    assert!(
        (z.mass.iter().sum::<f64>() - 1.0).abs() < 1e-12,
        "all-zero normalize → uniform sum 1"
    );
}
