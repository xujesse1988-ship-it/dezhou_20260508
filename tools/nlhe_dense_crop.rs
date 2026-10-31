//! dense blueprint 按街裁切 CLI（见 [`poker::training::nlhe_dense_crop`]）。
//!
//! 把 `train_cfr --dense` 产出的 checkpoint 裁成只含前若干街（默认翻前 + 翻牌）的 crop 文件。
//! 游戏参数（`--profile/--seats/--reshape/--postflop-cap/--stack-bb/--ante/--bucket-table`）必须与
//! 训练时一致——用它重建全树 layout 指纹与 checkpoint 比对，不符直接拒绝。全程流式，内存
//! ≈ 全树 indexer + touched bitset（9-max cap4 500 桶 < 2 GiB），可与训练同机并行跑。
//!
//! ```bash
//! cargo run --release --bin nlhe_dense_crop -- \
//!     --checkpoint artifacts/run_x/nlhe_es_mccfr_final_xxx.ckpt --out artifacts/run_x/pre_flop.crop \
//!     --bucket-table <v4 table> --profile six-max --seats 9 --reshape all-1pot \
//!     --postflop-cap 4 --stack-bb 100 [--keep-through flop] [--strategy-only] [--verify]
//! ```
//!
//! 已有的 crop 再裁到更早的街（如本机只要翻前 σ 的离线工具）：`--from-crop <crop 文件 | ->`
//! 代替 `--checkpoint`（`-` = 从 stdin 读，可接 `zstd -dc x.crop.zst |`），输出恒不含 regret。

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use poker::training::nlhe::SimplifiedNlheGame;
use poker::training::nlhe_betting_tree::{six_max_reshape, SIX_MAX_RESHAPE_NAMES};
use poker::training::nlhe_dense_crop::{
    crop_dense_checkpoint, recrop, CropOptions, CroppedBlueprint,
};
use poker::{BucketTable, ChipAmount, StreetTag, TableConfig};

struct Args {
    checkpoint: PathBuf,
    /// `--from-crop`：源是已有 crop（`-` = stdin），再裁到 `--keep-through`（[`recrop`]）。
    from_crop: Option<PathBuf>,
    out: PathBuf,
    bucket_table: PathBuf,
    profile: String,
    seats: u8,
    reshape: String,
    postflop_cap: u8,
    stack_bb: u32,
    /// 每座 ante（chips），须与 `train_cfr --ante` 相同（ante 进 layout 指纹）。
    ante: u64,
    keep_through: StreetTag,
    strategy_only: bool,
    verify: bool,
}

fn usage() -> &'static str {
    "usage: nlhe_dense_crop (--checkpoint <dense ckpt> | --from-crop <crop|->) --out <crop file> --bucket-table <path>\n\
     \t[--profile hu|six-max (default six-max)] [--seats <N> (default 6)]\n\
     \t[--reshape <name> (default none)] [--postflop-cap <N> (default 3)] [--stack-bb <N> (default 100)]\n\
     \t[--ante <chips> (default 0)]\n\
     \t[--keep-through preflop|flop|turn (default flop)] [--strategy-only] [--verify]\n\
     game args must match the ones used by train_cfr (layout fingerprint is checked)."
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        checkpoint: PathBuf::new(),
        from_crop: None,
        out: PathBuf::new(),
        bucket_table: PathBuf::new(),
        profile: "six-max".to_string(),
        seats: 6,
        reshape: "none".to_string(),
        postflop_cap: 3,
        stack_bb: 100,
        ante: 0,
        keep_through: StreetTag::Flop,
        strategy_only: false,
        verify: false,
    };
    let mut it = std::env::args().skip(1);
    let num = |v: Option<String>, name: &str| -> Result<u64, String> {
        let v = v.ok_or_else(|| format!("{name} requires a value"))?;
        v.parse::<u64>()
            .map_err(|e| format!("invalid {name} {v}: {e}"))
    };
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--checkpoint" => {
                a.checkpoint = it.next().ok_or("--checkpoint requires a value")?.into()
            }
            "--from-crop" => {
                a.from_crop = Some(it.next().ok_or("--from-crop requires a value")?.into())
            }
            "--out" => a.out = it.next().ok_or("--out requires a value")?.into(),
            "--bucket-table" => {
                a.bucket_table = it.next().ok_or("--bucket-table requires a value")?.into()
            }
            "--profile" => a.profile = it.next().ok_or("--profile requires a value")?,
            "--reshape" => a.reshape = it.next().ok_or("--reshape requires a value")?,
            "--seats" => {
                let v = num(it.next(), "--seats")?;
                a.seats = u8::try_from(v).map_err(|_| format!("--seats {v} out of range"))?;
            }
            "--postflop-cap" => {
                let v = num(it.next(), "--postflop-cap")?;
                a.postflop_cap =
                    u8::try_from(v).map_err(|_| format!("--postflop-cap {v} out of range"))?;
            }
            "--stack-bb" => {
                let v = num(it.next(), "--stack-bb")?;
                a.stack_bb =
                    u32::try_from(v).map_err(|_| format!("--stack-bb {v} out of range"))?;
            }
            "--ante" => a.ante = num(it.next(), "--ante")?,
            "--keep-through" => {
                a.keep_through = match it.next().as_deref() {
                    Some("preflop") => StreetTag::Preflop,
                    Some("flop") => StreetTag::Flop,
                    Some("turn") => StreetTag::Turn,
                    other => {
                        return Err(format!(
                            "--keep-through expects preflop|flop|turn, got {other:?}"
                        ))
                    }
                }
            }
            "--strategy-only" => a.strategy_only = true,
            "--verify" => a.verify = true,
            "--help" | "-h" => {
                eprintln!("{}", usage());
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}\n{}", usage())),
        }
    }
    let has_checkpoint = !a.checkpoint.as_os_str().is_empty();
    if a.from_crop.is_some() == has_checkpoint {
        return Err(format!(
            "exactly one of --checkpoint / --from-crop is required\n{}",
            usage()
        ));
    }
    for (v, name) in [(&a.out, "--out"), (&a.bucket_table, "--bucket-table")] {
        if v.as_os_str().is_empty() {
            return Err(format!("{name} is required\n{}", usage()));
        }
    }
    if a.checkpoint == a.out || a.from_crop.as_ref() == Some(&a.out) {
        return Err("--out must differ from the source".to_string());
    }
    Ok(a)
}

