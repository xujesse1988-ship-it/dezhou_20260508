//! VF-1 小表（169×6）：6-max blueprint **自对弈**的 `E[U | rel_pos, preflop 169 类]`
//! （solver chips，BB=100）——`aivat_multiway` 的 `c_deal_us` 值函数（缺口⑥；任意**固定**表
//! 保持无偏，表只影响降方差幅度，见 `aivat_multiway` 模块 doc）。
//!
//! 自对弈 = [`play_cross_abstraction_hand`]：全 6 座同一 blueprint（单影子、纯 on-tree、
//! desync ≈ 0）；每手 6 座各贡献一个 `(rel_pos, class169, U)` 样本。holes 由**同 `hand_seed`**
//! 的 `GameState::new` 在外部复现（权威局发牌确定性，harness 零改动）。
//!
//! ```bash
//! cargo run --release --bin vf1_deal_table_build -- \
//!   --bucket-table artifacts/bucket_table_200_200_200_seed_cafebabe_schemav4.bin \
//!   --postflop-cap 3 --reshape nolimp \
//!   --checkpoint artifacts/run_6max_s4_nolimp/nlhe_es_mccfr_final_001000000000.ckpt \
//!   --hands 100000 --out artifacts/vf1_deal_table.json
//! ```

use std::process::ExitCode;
use std::sync::Arc;

use rayon::prelude::*;

use poker::training::aivat_multiway::Vf1DealTable;
use poker::training::blueprint_advisor::{play_cross_abstraction_hand, Contestant, HandError};
use poker::training::nlhe::SimplifiedNlheGame;
use poker::training::nlhe_betting_tree::{
    first_small_6max, first_small_preopen_6max, first_small_preopen_small_6max,
};
use poker::training::nlhe_dense_trainer::DenseNlheEsMccfrTrainer;
use poker::{
    BucketTable, Card, ChaCha20Rng, GameState, InfoSetId, PreflopLossless169, TableConfig,
};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("[vf1_deal_table_build] failed: {e}");
            ExitCode::from(2)
        }
    }
}

struct Args {
    bucket_table: String,
    postflop_cap: u8,
    reshape: String,
    checkpoint: String,
    hands: u64,
    seed: u64,
    out: String,
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let table = Arc::new(
        BucketTable::open(std::path::Path::new(&args.bucket_table))
            .map_err(|e| format!("BucketTable::open({}) failed: {e:?}", args.bucket_table))?,
    );
    let cfg = TableConfig::default_6max_100bb();
    let n = cfg.n_seats as usize;
    let button = cfg.button_seat.0 as usize;

    let (abs, rules) = reshape_profile(&args.reshape, args.postflop_cap)?;
    let game =
        SimplifiedNlheGame::new_with_abstraction(Arc::clone(&table), cfg.clone(), abs, rules)
            .map_err(|e| format!("build game failed: {e:?}"))?;
    let trainer =
        DenseNlheEsMccfrTrainer::load_checkpoint(std::path::Path::new(&args.checkpoint), game)
            .map_err(|e| format!("load checkpoint {} failed: {e:?}", args.checkpoint))?;
    eprintln!(
        "[vf1_deal_table_build] loaded reshape={} update_count={} hands={} seed=0x{:016x}",
        args.reshape,
        trainer.update_count(),
        args.hands,
        args.seed
    );

    let strategy = |info: &InfoSetId, _n: usize| trainer.average_strategy(*info);
    let contestants = [Contestant {
        game: trainer.game(),
        strategy: &strategy,
        label: "self".to_string(),
        search: None,
        leaf_values: None,
    }];
    let seat_bp = vec![0usize; n];

    // 自对弈（每手独立 → rayon；seed 按手号确定派生 → 与线程调度无关、可复现）。
    enum Outcome {
        Sample {
            holes: Vec<[Card; 2]>,
            pnls: Vec<f64>,
        },
        Skipped,
        Illegal(String),
    }
    let tasks: Vec<u64> = (0..args.hands).collect();
    let outcomes: Vec<Outcome> = tasks
        .par_iter()
        .map(|&h| {
            let hand_seed = mix64(args.seed ^ h.wrapping_mul(0x9E37_79B9_7F4A_7C15));
            // 权威局发牌确定性：同 hand_seed 的 GameState::new 与 harness 内部 auth 同 holes。
            let deal = GameState::new(&cfg, hand_seed);
            let holes: Vec<[Card; 2]> = deal
                .players()
                .iter()
                .map(|p| p.hole_cards.expect("root 已发牌"))
                .collect();
            let mut sample_rng = ChaCha20Rng::from_seed(mix64(hand_seed ^ 0xA5A5_A5A5_5A5A_5A5A));
            match play_cross_abstraction_hand(
                &contestants,
                &seat_bp,
                &cfg,
                hand_seed,
                &mut sample_rng,
                512,
                None,
            ) {
                Ok(pnls) => Outcome::Sample { holes, pnls },
                Err(HandError::Desync(_)) | Err(HandError::NonTerminal) => Outcome::Skipped,
                Err(HandError::Illegal(e)) => Outcome::Illegal(e),
            }
        })
        .collect();

