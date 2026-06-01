//! `multiway_equity_probe` —— S3 实验 A（`docs/six_max_nlhe_target.md` S3 决策实验）。
//!
//! 回答的问题：当前 HU 桶用的"hero vs 1 个对手"equity，在 6-max（hero vs N 个
//! 对手同时摊牌）下还保不保得住手牌的**相对强度排序**？如果保得住（Spearman 接近
//! 1、跨桶率低），单对手桶是 6-max 的好代理；如果不保（尤其 river / suited-connector /
//! small-pair 段落重排），就必须把特征语义从单对手改成多人（S3 核心）。
//!
//! 度量（per street，over 均匀采样的 canonical 状态）：
//! - `e1` = hero vs 1 个 uniform 随机对手的 pot-share equity。
//! - `eN` = hero vs N 个 uniform 随机对手**同时**在场的 pot-share equity
//!   （hero 打赢全部才拿池；平分按并列人数 1/t）。
//! - Spearman(e1, eN)、Pearson、平均 |e1−eN|（dilution 幅度）。
//! - 分位桶跨桶率（把 e1 / eN 各切 B 个等量分位桶，统计落不同桶的比例）。
//! - e1-八分位压缩表（看每个 e1 段落的 eN 均值 / 跨度——dilution 是否均匀）。
//! - 重排序 top 表（按 percentile-rank 位移排序，列出多人下相对升 / 降最猛的手 +
//!   其牌面 label，验证 §2.3 "nut potential 升、showdown value 降" 方向）。
//!
//! **不是** production 特征——只用 public API + 自带 MC 采样器，不动 `equity.rs`
//! 的 byte-equal baseline。river 上用精确 `equity_river_exact` 校验 MC 采样器
//! （报告 e1_mc vs e1_exact 的最大 / 平均偏差）。
//!
//! 用法：
//!
//! ```bash
//! cargo run --release --bin multiway_equity_probe -- \
//!     --samples-per-street 4000 --mc-iters 20000 --n-opp 5 \
//!     --csv artifacts/multiway_probe.csv
//! ```

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use rayon::prelude::*;

use poker::abstraction::canonical_enum::{n_canonical_observation, nth_canonical_form};
use poker::abstraction::equity::equity_river_exact;
use poker::abstraction::info::StreetTag;
use poker::eval::NaiveHandEvaluator;
use poker::{Card, ChaCha20Rng, HandEvaluator, RngSource};

// =============================================================================
// 常量 / CLI
// =============================================================================

const DEFAULT_SAMPLES: u32 = 4_000;
const DEFAULT_MC_ITERS: u32 = 20_000;
const DEFAULT_N_OPP: usize = 5;
/// 固定 seed —— 实验可复现（同 seed / 同采样 → 同结果）。
const DEFAULT_SEED: u64 = 0x6D61_7869_7761_7901;
const QUANTILE_BINS: usize = 20;
const OCTILES: usize = 8;
const TOP_DIVERGERS: usize = 25;
/// river 上抽多少 sample 做 MC-vs-exact 采样器校验。
const RIVER_VALIDATE_N: usize = 256;

struct Opts {
    samples: u32,
    mc_iters: u32,
    n_opp: usize,
    seed: u64,
    streets: Vec<StreetTag>,
    threads: Option<usize>,
    csv: Option<PathBuf>,
}

fn parse_streets(s: &str) -> Result<Vec<StreetTag>, String> {
    let mut out = Vec::new();
    for part in s.split(',') {
        match part.trim() {
            "flop" => out.push(StreetTag::Flop),
            "turn" => out.push(StreetTag::Turn),
            "river" => out.push(StreetTag::River),
            other => {
                return Err(format!(
                    "--streets item must be flop|turn|river, got {other}"
                ))
            }
        }
    }
    if out.is_empty() {
        return Err("--streets empty".into());
    }
    Ok(out)
}

