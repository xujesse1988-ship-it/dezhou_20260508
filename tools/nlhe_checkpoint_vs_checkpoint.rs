//! 两个简化 heads-up NLHE checkpoint 互相对弈（head-to-head）。
//!
//! 加载 checkpoint A / B 两份 `EsMccfrTrainer`，双座位轮换各打 `--hands-per-seat`
//! 手，统计 A 相对 B 的 mbb/game + 95% 置信区间 + 分座位胜率。对弈与统计逻辑直接
//! 复用 `tests/nlhe_h3_eval.rs` 里已被 H3 评测套件验证过的实现，不引入新算法。
//!
//! checkpoint 各约 8.5 GB，加载两份的常驻 + 瞬时峰值约 30–40 GB，需在 ≥ 64 GB
//! 内存的机器上跑（vultr 7.7 GB 跑不动）。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use serde::Serialize;

use poker::training::game::{Game, NodeKind};
use poker::training::nlhe::{SimplifiedNlheAction, SimplifiedNlheState};
use poker::training::sampling::sample_discrete;
use poker::training::{EsMccfrTrainer, Trainer};
use poker::{BucketTable, ChaCha20Rng, InfoSetId, RngSource, SeatId, SimplifiedNlheGame};

/// strategy_sum 全零（off-policy 未学过）的 infoset 退化为 `current_strategy`
/// （regret matching），其余用 `average_strategy`。与 H3 评测套件同款 hybrid。
#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
enum FallbackPolicy {
    Average,
    Current,
    Hybrid,
}

impl FallbackPolicy {
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "average" | "avg" => Ok(FallbackPolicy::Average),
            "current" | "cur" => Ok(FallbackPolicy::Current),
            "hybrid" => Ok(FallbackPolicy::Hybrid),
            other => Err(format!(
                "unknown --fallback-policy {other:?}; expected average|current|hybrid"
            )),
        }
    }
    fn slug(self) -> &'static str {
        match self {
            FallbackPolicy::Average => "average",
            FallbackPolicy::Current => "current",
            FallbackPolicy::Hybrid => "hybrid",
        }
    }
}

struct Args {
    checkpoint_a: PathBuf,
    checkpoint_b: PathBuf,
    bucket_table: PathBuf,
    hands_per_seat: u64,
    seed: u64,
    max_actions_per_hand: usize,
    bb_chips: f64,
    fallback_policy: FallbackPolicy,
    output: Option<PathBuf>,
}

fn usage() -> String {
    "\
usage: nlhe_checkpoint_vs_checkpoint \\
    --checkpoint-a <A.ckpt> --checkpoint-b <B.ckpt> \\
    --bucket-table <bucket.bin> \\
    [--hands-per-seat 50000] [--seed 0xC4EC4E0F] \\
    [--max-actions-per-hand 512] [--bb-chips 100.0] \\
    [--fallback-policy hybrid|average|current] [--out report.md]

报告里所有 mbb/game 均从 checkpoint-a 视角：正数 = A 净赢 B。"
        .to_string()
}

fn parse_args() -> Result<Args, String> {
    let mut checkpoint_a: Option<PathBuf> = None;
    let mut checkpoint_b: Option<PathBuf> = None;
    let mut bucket_table: Option<PathBuf> = None;
    let mut hands_per_seat: u64 = 50_000;
    let mut seed: u64 = 0xC4EC_4E0F;
    let mut max_actions_per_hand: usize = 512;
    let mut bb_chips: f64 = 100.0;
    let mut fallback_policy = FallbackPolicy::Hybrid;
    let mut output: Option<PathBuf> = None;

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut next = || {
            it.next()
                .ok_or_else(|| format!("missing value after {arg}"))
        };
        match arg.as_str() {
            "-h" | "--help" => return Err(usage()),
            "--checkpoint-a" => checkpoint_a = Some(PathBuf::from(next()?)),
            "--checkpoint-b" => checkpoint_b = Some(PathBuf::from(next()?)),
            "--bucket-table" => bucket_table = Some(PathBuf::from(next()?)),
            "--hands-per-seat" => {
                hands_per_seat = next()?
                    .parse()
                    .map_err(|e| format!("--hands-per-seat: {e}"))?
            }
            "--seed" => seed = parse_u64(&next()?).map_err(|e| format!("--seed: {e}"))?,
            "--max-actions-per-hand" => {
                max_actions_per_hand = next()?
                    .parse()
                    .map_err(|e| format!("--max-actions-per-hand: {e}"))?
            }
            "--bb-chips" => bb_chips = next()?.parse().map_err(|e| format!("--bb-chips: {e}"))?,
            "--fallback-policy" => fallback_policy = FallbackPolicy::from_str(&next()?)?,
            "--out" => output = Some(PathBuf::from(next()?)),
            other => return Err(format!("unknown arg {other:?}\n\n{}", usage())),
        }
    }

    Ok(Args {
        checkpoint_a: checkpoint_a.ok_or("missing --checkpoint-a")?,
        checkpoint_b: checkpoint_b.ok_or("missing --checkpoint-b")?,
        bucket_table: bucket_table.ok_or("missing --bucket-table")?,
        hands_per_seat,
        seed,
        max_actions_per_hand,
        bb_chips,
        fallback_policy,
        output,
    })
}