    // 串行 reduce（collect 保 task 顺序 → f64 加法顺序确定、可复现）。
    let pf = PreflopLossless169::new();
    let mut sums = vec![vec![0.0_f64; 169]; n];
    let mut counts = vec![vec![0u64; 169]; n];
    let mut skipped = 0u64;
    for o in &outcomes {
        match o {
            Outcome::Sample { holes, pnls } => {
                for s in 0..n {
                    let rel = (s + n - button) % n;
                    let class = usize::from(pf.hand_class(holes[s]));
                    sums[rel][class] += pnls[s];
                    counts[rel][class] += 1;
                }
            }
            Outcome::Skipped => skipped += 1,
            // 自对弈 Illegal = harness/引擎真 bug，必须上抛（不计数掩盖）。
            Outcome::Illegal(e) => return Err(format!("自对弈 Illegal（真 bug）: {e}")),
        }
    }

    let means: Vec<Vec<f64>> = sums
        .iter()
        .zip(&counts)
        .map(|(srow, crow)| {
            srow.iter()
                .zip(crow)
                .map(|(s, &c)| if c > 0 { s / c as f64 } else { 0.0 })
                .collect()
        })
        .collect();
    let covered = counts.iter().flatten().filter(|&&c| c > 0).count();
    let min_count = counts
        .iter()
        .flatten()
        .filter(|&&c| c > 0)
        .min()
        .copied()
        .unwrap_or(0);

    let table_out = Vf1DealTable {
        n_seats: n,
        big_blind_chips: cfg.big_blind.as_u64(),
        blueprint: args.checkpoint.clone(),
        reshape: args.reshape.clone(),
        hands_played: args.hands - skipped,
        hands_skipped: skipped,
        seed: args.seed,
        means,
        counts,
    };
    table_out.save(std::path::Path::new(&args.out))?;
    eprintln!(
        "[vf1_deal_table_build] done: {} 手计入（skip {skipped}）→ {}；覆盖格 {covered}/{}（最薄格 {min_count} 样本）",
        args.hands - skipped,
        args.out,
        n * 169
    );
    Ok(())
}

fn mix64(mut x: u64) -> u64 {
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
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
    let mut reshape = "nolimp".to_string();
    let mut checkpoint = String::new();
    let mut hands = 100_000u64;
    let mut seed = 0x5646_315F_4445_414Cu64; // "VF1_DEAL"
    let mut out = String::new();
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--bucket-table" => bucket_table = next(&mut it, "--bucket-table")?,
            "--postflop-cap" => {
                postflop_cap = next(&mut it, "--postflop-cap")?
                    .parse()
                    .map_err(|e| format!("bad --postflop-cap: {e}"))?
            }
            "--reshape" => reshape = next(&mut it, "--reshape")?,
            "--checkpoint" => checkpoint = next(&mut it, "--checkpoint")?,
            "--hands" => {
                hands = next(&mut it, "--hands")?
                    .parse()
                    .map_err(|e| format!("bad --hands: {e}"))?
            }
            "--seed" => {
                let raw = next(&mut it, "--seed")?;
                seed = raw
                    .strip_prefix("0x")
                    .map(|h| u64::from_str_radix(h, 16))
                    .unwrap_or_else(|| raw.parse())
                    .map_err(|e| format!("bad --seed: {e}"))?;
            }
            "--out" => out = next(&mut it, "--out")?,
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if bucket_table.is_empty() || checkpoint.is_empty() || out.is_empty() {
        return Err("--bucket-table / --checkpoint / --out are required".to_string());
    }
    if !matches!(postflop_cap, 2..=4) {
        return Err(format!(
            "--postflop-cap must be 2, 3, or 4, got {postflop_cap}"
        ));
    }
    if hands == 0 {
        return Err("--hands must be > 0".to_string());
    }
    Ok(Args {
        bucket_table,
        postflop_cap,
        reshape,
        checkpoint,
        hands,
        seed,
        out,
    })
}

fn next(it: &mut impl Iterator<Item = String>, name: &str) -> Result<String, String> {
    it.next().ok_or_else(|| format!("{name} requires a value"))
}
