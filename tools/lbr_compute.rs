//! Stage 4 D-450 / API-452 — LBR (Local Best Response) computation CLI。
//!
//! 用法：
//!
//! ```text
//! cargo run --release --bin lbr_compute -- \
//!     --checkpoint PATH \
//!     --bucket-table PATH \
//!     [--n-hands 1000] \
//!     [--traverser N | --six-traverser] \
//!     [--action-set-size 14] \
//!     [--myopic-horizon 1] \
//!     [--seed S] \
//!     [--openspiel-export PATH]
//! ```
//!
//! - `--checkpoint`：Linear+RM+ NlheGame6 v2 checkpoint 路径（`EsMccfrTrainer<
//!   NlheGame6>::save_checkpoint` 输出形态）。
//! - `--bucket-table`：v3 production artifact 路径（用于 [`NlheGame6::new`]
//!   reconstruction）。
//! - `--n-hands`：D-452 字面 1000 hand / LBR-player（缺省 1000）。
//! - `--traverser N`：单 traverser 计算（N ∈ [0, 6)）；与 `--six-traverser`
//!   互斥。
//! - `--six-traverser`：D-459 主路径，6 traverser per-traverser min/max/average
//!   输出。
//! - `--action-set-size`：D-456 字面 ∈ {5, 14}，缺省 14。
//! - `--myopic-horizon`：D-455 字面，缺省 1。
//! - `--seed`：master seed for ChaCha20Rng（D-027 字面，缺省 0）。
//! - `--openspiel-export`：可选，写 OpenSpiel-compatible policy 文件到该路径
//!   （D-457 一次性 sanity 用）。

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use poker::training::lbr::LbrEvaluator;
use poker::training::nlhe_6max::NlheGame6;
use poker::training::Trainer;
use poker::{BucketTable, ChaCha20Rng, EsMccfrTrainer};

/// D-452 字面 — 1000 hand / LBR-player 默认。
const DEFAULT_N_HANDS: u64 = 1_000;

/// D-456 / D-455 主路径默认。
const DEFAULT_ACTION_SET_SIZE: usize = 14;
const DEFAULT_MYOPIC_HORIZON: u8 = 1;

struct Args {
    checkpoint: PathBuf,
    bucket_table: PathBuf,
    n_hands: u64,
    mode: Mode,
    action_set_size: usize,
    myopic_horizon: u8,
    seed: u64,
    openspiel_export: Option<PathBuf>,
}

enum Mode {
    SingleTraverser(u8),
    SixTraverser,
}

fn parse_args() -> Result<Args, String> {
    let mut checkpoint: Option<PathBuf> = None;
    let mut bucket_table: Option<PathBuf> = None;
    let mut n_hands: u64 = DEFAULT_N_HANDS;
    let mut traverser: Option<u8> = None;
    let mut six_traverser = false;
    let mut action_set_size: usize = DEFAULT_ACTION_SET_SIZE;
    let mut myopic_horizon: u8 = DEFAULT_MYOPIC_HORIZON;
    let mut seed: u64 = 0;
    let mut openspiel_export: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--checkpoint" => {
                checkpoint = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| "--checkpoint 需要值".to_string())?,
                ));
            }
            "--bucket-table" => {
                bucket_table = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| "--bucket-table 需要值".to_string())?,
                ));
            }
            "--n-hands" => {
                n_hands = args
                    .next()
                    .ok_or_else(|| "--n-hands 需要值".to_string())?
                    .parse()
                    .map_err(|e| format!("--n-hands parse 失败: {e}"))?;
            }
            "--traverser" => {
                traverser = Some(
                    args.next()
                        .ok_or_else(|| "--traverser 需要值".to_string())?
                        .parse()
                        .map_err(|e| format!("--traverser parse 失败: {e}"))?,
                );
            }
            "--six-traverser" => {
                six_traverser = true;
            }
            "--action-set-size" => {
                action_set_size = args
                    .next()
                    .ok_or_else(|| "--action-set-size 需要值".to_string())?
                    .parse()
                    .map_err(|e| format!("--action-set-size parse 失败: {e}"))?;
            }
            "--myopic-horizon" => {
                myopic_horizon = args
                    .next()
                    .ok_or_else(|| "--myopic-horizon 需要值".to_string())?
                    .parse()
                    .map_err(|e| format!("--myopic-horizon parse 失败: {e}"))?;
            }
            "--seed" => {
                seed = args
                    .next()
                    .ok_or_else(|| "--seed 需要值".to_string())?
                    .parse()
                    .map_err(|e| format!("--seed parse 失败: {e}"))?;
            }
            "--openspiel-export" => {
                openspiel_export = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| "--openspiel-export 需要值".to_string())?,
                ));
            }
            other => return Err(format!("未识别参数 `{other}`")),
        }
    }
    let checkpoint = checkpoint.ok_or_else(|| "--checkpoint 必填".to_string())?;
    let bucket_table = bucket_table.ok_or_else(|| "--bucket-table 必填".to_string())?;
    let mode = match (traverser, six_traverser) {
        (Some(t), false) => Mode::SingleTraverser(t),
        (None, true) => Mode::SixTraverser,
        (None, false) => Mode::SixTraverser, // 默认走 6-traverser 主路径
        (Some(_), true) => {
            return Err("--traverser 与 --six-traverser 互斥".to_string());
        }
    };
    Ok(Args {
        checkpoint,
        bucket_table,
        n_hands,
        mode,
        action_set_size,
        myopic_horizon,
        seed,
        openspiel_export,
    })
}

