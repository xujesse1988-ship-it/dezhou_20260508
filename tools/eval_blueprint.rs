//! Stage 4 D-461 + D-481 / API-462 + API-484 — Slumbot 100K 手 + baseline 1M
//! 手整合评测 CLI。
//!
//! 用法（F2 \[实现\] 落地后形态）：
//!
//! ```text
//! cargo run --release --bin eval_blueprint -- \
//!     --checkpoint PATH \
//!     --slumbot-endpoint http://www.slumbot.com/api/ \
//!     --slumbot-hands 100000 \
//!     --baseline-hands 1000000 \
//!     --master-seed S \
//!     [--duplicate-dealing] \
//!     [--no-slumbot]
//! ```
//!
//! 输出：JSONL 行格式（D-474）— 每条 result 一行 JSON 写入 stdout 让外部 pipe
//! 到 jq / Python script 走 stage 4 F3 \[报告\] 报告生成。

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use poker::core::rng::ChaCha20Rng;
use poker::training::baseline_eval::{
    evaluate_vs_baseline, CallStationOpponent, Opponent6Max, RandomOpponent, TagOpponent,
};
use poker::training::nlhe_6max::NlheGame6;
use poker::training::slumbot_eval::SlumbotBridge;
use poker::training::trainer::Trainer;
use poker::training::EsMccfrTrainer;
use poker::BucketTable;

struct Args {
    checkpoint: Option<PathBuf>,
    artifact: PathBuf,
    slumbot_endpoint: String,
    slumbot_hands: u64,
    baseline_hands: u64,
    master_seed: u64,
    duplicate_dealing: bool,
    no_slumbot: bool,
}

impl Args {
    fn parse() -> Result<Self, String> {
        let mut args = Args {
            checkpoint: None,
            artifact: PathBuf::from(
                "artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin",
            ),
            slumbot_endpoint: "http://www.slumbot.com/api/".to_string(),
            slumbot_hands: 100_000,
            baseline_hands: 1_000_000,
            master_seed: 42,
            duplicate_dealing: false,
            no_slumbot: false,
        };
        let mut it = std::env::args().skip(1);
        while let Some(flag) = it.next() {
            match flag.as_str() {
                "--checkpoint" => {
                    args.checkpoint = Some(PathBuf::from(
                        it.next().ok_or("--checkpoint missing value")?,
                    ));
                }
                "--artifact" => {
                    args.artifact = PathBuf::from(it.next().ok_or("--artifact missing value")?);
                }
                "--slumbot-endpoint" => {
                    args.slumbot_endpoint = it.next().ok_or("--slumbot-endpoint missing")?;
                }
                "--slumbot-hands" => {
                    args.slumbot_hands = it
                        .next()
                        .ok_or("--slumbot-hands missing")?
                        .parse()
                        .map_err(|e| format!("--slumbot-hands parse: {e}"))?;
                }
                "--baseline-hands" => {
                    args.baseline_hands = it
                        .next()
                        .ok_or("--baseline-hands missing")?
                        .parse()
                        .map_err(|e| format!("--baseline-hands parse: {e}"))?;
                }
                "--master-seed" => {
                    args.master_seed = it
                        .next()
                        .ok_or("--master-seed missing")?
                        .parse()
                        .map_err(|e| format!("--master-seed parse: {e}"))?;
                }
                "--duplicate-dealing" => args.duplicate_dealing = true,
                "--no-slumbot" => args.no_slumbot = true,
                "-h" | "--help" => {
                    print_usage();
                    std::process::exit(0);
                }
                other => return Err(format!("unknown flag: {other}")),
            }
        }
        Ok(args)
    }
}

fn print_usage() {
    eprintln!(
        "usage: eval_blueprint --artifact PATH [--checkpoint PATH] \\\n\
         \t[--slumbot-endpoint URL] [--slumbot-hands N] [--baseline-hands N] \\\n\
         \t[--master-seed S] [--duplicate-dealing] [--no-slumbot]\n\
         \nstage 4 D-461 + D-481 integration: Slumbot HU 100K + baseline 6-max 1M 手 evaluation."
    );
}