fn parse_u64(s: &str) -> Result<u64, String> {
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).map_err(|e| e.to_string())
    } else {
        s.parse()
            .map_err(|e: std::num::ParseIntError| e.to_string())
    }
}

#[derive(Clone, Debug, Serialize)]
struct H2hReport {
    checkpoint_a: String,
    checkpoint_b: String,
    bucket_table: String,
    bucket_table_blake3: String,
    a_update_count: u64,
    b_update_count: u64,
    fallback_policy: FallbackPolicy,
    hands: u64,
    hands_per_seat: u64,
    seed: u64,
    bb_chips: f64,
    a_total_chips: f64,
    a_mbb_per_game: f64,
    standard_error_mbb_per_game: f64,
    ci95_low_mbb_per_game: f64,
    ci95_high_mbb_per_game: f64,
    a_as_sb_mbb_per_game: f64,
    a_as_bb_mbb_per_game: f64,
    wall_seconds: f64,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("[nlhe_checkpoint_vs_checkpoint] failed: {e}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    if args.hands_per_seat == 0 {
        return Err("--hands-per-seat must be > 0".to_string());
    }
    if args.max_actions_per_hand == 0 {
        return Err("--max-actions-per-hand must be > 0".to_string());
    }
    if args.bb_chips <= 0.0 || !args.bb_chips.is_finite() {
        return Err("--bb-chips must be a positive finite number".to_string());
    }

    let table = Arc::new(BucketTable::open(&args.bucket_table).map_err(|e| {
        format!(
            "BucketTable::open({}) failed: {e:?}",
            args.bucket_table.display()
        )
    })?);
    let bucket_hash = hex32(&table.content_hash());
    eprintln!("[h2h] bucket_table   = {}", args.bucket_table.display());
    eprintln!("[h2h] bucket_blake3  = {bucket_hash}");

    eprintln!("[h2h] loading A = {}", args.checkpoint_a.display());
    let trainer_a = load_checkpoint(&args.checkpoint_a, Arc::clone(&table))?;
    eprintln!("[h2h]   A update_count = {}", trainer_a.update_count());
    eprintln!("[h2h] loading B = {}", args.checkpoint_b.display());
    let trainer_b = load_checkpoint(&args.checkpoint_b, Arc::clone(&table))?;
    eprintln!("[h2h]   B update_count = {}", trainer_b.update_count());

    eprintln!(
        "[h2h] fallback_policy = {} | hands_per_seat = {} (×2 seats) | seed = {:#x}",
        args.fallback_policy.slug(),
        args.hands_per_seat,
        args.seed
    );

    let t0 = Instant::now();
    let report = evaluate_head_to_head(Arc::clone(&table), &trainer_a, &trainer_b, &args)?;
    let wall = t0.elapsed().as_secs_f64();

    let json = H2hReport {
        checkpoint_a: args.checkpoint_a.display().to_string(),
        checkpoint_b: args.checkpoint_b.display().to_string(),
        bucket_table: args.bucket_table.display().to_string(),
        bucket_table_blake3: bucket_hash,
        a_update_count: trainer_a.update_count(),
        b_update_count: trainer_b.update_count(),
        fallback_policy: args.fallback_policy,
        wall_seconds: wall,
        ..report
    };

    let md = render_markdown(&json);
    print!("{md}");
    if let Some(ref out) = args.output {
        fs::write(out, &md).map_err(|e| format!("write {} failed: {e}", out.display()))?;
        let json_path = out.with_extension("json");
        let json_text =
            serde_json::to_string_pretty(&json).map_err(|e| format!("serialize json: {e}"))?;
        fs::write(&json_path, json_text)
            .map_err(|e| format!("write {} failed: {e}", json_path.display()))?;
        eprintln!("[h2h] wrote {} + {}", out.display(), json_path.display());
    }
    Ok(())
}

fn load_checkpoint(
    path: &Path,
    table: Arc<BucketTable>,
) -> Result<EsMccfrTrainer<SimplifiedNlheGame>, String> {
    if !path.exists() {
        return Err(format!("checkpoint {} 不存在", path.display()));
    }
    let game = SimplifiedNlheGame::new(table)
        .map_err(|e| format!("SimplifiedNlheGame::new failed: {e:?}"))?;
    <EsMccfrTrainer<SimplifiedNlheGame> as Trainer<SimplifiedNlheGame>>::load_checkpoint(path, game)
        .map_err(|e| format!("load_checkpoint({}) failed: {e:?}", path.display()))
}

