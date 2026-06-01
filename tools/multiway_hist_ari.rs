//! `multiway_hist_ari` —— S3 实验 A2（接 `tools/multiway_equity_probe.rs` 实验 A1）。
//!
//! A1 发现 flop/turn 的标量 equity 在多人下有真实重排（draws/nut-potential 升）。
//! production 特征是 8-bin equity 直方图（potential-aware hist），其形状本就编码
//! draw/made 之分。A2 问：vs-1 的 hist 桶是不是已经把多人重排吸收了？
//!
//! 关键方法学（两处，否则结论会被假象污染）：
//!
//! 1. 多人 equity 用 **真值 disjoint** 而非 q^N 独立近似。q^N 是逐 board 的单调
//!    变换，携带与 q 相同的序信息——用它比 ARI 测到的只是分箱分辨率假象，不是真
//!    重排。N=2（A3×A4 的 3-way cap = hero + 2 对手）下真值可**精确闭式**算：给定
//!    完整 board，设 hero 击败（≥）的对手手集为 B（大小 b，各 card 在 B 中出现
//!    d_c 次），则 P(hero 同时不输给 2 副互斥对手手) =
//!    (b(b-1) − Σ_c d_c(d_c−1)) / (990·989 − 45·44·43)。与 q 同一次 990 枚举出，
//!    零额外成本、零 MC 噪声。
//!
//! 2. **等量分箱**（quantile/equal-mass）而非固定 [0,1] 等宽。多人 equity 被压到
//!    [0, ~0.4]，固定 [0,1] 分箱让高 bin 近空 → 两特征落 simplex 不同角落 → k-means
//!    必然切不同，纯属分辨率假象。等量分箱让两特征各用满动态范围，ARI 才纯测
//!    "1↔2 对手是否重排手的分组"。
//!
//! 度量（flop/turn，hist-only 8 维）：
//!   ARI(vs-1 桶, vs-2 桶)  = 真实重排信号
//!   ARI(vs-1 桶, vs-1 桶') = k-means 初值噪声底
//!   信号 ≈ 底 → vs-1 hist 桶已吸收多人重排（flop/turn 复用单对手 hist，只需重标 bin
//!   边界，省下重算多人 equity 的大头）；信号 ≪ 底 → 真重排（blocker/card-removal
//!   结构），flop/turn 须算多人 equity。
//!
//! river 不在此实验：A1 已证 river 单对手 ≈ 完美迁移（无 future card / 无 hist 形状）。

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use rayon::prelude::*;

use poker::abstraction::canonical_enum::{n_canonical_observation, nth_canonical_form};
use poker::abstraction::cluster::rng_substream::{
    EMPTY_CLUSTER_SPLIT_FLOP, EMPTY_CLUSTER_SPLIT_TURN, KMEANS_PP_INIT_FLOP, KMEANS_PP_INIT_TURN,
};
use poker::abstraction::cluster::{kmeans_fit_production, KMeansConfig};
use poker::abstraction::info::StreetTag;
use poker::eval::NaiveHandEvaluator;
use poker::{Card, ChaCha20Rng, HandEvaluator, RngSource};

const DEFAULT_SAMPLES_FLOP: u32 = 40_000;
const DEFAULT_SAMPLES_TURN: u32 = 100_000;
const DEFAULT_K: u32 = 500;
const SEED_A: u64 = 0x0A20_5EED_0000_0001;
const SEED_B: u64 = 0x0A20_5EED_0000_0002;
/// 算等量分箱边界用的 hand 子样本数（全局 per-board equity 分布很稳，子样本足够）。
const EDGE_SUBSAMPLE_HANDS: usize = 3_000;
/// disjoint 真值公式的 MC 自检 board 数 / 每 board MC 抽样数。
const SELFCHECK_BOARDS: usize = 6;
const SELFCHECK_MC_ITERS: u32 = 400_000;

/// N=2 disjoint 分母：从 45 张未用牌发 2 副互斥对手手的有序对数 = 990·989 − 45·44·43。
const DISJOINT_PAIRS_ORDERED: f64 = 990.0 * 989.0 - 45.0 * 44.0 * 43.0;