fn build_game(a: &Args, table: Arc<BucketTable>) -> Result<SimplifiedNlheGame, String> {
    let ante = ChipAmount::new(a.ante);
    match a.profile.as_str() {
        "hu" => SimplifiedNlheGame::new_hu_with_config(
            table,
            TableConfig::default_hu_200bb().with_ante(ante),
        )
        .map_err(|e| format!("build hu game: {e:?}")),
        "six-max" => {
            let (abs, rules) = six_max_reshape(&a.reshape, a.postflop_cap).ok_or_else(|| {
                format!(
                    "unknown --reshape {} (expected {SIX_MAX_RESHAPE_NAMES})",
                    a.reshape
                )
            })?;
            SimplifiedNlheGame::new_with_abstraction(
                table,
                TableConfig::n_max_at_bb(a.seats, a.stack_bb).with_ante(ante),
                abs,
                rules,
            )
            .map_err(|e| format!("build six-max game: {e:?}"))
        }
        other => Err(format!("unknown --profile {other} (expected hu | six-max)")),
    }
}

fn gib(bytes: u64) -> f64 {
    bytes as f64 / (1u64 << 30) as f64
}

fn run() -> Result<(), String> {
    let a = parse_args()?;
    let t0 = Instant::now();
    let table = Arc::new(
        BucketTable::open(&a.bucket_table)
            .map_err(|e| format!("BucketTable::open({}): {e:?}", a.bucket_table.display()))?,
    );
    let game = build_game(&a, table)?;
    eprintln!(
        "[crop] game built: n_players={} tree_nodes={} ({:.1}s)",
        poker::training::Game::n_players(&game),
        game.tree().num_nodes(),
        t0.elapsed().as_secs_f64()
    );

    let t1 = Instant::now();
    let opts = CropOptions {
        keep_through: a.keep_through,
        keep_regret: !a.strategy_only,
    };
    let s = match &a.from_crop {
        Some(src) if src.as_os_str() == "-" => {
            recrop(std::io::stdin().lock(), &a.out, &game, a.keep_through)
        }
        Some(src) => {
            let f = std::fs::File::open(src).map_err(|e| format!("open {}: {e}", src.display()))?;
            recrop(f, &a.out, &game, a.keep_through)
        }
        None => crop_dense_checkpoint(&a.checkpoint, &a.out, &game, opts),
    }
    .map_err(|e| format!("crop failed: {e}"))?;
    eprintln!(
        "[crop] done in {:.1}s: update_count={} keep_through={:?} regret={}",
        t1.elapsed().as_secs_f64(),
        s.source_update_count,
        a.keep_through,
        if a.strategy_only || a.from_crop.is_some() {
            "dropped"
        } else {
            "kept"
        },
    );
    eprintln!(
        "[crop] nodes {} -> {} | infosets {} -> {} | slots {} -> {} ({:.2}%)",
        s.source_nodes,
        s.kept_nodes,
        s.source_rows,
        s.kept_rows,
        s.source_slots,
        s.kept_slots,
        100.0 * s.kept_slots as f64 / s.source_slots as f64
    );
    eprintln!(
        "[crop] output {} = {} bytes ({:.2} GiB)",
        a.out.display(),
        s.output_bytes,
        gib(s.output_bytes)
    );

    if a.verify {
        let t2 = Instant::now();
        let bp = CroppedBlueprint::load(&a.out, &game)
            .map_err(|e| format!("verify: reload crop failed: {e}"))?;
        if bp.update_count() != s.source_update_count || bp.keep_through() != a.keep_through {
            return Err("verify: reloaded crop metadata differs".to_string());
        }
        eprintln!(
            "[crop] verify ok: reloaded + trailer/fingerprint checked ({:.1}s)",
            t2.elapsed().as_secs_f64()
        );
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("[crop] error: {e}");
            ExitCode::FAILURE
        }
    }
}
