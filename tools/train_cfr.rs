//! Stage 3 CFR / MCCFR 训练 CLI（H3 NLHE 路径）。
//!
//! H3 只要求补齐简化 heads-up NLHE blueprint 训练入口；Kuhn / Leduc 仍通过
//! 测试和专用 report 工具覆盖，不在本 CLI 中新增行为。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use poker::training::nlhe::SimplifiedNlheGame;
use poker::training::{EsMccfrTrainer, Trainer};
use poker::{BucketTable, ChaCha20Rng, RngSource};

#[derive(Debug)]
struct Args {
    game: String,
    trainer: String,
    updates: u64,
    seed: u64,
    checkpoint_dir: PathBuf,
    resume: Option<PathBuf>,
    checkpoint_every: u64,
    keep_last: usize,
    bucket_table: PathBuf,
    threads: usize,
    quiet: bool,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            game: String::new(),
            trainer: "es-mccfr".to_string(),
            updates: 0,
            seed: 0,
            checkpoint_dir: PathBuf::from("artifacts"),
            resume: None,
            checkpoint_every: 0,
            keep_last: 5,
            bucket_table: PathBuf::new(),
            threads: 1,
            quiet: false,
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("[train_cfr] failed: {e}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    if args.game != "nlhe" {
        return Err("H3 train_cfr currently supports only --game nlhe".to_string());
    }
    if args.trainer != "es-mccfr" {
        return Err("H3 NLHE path supports only --trainer es-mccfr".to_string());
    }
    if args.updates == 0 {
        return Err("--updates must be > 0".to_string());
    }
    if args.threads == 0 {
        return Err("--threads must be > 0".to_string());
    }
    if args.bucket_table.as_os_str().is_empty() {
        return Err("--bucket-table is required for --game nlhe".to_string());
    }
    fs::create_dir_all(&args.checkpoint_dir)
        .map_err(|e| format!("create checkpoint dir failed: {e}"))?;

    let table = Arc::new(BucketTable::open(&args.bucket_table).map_err(|e| {
        format!(
            "BucketTable::open({}) failed: {e:?}",
            args.bucket_table.display()
        )
    })?);
    let game = SimplifiedNlheGame::new(Arc::clone(&table))
        .map_err(|e| format!("SimplifiedNlheGame::new failed: {e:?}"))?;
    let table_hash = hex32(&table.content_hash());

    let mut trainer = if let Some(ref resume) = args.resume {
        <EsMccfrTrainer<SimplifiedNlheGame> as Trainer<SimplifiedNlheGame>>::load_checkpoint(
            resume, game,
        )
        .map_err(|e| format!("load checkpoint {} failed: {e:?}", resume.display()))?
    } else {
        EsMccfrTrainer::new(game, args.seed)
    };

    let start_update = trainer.update_count();
    if start_update >= args.updates {
        if !args.quiet {
            eprintln!(
                "[train_cfr] checkpoint already at update_count={} >= target {}; no-op",
                start_update, args.updates
            );
        }
        save_checkpoint(&trainer, &args.checkpoint_dir, "final")?;
        return Ok(());
    }

    if !args.quiet {
        eprintln!("[train_cfr] game             = nlhe");
        eprintln!("[train_cfr] trainer          = es-mccfr");
        eprintln!("[train_cfr] target_updates   = {}", args.updates);
        eprintln!("[train_cfr] start_update     = {start_update}");
        eprintln!("[train_cfr] seed             = 0x{:016x}", args.seed);
        eprintln!("[train_cfr] threads          = {}", args.threads);
        eprintln!(
            "[train_cfr] bucket_table     = {}",
            args.bucket_table.display()
        );
        eprintln!("[train_cfr] bucket_blake3    = {table_hash}");
        eprintln!(
            "[train_cfr] checkpoint_dir   = {}",
            args.checkpoint_dir.display()
        );
    }

    let mut single_rng = ChaCha20Rng::from_seed(args.seed);
    let mut rng_pool: Vec<Box<dyn RngSource>> = (0..args.threads as u64)
        .map(|tid| {
            let seed = mix3(args.seed, 0x5448_5244, tid);
            Box::new(ChaCha20Rng::from_seed(seed)) as Box<dyn RngSource>
        })
        .collect();

    let t0 = Instant::now();
    let report_every = args
        .checkpoint_every
        .max((args.updates - start_update).saturating_div(10).max(1));
    let mut next_report = start_update.saturating_add(report_every);
    let mut next_checkpoint = if args.checkpoint_every > 0 {
        ((start_update / args.checkpoint_every) + 1) * args.checkpoint_every
    } else {
        u64::MAX
    };
    let mut saved_this_run: Vec<PathBuf> = Vec::new();