struct Opts {
    samples_flop: u32,
    samples_turn: u32,
    k: u32,
    streets: Vec<StreetTag>,
    threads: Option<usize>,
}

fn parse_args(args: &[String]) -> Result<Opts, String> {
    let mut samples_flop = DEFAULT_SAMPLES_FLOP;
    let mut samples_turn = DEFAULT_SAMPLES_TURN;
    let mut k = DEFAULT_K;
    let mut streets = vec![StreetTag::Flop, StreetTag::Turn];
    let mut threads: Option<usize> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--samples-flop" => {
                i += 1;
                samples_flop = num(args, i, "--samples-flop")?;
            }
            "--samples-turn" => {
                i += 1;
                samples_turn = num(args, i, "--samples-turn")?;
            }
            "--k" => {
                i += 1;
                k = num(args, i, "--k")?;
            }
            "--streets" => {
                i += 1;
                streets = parse_streets(get(args, i, "--streets")?)?;
            }
            "--threads" => {
                i += 1;
                threads = Some(num(args, i, "--threads")?);
            }
            "-h" | "--help" => {
                eprintln!(
                    "usage: {} [--samples-flop N] [--samples-turn N] [--k K] [--streets flop,turn] [--threads N]",
                    args.first().map(String::as_str).unwrap_or("multiway_hist_ari")
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown arg {other}")),
        }
        i += 1;
    }
    if k < 2 {
        return Err("--k must be >= 2".into());
    }
    Ok(Opts {
        samples_flop,
        samples_turn,
        k,
        streets,
        threads,
    })
}

fn get<'a>(args: &'a [String], i: usize, flag: &str) -> Result<&'a str, String> {
    args.get(i)
        .map(String::as_str)
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn num<T: std::str::FromStr>(args: &[String], i: usize, flag: &str) -> Result<T, String>
where
    T::Err: std::fmt::Display,
{
    get(args, i, flag)?
        .parse::<T>()
        .map_err(|e| format!("{flag}: {e}"))
}

fn parse_streets(s: &str) -> Result<Vec<StreetTag>, String> {
    let mut out = Vec::new();
    for part in s.split(',') {
        match part.trim() {
            "flop" => out.push(StreetTag::Flop),
            "turn" => out.push(StreetTag::Turn),
            other => return Err(format!("--streets item must be flop|turn, got {other}")),
        }
    }
    Ok(out)
}

// =============================================================================
// per-board equity：q1（vs 1 对手，≥ 约定）+ q2_true（vs 2 互斥对手，精确 disjoint）
// =============================================================================

/// 给定 hero 与完整 5-card board，一次枚举 990 副对手手，返回：
/// - q1 = b/990（hero 不输给 1 副随机对手手的频率，≥ 约定）
/// - q2 = P(hero 同时不输给 2 副互斥对手手)（精确 disjoint，N=2）
#[allow(clippy::needless_range_loop)] // i<j 双层 index over unused[] 是有意的有序对枚举
fn board_equity_q1_q2(
    hole: [Card; 2],
    full_board: &[Card; 5],
    eval: &dyn HandEvaluator,
) -> (f32, f32) {
    let hero7 = [
        hole[0],
        hole[1],
        full_board[0],
        full_board[1],
        full_board[2],
        full_board[3],
        full_board[4],
    ];
    let hero_rank = eval.eval7(&hero7);

    let mut used = [false; 52];
    for c in hole.iter() {
        used[c.to_u8() as usize] = true;
    }
    for c in full_board.iter() {
        used[c.to_u8() as usize] = true;
    }
    let unused: Vec<Card> = (0..52u8)
        .filter(|c| !used[*c as usize])
        .map(|c| Card::from_u8(c).unwrap())
        .collect();
    debug_assert_eq!(unused.len(), 45);

    // opp7 模板：board 5 张固定，只在内层换前两张 opp hole（避免每 iter 重建数组 + from_u8）。
    let mut opp7 = [
        unused[0],
        unused[0],
        full_board[0],
        full_board[1],
        full_board[2],
        full_board[3],
        full_board[4],
    ];
    let mut b: u64 = 0; // hero ≥ opp 的手数
    let mut d = [0u64; 52]; // d[c] = B 中含 card c 的手数
    let n = unused.len();
    for i in 0..n {
        let ci = unused[i];
        opp7[0] = ci;
        for j in (i + 1)..n {
            let cj = unused[j];
            opp7[1] = cj;
            if hero_rank >= eval.eval7(&opp7) {
                b += 1;
                d[ci.to_u8() as usize] += 1;
                d[cj.to_u8() as usize] += 1;
            }
        }
    }

    let q1 = b as f32 / 990.0;
    // disjoint 有序对：分子 = b(b-1) − Σ_c d_c(d_c-1)。
    let sum_dd: u64 = d.iter().map(|&x| x * x.saturating_sub(1)).sum();
    let numer = (b * b.saturating_sub(1)) as f64 - sum_dd as f64;
    let q2 = (numer / DISJOINT_PAIRS_ORDERED).clamp(0.0, 1.0) as f32;
    (q1, q2)
}