fn parse_args(args: &[String]) -> Result<Opts, String> {
    let mut samples = DEFAULT_SAMPLES;
    let mut mc_iters = DEFAULT_MC_ITERS;
    let mut n_opp = DEFAULT_N_OPP;
    let mut seed = DEFAULT_SEED;
    let mut streets = vec![StreetTag::Flop, StreetTag::Turn, StreetTag::River];
    let mut threads: Option<usize> = None;
    let mut csv: Option<PathBuf> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--samples-per-street" => {
                i += 1;
                samples = next(args, i, "--samples-per-street")?
                    .parse()
                    .map_err(|e| format!("--samples-per-street: {e}"))?;
            }
            "--mc-iters" => {
                i += 1;
                mc_iters = next(args, i, "--mc-iters")?
                    .parse()
                    .map_err(|e| format!("--mc-iters: {e}"))?;
            }
            "--n-opp" => {
                i += 1;
                n_opp = next(args, i, "--n-opp")?
                    .parse()
                    .map_err(|e| format!("--n-opp: {e}"))?;
            }
            "--seed" => {
                i += 1;
                let s = next(args, i, "--seed")?;
                seed = s
                    .strip_prefix("0x")
                    .map(|h| u64::from_str_radix(h, 16))
                    .unwrap_or_else(|| s.parse())
                    .map_err(|e| format!("--seed: {e}"))?;
            }
            "--streets" => {
                i += 1;
                streets = parse_streets(next(args, i, "--streets")?)?;
            }
            "--threads" => {
                i += 1;
                threads = Some(
                    next(args, i, "--threads")?
                        .parse()
                        .map_err(|e| format!("--threads: {e}"))?,
                );
            }
            "--csv" => {
                i += 1;
                csv = Some(PathBuf::from(next(args, i, "--csv")?));
            }
            "-h" | "--help" => {
                print_usage(args);
                std::process::exit(0);
            }
            other => return Err(format!("unknown arg {other}")),
        }
        i += 1;
    }
    if samples == 0 {
        return Err("--samples-per-street must be >= 1".into());
    }
    if mc_iters == 0 {
        return Err("--mc-iters must be >= 1".into());
    }
    if !(1..=8).contains(&n_opp) {
        return Err(format!("--n-opp out of [1, 8]: {n_opp}"));
    }
    Ok(Opts {
        samples,
        mc_iters,
        n_opp,
        seed,
        streets,
        threads,
        csv,
    })
}

fn next<'a>(args: &'a [String], i: usize, flag: &str) -> Result<&'a str, String> {
    args.get(i)
        .map(String::as_str)
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn print_usage(args: &[String]) {
    eprintln!(
        "usage: {} [--samples-per-street N] [--mc-iters M] [--n-opp K] \\\n         [--seed S] [--streets flop,turn,river] [--threads N] [--csv path]",
        args.first()
            .map(String::as_str)
            .unwrap_or("multiway_equity_probe")
    );
}

// =============================================================================
// 多人 equity Monte Carlo（实验专用采样器，非 production 特征）
// =============================================================================

/// hero 对 `n_opp` 个 uniform 随机对手**同时**摊牌的 pot-share equity。
///
/// 每 iter：补全 board 到 5 张 + 从剩余牌堆发 `n_opp` 副互斥对手手牌（partial
/// Fisher-Yates，全部互不重叠、且不与 hero/board 重叠）→ eval7 全员 → hero 拿池
/// 份额 = 若被任一对手严格压过则 0，否则 `1 / t`（t = 含 hero 的并列最强人数）。
/// 返回 iters 次份额均值 ∈ [0, 1]。
///
/// `n_opp = 1` 退化为标准单对手 EHS（pot share = win 1 / tie 0.5 / lose 0）。
fn multiway_equity_mc(
    hole: [Card; 2],
    board: &[Card],
    n_opp: usize,
    iters: u32,
    rng: &mut dyn RngSource,
    eval: &dyn HandEvaluator,
) -> f64 {
    let mut used = [false; 52];
    for c in hole.iter() {
        used[c.to_u8() as usize] = true;
    }
    for c in board.iter() {
        used[c.to_u8() as usize] = true;
    }
    let mut deck: Vec<u8> = (0..52u8).filter(|c| !used[*c as usize]).collect();
    let dn = deck.len();
    let board_len = board.len();
    let needed = 5 - board_len;
    let draw = needed + 2 * n_opp;
    assert!(dn >= draw, "deck {dn} < draw {draw}");

    let mut fb = [hole[0]; 5];
    fb[..board_len].copy_from_slice(board);

    let mut share_sum = 0.0_f64;
    for _ in 0..iters {
        // partial Fisher-Yates：把 `draw` 张互斥牌洗到 deck 前缀。
        for k in 0..draw {
            let span = (dn - k) as u64;
            let j = k + (rng.next_u64() % span) as usize;
            deck.swap(k, j);
        }
        for k in 0..needed {
            fb[board_len + k] = Card::from_u8(deck[k]).expect("deck card < 52");
        }
        let hero7 = [hole[0], hole[1], fb[0], fb[1], fb[2], fb[3], fb[4]];
        let hero_rank = eval.eval7(&hero7);

        let mut tie = 1usize;
        let mut beaten = false;
        for o in 0..n_opp {
            let a = Card::from_u8(deck[needed + 2 * o]).expect("deck card < 52");
            let b = Card::from_u8(deck[needed + 2 * o + 1]).expect("deck card < 52");
            let opp7 = [a, b, fb[0], fb[1], fb[2], fb[3], fb[4]];
            let opp_rank = eval.eval7(&opp7);
            if opp_rank > hero_rank {
                beaten = true;
                break;
            } else if opp_rank == hero_rank {
                tie += 1;
            }
        }
        if !beaten {
            share_sum += 1.0 / tie as f64;
        }
    }
    share_sum / iters as f64
}

