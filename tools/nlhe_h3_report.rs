//! H3 简化 heads-up NLHE blueprint 评测报告工具。
//!
//! 支持从 checkpoint 评测，也支持现场训练一段 update 后评测。输出 Markdown 与
//! 同名 JSON，供 `docs/status.md` / release 记录引用。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use blake3::Hasher;
use serde::Serialize;

use poker::training::game::{Game, NodeKind};
use poker::training::nlhe::{SimplifiedNlheGame, SimplifiedNlheState};
use poker::training::sampling::sample_discrete;
use poker::training::{
    estimate_simplified_nlhe_lbr, evaluate_blueprint_vs_baseline, EsMccfrTrainer,
    NlheBaselinePolicy, NlheEvaluationConfig, NlheEvaluationReport, NlheLbrConfig, NlheLbrReport,
    Trainer,
};
use poker::{BucketTable, ChaCha20Rng, InfoSetId, RngSource};

#[derive(Debug)]
struct Args {
    artifact: PathBuf,
    checkpoint: Option<PathBuf>,
    curve_checkpoints: Vec<(String, PathBuf)>,
    train_updates: u64,
    seed: u64,
    threads: usize,
    eval_hands_per_seat: u64,
    lbr_probes: u64,
    lbr_rollouts: u64,
    output: PathBuf,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            artifact: PathBuf::from(
                "artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin",
            ),
            checkpoint: None,
            curve_checkpoints: Vec::new(),
            train_updates: 0,
            seed: 0x4833_5f4e_4c48_455f,
            threads: 1,
            eval_hands_per_seat: 1_000,
            lbr_probes: 1_000,
            lbr_rollouts: 16,
            output: PathBuf::from("artifacts/h3_nlhe_report.md"),
        }
    }
}

#[derive(Serialize)]
struct H3JsonReport {
    artifact: String,
    bucket_table_blake3: String,
    checkpoint: Option<String>,
    update_count: u64,
    strategy_blake3: String,
    evaluations: Vec<NlheEvaluationReport>,
    lbr: NlheLbrReport,
    lbr_curve: Vec<LbrCurvePoint>,
}

#[derive(Clone, Debug, Serialize)]
struct LbrCurvePoint {
    label: String,
    update_count: u64,
    strategy_blake3: String,
    mean_best_response_chips: f64,
    standard_error_chips: f64,
    probes_used: u64,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("[nlhe_h3_report] failed: {e}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    if args.threads == 0 {
        return Err("--threads must be > 0".to_string());
    }
    if args.eval_hands_per_seat == 0 {
        return Err("--eval-hands-per-seat must be > 0".to_string());
    }
    if args.lbr_probes == 0 || args.lbr_rollouts == 0 {
        return Err("--lbr-probes and --lbr-rollouts must be > 0".to_string());
    }

    let table = Arc::new(BucketTable::open(&args.artifact).map_err(|e| {
        format!(
            "BucketTable::open({}) failed: {e:?}",
            args.artifact.display()
        )
    })?);
    let bucket_hash = hex32(&table.content_hash());
    eprintln!(
        "[nlhe_h3_report] artifact       = {}",
        args.artifact.display()
    );
    eprintln!("[nlhe_h3_report] bucket_blake3  = {bucket_hash}");

    let game = SimplifiedNlheGame::new(Arc::clone(&table))
        .map_err(|e| format!("SimplifiedNlheGame::new failed: {e:?}"))?;
    let mut trainer = if let Some(ref checkpoint) = args.checkpoint {
        eprintln!("[nlhe_h3_report] checkpoint     = {}", checkpoint.display());
        <EsMccfrTrainer<SimplifiedNlheGame> as Trainer<SimplifiedNlheGame>>::load_checkpoint(
            checkpoint, game,
        )
        .map_err(|e| format!("load checkpoint {} failed: {e:?}", checkpoint.display()))?
    } else {
        EsMccfrTrainer::new(game, args.seed)
    };

    if args.train_updates > 0 {
        train_inline(&mut trainer, args.train_updates, args.seed, args.threads)?;
    }
    let eval_game = SimplifiedNlheGame::new(Arc::clone(&table))
        .map_err(|e| format!("SimplifiedNlheGame::new for eval failed: {e:?}"))?;

    let eval_cfg = NlheEvaluationConfig {
        hands_per_seat: args.eval_hands_per_seat,
        seed: args.seed ^ 0x4556_414c,
        max_actions_per_hand: 512,
    };
    let lbr_cfg = NlheLbrConfig {
        probes: args.lbr_probes,
        rollouts_per_action: args.lbr_rollouts,
        seed: args.seed ^ 0x4c42_5200,
        max_actions_per_probe: 128,
        max_actions_per_rollout: 512,
    };

