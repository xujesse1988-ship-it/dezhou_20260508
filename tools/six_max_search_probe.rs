//! S6 6a step-6：实时搜索**不退化探针**（search-on vs search-off，受控 A/B）。
//!
//! 回答 `docs/temp/realtime_search_design_2026_06_03.md` §2/§10 step 6 的探针问题：把
//! **同一**个 blueprint 拆成两个参赛者——hero = blueprint + 实时搜索（subgame re-solve），
//! field = 纯 blueprint——用 [`evaluate_cross_abstraction_h2h`] 跑同 seed 池、同座次轮换的
//! 受控对局，输出 hero（search-on）视角 mbb/g + CI95。统计金标准 = 配对差 + CI（与 S5 h2h
//! 一致），**取代** 6-max 失去理论意义的 LBR/exploitability 闸门。
//!
//! 判读：
//! - CI95 跨 0 → **不退化**（搜索没把对局打坏，plumbing 健康）。
//! - CI95 下界 > 0 → 搜索**显著正收益**（即便均匀-range 全解都能赢 blueprint → blueprint 该
//!   决策点偏弱）。
//! - CI95 上界 < 0 → 搜索**显著退化**（见下方 confound：未必是 blueprint 太弱）。
//!
//! # ⚠ MVP confound（务必随结果一并解读，`subgame.rs` 顶部 doc 有完整版）
//!
//! 本探针建在 6a **MVP** 搜索上，有三个有意为之的近似，使信号是**弱**信号：
//! 1. **uniform range**：subgame 在「各家 flop range 均匀」的错设游戏上求解（非 blueprint 真
//!    range）→ 退化可能源于 range 错设，**不**等于 blueprint 弱。
//! 2. **解到真实终局、无 blueprint 续局值 / biased leaf**：搜索里**没有 blueprint**，故本 MVP
//!    **测不到** §2 的「搜索放大 blueprint 偏差」——它测的是「均匀-range 全解 vs blueprint」。
//! 3. **per-bucket 欠采样**：postflop 200 桶，`--search-iterations` 摊到每桶很少 → 桶策略噪声大
//!    （CI 宽）、极端时回落 blueprint。提高迭代数才稳，但成本随手数线性放大。
//!
//! 故：CI 跨 0 = plumbing 健康（强结论）；CI<0 退化 = 弱信号（含 range/迭代 confound，别据此
//! 直接判 blueprint 太弱）。要把它升级成真正的 §2 判别器须接 §5b range + §5c blueprint 叶子值。
//!
//! 用法（vultr 小样本 smoke；真探针上 AWS）：
//! ```bash
//! cargo run --release --bin six_max_search_probe -- \
//!   --bucket-table artifacts/bucket_table_200_200_200_seed_cafebabe_schemav4.bin \
//!   --reshape nolimp --postflop-cap 3 \
//!   --checkpoint artifacts/run_6max_s4_nolimp/nlhe_es_mccfr_final_001000000000.ckpt \
//!   --hands-per-seat 2000 --search-iterations 1000
//! ```

use std::process::ExitCode;
use std::sync::Arc;

use poker::training::blueprint_advisor::{
    evaluate_cross_abstraction_h2h, Contestant, CrossAbstractionH2hReport, CrossH2hConfig,
};
use poker::training::nlhe::SimplifiedNlheGame;
use poker::training::nlhe_betting_tree::{
    first_small_6max, first_small_preopen_6max, first_small_preopen_small_6max,
    BettingAbstractionRules,
};
use poker::training::nlhe_dense_trainer::DenseNlheEsMccfrTrainer;
use poker::training::SubgameSearchConfig;
use poker::{BucketTable, InfoSetId, StreetActionAbstraction, TableConfig};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("[six_max_search_probe] failed: {e}");
            ExitCode::from(2)
        }
    }
}

struct Args {
    bucket_table: String,
    reshape: String,
    checkpoint: String,
    postflop_cap: u8,
    hands_per_seat: u64,
    seed: u64,
    search: SubgameSearchConfig,
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
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

    let (abs, rules) = reshape_profile(&args.reshape, args.postflop_cap)?;
    let game =
        SimplifiedNlheGame::new_with_abstraction(Arc::clone(&table), cfg.clone(), abs, rules)
            .map_err(|e| format!("build game failed: {e:?}"))?;
    let trainer =
        DenseNlheEsMccfrTrainer::load_checkpoint(std::path::Path::new(&args.checkpoint), game)
            .map_err(|e| format!("load checkpoint {} failed: {e:?}", args.checkpoint))?;
    eprintln!(
        "[six_max_search_probe] loaded reshape={} cap={} update_count={} ({})",
        args.reshape,
        args.postflop_cap,
        trainer.update_count(),
        args.checkpoint
    );
    eprintln!(
        "[six_max_search_probe] search: iterations={} max_subtree_nodes={} seed=0x{:016x} \
         （MVP：flop 未起注首决策点触发；uniform range；解到终局无 blueprint 叶子）",
        args.search.iterations, args.search.max_subtree_nodes, args.search.seed
    );

    // 同一 trainer 的 dense average strategy，hero/field 共用（blueprint 完全相同）；
    // 唯一差异 = hero.search = Some（命中触发面则 subgame re-solve），field.search = None。
    let strat = |info: &InfoSetId, _n: usize| trainer.average_strategy(*info);
    let hero = Contestant {
        game: trainer.game(),
        strategy: &strat,
        label: "search-on".to_string(),
        search: Some(args.search),
    };
    let field = Contestant {
        game: trainer.game(),
        strategy: &strat,
        label: "search-off".to_string(),
        search: None,
    };