/// 每个 canonical 状态独立确定性 seed —— splitmix 风格混合，复现稳定。
fn mix_seed(global: u64, street_code: u64, id: u32) -> u64 {
    let mut x = global ^ street_code.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    x = x.wrapping_add((id as u64).wrapping_mul(0x1234_5678_9ABC_DEF1));
    x ^= x >> 30;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    x
}

// =============================================================================
// 每个 sample 的记录 + 统计
// =============================================================================

struct Sample {
    id: u32,
    hole: [Card; 2],
    board: Vec<Card>,
    e1: f64,
    en: f64,
    /// river 专用：精确单对手 equity，用于校验 MC 采样器。
    e1_exact: Option<f64>,
}

fn street_code(s: StreetTag) -> u64 {
    match s {
        StreetTag::Flop => 0,
        StreetTag::Turn => 1,
        StreetTag::River => 2,
        StreetTag::Preflop => unreachable!(),
    }
}

fn street_name(s: StreetTag) -> &'static str {
    match s {
        StreetTag::Flop => "flop",
        StreetTag::Turn => "turn",
        StreetTag::River => "river",
        StreetTag::Preflop => "preflop",
    }
}

fn run_street(opts: &Opts, street: StreetTag, eval: &Arc<dyn HandEvaluator>) -> Vec<Sample> {
    let n_full = n_canonical_observation(street);
    let s = opts.samples.min(n_full);
    // 均匀铺开 [0, n_full)。
    let ids: Vec<u32> = (0..s)
        .map(|i| ((i as u64 * n_full as u64) / s as u64) as u32)
        .collect();
    let sc = street_code(street);
    let is_river = matches!(street, StreetTag::River);

    ids.into_par_iter()
        .map(|id| {
            let (board, hole) = nth_canonical_form(street, id);
            let mut rng = ChaCha20Rng::from_seed(mix_seed(opts.seed, sc, id));
            let e1 = multiway_equity_mc(hole, &board, 1, opts.mc_iters, &mut rng, &**eval);
            let en = multiway_equity_mc(hole, &board, opts.n_opp, opts.mc_iters, &mut rng, &**eval);
            let e1_exact = is_river.then(|| equity_river_exact(hole, &board, &**eval));
            Sample {
                id,
                hole,
                board,
                e1,
                en,
                e1_exact,
            }
        })
        .collect()
}

// =============================================================================
// 度量
// =============================================================================