// ---- 以下对弈 / 统计逻辑复用 tests/nlhe_h3_eval.rs（H3 评测套件已验证）----

fn evaluate_head_to_head(
    table: Arc<BucketTable>,
    trainer_a: &EsMccfrTrainer<SimplifiedNlheGame>,
    trainer_b: &EsMccfrTrainer<SimplifiedNlheGame>,
    args: &Args,
) -> Result<H2hReport, String> {
    let game = SimplifiedNlheGame::new(table)
        .map_err(|e| format!("SimplifiedNlheGame::new failed: {e:?}"))?;
    let mut all_a_pnl = Vec::with_capacity((args.hands_per_seat * 2) as usize);
    let mut a_as_sb_total = 0.0;
    let mut a_as_bb_total = 0.0;

    // a_seat 轮换：A 先坐 SB(0) 打 hands_per_seat 手，再坐 BB(1) 打 hands_per_seat 手。
    for a_seat in [SeatId(0), SeatId(1)] {
        for hand_idx in 0..args.hands_per_seat {
            let seed = mix3(args.seed, a_seat.0 as u64, hand_idx);
            let mut rng = ChaCha20Rng::from_seed(seed);
            let root = game.root(&mut rng);
            let terminal = rollout_head_to_head(
                root,
                a_seat,
                trainer_a,
                trainer_b,
                args.fallback_policy,
                &mut rng,
                args.max_actions_per_hand,
            )?;
            let pnl = SimplifiedNlheGame::payoff(&terminal, a_seat.0);
            if a_seat == SeatId(0) {
                a_as_sb_total += pnl;
            } else {
                a_as_bb_total += pnl;
            }
            all_a_pnl.push(pnl);
        }
    }

    let hands = all_a_pnl.len() as u64;
    let (mean, se) = sample_mean_se(&all_a_pnl);
    let scale = 1000.0 / args.bb_chips;
    let mean_mbb = mean * scale;
    let se_mbb = se * scale;
    Ok(H2hReport {
        // 以下字段在 run() 中用 `..` 覆盖填入。
        checkpoint_a: String::new(),
        checkpoint_b: String::new(),
        bucket_table: String::new(),
        bucket_table_blake3: String::new(),
        a_update_count: 0,
        b_update_count: 0,
        fallback_policy: args.fallback_policy,
        wall_seconds: 0.0,
        hands,
        hands_per_seat: args.hands_per_seat,
        seed: args.seed,
        bb_chips: args.bb_chips,
        a_total_chips: all_a_pnl.iter().sum(),
        a_mbb_per_game: mean_mbb,
        standard_error_mbb_per_game: se_mbb,
        ci95_low_mbb_per_game: mean_mbb - 1.96 * se_mbb,
        ci95_high_mbb_per_game: mean_mbb + 1.96 * se_mbb,
        a_as_sb_mbb_per_game: (a_as_sb_total / args.hands_per_seat as f64) * scale,
        a_as_bb_mbb_per_game: (a_as_bb_total / args.hands_per_seat as f64) * scale,
    })
}

#[allow(clippy::too_many_arguments)]
fn rollout_head_to_head(
    mut state: SimplifiedNlheState,
    a_seat: SeatId,
    trainer_a: &EsMccfrTrainer<SimplifiedNlheGame>,
    trainer_b: &EsMccfrTrainer<SimplifiedNlheGame>,
    policy: FallbackPolicy,
    rng: &mut dyn RngSource,
    max_actions: usize,
) -> Result<SimplifiedNlheState, String> {
    for _ in 0..max_actions {
        match SimplifiedNlheGame::current(&state) {
            NodeKind::Terminal => return Ok(state),
            NodeKind::Chance => {
                let dist = SimplifiedNlheGame::chance_distribution(&state);
                let action = sample_discrete(&dist, rng);
                state = SimplifiedNlheGame::next(state, action, rng);
            }
            NodeKind::Player(actor) => {
                let trainer = if SeatId(actor) == a_seat {
                    trainer_a
                } else {
                    trainer_b
                };
                let action = sample_action(&state, actor, trainer, policy, rng)?;
                state = SimplifiedNlheGame::next(state, action, rng);
            }
        }
    }
    Err(format!(
        "head-to-head rollout did not terminate within {max_actions} actions"
    ))
}

