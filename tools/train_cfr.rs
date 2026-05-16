//! Stage 4 §F2-revM — `train_cfr` CLI 落地（API-490）。
//!
//! F2 \[实现\] commit 主动 deferred 本 binary 整合到 stage 5（详见
//! `docs/pluribus_stage4_workflow.md` §修订历史 F2 line 564）；F3 \[报告\] 起步前
//! AWS c7a.8xlarge first usable 10⁹ update 训练需要可调用入口，§F2-revM
//! 用户授权 carve-out 把训练 dispatch 提前到 F3 起步同 commit 落地。
//!
//! **scope**：本 commit 只覆盖 stage 4 F3 critical path
//! `--game nlhe-6max --trainer es-mccfr-linear-rm-plus --abstraction pluribus-14`
//! 加 checkpoint cadence、JSONL metrics log、5-variant alarm dispatch、
//! `--abort-on-alarm` flag 接 `MetricsCollector.last_alarm`。
//! Kuhn / Leduc / SimplifiedNlhe stage 3 game variant 走显式 "deferred to
//! stage 5" 错误退出（stage 3 D-372 spec scaffold 占位 — 实际 stage 3 训练
//! 路径走 `cargo test --release --test cfr_simplified_nlhe -- --ignored` 等
//! 集成测试入口，不消费 `train_cfr` 二进制）。
//!
//! 用法：
//!
//! ```text
//! cargo run --release --bin train_cfr -- \
//!     --game nlhe-6max \
//!     --trainer es-mccfr-linear-rm-plus \
//!     --abstraction pluribus-14 \
//!     --bucket-table artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin \
//!     --updates 1000000000 \
//!     --seed 6004773661320960280 \
//!     --warmup-update-count 1000000 \
//!     --threads 32 \
//!     --parallel-batch-size 8 \
//!     --checkpoint-dir artifacts/stage4_first_usable/ \
//!     --checkpoint-every 100000000 \
//!     --keep-last 5 \
//!     --metrics-interval 100000 \
//!     --log-file artifacts/stage4_first_usable/metrics.jsonl \
//!     --max-rss-bytes 32000000000 \
//!     --abort-on-alarm p0
//! ```

use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use poker::core::rng::{ChaCha20Rng, RngSource};
use poker::training::metrics::{
    write_metrics_jsonl, MetricsCollector, TrainingAlarm, TrainingMetrics,
};
use poker::training::nlhe_6max::NlheGame6;
use poker::training::trainer::{EsMccfrTrainer, Trainer};
use poker::BucketTable;

const DEFAULT_BUCKET_TABLE: &str =
    "artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin";
const DEFAULT_CHECKPOINT_DIR: &str = "artifacts/stage4_first_usable";
const DEFAULT_WARMUP_UPDATE_COUNT: u64 = 1_000_000;
const DEFAULT_PARALLEL_BATCH_SIZE: usize = 8;
const DEFAULT_METRICS_INTERVAL: u64 = 100_000;
const DEFAULT_KEEP_LAST: usize = 5;
const DEFAULT_MAX_RSS_BYTES: u64 = 32 * 1024 * 1024 * 1024;
/// FIXED_SEED 字面 ASCII "STG4_F3\x18"（与 24h continuous test FIXED_SEED 共用
/// 模式；F3 first usable 主 seed by user 选择 0x53_54_47_34_5F_46_33_18）。
const DEFAULT_SEED: u64 = 0x53_54_47_34_5F_46_33_18;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AbortPolicy {
    None,
    P0,
    All,
}

struct Args {
    game: String,
    trainer_kind: String,
    abstraction: String,
    bucket_table: PathBuf,
    updates: u64,
    seed: u64,
    warmup_update_count: u64,
    threads: usize,
    parallel_batch_size: usize,
    checkpoint_dir: PathBuf,
    checkpoint_every: u64,
    keep_last: usize,
    metrics_interval: u64,
    log_file: Option<PathBuf>,
    max_rss_bytes: u64,
    abort_on_alarm: AbortPolicy,
    resume: Option<PathBuf>,
    quiet: bool,
}

