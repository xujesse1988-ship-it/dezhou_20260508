//! S5① 相对强度：6-max blueprint **跨抽象**互评（受控自对弈）。
//!
//! 回答 `six_max_nlhe_target.md` S5 第一要务——「reshape 真的产出更强 blueprint 吗」。
//! 加载 ≥2 个 dense blueprint（可不同 betting tree：baseline / nolimp / preopen），用
//! [`evaluate_cross_abstraction_h2h`](poker::training::evaluate_cross_abstraction_h2h)
//! 跑一张权威 `GameState` + 每方抽象影子的 off-tree 对局，输出每个有序对
//! `(hero, field)` 的 hero 视角 mbb/g + CI95 + 按位置拆 + **desync 计数**。
//!
//! 用法（vultr，nolimp vs preopen ≈ 8 GiB 内存可装）：
//! ```bash
//! cargo run --release --bin six_max_blueprint_h2h -- \
//!   --bucket-table artifacts/bucket_table_200_200_200_seed_cafebabe_schemav4.bin \
//!   --postflop-cap 3 \
//!   --blueprint nolimp:artifacts/run_6max_s4_nolimp/nlhe_es_mccfr_final_001000000000.ckpt \
//!   --blueprint preopen:artifacts/run_6max_s4_preopen/nlhe_es_mccfr_auto_002000003072.ckpt \
//!   --hands-per-seat 50000
//! ```
//!
//! **正确性边界（必读）**：off-tree 只忠实表达**尺寸**差异。nolimp×preopen（都 no-limp，
//! 仅 preflop 开池尺寸不同）= 纯尺寸 → desync ≈ 0、结果可信。任何牵涉 baseline（含
//! open-limp）的对 → limp 进 no-limp 影子无对应节点 → 大量 desync，报告会显式计数；
//! desync 占比高的对**不可信**（见 `six_max_nlhe_target.md` S5）。

use std::process::ExitCode;
use std::sync::Arc;

use poker::training::blueprint_advisor::{
    evaluate_cross_abstraction_h2h, Contestant, CrossAbstractionH2hReport, CrossH2hConfig,
};
use poker::training::nlhe::SimplifiedNlheGame;
use poker::training::nlhe_betting_tree::{
    first_small_6max, first_small_preopen_6max, first_small_preopen_small_6max,
};
use poker::training::nlhe_dense_trainer::DenseNlheEsMccfrTrainer;
use poker::{BucketTable, InfoSetId, TableConfig};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("[six_max_blueprint_h2h] failed: {e}");
            ExitCode::from(2)
        }
    }
}

struct BlueprintSpec {
    label: String,
    reshape: String,
    checkpoint: String,
}

struct Args {
    bucket_table: String,
    postflop_cap: u8,
    blueprints: Vec<BlueprintSpec>,
    hands_per_seat: u64,
    seed: u64,
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    if args.blueprints.len() < 2 {
        return Err(format!(
            "需要 ≥2 个 --blueprint 做互评，收到 {}",
            args.blueprints.len()
        ));
    }
    if !matches!(args.postflop_cap, 2..=4) {
        return Err(format!(
            "--postflop-cap must be 2, 3, or 4, got {}",
            args.postflop_cap
        ));
    }

    let table = Arc::new(
        BucketTable::open(std::path::Path::new(&args.bucket_table))
            .map_err(|e| format!("BucketTable::open({}) failed: {e:?}", args.bucket_table))?,
    );
    let cfg = TableConfig::default_6max_100bb();

    // 加载每个 blueprint（同一桶表 Arc 共享，省内存）。
    let mut trainers: Vec<(String, DenseNlheEsMccfrTrainer)> =
        Vec::with_capacity(args.blueprints.len());
    for spec in &args.blueprints {
        let (abs, rules) = reshape_profile(&spec.reshape, args.postflop_cap)?;
        let game =
            SimplifiedNlheGame::new_with_abstraction(Arc::clone(&table), cfg.clone(), abs, rules)
                .map_err(|e| format!("build game[{}] failed: {e:?}", spec.label))?;
        let trainer =
            DenseNlheEsMccfrTrainer::load_checkpoint(std::path::Path::new(&spec.checkpoint), game)
                .map_err(|e| format!("load checkpoint {} failed: {e:?}", spec.checkpoint))?;
        eprintln!(
            "[six_max_blueprint_h2h] loaded {} reshape={} update_count={} ({})",
            spec.label,
            spec.reshape,
            trainer.update_count(),
            spec.checkpoint
        );
        trainers.push((spec.label.clone(), trainer));
    }

    // 策略闭包：dense average strategy（空 Vec → 引擎按 uniform 兜底）。各借自对应 trainer。
    // `+ Sync`：评测层 rayon 并行跑独立手时跨线程只读共享（average_strategy 是 &self 只读）。
    #[allow(clippy::type_complexity)]
    let strategies: Vec<Box<dyn Fn(&InfoSetId, usize) -> Vec<f64> + Sync + '_>> = trainers
        .iter()
        .map(|(_, t)| {
            let tref = t;
            Box::new(move |info: &InfoSetId, _n: usize| tref.average_strategy(*info))
                as Box<dyn Fn(&InfoSetId, usize) -> Vec<f64> + Sync + '_>
        })
        .collect();
    let contestants: Vec<Contestant> = trainers
        .iter()
        .zip(&strategies)
        .map(|((label, t), s)| Contestant {
            game: t.game(),
            strategy: s.as_ref(),
            label: label.clone(),
            search: None, // 互评工具纯 blueprint；实时搜索探针走 tools/six_max_search_probe。
            leaf_values: None,
        })
        .collect();