fn run() -> Result<(), String> {
    let args = Args::parse()?;
    // 1. 加载 v3 artifact + 构造 NlheGame6 6-max（baseline）+ HU（Slumbot）
    let table = BucketTable::open(&args.artifact)
        .map_err(|e| format!("BucketTable::open({:?}) failed: {e:?}", args.artifact))?;
    let table_arc = Arc::new(table);

    // 2. blueprint 构造（checkpoint 加载或 fresh trainer）
    let game_6max = NlheGame6::new(Arc::clone(&table_arc))
        .map_err(|e| format!("NlheGame6::new failed: {e:?}"))?;
    let blueprint_6max = if let Some(ref ckpt) = args.checkpoint {
        EsMccfrTrainer::<NlheGame6>::load_checkpoint(ckpt, game_6max)
            .map_err(|e| format!("load_checkpoint({ckpt:?}) failed: {e:?}"))?
    } else {
        EsMccfrTrainer::new(game_6max, args.master_seed).with_linear_rm_plus(1_000_000)
    };

    // 3. 跑 baseline eval × 3
    eval_baseline_dispatch(&mut RandomOpponent, &blueprint_6max, &args, "random");
    eval_baseline_dispatch(
        &mut CallStationOpponent,
        &blueprint_6max,
        &args,
        "call_station",
    );
    eval_baseline_dispatch(&mut TagOpponent::default(), &blueprint_6max, &args, "tag");

    // 4. Slumbot eval（HU）
    if !args.no_slumbot {
        let game_hu = NlheGame6::new_hu(Arc::clone(&table_arc))
            .map_err(|e| format!("NlheGame6::new_hu failed: {e:?}"))?;
        let blueprint_hu = if let Some(ref ckpt) = args.checkpoint {
            // HU checkpoint typically separate；这里走 fresh trainer 占位（实际
            // first usable HU blueprint 由用户授权 D-441-rev0 期间训练）。
            let _ = ckpt;
            EsMccfrTrainer::new(game_hu, args.master_seed).with_linear_rm_plus(1_000_000)
        } else {
            EsMccfrTrainer::new(game_hu, args.master_seed).with_linear_rm_plus(1_000_000)
        };
        let mut bridge = SlumbotBridge::new(args.slumbot_endpoint.clone());
        match bridge.evaluate_blueprint(
            &blueprint_hu,
            args.slumbot_hands,
            args.master_seed,
            args.duplicate_dealing,
        ) {
            Ok(r) => {
                let json = serde_json::json!({
                    "kind": "slumbot",
                    "mean_mbbg": r.mean_mbbg,
                    "standard_error_mbbg": r.standard_error_mbbg,
                    "ci_lower": r.confidence_interval_95.0,
                    "ci_upper": r.confidence_interval_95.1,
                    "n_hands": r.n_hands,
                    "duplicate_dealing": r.duplicate_dealing,
                    "wall_clock_seconds": r.wall_clock_seconds,
                });
                println!("{}", json);
            }
            Err(e) => {
                eprintln!("[eval_blueprint] Slumbot evaluation failed: {e:?}");
                let json = serde_json::json!({
                    "kind": "slumbot",
                    "status": "error",
                    "message": format!("{e:?}"),
                });
                println!("{}", json);
            }
        }
    }

    Ok(())
}

fn eval_baseline_dispatch<O: Opponent6Max>(
    opponent: &mut O,
    blueprint: &EsMccfrTrainer<NlheGame6>,
    args: &Args,
    label: &str,
) {
    let mut rng = ChaCha20Rng::from_seed(args.master_seed);
    let result = evaluate_vs_baseline::<NlheGame6, _, _>(
        blueprint,
        opponent,
        args.baseline_hands,
        args.master_seed,
        &mut rng,
    );
    match result {
        Ok(r) => {
            let json = serde_json::json!({
                "kind": "baseline",
                "opponent": label,
                "mean_mbbg": r.mean_mbbg,
                "standard_error_mbbg": r.standard_error_mbbg,
                "n_hands": r.n_hands,
                "opponent_name": r.opponent_name,
                "blueprint_seats": r.blueprint_seats,
                "opponent_seats": r.opponent_seats,
            });
            println!("{}", json);
        }
        Err(e) => {
            let json = serde_json::json!({
                "kind": "baseline",
                "opponent": label,
                "status": "error",
                "message": format!("{e:?}"),
            });
            println!("{}", json);
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("eval_blueprint: {e}");
            ExitCode::FAILURE
        }
    }
}