/// 枚举一个 (board, hole) 的所有未来完整 board，返回每个的 (q1, q2)。
fn board_equities(hole: [Card; 2], board: &[Card], eval: &dyn HandEvaluator) -> Vec<(f32, f32)> {
    let mut used = [false; 52];
    for c in hole.iter() {
        used[c.to_u8() as usize] = true;
    }
    for c in board.iter() {
        used[c.to_u8() as usize] = true;
    }
    let unused: Vec<Card> = (0..52u8)
        .filter(|c| !used[*c as usize])
        .map(|c| Card::from_u8(c).unwrap())
        .collect();

    let mut out = Vec::new();
    match board.len() {
        3 => {
            for i in 0..unused.len() {
                for j in (i + 1)..unused.len() {
                    out.push(board_equity_q1_q2(
                        hole,
                        &[board[0], board[1], board[2], unused[i], unused[j]],
                        eval,
                    ));
                }
            }
        }
        4 => {
            for &c in &unused {
                out.push(board_equity_q1_q2(
                    hole,
                    &[board[0], board[1], board[2], board[3], c],
                    eval,
                ));
            }
        }
        other => panic!("board_equities: board.len() = {other}"),
    }
    out
}

/// disjoint 公式 MC 自检：对一个完整 board，brute MC 抽 2 副互斥对手手估 q2，
/// 与精确公式比。返回 (exact, mc)。
fn q2_selfcheck_mc(
    hole: [Card; 2],
    full_board: &[Card; 5],
    rng: &mut dyn RngSource,
    eval: &dyn HandEvaluator,
) -> f32 {
    let hero7 = [
        hole[0],
        hole[1],
        full_board[0],
        full_board[1],
        full_board[2],
        full_board[3],
        full_board[4],
    ];
    let hero_rank = eval.eval7(&hero7);
    let mut used = [false; 52];
    for c in hole.iter() {
        used[c.to_u8() as usize] = true;
    }
    for c in full_board.iter() {
        used[c.to_u8() as usize] = true;
    }
    let mut deck: Vec<u8> = (0..52u8).filter(|c| !used[*c as usize]).collect();
    let dn = deck.len();
    let mut win = 0u64;
    for _ in 0..SELFCHECK_MC_ITERS {
        for kk in 0..4 {
            let span = (dn - kk) as u64;
            let jj = kk + (rng.next_u64() % span) as usize;
            deck.swap(kk, jj);
        }
        let o1 = [
            Card::from_u8(deck[0]).unwrap(),
            Card::from_u8(deck[1]).unwrap(),
            full_board[0],
            full_board[1],
            full_board[2],
            full_board[3],
            full_board[4],
        ];
        let o2 = [
            Card::from_u8(deck[2]).unwrap(),
            Card::from_u8(deck[3]).unwrap(),
            full_board[0],
            full_board[1],
            full_board[2],
            full_board[3],
            full_board[4],
        ];
        if hero_rank >= eval.eval7(&o1) && hero_rank >= eval.eval7(&o2) {
            win += 1;
        }
    }
    win as f32 / SELFCHECK_MC_ITERS as f32
}