    let h2h_config = CrossH2hConfig {
        hands_per_seat: args.hands_per_seat,
        seed: args.seed,
        max_actions_per_hand: 512,
    };
    eprintln!(
        "[six_max_search_probe] hands/seat={} (×6 座 = {} 手) seed=0x{:016x}",
        args.hands_per_seat,
        args.hands_per_seat * 6,
        args.seed
    );

    let report = evaluate_cross_abstraction_h2h(&hero, &field, &cfg, &h2h_config);
    print_report(&report);
    Ok(())
}

fn print_report(r: &CrossAbstractionH2hReport) {
    let desync_frac = if r.hands_attempted > 0 {
        r.desync_hands as f64 / r.hands_attempted as f64
    } else {
        0.0
    };
    let verdict = if r.ci95_low_mbb_per_game > 0.0 {
        "搜索显著正收益（CI95 下界 > 0）—— 即便均匀-range 全解都赢 blueprint，blueprint 该决策点偏弱"
    } else if r.ci95_high_mbb_per_game < 0.0 {
        "搜索显著退化（CI95 上界 < 0）—— ⚠ 含 range/迭代 confound，别直接判 blueprint 太弱，见 doc"
    } else {
        "不退化（CI95 跨 0）—— plumbing 健康；搜索在该 MVP 设定下与 blueprint 持平"
    };
    println!(
        "=== 实时搜索不退化探针：hero=search-on vs field=search-off ({} 手计入 / {} 尝试) ===",
        r.hands_counted, r.hands_attempted
    );
    println!(
        "  hero(search-on) mbb/g = {:+.2}  SE = {:.2}  CI95 = [{:+.2}, {:+.2}]",
        r.mbb_per_game,
        r.standard_error_mbb_per_game,
        r.ci95_low_mbb_per_game,
        r.ci95_high_mbb_per_game,
    );
    println!("  → {verdict}");
    let fallback_pct = if r.search_attempts > 0 {
        (1.0 - r.search_successes as f64 / r.search_attempts as f64) * 100.0
    } else {
        0.0
    };
    println!(
        "  search: 触发 {} 决策点, 真搜索 {} (fallback {:.1}% → 回落 blueprint)",
        r.search_attempts, r.search_successes, fallback_pct
    );
    if r.search_attempts == 0 {
        println!("  ⚠ search 从未触发（flop 未起注首决策点 0 次命中）—— mbb/g 必 ≈0，探针无意义");
    } else if fallback_pct > 80.0 {
        println!(
            "  ⚠ fallback 率高（{fallback_pct:.0}%）—— 多数触发点回落 blueprint，CI 主要由相同决策主导；\
             提高 --search-iterations 才让搜索真生效（per-bucket 欠采样，见 doc）"
        );
    }
    println!(
        "  desync={} ({:.2}%) illegal={}（同抽象自对弈应 ≈0；非 0 = search 路径异常，须查）",
        r.desync_hands,
        desync_frac * 100.0,
        r.illegal_hands,
    );
    let labels = ["BTN", "SB", "BB", "UTG", "HJ", "CO"];
    let per_pos: Vec<String> = r
        .per_position_mbb_per_game
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let l = labels.get(i).copied().unwrap_or("pos?");
            format!("{l}={v:+.0}")
        })
        .collect();
    println!("  per-position mbb/g (hero 该位): {}", per_pos.join("  "));
    println!(
        "  per-position 手数: {}",
        r.per_position_hands
            .iter()
            .map(|h| h.to_string())
            .collect::<Vec<_>>()
            .join("  ")
    );
}

fn reshape_profile(
    reshape: &str,
    postflop_cap: u8,
) -> Result<(StreetActionAbstraction, BettingAbstractionRules), String> {
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
    let mut reshape = "nolimp".to_string();
    let mut checkpoint = String::new();
    let mut postflop_cap = 3u8;
    let mut hands_per_seat = 2000u64;
    let mut seed = 0x5835_4831_5f48_3268u64;
    let mut search = SubgameSearchConfig::default();
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--bucket-table" => bucket_table = next(&mut it, "--bucket-table")?,
            "--reshape" => reshape = next(&mut it, "--reshape")?,
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
            "--seed" => seed = parse_u64(&next(&mut it, "--seed")?, "--seed")?,
            "--search-iterations" => {
                search.iterations = next(&mut it, "--search-iterations")?
                    .parse()
                    .map_err(|e| format!("bad --search-iterations: {e}"))?
            }
            "--search-max-nodes" => {
                search.max_subtree_nodes = next(&mut it, "--search-max-nodes")?
                    .parse()
                    .map_err(|e| format!("bad --search-max-nodes: {e}"))?
            }
            "--search-seed" => {
                search.seed = parse_u64(&next(&mut it, "--search-seed")?, "--search-seed")?
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
    if search.iterations == 0 {
        return Err("--search-iterations must be > 0".to_string());
    }
    Ok(Args {
        bucket_table,
        reshape,
        checkpoint,
        postflop_cap,
        hands_per_seat,
        seed,
        search,
    })
}

fn parse_u64(raw: &str, name: &str) -> Result<u64, String> {
    raw.strip_prefix("0x")
        .map(|h| u64::from_str_radix(h, 16))
        .unwrap_or_else(|| raw.parse())
        .map_err(|e| format!("bad {name}: {e}"))
}

fn next(it: &mut impl Iterator<Item = String>, name: &str) -> Result<String, String> {
    it.next().ok_or_else(|| format!("{name} requires a value"))
}