    let strategy = |info: &InfoSetId, _n: usize| trainer.average_strategy(info);
    let mut evaluations = Vec::new();
    for baseline in [
        NlheBaselinePolicy::Random,
        NlheBaselinePolicy::CallStation,
        NlheBaselinePolicy::OverlyTight,
    ] {
        eprintln!("[nlhe_h3_report] evaluating baseline {}", baseline.label());
        evaluations.push(
            evaluate_blueprint_vs_baseline(&eval_game, &strategy, baseline, &eval_cfg)
                .map_err(|e| format!("evaluate {} failed: {e:?}", baseline.label()))?,
        );
    }

    eprintln!("[nlhe_h3_report] estimating LBR proxy");
    let lbr = estimate_simplified_nlhe_lbr(&eval_game, &strategy, &lbr_cfg)
        .map_err(|e| format!("LBR proxy failed: {e:?}"))?;
    let strategy_hash = strategy_hash(&trainer, &eval_game);

    let mut lbr_curve = Vec::new();
    lbr_curve.push(uniform_lbr_curve_point(Arc::clone(&table), &lbr_cfg)?);
    for (label, path) in &args.curve_checkpoints {
        lbr_curve.push(load_lbr_curve_point(
            Arc::clone(&table),
            label.clone(),
            path,
            &lbr_cfg,
        )?);
    }
    lbr_curve.push(LbrCurvePoint {
        label: format!("active-{}", trainer.update_count()),
        update_count: trainer.update_count(),
        strategy_blake3: strategy_hash.clone(),
        mean_best_response_chips: lbr.mean_best_response_chips,
        standard_error_chips: lbr.standard_error_chips,
        probes_used: lbr.probes_used,
    });

    let json = H3JsonReport {
        artifact: args.artifact.display().to_string(),
        bucket_table_blake3: bucket_hash,
        checkpoint: args.checkpoint.as_ref().map(|p| p.display().to_string()),
        update_count: trainer.update_count(),
        strategy_blake3: strategy_hash,
        evaluations,
        lbr,
        lbr_curve,
    };

    write_reports(&args.output, &json)?;
    eprintln!("[nlhe_h3_report] wrote {}", args.output.display());
    eprintln!(
        "[nlhe_h3_report] wrote {}",
        args.output.with_extension("json").display()
    );
    Ok(())
}

fn train_inline(
    trainer: &mut EsMccfrTrainer<SimplifiedNlheGame>,
    target_updates: u64,
    seed: u64,
    threads: usize,
) -> Result<(), String> {
    let start = trainer.update_count();
    if start >= target_updates {
        return Ok(());
    }
    let mut single_rng = ChaCha20Rng::from_seed(seed);
    let mut rng_pool: Vec<Box<dyn RngSource>> = (0..threads as u64)
        .map(|tid| {
            let seeded = mix3(seed, 0x5245_504f_5254, tid);
            Box::new(ChaCha20Rng::from_seed(seeded)) as Box<dyn RngSource>
        })
        .collect();
    let t0 = Instant::now();
    while trainer.update_count() < target_updates {
        let remaining = target_updates - trainer.update_count();
        if threads == 1 {
            trainer
                .step(&mut single_rng)
                .map_err(|e| format!("inline step failed: {e:?}"))?;
        } else {
            let n = threads.min(remaining as usize);
            trainer
                .step_parallel(&mut rng_pool, n)
                .map_err(|e| format!("inline step_parallel failed: {e:?}"))?;
        }
    }
    let elapsed = t0.elapsed().as_secs_f64();
    let throughput = (trainer.update_count() - start) as f64 / elapsed.max(1e-9);
    eprintln!(
        "[nlhe_h3_report] trained inline {} -> {} in {:.1}s ({throughput:.0}/s)",
        start,
        trainer.update_count(),
        elapsed
    );
    Ok(())
}

fn uniform_lbr_curve_point(
    table: Arc<BucketTable>,
    cfg: &NlheLbrConfig,
) -> Result<LbrCurvePoint, String> {
    let game = SimplifiedNlheGame::new(table)
        .map_err(|e| format!("SimplifiedNlheGame::new for uniform curve failed: {e:?}"))?;
    let uniform = |_info: &InfoSetId, _n: usize| Vec::new();
    let report = estimate_simplified_nlhe_lbr(&game, &uniform, cfg)
        .map_err(|e| format!("uniform LBR proxy failed: {e:?}"))?;
    Ok(LbrCurvePoint {
        label: "uniform-0".to_string(),
        update_count: 0,
        strategy_blake3: "uniform-empty".to_string(),
        mean_best_response_chips: report.mean_best_response_chips,
        standard_error_chips: report.standard_error_chips,
        probes_used: report.probes_used,
    })
}