// =============================================================================
// 分箱：等量（octile 边界）+ 固定 [0,1]
// =============================================================================

/// 从 pooled per-board 值算 7 个 octile 边界（1/8..7/8 分位）。
fn octile_edges(mut vals: Vec<f32>) -> [f32; 7] {
    vals.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = vals.len();
    std::array::from_fn(|q| {
        let idx = ((q + 1) * n / 8).min(n - 1);
        vals[idx]
    })
}

#[inline]
fn bin_eqmass(x: f32, edges: &[f32; 7]) -> usize {
    let mut b = 0;
    for &e in edges {
        if x >= e {
            b += 1;
        } else {
            break;
        }
    }
    b.min(7)
}

#[inline]
fn bin_fixed(x: f32) -> usize {
    ((x * 8.0).floor() as usize).min(7)
}

fn hist_eqmass(boards: &[(f32, f32)], edges: &[f32; 7], pick_q2: bool) -> Vec<f64> {
    let mut h = [0u32; 8];
    for &(q1, q2) in boards {
        let x = if pick_q2 { q2 } else { q1 };
        h[bin_eqmass(x, edges)] += 1;
    }
    let z = boards.len() as f64;
    h.iter().map(|&n| n as f64 / z).collect()
}

fn hist_fixed(boards: &[(f32, f32)], pick_q2: bool) -> Vec<f64> {
    let mut h = [0u32; 8];
    for &(q1, q2) in boards {
        let x = if pick_q2 { q2 } else { q1 };
        h[bin_fixed(x)] += 1;
    }
    let z = boards.len() as f64;
    h.iter().map(|&n| n as f64 / z).collect()
}

// =============================================================================
// Adjusted Rand Index
// =============================================================================

fn comb2(x: u64) -> f64 {
    (x as f64) * (x as f64 - 1.0) / 2.0
}

fn adjusted_rand_index(a: &[u32], b: &[u32]) -> f64 {
    use std::collections::HashMap;
    assert_eq!(a.len(), b.len());
    let n = a.len() as u64;
    let mut nij: HashMap<(u32, u32), u64> = HashMap::new();
    let mut ai: HashMap<u32, u64> = HashMap::new();
    let mut bj: HashMap<u32, u64> = HashMap::new();
    for i in 0..a.len() {
        *nij.entry((a[i], b[i])).or_insert(0) += 1;
        *ai.entry(a[i]).or_insert(0) += 1;
        *bj.entry(b[i]).or_insert(0) += 1;
    }
    let sum_ij: f64 = nij.values().map(|&v| comb2(v)).sum();
    let sum_a: f64 = ai.values().map(|&v| comb2(v)).sum();
    let sum_b: f64 = bj.values().map(|&v| comb2(v)).sum();
    let total = comb2(n);
    let expected = sum_a * sum_b / total;
    let max_index = 0.5 * (sum_a + sum_b);
    if (max_index - expected).abs() < 1e-12 {
        return 1.0;
    }
    (sum_ij - expected) / (max_index - expected)
}

// =============================================================================
// per-street
// =============================================================================

fn street_name(s: StreetTag) -> &'static str {
    match s {
        StreetTag::Flop => "flop",
        StreetTag::Turn => "turn",
        _ => "?",
    }
}

fn op_ids(s: StreetTag) -> (u32, u32) {
    match s {
        StreetTag::Flop => (KMEANS_PP_INIT_FLOP, EMPTY_CLUSTER_SPLIT_FLOP),
        StreetTag::Turn => (KMEANS_PP_INIT_TURN, EMPTY_CLUSTER_SPLIT_TURN),
        _ => unreachable!(),
    }
}