    while trainer.update_count() < args.updates {
        let remaining = args.updates - trainer.update_count();
        if args.threads == 1 {
            trainer
                .step(&mut single_rng)
                .map_err(|e| format!("step failed at update {}: {e:?}", trainer.update_count()))?;
        } else {
            let n = args.threads.min(remaining as usize);
            trainer.step_parallel(&mut rng_pool, n).map_err(|e| {
                format!(
                    "step_parallel failed at update {}: {e:?}",
                    trainer.update_count()
                )
            })?;
        }

        let cur = trainer.update_count();
        if !args.quiet && cur >= next_report {
            let elapsed = t0.elapsed().as_secs_f64();
            let throughput = (cur - start_update) as f64 / elapsed.max(1e-9);
            eprintln!(
                "[train_cfr] update {cur} / {} elapsed={elapsed:.1}s throughput={throughput:.0}/s",
                args.updates
            );
            while next_report <= cur {
                next_report = next_report.saturating_add(report_every);
            }
        }

        if args.checkpoint_every > 0 && cur >= next_checkpoint {
            let path = save_checkpoint(&trainer, &args.checkpoint_dir, "auto")?;
            saved_this_run.push(path);
            prune_saved(&mut saved_this_run, args.keep_last);
            while next_checkpoint <= cur {
                next_checkpoint = next_checkpoint.saturating_add(args.checkpoint_every);
            }
        }
    }

    let final_path = save_checkpoint(&trainer, &args.checkpoint_dir, "final")?;
    if !args.quiet {
        let elapsed = t0.elapsed().as_secs_f64();
        let throughput = (trainer.update_count() - start_update) as f64 / elapsed.max(1e-9);
        eprintln!(
            "[train_cfr] done update_count={} wall={elapsed:.1}s throughput={throughput:.0}/s final={}",
            trainer.update_count(),
            final_path.display()
        );
    }
    Ok(())
}

fn parse_args() -> Result<Args, String> {
    let mut out = Args::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--game" => out.game = next_value(&mut args, "--game")?,
            "--trainer" => out.trainer = next_value(&mut args, "--trainer")?,
            "--updates" => out.updates = parse_u64(&next_value(&mut args, "--updates")?)?,
            "--iter" => {
                let _ = next_value(&mut args, "--iter")?;
            }
            "--seed" => out.seed = parse_u64(&next_value(&mut args, "--seed")?)?,
            "--checkpoint-dir" => {
                out.checkpoint_dir = PathBuf::from(next_value(&mut args, "--checkpoint-dir")?)
            }
            "--resume" => out.resume = Some(PathBuf::from(next_value(&mut args, "--resume")?)),
            "--checkpoint-every" => {
                out.checkpoint_every = parse_u64(&next_value(&mut args, "--checkpoint-every")?)?
            }
            "--keep-last" => {
                out.keep_last = parse_u64(&next_value(&mut args, "--keep-last")?)? as usize
            }
            "--bucket-table" => {
                out.bucket_table = PathBuf::from(next_value(&mut args, "--bucket-table")?)
            }
            "--threads" => out.threads = parse_u64(&next_value(&mut args, "--threads")?)? as usize,
            "--quiet" => out.quiet = true,
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if out.game.is_empty() {
        return Err("--game is required".to_string());
    }
    Ok(out)
}

fn next_value(args: &mut impl Iterator<Item = String>, name: &str) -> Result<String, String> {
    args.next()
        .ok_or_else(|| format!("{name} requires a value"))
}

fn parse_u64(raw: &str) -> Result<u64, String> {
    if let Some(hex) = raw.strip_prefix("0x") {
        u64::from_str_radix(hex, 16).map_err(|e| format!("invalid hex integer {raw}: {e}"))
    } else {
        raw.parse::<u64>()
            .map_err(|e| format!("invalid integer {raw}: {e}"))
    }
}

fn save_checkpoint(
    trainer: &EsMccfrTrainer<SimplifiedNlheGame>,
    dir: &Path,
    label: &str,
) -> Result<PathBuf, String> {
    let path = dir.join(format!(
        "nlhe_es_mccfr_{label}_{:012}.ckpt",
        trainer.update_count()
    ));
    trainer
        .save_checkpoint(&path)
        .map_err(|e| format!("save checkpoint {} failed: {e:?}", path.display()))?;
    Ok(path)
}

fn prune_saved(saved: &mut Vec<PathBuf>, keep_last: usize) {
    if keep_last == 0 {
        return;
    }
    while saved.len() > keep_last {
        let old = saved.remove(0);
        let _ = fs::remove_file(old);
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
        "usage: cargo run --release --bin train_cfr -- \\\n\
         \t--game nlhe --trainer es-mccfr --bucket-table <path> --updates <N> [options]\n\n\
         options:\n\
         \t--seed <N|0xHEX>\n\
         \t--threads <N>\n\
         \t--resume <checkpoint>\n\
         \t--checkpoint-dir <dir>\n\
         \t--checkpoint-every <N>\n\
         \t--keep-last <N>\n\
         \t--quiet"
    );
}
