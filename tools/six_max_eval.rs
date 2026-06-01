//! S4 gate runner：加载 6-max A3×A4 dense blueprint checkpoint，跑 N 座 baseline 评测
//! （`evaluate_blueprint_vs_baseline_multiway`）。门槛 = 1,000,000 手稳定击败
//! random / call-station / overly-tight（必要非充分，`six_max_nlhe_target.md` S4）。
//!
//! 用法：
//! ```bash
//! cargo run --release --bin six_max_eval -- \
//!   --bucket-table artifacts/bucket_table_200_200_200_seed_cafebabe_schemav4.bin \
//!   --checkpoint artifacts/run_6max_s4_n3/nlhe_es_mccfr_auto_000000200000000.ckpt \
//!   --postflop-cap 3 --hands-per-seat 170000
//! ```
//! `hands-per-seat × n_players` = 总手数（170000 × 6 ≈ 1.02M）。blueprint 走 dense
//! checkpoint 的 average strategy；未访问信息集 harness 兜底均匀分布。

use std::process::ExitCode;
use std::sync::Arc;

use poker::training::nlhe::SimplifiedNlheGame;
use poker::training::nlhe_betting_tree::first_small_6max;
use poker::training::nlhe_dense_trainer::DenseNlheEsMccfrTrainer;
use poker::training::{
    evaluate_blueprint_vs_baseline_multiway, Game, NlheBaselinePolicy, NlheEvaluationConfig,
    NlheMultiwayEvalReport,
};
use poker::{BucketTable, TableConfig};

fn main() -> ExitCode {
    match run() {
        Ok(all_beat) => {
            if all_beat {
                ExitCode::SUCCESS
            } else {
                // gate 未全过：非零退出，便于脚本判定。
                eprintln!("[six_max_eval] S4 gate NOT fully passed (见上方 verdict)");
                ExitCode::from(1)
            }
        }
        Err(e) => {
            eprintln!("[six_max_eval] failed: {e}");
            ExitCode::from(2)
        }
    }
}

struct Args {
    bucket_table: String,
    checkpoint: String,
    postflop_cap: u8,
    hands_per_seat: u64,
    seed: u64,
}

fn run() -> Result<bool, String> {
    let args = parse_args()?;
    let table = Arc::new(
        BucketTable::open(std::path::Path::new(&args.bucket_table))
            .map_err(|e| format!("BucketTable::open({}) failed: {e:?}", args.bucket_table))?,
    );
    if !matches!(args.postflop_cap, 2 | 3) {
        return Err(format!(
            "--postflop-cap must be 2 or 3, got {}",
            args.postflop_cap
        ));
    }
    let (abs, rules) = first_small_6max(args.postflop_cap);
    let game = SimplifiedNlheGame::new_with_abstraction(
        Arc::clone(&table),
        TableConfig::default_6max_100bb(),
        abs,
        rules,
    )
    .map_err(|e| format!("build six-max game failed: {e:?}"))?;

    let trainer =
        DenseNlheEsMccfrTrainer::load_checkpoint(std::path::Path::new(&args.checkpoint), game)
            .map_err(|e| format!("load checkpoint {} failed: {e:?}", args.checkpoint))?;
    let game = trainer.game();
    let n = game.n_players();

    eprintln!("[six_max_eval] checkpoint    = {}", args.checkpoint);
    eprintln!("[six_max_eval] update_count  = {}", trainer.update_count());
    eprintln!(
        "[six_max_eval] n_players     = {n} (postflop_cap={})",
        args.postflop_cap
    );
    eprintln!(
        "[six_max_eval] hands/seat    = {} (total {} hands per baseline)",
        args.hands_per_seat,
        args.hands_per_seat * n as u64
    );
    eprintln!("[six_max_eval] seed         = 0x{:016x}", args.seed);

    // blueprint = dense average strategy；harness 对空 Vec（未访问）兜底均匀分布。
    let blueprint = |info: &poker::InfoSetId, _n: usize| trainer.average_strategy(*info);

    let config = NlheEvaluationConfig {
        hands_per_seat: args.hands_per_seat,
        seed: args.seed,
        max_actions_per_hand: 512,
    };

    let baselines = [
        NlheBaselinePolicy::Random,
        NlheBaselinePolicy::CallStation,
        NlheBaselinePolicy::OverlyTight,
    ];

    let mut all_beat = true;
    for baseline in baselines {
        let report = evaluate_blueprint_vs_baseline_multiway(game, &blueprint, baseline, &config)
            .map_err(|e| format!("eval vs {} failed: {e:?}", baseline.label()))?;
        let beat = print_report(&report);
        all_beat &= beat;
    }
    eprintln!(
        "[six_max_eval] S4 gate（1M 手稳定击败 random/call-station/overly-tight）: {}",
        if all_beat {
            "PASS（全部 CI95 下界 > 0）"
        } else {
            "FAIL"
        }
    );
    Ok(all_beat)
}