fn sample_action(
    state: &SimplifiedNlheState,
    actor: u8,
    trainer: &EsMccfrTrainer<SimplifiedNlheGame>,
    policy: FallbackPolicy,
    rng: &mut dyn RngSource,
) -> Result<SimplifiedNlheAction, String> {
    let actions = SimplifiedNlheGame::legal_actions(state);
    if actions.is_empty() {
        return Err(format!(
            "empty legal actions at non-terminal state: {:?}",
            SimplifiedNlheGame::current(state)
        ));
    }
    let info = SimplifiedNlheGame::info_set(state, actor);
    let raw = strategy_for(trainer, &info, policy);
    let dist = strategy_distribution(&actions, &raw)?;
    Ok(sample_discrete(&dist, rng))
}

fn strategy_for(
    trainer: &EsMccfrTrainer<SimplifiedNlheGame>,
    info: &InfoSetId,
    policy: FallbackPolicy,
) -> Vec<f64> {
    match policy {
        FallbackPolicy::Average => trainer.average_strategy(info),
        FallbackPolicy::Current => trainer.current_strategy(info),
        FallbackPolicy::Hybrid => {
            let degenerate = match trainer.strategy_sum().inner().get(info) {
                None => true,
                Some(v) => v.iter().sum::<f64>() <= 0.0,
            };
            if degenerate {
                trainer.current_strategy(info)
            } else {
                trainer.average_strategy(info)
            }
        }
    }
}

fn strategy_distribution(
    actions: &[SimplifiedNlheAction],
    raw: &[f64],
) -> Result<Vec<(SimplifiedNlheAction, f64)>, String> {
    if raw.is_empty() {
        let p = 1.0 / actions.len() as f64;
        return Ok(actions.iter().copied().map(|a| (a, p)).collect());
    }
    if raw.len() != actions.len() {
        return Err(format!(
            "strategy length mismatch: expected {}, got {}",
            actions.len(),
            raw.len()
        ));
    }
    let mut sum = 0.0;
    for (idx, &p) in raw.iter().enumerate() {
        if !p.is_finite() || p < 0.0 {
            return Err(format!("invalid strategy probability at {idx}: {p}"));
        }
        sum += p;
    }
    if !sum.is_finite() || sum <= 0.0 {
        return Err(format!("invalid strategy sum: {sum}"));
    }
    Ok(actions
        .iter()
        .copied()
        .zip(raw.iter().copied())
        .filter(|(_, p)| *p > 0.0)
        .map(|(action, p)| (action, p / sum))
        .collect())
}

fn sample_mean_se(xs: &[f64]) -> (f64, f64) {
    let n = xs.len() as f64;
    let mean = xs.iter().sum::<f64>() / n;
    if xs.len() == 1 {
        return (mean, 0.0);
    }
    let var = xs
        .iter()
        .map(|x| {
            let d = x - mean;
            d * d
        })
        .sum::<f64>()
        / (n - 1.0);
    (mean, var.sqrt() / n.sqrt())
}

fn mix3(seed: u64, a: u64, b: u64) -> u64 {
    mix64(seed ^ a.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ b.wrapping_mul(0xBF58_476D_1CE4_E5B9))
}

fn mix64(mut x: u64) -> u64 {
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

fn hex32(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn render_markdown(r: &H2hReport) -> String {
    format!(
        "# checkpoint head-to-head（mbb/game 从 A 视角，正数 = A 赢 B）\n\
         \n\
         - checkpoint A : `{a}`（update_count = {auc}）\n\
         - checkpoint B : `{b}`（update_count = {buc}）\n\
         - bucket_table : `{bt}`（blake3 = {bth}）\n\
         - fallback_policy = {fp} | seed = {seed:#x} | bb_chips = {bb}\n\
         - hands = {hands}（每座 {hps} 手 ×2 座位）| wall = {wall:.1}s\n\
         \n\
         | 指标 | 值 |\n\
         |---|---|\n\
         | A mbb/game | {mbb:.2} |\n\
         | 95% CI | [{lo:.2}, {hi:.2}] |\n\
         | SE | {se:.2} |\n\
         | A 作 SB | {sb:.2} |\n\
         | A 作 BB | {bbm:.2} |\n\
         | A 净筹码 | {chips:.1} |\n",
        a = r.checkpoint_a,
        auc = r.a_update_count,
        b = r.checkpoint_b,
        buc = r.b_update_count,
        bt = r.bucket_table,
        bth = r.bucket_table_blake3,
        fp = r.fallback_policy.slug(),
        seed = r.seed,
        bb = r.bb_chips,
        hands = r.hands,
        hps = r.hands_per_seat,
        wall = r.wall_seconds,
        mbb = r.a_mbb_per_game,
        lo = r.ci95_low_mbb_per_game,
        hi = r.ci95_high_mbb_per_game,
        se = r.standard_error_mbb_per_game,
        sb = r.a_as_sb_mbb_per_game,
        bbm = r.a_as_bb_mbb_per_game,
        chips = r.a_total_chips,
    )
}