fn load_lbr_curve_point(
    table: Arc<BucketTable>,
    label: String,
    path: &Path,
    cfg: &NlheLbrConfig,
) -> Result<LbrCurvePoint, String> {
    let load_game = SimplifiedNlheGame::new(Arc::clone(&table))
        .map_err(|e| format!("SimplifiedNlheGame::new for curve failed: {e:?}"))?;
    let trainer =
        <EsMccfrTrainer<SimplifiedNlheGame> as Trainer<SimplifiedNlheGame>>::load_checkpoint(
            path, load_game,
        )
        .map_err(|e| format!("load curve checkpoint {} failed: {e:?}", path.display()))?;
    let eval_game = SimplifiedNlheGame::new(table)
        .map_err(|e| format!("SimplifiedNlheGame::new for curve eval failed: {e:?}"))?;
    let strategy = |info: &InfoSetId, _n: usize| trainer.average_strategy(info);
    let report = estimate_simplified_nlhe_lbr(&eval_game, &strategy, cfg)
        .map_err(|e| format!("curve LBR proxy {label} failed: {e:?}"))?;
    Ok(LbrCurvePoint {
        label,
        update_count: trainer.update_count(),
        strategy_blake3: strategy_hash(&trainer, &eval_game),
        mean_best_response_chips: report.mean_best_response_chips,
        standard_error_chips: report.standard_error_chips,
        probes_used: report.probes_used,
    })
}

fn strategy_hash(
    trainer: &EsMccfrTrainer<SimplifiedNlheGame>,
    game: &SimplifiedNlheGame,
) -> String {
    let probes = collect_strategy_probes(game);
    let mut hasher = Hasher::new();
    hasher.update(&trainer.update_count().to_le_bytes());
    hasher.update(&(probes.len() as u64).to_le_bytes());
    for info in probes {
        let strategy = trainer.average_strategy(&info);
        hasher.update(&info.raw().to_le_bytes());
        hasher.update(&(strategy.len() as u32).to_le_bytes());
        for p in strategy {
            hasher.update(&p.to_le_bytes());
        }
    }
    hex32(&hasher.finalize().into())
}

fn collect_strategy_probes(game: &SimplifiedNlheGame) -> Vec<InfoSetId> {
    let mut rng = ChaCha20Rng::from_seed(0x4833_5052_4f42_4553);
    let mut state: SimplifiedNlheState = game.root(&mut rng);
    let mut probes = Vec::with_capacity(4096);
    for _ in 0..4096 {
        match SimplifiedNlheGame::current(&state) {
            NodeKind::Terminal => break,
            NodeKind::Chance => {
                let dist = SimplifiedNlheGame::chance_distribution(&state);
                let action = sample_discrete(&dist, &mut rng);
                state = SimplifiedNlheGame::next(state, action, &mut rng);
            }
            NodeKind::Player(actor) => {
                probes.push(SimplifiedNlheGame::info_set(&state, actor));
                let actions = SimplifiedNlheGame::legal_actions(&state);
                if actions.is_empty() {
                    break;
                }
                let idx = (rng.next_u64() as usize) % actions.len();
                state = SimplifiedNlheGame::next(state, actions[idx], &mut rng);
            }
        }
    }
    probes
}

fn write_reports(path: &PathBuf, report: &H3JsonReport) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create report dir failed: {e}"))?;
    }
    let json_path = path.with_extension("json");
    let json = serde_json::to_string_pretty(report)
        .map_err(|e| format!("serialize JSON report failed: {e}"))?;
    fs::write(&json_path, json)
        .map_err(|e| format!("write {} failed: {e}", json_path.display()))?;
    fs::write(path, markdown_report(report))
        .map_err(|e| format!("write {} failed: {e}", path.display()))?;
    Ok(())
}