fn ari3(
    hist1: &[Vec<f64>],
    histn: &[Vec<f64>],
    cfg: KMeansConfig,
    op_init: u32,
    op_split: u32,
) -> (f64, f64) {
    let l1a = kmeans_fit_production(hist1, cfg, SEED_A, op_init, op_split).assignments;
    let l1b = kmeans_fit_production(hist1, cfg, SEED_B, op_init, op_split).assignments;
    let ln = kmeans_fit_production(histn, cfg, SEED_A, op_init, op_split).assignments;
    let signal = adjusted_rand_index(&l1a, &ln);
    let floor = adjusted_rand_index(&l1a, &l1b);
    (signal, floor)
}

fn run_street(opts: &Opts, street: StreetTag, eval: &Arc<dyn HandEvaluator>) {
    let n_full = n_canonical_observation(street);
    let s = match street {
        StreetTag::Flop => opts.samples_flop,
        StreetTag::Turn => opts.samples_turn,
        _ => unreachable!(),
    }
    .min(n_full);
    let ids: Vec<u32> = (0..s)
        .map(|i| ((i as u64 * n_full as u64) / s as u64) as u32)
        .collect();

    // 1. 每个 hand 算全 future board 的 (q1, q2)，存下来。
    let t_feat = Instant::now();
    let per_hand: Vec<Vec<(f32, f32)>> = ids
        .par_iter()
        .map(|&id| {
            let (board, hole) = nth_canonical_form(street, id);
            board_equities(hole, &board, &**eval)
        })
        .collect();
    eprintln!(
        "[multiway_hist_ari] {} feature {} hands wall={:?}",
        street_name(street),
        per_hand.len(),
        t_feat.elapsed()
    );

    // 1b. disjoint 公式 MC 自检（取前几手的首 board）。
    {
        let mut rng = ChaCha20Rng::from_seed(0xC0FFEE ^ street as u64);
        let mut max_d = 0.0f32;
        for &id in ids.iter().take(SELFCHECK_BOARDS.min(ids.len())) {
            let (board, hole) = nth_canonical_form(street, id);
            // 复制首 future board
            let mut used = [false; 52];
            for c in hole.iter() {
                used[c.to_u8() as usize] = true;
            }
            for c in board.iter() {
                used[c.to_u8() as usize] = true;
            }
            let unused: Vec<Card> = (0..52u8)
                .filter(|c| !used[*c as usize])
                .map(|c| Card::from_u8(c).unwrap())
                .collect();
            let full: [Card; 5] = match board.len() {
                3 => [board[0], board[1], board[2], unused[0], unused[1]],
                4 => [board[0], board[1], board[2], board[3], unused[0]],
                _ => unreachable!(),
            };
            let (_, q2_exact) = board_equity_q1_q2(hole, &full, &**eval);
            let q2_mc = q2_selfcheck_mc(hole, &full, &mut rng, &**eval);
            max_d = max_d.max((q2_exact - q2_mc).abs());
        }
        eprintln!(
            "[multiway_hist_ari] {} disjoint 公式 self-check: max|q2_exact − q2_mc| = {max_d:.4} (over {} boards)",
            street_name(street),
            SELFCHECK_BOARDS.min(ids.len())
        );
    }

    // 2. 等量分箱边界：子样本 pooled per-board q1 / q2。
    let mut pool_q1 = Vec::new();
    let mut pool_q2 = Vec::new();
    for boards in per_hand.iter().take(EDGE_SUBSAMPLE_HANDS) {
        for &(q1, q2) in boards {
            pool_q1.push(q1);
            pool_q2.push(q2);
        }
    }
    let edges1 = octile_edges(pool_q1);
    let edges2 = octile_edges(pool_q2);

    // 3. 构造 4 套 hist：等量(q1/q2) + 固定(q1/q2)。
    let h1_eq: Vec<Vec<f64>> = per_hand
        .par_iter()
        .map(|b| hist_eqmass(b, &edges1, false))
        .collect();
    let hn_eq: Vec<Vec<f64>> = per_hand
        .par_iter()
        .map(|b| hist_eqmass(b, &edges2, true))
        .collect();
    let h1_fx: Vec<Vec<f64>> = per_hand.par_iter().map(|b| hist_fixed(b, false)).collect();
    let hn_fx: Vec<Vec<f64>> = per_hand.par_iter().map(|b| hist_fixed(b, true)).collect();

    // 4. k-means + ARI。
    let cfg = KMeansConfig::default_d232(opts.k);
    let (op_init, op_split) = op_ids(street);
    let t_km = Instant::now();
    let (sig_eq, flr_eq) = ari3(&h1_eq, &hn_eq, cfg, op_init, op_split);
    let (sig_fx, flr_fx) = ari3(&h1_fx, &hn_fx, cfg, op_init, op_split);
    eprintln!(
        "[multiway_hist_ari] {} 6×kmeans(K={}) wall={:?}",
        street_name(street),
        opts.k,
        t_km.elapsed()
    );

    // 描述：平均 q1 / q2。
    let (mut sq1, mut sq2, mut cnt) = (0.0f64, 0.0f64, 0u64);
    for boards in &per_hand {
        for &(q1, q2) in boards {
            sq1 += q1 as f64;
            sq2 += q2 as f64;
            cnt += 1;
        }
    }

    println!(
        "\n================ street = {} ================",
        street_name(street)
    );
    println!(
        "samples={s}  K={}  (N=2 真值 disjoint，3-way = hero+2)",
        opts.k
    );
    println!(
        "per-board mean q1(vs1)={:.4}  mean q2(vs2 真值)={:.4}",
        sq1 / cnt as f64,
        sq2 / cnt as f64
    );
    println!(
        "edges1(q1 octile)=[{}]",
        edges1
            .iter()
            .map(|v| format!("{v:.3}"))
            .collect::<Vec<_>>()
            .join(" ")
    );
    println!(
        "edges2(q2 octile)=[{}]",
        edges2
            .iter()
            .map(|v| format!("{v:.3}"))
            .collect::<Vec<_>>()
            .join(" ")
    );
    println!("\n  ── 等量分箱（去分辨率假象，真重排信号）──");
    println!("  ARI(vs-1 桶, vs-2 桶)  = {sig_eq:.4}   ← 信号");
    println!("  ARI(vs-1 桶, vs-1 桶') = {flr_eq:.4}   ← k-means 初值底");
    println!("  signal / floor = {:.3}", sig_eq / flr_eq);
    println!("\n  ── 固定 [0,1] 分箱（含分辨率假象，仅对照）──");
    println!("  ARI(vs-1 桶, vs-2 桶)  = {sig_fx:.4}");
    println!("  ARI(vs-1 桶, vs-1 桶') = {flr_fx:.4}");

    let verdict = if sig_eq >= flr_eq - 0.04 {
        "信号≈底 → vs-1 hist 桶已吸收多人重排；flop/turn 复用单对手 hist 即可（只需重标 bin 边界）"
    } else if sig_eq >= 0.6 * flr_eq {
        "信号<底但仍高 → 中度真重排；建议算多人 equity hist，损失有界"
    } else {
        "信号≪底 → 显著真重排（blocker/card-removal 结构）；flop/turn 须算多人 equity hist"
    };
    println!("\n  判定：{verdict}");
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let opts = match parse_args(&args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    if let Some(n) = opts.threads {
        if let Err(e) = rayon::ThreadPoolBuilder::new()
            .num_threads(n)
            .build_global()
        {
            eprintln!("warning: rayon pool {n}: {e}");
        }
    }
    let t0 = Instant::now();
    let eval: Arc<dyn HandEvaluator> = Arc::new(NaiveHandEvaluator);
    println!("# multiway_hist_ari (S3 实验 A2)");
    println!(
        "k={} samples(flop={}, turn={}) seedA=0x{:016x} seedB=0x{:016x}",
        opts.k, opts.samples_flop, opts.samples_turn, SEED_A, SEED_B
    );
    for &street in &opts.streets {
        run_street(&opts, street, &eval);
    }
    println!("\n# total wall = {:?}", t0.elapsed());
    ExitCode::from(0)
}