fn run(args: Args) -> Result<(), String> {
    // 1. 加载 v3 bucket_table artifact + 构造 NlheGame6
    let table = BucketTable::open(Path::new(&args.bucket_table))
        .map_err(|e| format!("BucketTable::open({:?}) 失败: {e:?}", args.bucket_table))?;
    let game =
        NlheGame6::new(Arc::new(table)).map_err(|e| format!("NlheGame6::new 失败: {e:?}"))?;
    // 2. 加载 checkpoint
    let trainer = EsMccfrTrainer::<NlheGame6>::load_checkpoint(&args.checkpoint, game)
        .map_err(|e| format!("load_checkpoint({:?}) 失败: {e:?}", args.checkpoint))?;
    eprintln!(
        "[lbr_compute] checkpoint loaded: update_count={} per_traverser_active={}",
        trainer.update_count(),
        trainer.per_traverser_active(),
    );

    let trainer_arc = Arc::new(trainer);
    // 3. 构造 LbrEvaluator
    let evaluator = LbrEvaluator::<NlheGame6>::new(
        Arc::clone(&trainer_arc),
        args.action_set_size,
        args.myopic_horizon,
    )
    .map_err(|e| format!("LbrEvaluator::new 失败: {e:?}"))?;

    let mut rng = ChaCha20Rng::from_seed(args.seed);

    // 4. dispatch by mode
    match args.mode {
        Mode::SingleTraverser(t) => {
            let r = evaluator
                .compute(t, args.n_hands, &mut rng)
                .map_err(|e| format!("LBR compute(traverser={t}) 失败: {e:?}"))?;
            println!(
                "{{\"lbr_player\":{},\"lbr_value_mbbg\":{},\"standard_error_mbbg\":{},\
                 \"n_hands\":{},\"computation_seconds\":{}}}",
                r.lbr_player,
                r.lbr_value_mbbg,
                r.standard_error_mbbg,
                r.n_hands,
                r.computation_seconds
            );
        }
        Mode::SixTraverser => {
            let r = evaluator
                .compute_six_traverser_average(args.n_hands, &mut rng)
                .map_err(|e| format!("compute_six_traverser_average 失败: {e:?}"))?;
            println!(
                "{{\"average_mbbg\":{},\"max_mbbg\":{},\"min_mbbg\":{},\"per_traverser\":[",
                r.average_mbbg, r.max_mbbg, r.min_mbbg
            );
            for (i, lr) in r.per_traverser.iter().enumerate() {
                println!(
                    "  {{\"lbr_player\":{},\"lbr_value_mbbg\":{},\
                     \"standard_error_mbbg\":{},\"n_hands\":{},\
                     \"computation_seconds\":{}}}{}",
                    lr.lbr_player,
                    lr.lbr_value_mbbg,
                    lr.standard_error_mbbg,
                    lr.n_hands,
                    lr.computation_seconds,
                    if i + 1 < r.per_traverser.len() {
                        ","
                    } else {
                        ""
                    },
                );
            }
            println!("]}}");
        }
    }

    // 5. optional OpenSpiel export（D-457）
    if let Some(path) = &args.openspiel_export {
        evaluator
            .export_policy_for_openspiel(path)
            .map_err(|e| format!("export_policy_for_openspiel({path:?}) 失败: {e:?}"))?;
        eprintln!("[lbr_compute] OpenSpiel policy export → {path:?}");
    }

    Ok(())
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("[lbr_compute] arg parse error: {e}");
            return ExitCode::from(2);
        }
    };
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("[lbr_compute] error: {e}");
            ExitCode::FAILURE
        }
    }
}