fn markdown_report(report: &H3JsonReport) -> String {
    let mut out = String::new();
    out.push_str("# H3 Simplified Heads-Up NLHE Report\n\n");
    out.push_str(&format!("- artifact: `{}`\n", report.artifact));
    out.push_str(&format!(
        "- bucket_table_blake3: `{}`\n",
        report.bucket_table_blake3
    ));
    out.push_str(&format!(
        "- checkpoint: `{}`\n",
        report.checkpoint.as_deref().unwrap_or("<inline/fresh>")
    ));
    out.push_str(&format!("- update_count: `{}`\n", report.update_count));
    out.push_str(&format!(
        "- strategy_blake3: `{}`\n\n",
        report.strategy_blake3
    ));

    out.push_str("## Baseline Evaluation\n\n");
    out.push_str(
        "| baseline | hands | mbb/g | SE | 95% CI low | 95% CI high | SB mbb/g | BB mbb/g |\n",
    );
    out.push_str("|---|---:|---:|---:|---:|---:|---:|---:|\n");
    for r in &report.evaluations {
        out.push_str(&format!(
            "| {} | {} | {:.3} | {:.3} | {:.3} | {:.3} | {:.3} | {:.3} |\n",
            r.baseline.label(),
            r.hands,
            r.mbb_per_game,
            r.standard_error_mbb_per_game,
            r.ci95_low_mbb_per_game,
            r.ci95_high_mbb_per_game,
            r.sb_mbb_per_game,
            r.bb_mbb_per_game
        ));
    }

    out.push_str("\n## LBR Proxy\n\n");
    out.push_str("H3 local best-response proxy only; not formal exploitability.\n\n");
    out.push_str(&format!(
        "- mean_best_response_chips: `{:.6}`\n- standard_error_chips: `{:.6}`\n- probes_used: `{}` / `{}`\n\n",
        report.lbr.mean_best_response_chips,
        report.lbr.standard_error_chips,
        report.lbr.probes_used,
        report.lbr.probes_requested
    ));
    out.push_str("| label | updates | mean BR chips | SE chips | probes | strategy hash |\n");
    out.push_str("|---|---:|---:|---:|---:|---|\n");
    for p in &report.lbr_curve {
        out.push_str(&format!(
            "| {} | {} | {:.6} | {:.6} | {} | `{}` |\n",
            p.label,
            p.update_count,
            p.mean_best_response_chips,
            p.standard_error_chips,
            p.probes_used,
            p.strategy_blake3
        ));
    }
    out
}

fn parse_args() -> Result<Args, String> {
    let mut out = Args::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--artifact" | "--bucket-table" => {
                out.artifact = PathBuf::from(next_value(&mut args, &arg)?)
            }
            "--checkpoint" => out.checkpoint = Some(PathBuf::from(next_value(&mut args, &arg)?)),
            "--curve-checkpoint" => {
                let raw = next_value(&mut args, &arg)?;
                out.curve_checkpoints.push(parse_curve_checkpoint(&raw));
            }
            "--train-updates" => out.train_updates = parse_u64(&next_value(&mut args, &arg)?)?,
            "--seed" => out.seed = parse_u64(&next_value(&mut args, &arg)?)?,
            "--threads" => out.threads = parse_u64(&next_value(&mut args, &arg)?)? as usize,
            "--eval-hands-per-seat" => {
                out.eval_hands_per_seat = parse_u64(&next_value(&mut args, &arg)?)?
            }
            "--lbr-probes" => out.lbr_probes = parse_u64(&next_value(&mut args, &arg)?)?,
            "--lbr-rollouts" => out.lbr_rollouts = parse_u64(&next_value(&mut args, &arg)?)?,
            "--output" => out.output = PathBuf::from(next_value(&mut args, &arg)?),
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(out)
}

fn next_value(args: &mut impl Iterator<Item = String>, name: &str) -> Result<String, String> {
    args.next()
        .ok_or_else(|| format!("{name} requires a value"))
}

fn parse_curve_checkpoint(raw: &str) -> (String, PathBuf) {
    if let Some((label, path)) = raw.split_once('=') {
        (label.to_string(), PathBuf::from(path))
    } else {
        (raw.to_string(), PathBuf::from(raw))
    }
}

fn parse_u64(raw: &str) -> Result<u64, String> {
    if let Some(hex) = raw.strip_prefix("0x") {
        u64::from_str_radix(hex, 16).map_err(|e| format!("invalid hex integer {raw}: {e}"))
    } else {
        raw.parse::<u64>()
            .map_err(|e| format!("invalid integer {raw}: {e}"))
    }
}

fn hex32(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn mix3(seed: u64, a: u64, b: u64) -> u64 {
    let mut x =
        seed ^ a.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ b.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

fn print_usage() {
    eprintln!(
        "usage: cargo run --release --bin nlhe_h3_report -- \\\n\
         \t--checkpoint <ckpt> --artifact <bucket-table> --output artifacts/h3_nlhe_report.md\n\n\
         options:\n\
         \t--train-updates <N>          train inline before evaluating when no checkpoint is supplied\n\
         \t--curve-checkpoint label=path  add extra checkpoint to LBR proxy curve\n\
         \t--eval-hands-per-seat <N>    default 1000\n\
         \t--lbr-probes <N>             default 1000\n\
         \t--lbr-rollouts <N>           default 16\n\
         \t--seed <N|0xHEX>\n\
         \t--threads <N>"
    );
}