/// 打印一个 baseline 的评测报告；返回是否「显著击败」（CI95 下界 > 0）。
fn print_report(r: &NlheMultiwayEvalReport) -> bool {
    let beat = r.ci95_low_mbb_per_game > 0.0;
    println!(
        "=== vs {} ({} hands) ===\n  \
         mbb/g = {:+.2}  SE = {:.2}  CI95 = [{:+.2}, {:+.2}]  → {}",
        r.baseline.label(),
        r.hands,
        r.mbb_per_game,
        r.standard_error_mbb_per_game,
        r.ci95_low_mbb_per_game,
        r.ci95_high_mbb_per_game,
        if beat {
            "BEAT (显著 > 0)"
        } else if r.ci95_high_mbb_per_game < 0.0 {
            "LOSE (显著 < 0)"
        } else {
            "tie (CI 跨 0)"
        },
    );
    // 按位置拆收益（6-max：BTN/SB/BB/UTG/HJ/CO）。
    let labels = position_labels(r.n_players);
    let per_pos: Vec<String> = r
        .per_position_mbb_per_game
        .iter()
        .enumerate()
        .map(|(i, v)| format!("{}={:+.1}", labels[i], v))
        .collect();
    println!("  per-position mbb/g: {}", per_pos.join("  "));
    beat
}

/// 相对按钮的位置标签（offset 0 = BTN）。6-max 用标准名；其他人数退化为 pos{i}。
fn position_labels(n: usize) -> Vec<String> {
    if n == 6 {
        ["BTN", "SB", "BB", "UTG", "HJ", "CO"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    } else if n == 2 {
        vec!["BTN/SB".to_string(), "BB".to_string()]
    } else {
        (0..n).map(|i| format!("pos{i}")).collect()
    }
}

fn parse_args() -> Result<Args, String> {
    let mut bucket_table = String::new();
    let mut checkpoint = String::new();
    let mut postflop_cap = 3u8;
    let mut hands_per_seat = 170_000u64;
    let mut seed = 0x3645_5641_4C5F_4556u64; // "6EVAL_EV"-ish
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--bucket-table" => bucket_table = next(&mut it, "--bucket-table")?,
            "--checkpoint" => checkpoint = next(&mut it, "--checkpoint")?,
            "--postflop-cap" => {
                postflop_cap = next(&mut it, "--postflop-cap")?
                    .parse()
                    .map_err(|e| format!("bad --postflop-cap: {e}"))?
            }
            "--hands-per-seat" => {
                hands_per_seat = next(&mut it, "--hands-per-seat")?
                    .parse()
                    .map_err(|e| format!("bad --hands-per-seat: {e}"))?
            }
            "--seed" => {
                let raw = next(&mut it, "--seed")?;
                seed = raw
                    .strip_prefix("0x")
                    .map(|h| u64::from_str_radix(h, 16))
                    .unwrap_or_else(|| raw.parse())
                    .map_err(|e| format!("bad --seed: {e}"))?;
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if bucket_table.is_empty() {
        return Err("--bucket-table is required".to_string());
    }
    if checkpoint.is_empty() {
        return Err("--checkpoint is required".to_string());
    }
    if hands_per_seat == 0 {
        return Err("--hands-per-seat must be > 0".to_string());
    }
    Ok(Args {
        bucket_table,
        checkpoint,
        postflop_cap,
        hands_per_seat,
        seed,
    })
}

fn next(it: &mut impl Iterator<Item = String>, name: &str) -> Result<String, String> {
    it.next().ok_or_else(|| format!("{name} requires a value"))
}