impl Args {
    fn parse() -> Result<Self, String> {
        let mut args = Args {
            game: "nlhe-6max".to_string(),
            trainer_kind: "es-mccfr-linear-rm-plus".to_string(),
            abstraction: "pluribus-14".to_string(),
            bucket_table: PathBuf::from(DEFAULT_BUCKET_TABLE),
            updates: 0,
            seed: DEFAULT_SEED,
            warmup_update_count: DEFAULT_WARMUP_UPDATE_COUNT,
            threads: 1,
            parallel_batch_size: DEFAULT_PARALLEL_BATCH_SIZE,
            checkpoint_dir: PathBuf::from(DEFAULT_CHECKPOINT_DIR),
            checkpoint_every: 100_000_000,
            keep_last: DEFAULT_KEEP_LAST,
            metrics_interval: DEFAULT_METRICS_INTERVAL,
            log_file: None,
            max_rss_bytes: DEFAULT_MAX_RSS_BYTES,
            abort_on_alarm: AbortPolicy::P0,
            resume: None,
            quiet: false,
        };

        let mut it = std::env::args().skip(1);
        while let Some(flag) = it.next() {
            match flag.as_str() {
                "--game" => args.game = it.next().ok_or("--game missing value")?,
                "--trainer" => args.trainer_kind = it.next().ok_or("--trainer missing value")?,
                "--abstraction" => {
                    args.abstraction = it.next().ok_or("--abstraction missing value")?
                }
                "--bucket-table" => {
                    args.bucket_table = PathBuf::from(it.next().ok_or("--bucket-table missing")?)
                }
                "--updates" | "--iter" => {
                    args.updates = it
                        .next()
                        .ok_or("--updates missing")?
                        .parse()
                        .map_err(|e| format!("--updates parse: {e}"))?
                }
                "--seed" => {
                    args.seed = it
                        .next()
                        .ok_or("--seed missing")?
                        .parse()
                        .map_err(|e| format!("--seed parse: {e}"))?
                }
                "--warmup-update-count" => {
                    args.warmup_update_count = it
                        .next()
                        .ok_or("--warmup-update-count missing")?
                        .parse()
                        .map_err(|e| format!("--warmup-update-count parse: {e}"))?
                }
                "--threads" => {
                    args.threads = it
                        .next()
                        .ok_or("--threads missing")?
                        .parse()
                        .map_err(|e| format!("--threads parse: {e}"))?
                }
                "--parallel-batch-size" => {
                    args.parallel_batch_size = it
                        .next()
                        .ok_or("--parallel-batch-size missing")?
                        .parse()
                        .map_err(|e| format!("--parallel-batch-size parse: {e}"))?
                }
                "--checkpoint-dir" => {
                    args.checkpoint_dir =
                        PathBuf::from(it.next().ok_or("--checkpoint-dir missing")?)
                }
                "--checkpoint-every" => {
                    args.checkpoint_every = it
                        .next()
                        .ok_or("--checkpoint-every missing")?
                        .parse()
                        .map_err(|e| format!("--checkpoint-every parse: {e}"))?
                }
                "--keep-last" => {
                    args.keep_last = it
                        .next()
                        .ok_or("--keep-last missing")?
                        .parse()
                        .map_err(|e| format!("--keep-last parse: {e}"))?
                }
                "--metrics-interval" => {
                    args.metrics_interval = it
                        .next()
                        .ok_or("--metrics-interval missing")?
                        .parse()
                        .map_err(|e| format!("--metrics-interval parse: {e}"))?
                }
                "--log-file" => {
                    args.log_file = Some(PathBuf::from(it.next().ok_or("--log-file missing")?))
                }
                "--max-rss-bytes" => {
                    args.max_rss_bytes = it
                        .next()
                        .ok_or("--max-rss-bytes missing")?
                        .parse()
                        .map_err(|e| format!("--max-rss-bytes parse: {e}"))?
                }
                "--abort-on-alarm" => {
                    let v = it.next().ok_or("--abort-on-alarm missing")?;
                    args.abort_on_alarm = match v.as_str() {
                        "none" => AbortPolicy::None,
                        "p0" => AbortPolicy::P0,
                        "all" => AbortPolicy::All,
                        other => return Err(format!("--abort-on-alarm 不识别值: {other}")),
                    };
                }
                "--resume" => {
                    args.resume = Some(PathBuf::from(it.next().ok_or("--resume missing")?))
                }
                "--quiet" => args.quiet = true,
                "-h" | "--help" => {
                    print_help();
                    std::process::exit(0);
                }
                other => return Err(format!("unrecognized flag: {other}")),
            }
        }
        if args.updates == 0 {
            return Err("--updates required (e.g. --updates 1000000000 for first usable)".into());
        }
        Ok(args)
    }
}

