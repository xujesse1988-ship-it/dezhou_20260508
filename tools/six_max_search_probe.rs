//! S6 6a step-6：实时搜索**不退化探针**（search-on vs search-off，受控 A/B）。
//!
//! 回答 `docs/temp/realtime_search_design_2026_06_03.md` §2/§10 step 6 的探针问题：把
//! **同一**个 blueprint 拆成两个参赛者——hero = blueprint + 实时搜索（subgame re-solve），
//! field = 纯 blueprint——用 [`evaluate_cross_abstraction_h2h`] 跑同 seed 池、同座次轮换的
//! 受控对局，输出 hero（search-on）视角 mbb/g + CI95。统计金标准 = 配对差 + CI（与 S5 h2h
//! 一致），**取代** 6-max 失去理论意义的 LBR/exploitability 闸门。
//!
//! 判读（§5b range 去 confound 后，探针**能**有意义测 §2「搜索放大 blueprint 偏差」）：
//! - CI95 跨 0 → 不退化（plumbing 健康；blueprint range 下全解与 blueprint 持平）。
//! - CI95 下界 > 0 → 搜索**显著正收益**（blueprint range 下的精确 subgame 全解赢 blueprint →
//!   基底够用、可继续堆 6b，§2 路线乙的「不退化甚至小赢」分支）。
//! - CI95 上界 < 0 → 搜索**显著退化**（§2 实锤候选：blueprint range 本身偏 → 全解放大它；
//!   但须先排除下方近似 #1/#3 的贡献，再判 blueprint 太弱）。
//!
//! # 边界（务必随结果一并解读，`subgame.rs` 顶部 doc 有完整版）
//!
//! §5b 后 subgame 在 blueprint 真 range（非均匀）上**解到真实 showdown 终局**（小子树、无叶子
//! 近似）。仍在的近似：
//! 1. **range = per-seat marginal + 桶粒度**：玩家间负相关只靠采样期 card-removal 近似；
//!    postflop range 落桶（有损）、preflop 精确。
//! 2. **per-bucket 欠采样**：`--search-iterations` 摊到每桶有限 → 桶策略有噪声、CI 偏宽；
//!    提高迭代数收敛（成本随手数线性放大）。`--uniform-range` 关 §5b 作 A/B 对照。
//! 3. search 触发/fallback 计数随报告输出——fallback 率高 = 多数触发点回落 blueprint，CI 主要
//!    由相同决策主导，须据此解读。
//!
//! 用法（vultr 小样本 smoke；大样本判决上 AWS）：
//! ```bash
//! cargo run --release --bin six_max_search_probe -- \
//!   --bucket-table artifacts/bucket_table_200_200_200_seed_cafebabe_schemav4.bin \
//!   --reshape nolimp --postflop-cap 3 \
//!   --checkpoint artifacts/run_6max_s4_nolimp/nlhe_es_mccfr_final_001000000000.ckpt \
//!   --hands-per-seat 2000 --search-iterations 1000
//!   # --trigger flop-first(默认,窄) | all-postflop(宽,放宽触发面)
//!   # --resolve round-start(默认,§6 #1 正确) | current-decision(旧 MVP,撞 landmine,A/B)
//!   # --uniform-range 关 §5b 作 MVP 对照
//!   # §10.5 关键 A/B：all-postflop × {round-start vs current-decision} 验 round-start 修复退化
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
use poker::training::{
    build_leaf_value_tables, default_continuations, ResolveRoot, SearchTrigger, SubgameSearchConfig,
};
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
    /// 6b depth-limit：叶子值表每续局 self-play 手数（仅 `search.depth_limit` 时用）。
    leaf_hands: u64,
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
         range={} trigger={:?} resolve={:?} leaf={}",
        args.search.iterations,
        args.search.max_subtree_nodes,
        args.search.seed,
        if args.search.use_blueprint_range {
            "blueprint(§5b 去 confound)"
        } else {
            "uniform(MVP)"
        },
        args.search.trigger,
        args.search.resolve_root,
        if args.search.depth_limit {
            if args.search.biased_leaf {
                "6b depth-limit 截当前街 + biased 续局(下一 actor argmax)"
            } else {
                "6b depth-limit 截当前街 + unbiased 续局"
            }
        } else {
            "6a 解到终局无 blueprint 叶子"
        }
    );

    // 6b depth-limit：先用同一 blueprint 跑 self-play 建叶子续局值表（unbiased + 默认 4 续局）。
    // 只在 depth_limit 时建（6a 解到终局不需要）。
    let leaf_values = if args.search.depth_limit {
        let conts = default_continuations();
        eprintln!(
            "[six_max_search_probe] 6b depth-limit：建叶子值表（{} 续局 × {} 手 self-play）…",
            conts.len(),
            args.leaf_hands
        );
        let t = std::sync::Arc::new(build_leaf_value_tables(
            &trainer,
            &conts,
            args.leaf_hands,
            args.seed ^ 0x6C65_6166_7661_6C75, // "leafvalu"
            512,
        ));
        eprintln!(
            "[six_max_search_probe]   叶子值表项数 = {}（unbiased 覆盖 = {}）",
            t.len(),
            t.populated_for_cont(0)
        );
        Some(t)
    } else {
        None
    };

    // 同一 trainer 的 dense average strategy，hero/field 共用（blueprint 完全相同）；
    // 唯一差异 = hero.search = Some（命中触发面则 subgame re-solve），field.search = None。
    let strat = |info: &InfoSetId, _n: usize| trainer.average_strategy(*info);
    let hero = Contestant {
        game: trainer.game(),
        strategy: &strat,
        label: "search-on".to_string(),
        search: Some(args.search),
        leaf_values: leaf_values.clone(),
    };
    let field = Contestant {
        game: trainer.game(),
        strategy: &strat,
        label: "search-off".to_string(),
        search: None,
        leaf_values: None,
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
        "搜索显著正收益（CI95 下界 > 0）—— blueprint range 下精确全解赢 blueprint，基底够用、可堆 6b"
    } else if r.ci95_high_mbb_per_game < 0.0 {
        "搜索显著退化（CI95 上界 < 0）—— §2 实锤候选（blueprint range 偏）；先排除 marginal/迭代近似再判"
    } else {
        "不退化（CI95 跨 0）—— plumbing 健康；blueprint range 下全解与 blueprint 持平"
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
    let mut leaf_hands = 200_000u64;
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
            // A/B 对照：关 §5b range，回 uniform resample（MVP 旧行为）。
            "--uniform-range" => search.use_blueprint_range = false,
            // 6b：depth-limit 搜索（子树街边界截断 + 叶子查 blueprint 续局值，绕开深层欠训练）。
            "--depth-limit" => search.depth_limit = true,
            // 6b-4：叶子续局用 biased（下一 actor argmax）= Modicum/Pluribus 鲁棒机制；否则 unbiased。
            "--biased-leaf" => search.biased_leaf = true,
            "--leaf-hands" => {
                leaf_hands = next(&mut it, "--leaf-hands")?
                    .parse()
                    .map_err(|e| format!("bad --leaf-hands: {e}"))?
            }
            // 触发面：all-postflop（宽）vs flop-first（默认，窄 A/B 基线）。
            "--trigger" => {
                search.trigger = match next(&mut it, "--trigger")?.as_str() {
                    "all-postflop" => SearchTrigger::AllPostflop,
                    "flop-first" => SearchTrigger::FlopFirstUnraised,
                    other => {
                        return Err(format!(
                            "unknown --trigger {other} (expected all-postflop | flop-first)"
                        ))
                    }
                }
            }
            // 重解根（§6 #1）：round-start（默认，正确）vs current-decision（旧 MVP，A/B）。
            "--resolve" => {
                search.resolve_root = match next(&mut it, "--resolve")?.as_str() {
                    "round-start" => ResolveRoot::RoundStart,
                    "current-decision" => ResolveRoot::CurrentDecision,
                    other => {
                        return Err(format!(
                            "unknown --resolve {other} (expected round-start | current-decision)"
                        ))
                    }
                }
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
        leaf_hands,
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