    let h2h_config = CrossH2hConfig {
        hands_per_seat: args.hands_per_seat,
        seed: args.seed,
        max_actions_per_hand: 512,
    };
    eprintln!(
        "[six_max_blueprint_h2h] hands/seat={} (×6 座 = {}/有序对) seed=0x{:016x}",
        args.hands_per_seat,
        args.hands_per_seat * 6,
        args.seed
    );

    // 所有有序对 (hero, field)。
    let mut reports: Vec<CrossAbstractionH2hReport> = Vec::new();
    for i in 0..contestants.len() {
        for j in 0..contestants.len() {
            if i == j {
                continue;
            }
            let report =
                evaluate_cross_abstraction_h2h(&contestants[i], &contestants[j], &cfg, &h2h_config);
            print_report(&report);
            reports.push(report);
        }
    }

    print_summary(&reports);
    Ok(())
}

fn print_report(r: &CrossAbstractionH2hReport) {
    let desync_frac = if r.hands_attempted > 0 {
        r.desync_hands as f64 / r.hands_attempted as f64
    } else {
        0.0
    };
    let trust = if desync_frac > 0.02 {
        "  ⚠ desync 占比高 → 结果不可信（结构性 gap）"
    } else {
        ""
    };
    let verdict = if r.ci95_low_mbb_per_game > 0.0 {
        "hero 显著更强 (CI95 下界 > 0)"
    } else if r.ci95_high_mbb_per_game < 0.0 {
        "hero 显著更弱 (CI95 上界 < 0)"
    } else {
        "未分出 (CI 跨 0)"
    };
    println!(
        "=== hero={} vs field={} ({} 手计入 / {} 尝试) ===",
        r.hero_label, r.field_label, r.hands_counted, r.hands_attempted
    );
    println!(
        "  hero mbb/g = {:+.2}  SE = {:.2}  CI95 = [{:+.2}, {:+.2}]  → {}",
        r.mbb_per_game,
        r.standard_error_mbb_per_game,
        r.ci95_low_mbb_per_game,
        r.ci95_high_mbb_per_game,
        verdict
    );
    println!(
        "  desync={} ({:.2}%) illegal={}{}",
        r.desync_hands,
        desync_frac * 100.0,
        r.illegal_hands,
        trust
    );
    let labels = position_labels(r.n_players);
    let per_pos: Vec<String> = r
        .per_position_mbb_per_game
        .iter()
        .enumerate()
        .map(|(i, v)| format!("{}={:+.0}", labels[i], v))
        .collect();
    println!("  per-position mbb/g (hero 该位): {}", per_pos.join("  "));
}

fn print_summary(reports: &[CrossAbstractionH2hReport]) {
    println!("\n===== 汇总（hero 视角 mbb/g，⚠=desync 占比 >2% 不可信）=====");
    for r in reports {
        let desync_frac = if r.hands_attempted > 0 {
            r.desync_hands as f64 / r.hands_attempted as f64
        } else {
            0.0
        };
        let flag = if desync_frac > 0.02 { " ⚠" } else { "" };
        println!(
            "  {:<10} vs {:<10}: {:+8.1}  CI95 [{:+.1}, {:+.1}]  desync {:.1}%{}",
            r.hero_label,
            r.field_label,
            r.mbb_per_game,
            r.ci95_low_mbb_per_game,
            r.ci95_high_mbb_per_game,
            desync_frac * 100.0,
            flag
        );
    }
}

fn position_labels(n: usize) -> Vec<String> {
    if n == 6 {
        ["BTN", "SB", "BB", "UTG", "HJ", "CO"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        (0..n).map(|i| format!("pos{i}")).collect()
    }
}

fn reshape_profile(
    reshape: &str,
    postflop_cap: u8,
) -> Result<
    (
        poker::StreetActionAbstraction,
        poker::training::nlhe_betting_tree::BettingAbstractionRules,
    ),
    String,
> {
    Ok(match reshape {
        "none" => first_small_6max(postflop_cap),
        "nolimp" => {
            let (a, mut r) = first_small_6max(postflop_cap);
            r.no_open_limp = true;
            (a, r)
        }
        "preopen" => first_small_preopen_6max(postflop_cap),
        "preopen-small" => first_small_preopen_small_6max(postflop_cap),
        other => {
            return Err(format!(
                "unknown reshape {other} (expected none | nolimp | preopen | preopen-small)"
            ))
        }
    })
}

fn parse_args() -> Result<Args, String> {
    let mut bucket_table = String::new();
    let mut postflop_cap = 3u8;
    let mut blueprints: Vec<BlueprintSpec> = Vec::new();
    let mut hands_per_seat = 50_000u64;
    let mut seed = 0x5835_4831_5f48_3268u64;
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--bucket-table" => bucket_table = next(&mut it, "--bucket-table")?,
            "--postflop-cap" => {
                postflop_cap = next(&mut it, "--postflop-cap")?
                    .parse()
                    .map_err(|e| format!("bad --postflop-cap: {e}"))?
            }
            "--blueprint" => {
                let raw = next(&mut it, "--blueprint")?;
                let (reshape, checkpoint) = raw.split_once(':').ok_or_else(|| {
                    format!("--blueprint 须为 <reshape>:<ckpt path>，收到 {raw:?}")
                })?;
                blueprints.push(BlueprintSpec {
                    label: reshape.to_string(),
                    reshape: reshape.to_string(),
                    checkpoint: checkpoint.to_string(),
                });
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
    if hands_per_seat == 0 {
        return Err("--hands-per-seat must be > 0".to_string());
    }
    Ok(Args {
        bucket_table,
        postflop_cap,
        blueprints,
        hands_per_seat,
        seed,
    })
}

fn next(it: &mut impl Iterator<Item = String>, name: &str) -> Result<String, String> {
    it.next().ok_or_else(|| format!("{name} requires a value"))
}