fn print_help() {
    eprintln!("train_cfr — stage 4 F3 first usable training driver (API-490 / §F2-revM)");
    eprintln!();
    eprintln!("USAGE:");
    eprintln!(
        "    cargo run --release --bin train_cfr -- --game nlhe-6max \\\n         --trainer es-mccfr-linear-rm-plus --abstraction pluribus-14 \\\n         --bucket-table PATH --updates N [OPTIONS]"
    );
    eprintln!();
    eprintln!("REQUIRED:");
    eprintln!("    --game {{nlhe-6max}}                       (kuhn/leduc/nlhe-simplified deferred to stage 5)");
    eprintln!(
        "    --updates N                              update 总数（first usable = 1_000_000_000）"
    );
    eprintln!();
    eprintln!("OPTIONAL (defaults shown):");
    eprintln!("    --trainer es-mccfr-linear-rm-plus");
    eprintln!("    --abstraction pluribus-14");
    eprintln!("    --bucket-table {DEFAULT_BUCKET_TABLE}");
    eprintln!("    --seed {DEFAULT_SEED} (0x53_54_47_34_5F_46_33_18 = ASCII \"STG4_F3\\x18\")");
    eprintln!("    --warmup-update-count {DEFAULT_WARMUP_UPDATE_COUNT}    (D-409)");
    eprintln!("    --threads 1                              (32 = AWS c7a.8xlarge full)");
    eprintln!(
        "    --parallel-batch-size {DEFAULT_PARALLEL_BATCH_SIZE}                  (§E-rev2 / A2)"
    );
    eprintln!("    --checkpoint-dir {DEFAULT_CHECKPOINT_DIR}");
    eprintln!("    --checkpoint-every 100000000             (D-461 cadence)");
    eprintln!("    --keep-last {DEFAULT_KEEP_LAST}                            (D-359)");
    eprintln!("    --metrics-interval {DEFAULT_METRICS_INTERVAL}                 (D-476)");
    eprintln!("    --log-file PATH                          (D-474 JSONL；缺省走 stdout)");
    eprintln!(
        "    --max-rss-bytes {}                (D-431 = 32 GiB)",
        DEFAULT_MAX_RSS_BYTES
    );
    eprintln!("    --abort-on-alarm {{none,p0,all}}           (D-473；默认 p0)");
    eprintln!("    --resume PATH                            (从 checkpoint 恢复)");
    eprintln!("    --quiet                                   静默 progress log");
}

fn main() -> ExitCode {
    let args = match Args::parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("[train_cfr] arg error: {e}");
            print_help();
            return ExitCode::from(2);
        }
    };

    match args.game.as_str() {
        "nlhe-6max" => match run_nlhe6max(args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("[train_cfr] run failed: {e}");
                ExitCode::from(1)
            }
        },
        "kuhn" | "leduc" | "nlhe-simplified" | "nlhe" => {
            eprintln!(
                "[train_cfr] §F2-revM scope — `--game {}` deferred to stage 5（stage 3 训练入口走 \
                 `cargo test --release --test cfr_simplified_nlhe -- --ignored` 等集成测试，\
                 不消费本 binary）。",
                args.game
            );
            ExitCode::from(2)
        }
        other => {
            eprintln!(
                "[train_cfr] unrecognized --game {other}（accepted: nlhe-6max；\
                 kuhn / leduc / nlhe-simplified deferred to stage 5）。"
            );
            ExitCode::from(2)
        }
    }
}