fn avg_ranks(xs: &[f64]) -> Vec<f64> {
    let n = xs.len();
    let mut idx: Vec<usize> = (0..n).collect();
    idx.sort_by(|&a, &b| {
        xs[a]
            .partial_cmp(&xs[b])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut ranks = vec![0.0_f64; n];
    let mut i = 0;
    while i < n {
        let mut j = i;
        while j + 1 < n && xs[idx[j + 1]] == xs[idx[i]] {
            j += 1;
        }
        let avg = (i + j) as f64 / 2.0 + 1.0; // 1-based average rank
        for &p in &idx[i..=j] {
            ranks[p] = avg;
        }
        i = j + 1;
    }
    ranks
}

fn pearson(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len() as f64;
    let ma = a.iter().sum::<f64>() / n;
    let mb = b.iter().sum::<f64>() / n;
    let (mut cov, mut va, mut vb) = (0.0, 0.0, 0.0);
    for i in 0..a.len() {
        let da = a[i] - ma;
        let db = b[i] - mb;
        cov += da * db;
        va += da * da;
        vb += db * db;
    }
    if va == 0.0 || vb == 0.0 {
        return f64::NAN;
    }
    cov / (va.sqrt() * vb.sqrt())
}

/// 分位桶跨桶率：按 e1-rank / eN-rank 各分 B 个等量桶，落不同桶的比例。
fn quantile_crossing(rank_a: &[f64], rank_b: &[f64], bins: usize) -> f64 {
    let n = rank_a.len();
    let bin_of = |r: f64| -> usize {
        let b = (((r - 1.0) / n as f64) * bins as f64).floor() as usize;
        b.min(bins - 1)
    };
    let mut crossed = 0usize;
    for i in 0..n {
        let ba = bin_of(rank_a[i]);
        let bb = bin_of(rank_b[i]);
        if ba != bb {
            crossed += 1;
        }
    }
    crossed as f64 / n as f64
}

fn card_str(c: Card) -> String {
    const R: &[u8; 13] = b"23456789TJQKA";
    const S: &[u8; 4] = b"cdhs";
    let v = c.to_u8();
    format!(
        "{}{}",
        R[(v / 4) as usize] as char,
        S[(v % 4) as usize] as char
    )
}

fn cards_str(cs: &[Card]) -> String {
    cs.iter().map(|c| card_str(*c)).collect::<Vec<_>>().join("")
}

fn report_street(opts: &Opts, street: StreetTag, samples: &[Sample]) {
    let n = samples.len();
    let e1: Vec<f64> = samples.iter().map(|s| s.e1).collect();
    let en: Vec<f64> = samples.iter().map(|s| s.en).collect();
    let r1 = avg_ranks(&e1);
    let rn = avg_ranks(&en);

    let spearman = pearson(&r1, &rn);
    let pear = pearson(&e1, &en);
    let mean_e1 = e1.iter().sum::<f64>() / n as f64;
    let mean_en = en.iter().sum::<f64>() / n as f64;
    let mean_abs_diff = samples.iter().map(|s| (s.e1 - s.en).abs()).sum::<f64>() / n as f64;
    let crossing = quantile_crossing(&r1, &rn, QUANTILE_BINS);

    println!(
        "\n================ street = {} ================",
        street_name(street)
    );
    println!(
        "samples={n}  mc_iters={}  n_opp={}",
        opts.mc_iters, opts.n_opp
    );
    println!("Spearman(e1, e{0}) = {1:.4}", opts.n_opp, spearman);
    println!("Pearson (e1, e{0})  = {1:.4}", opts.n_opp, pear);
    println!(
        "mean e1 = {mean_e1:.4}   mean e{} = {mean_en:.4}   (dilution)",
        opts.n_opp
    );
    println!("mean |e1 - e{0}| = {1:.4}", opts.n_opp, mean_abs_diff);
    println!(
        "quantile crossing rate (B={QUANTILE_BINS} 等量桶) = {:.3}  ({} / {n} 落不同桶)",
        crossing,
        (crossing * n as f64).round() as usize
    );

    // river：MC 采样器校验
    if matches!(street, StreetTag::River) {
        let mut diffs: Vec<f64> = samples
            .iter()
            .take(RIVER_VALIDATE_N)
            .filter_map(|s| s.e1_exact.map(|ex| (s.e1 - ex).abs()))
            .collect();
        if !diffs.is_empty() {
            diffs.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let max = *diffs.last().unwrap();
            let mean = diffs.iter().sum::<f64>() / diffs.len() as f64;
            println!(
                "[sampler check] e1_mc vs equity_river_exact over {} samples: mean|Δ|={mean:.4} max|Δ|={max:.4}",
                diffs.len()
            );
        }
    }

    // e1-八分位压缩表
    println!(
        "\n  e1-octile 压缩表（每段 e1 区间内 e{} 的分布）:",
        opts.n_opp
    );
    println!(
        "  octile |  e1 范围        |  mean e1 | mean e{0} | min e{0} | max e{0} | count",
        opts.n_opp
    );
    let order = {
        let mut idx: Vec<usize> = (0..n).collect();
        idx.sort_by(|&a, &b| e1[a].partial_cmp(&e1[b]).unwrap());
        idx
    };
    for oc in 0..OCTILES {
        let lo = oc * n / OCTILES;
        let hi = ((oc + 1) * n / OCTILES).max(lo + 1).min(n);
        let seg = &order[lo..hi];
        let cnt = seg.len();
        if cnt == 0 {
            continue;
        }
        let e1_lo = e1[seg[0]];
        let e1_hi = e1[seg[cnt - 1]];
        let me1 = seg.iter().map(|&i| e1[i]).sum::<f64>() / cnt as f64;
        let men = seg.iter().map(|&i| en[i]).sum::<f64>() / cnt as f64;
        let mn = seg.iter().map(|&i| en[i]).fold(f64::INFINITY, f64::min);
        let mx = seg.iter().map(|&i| en[i]).fold(f64::NEG_INFINITY, f64::max);
        println!(
            "    {oc:2}   | [{e1_lo:.3}, {e1_hi:.3}] |  {me1:.3}  |  {men:.3}  |  {mn:.3}  |  {mx:.3}  | {cnt}",
        );
    }

    // 重排序 top：按 percentile-rank 位移
    let mut disp: Vec<(usize, f64)> = (0..n)
        .map(|i| (i, (rn[i] - r1[i]) / n as f64)) // +：多人下相对升；−：相对降
        .collect();
    disp.sort_by(|a, b| b.1.abs().partial_cmp(&a.1.abs()).unwrap());

    println!(
        "\n  重排序 top {TOP_DIVERGERS}（Δpct = e{} 百分位 − e1 百分位；+升 −降）:",
        opts.n_opp
    );
    println!(
        "  hole  board            |   e1   |  e{0}   | Δpct",
        opts.n_opp
    );
    for &(i, d) in disp.iter().take(TOP_DIVERGERS) {
        let s = &samples[i];
        println!(
            "  {:<5} {:<16} | {:.3} | {:.3} | {:+.3}",
            cards_str(&s.hole),
            cards_str(&s.board),
            s.e1,
            s.en,
            d,
        );
    }
}

// =============================================================================
// CSV
// =============================================================================

fn write_csv(path: &PathBuf, all: &[(StreetTag, Vec<Sample>)]) -> std::io::Result<()> {
    use std::io::Write;
    let mut w = std::io::BufWriter::new(std::fs::File::create(path)?);
    writeln!(w, "street,canonical_id,hole,board,e1,eN,e1_exact")?;
    for (street, samples) in all {
        for s in samples {
            writeln!(
                w,
                "{},{},{},{},{:.6},{:.6},{}",
                street_name(*street),
                s.id,
                cards_str(&s.hole),
                cards_str(&s.board),
                s.e1,
                s.en,
                s.e1_exact.map(|x| format!("{x:.6}")).unwrap_or_default(),
            )?;
        }
    }
    w.flush()?;
    Ok(())
}

// =============================================================================
// main
// =============================================================================

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let opts = match parse_args(&args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: {e}");
            print_usage(&args);
            return ExitCode::from(2);
        }
    };

    if let Some(n) = opts.threads {
        if let Err(e) = rayon::ThreadPoolBuilder::new()
            .num_threads(n)
            .build_global()
        {
            eprintln!("warning: failed to set rayon pool to {n}: {e}");
        }
    }

    let t0 = Instant::now();
    let eval: Arc<dyn HandEvaluator> = Arc::new(NaiveHandEvaluator);

    println!("# multiway_equity_probe (S3 实验 A)");
    println!(
        "seed=0x{:016x}  samples/street={}  mc_iters={}  n_opp={}",
        opts.seed, opts.samples, opts.mc_iters, opts.n_opp
    );

    let mut all: Vec<(StreetTag, Vec<Sample>)> = Vec::new();
    for &street in &opts.streets {
        let t = Instant::now();
        let samples = run_street(&opts, street, &eval);
        eprintln!(
            "[multiway_equity_probe] {} {} samples wall={:?}",
            street_name(street),
            samples.len(),
            t.elapsed()
        );
        report_street(&opts, street, &samples);
        all.push((street, samples));
    }

    if let Some(csv) = &opts.csv {
        match write_csv(csv, &all) {
            Ok(()) => eprintln!("[multiway_equity_probe] csv → {}", csv.display()),
            Err(e) => eprintln!("error: csv write failed: {e}"),
        }
    }

    println!("\n# total wall = {:?}", t0.elapsed());
    ExitCode::from(0)
}