fn run_nlhe6max(args: Args) -> Result<(), String> {
    if args.trainer_kind != "es-mccfr-linear-rm-plus" {
        return Err(format!(
            "--trainer 必须 es-mccfr-linear-rm-plus（got {}）— stage 4 F3 critical path lock",
            args.trainer_kind
        ));
    }
    if args.abstraction != "pluribus-14" {
        return Err(format!(
            "--abstraction 必须 pluribus-14（got {}）— D-420 lock",
            args.abstraction
        ));
    }

    fs::create_dir_all(&args.checkpoint_dir).map_err(|e| format!("checkpoint-dir create: {e}"))?;

    let table = BucketTable::open(&args.bucket_table)
        .map_err(|e| format!("BucketTable::open {:?}: {e:?}", args.bucket_table))?;
    let table = Arc::new(table);
    let body_hex: String = table
        .content_hash()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if !args.quiet {
        eprintln!(
            "[train_cfr] bucket-table loaded: {:?} body BLAKE3 = {body_hex}",
            args.bucket_table
        );
    }

    // Trainer 构造：resume 路径走 load_checkpoint，否则 fresh new()。
    let mut trainer: EsMccfrTrainer<NlheGame6> = if let Some(resume_path) = args.resume.as_ref() {
        let game =
            NlheGame6::new(Arc::clone(&table)).map_err(|e| format!("NlheGame6::new: {e:?}"))?;
        let t = EsMccfrTrainer::<NlheGame6>::load_checkpoint(resume_path, game)
            .map_err(|e| format!("load_checkpoint {resume_path:?}: {e:?}"))?;
        if !args.quiet {
            eprintln!(
                "[train_cfr] resumed from {resume_path:?} @ update {}",
                t.update_count()
            );
        }
        t
    } else {
        let game =
            NlheGame6::new(Arc::clone(&table)).map_err(|e| format!("NlheGame6::new: {e:?}"))?;
        EsMccfrTrainer::new(game, args.seed)
            .with_linear_rm_plus(args.warmup_update_count)
            .with_parallel_batch_size(args.parallel_batch_size)
    };

    let start_update = trainer.update_count();
    if start_update >= args.updates {
        eprintln!(
            "[train_cfr] start_update {start_update} >= --updates {} → nothing to do",
            args.updates
        );
        return Ok(());
    }

    // RNG pool — N independent ChaCha20Rng seeded from master_seed × per-thread offset。
    // 与 perf_slo / 24h_continuous test 同型 splitmix-style seeded（D-027 字面）。
    let n_threads = args.threads.max(1);
    let mut rng_pool: Vec<Box<dyn RngSource>> = (0..n_threads as u64)
        .map(|tid| {
            let seeded = args
                .seed
                .wrapping_add(0xDEAD_BEEF_u64.wrapping_mul(tid + 1));
            Box::new(ChaCha20Rng::from_seed(seeded)) as Box<dyn RngSource>
        })
        .collect();
    let single_threaded = n_threads == 1;

    // Metrics + log file
    let mut metrics_writer: Option<BufWriter<File>> = if let Some(log_path) = args.log_file.as_ref()
    {
        let f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)
            .map_err(|e| format!("log-file open {log_path:?}: {e}"))?;
        Some(BufWriter::new(f))
    } else {
        None
    };
    let mut collector = MetricsCollector::new(args.metrics_interval);
    let mut metrics = TrainingMetrics::zero();
    let mut last_metrics_t: u64 = start_update;
    let mut next_checkpoint_at = match start_update.checked_div(args.checkpoint_every) {
        Some(q) => (q + 1) * args.checkpoint_every,
        None => u64::MAX, // checkpoint_every == 0 → 关闭周期 checkpoint
    };

    let start = Instant::now();
    let mut last_log_t = start_update;
    let mut last_log_at = Instant::now();

    if !args.quiet {
        eprintln!(
            "[train_cfr] starting: target_updates={} threads={} batch={} warmup_at={} \
             checkpoint_every={} log_file={:?}",
            args.updates,
            n_threads,
            args.parallel_batch_size,
            args.warmup_update_count,
            args.checkpoint_every,
            args.log_file
        );
    }

    while trainer.update_count() < args.updates {
        // step / step_parallel dispatch
        if single_threaded {
            let rng = rng_pool[0].as_mut();
            trainer
                .step(rng)
                .map_err(|e| format!("step @ {}: {e:?}", trainer.update_count()))?;
        } else {
            trainer
                .step_parallel(&mut rng_pool, n_threads)
                .map_err(|e| format!("step_parallel @ {}: {e:?}", trainer.update_count()))?;
        }

        let cur = trainer.update_count();

        // metrics observe + JSONL log
        if cur - last_metrics_t >= args.metrics_interval || cur >= args.updates {
            last_metrics_t = cur;
            let observe_rng = rng_pool[0].as_mut();
            collector
                .observe(&trainer, observe_rng, &mut metrics)
                .map_err(|e| format!("metrics.observe @ {cur}: {e:?}"))?;
            metrics.update_count = cur;
            metrics.wall_clock_seconds = start.elapsed().as_secs_f64();
            // D-431 RSS over-limit → CLI 决策（trainer 不主动 abort）
            if metrics.peak_rss_bytes > args.max_rss_bytes {
                metrics.last_alarm = Some(TrainingAlarm::OutOfMemory {
                    rss_bytes: metrics.peak_rss_bytes,
                    limit_bytes: args.max_rss_bytes,
                });
            }
            if let Some(w) = metrics_writer.as_mut() {
                write_metrics_jsonl(w, &metrics)
                    .map_err(|e| format!("write_metrics_jsonl @ {cur}: {e}"))?;
                w.flush().map_err(|e| format!("flush log: {e}"))?;
            } else if !args.quiet {
                let s = serde_json::to_string(&metrics).map_err(|e| format!("serde_json: {e}"))?;
                println!("{s}");
            }
            // alarm dispatch — abort on policy
            if let Some(alarm) = metrics.last_alarm.as_ref() {
                let is_p0 = matches!(
                    alarm,
                    TrainingAlarm::RegretGrowthTrendUp { .. }
                        | TrainingAlarm::OutOfMemory { .. }
                        | TrainingAlarm::EvSumViolation { .. }
                );
                let should_abort = match args.abort_on_alarm {
                    AbortPolicy::None => false,
                    AbortPolicy::P0 => is_p0,
                    AbortPolicy::All => true,
                };
                if should_abort {
                    eprintln!(
                        "[train_cfr] abort-on-alarm 触发 @ update {cur}: {alarm:?}（写最终 \
                         checkpoint 后退出）"
                    );
                    let final_path = checkpoint_path(&args.checkpoint_dir, cur, "abort");
                    trainer
                        .save_checkpoint(&final_path)
                        .map_err(|e| format!("save_checkpoint abort {final_path:?}: {e:?}"))?;
                    return Err(format!("aborted on alarm: {alarm:?}"));
                }
            }
        }

        // checkpoint cadence
        if cur >= next_checkpoint_at {
            let path = checkpoint_path(&args.checkpoint_dir, cur, "auto");
            trainer
                .save_checkpoint(&path)
                .map_err(|e| format!("save_checkpoint {path:?}: {e:?}"))?;
            next_checkpoint_at = cur + args.checkpoint_every;
            rotate_checkpoints(&args.checkpoint_dir, args.keep_last)
                .map_err(|e| format!("rotate_checkpoints: {e}"))?;
            if !args.quiet {
                eprintln!(
                    "[train_cfr] checkpoint @ update {cur} → {path:?}（keep-last={}）",
                    args.keep_last
                );
            }
        }

        // progress log（每 30s 一次）
        if !args.quiet && last_log_at.elapsed().as_secs() >= 30 {
            let dt = last_log_at.elapsed().as_secs_f64();
            let du = cur - last_log_t;
            let throughput = (du as f64) / dt.max(1e-9);
            let pct = 100.0 * (cur as f64) / (args.updates as f64);
            let eta_sec = (args.updates - cur) as f64 / throughput.max(1e-9);
            eprintln!(
                "[train_cfr] progress: update {cur}/{} ({pct:.2}%) {throughput:.0} update/s \
                 ETA {:.1}h RSS {} MB",
                args.updates,
                eta_sec / 3600.0,
                metrics.peak_rss_bytes / 1024 / 1024
            );
            last_log_at = Instant::now();
            last_log_t = cur;
        }
    }

    // 最终 checkpoint
    let final_path = checkpoint_path(&args.checkpoint_dir, trainer.update_count(), "final");
    trainer
        .save_checkpoint(&final_path)
        .map_err(|e| format!("save_checkpoint final {final_path:?}: {e:?}"))?;

    if let Some(mut w) = metrics_writer {
        w.flush().map_err(|e| format!("flush log final: {e}"))?;
    }

    let elapsed = start.elapsed();
    let total = trainer.update_count() - start_update;
    let throughput = (total as f64) / elapsed.as_secs_f64().max(1e-9);
    eprintln!(
        "[train_cfr] done: {total} update / {:.1}s = {throughput:.0} update/s（final \
         checkpoint {final_path:?}）",
        elapsed.as_secs_f64()
    );
    Ok(())
}

fn checkpoint_path(dir: &Path, update_count: u64, tag: &str) -> PathBuf {
    dir.join(format!(
        "nlhe6max_linear_rm_plus_t{:020}_{}.ckpt",
        update_count, tag
    ))
}

/// 保留 keep_last 个最新 auto checkpoint（按 mtime 倒序）；final / abort
/// checkpoint 不参与 rotation（手动管理）。
fn rotate_checkpoints(dir: &Path, keep_last: usize) -> Result<(), String> {
    if keep_last == 0 {
        return Ok(());
    }
    let mut autos: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| format!("read_dir {dir:?}: {e}"))? {
        let entry = entry.map_err(|e| format!("read_dir entry: {e}"))?;
        let path = entry.path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        if !name.starts_with("nlhe6max_linear_rm_plus_t") || !name.ends_with("_auto.ckpt") {
            continue;
        }
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        autos.push((path, mtime));
    }
    if autos.len() <= keep_last {
        return Ok(());
    }
    autos.sort_by_key(|x| std::cmp::Reverse(x.1));
    for (path, _) in autos.iter().skip(keep_last) {
        let _ = fs::remove_file(path);
    }
    Ok(())
}
