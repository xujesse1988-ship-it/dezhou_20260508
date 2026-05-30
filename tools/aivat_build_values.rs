//! 构建 AIVAT 值函数表（NLHE 自对弈 Monte Carlo）。见 `docs/aivat_eval.md` §5。
//!
//! 用法：
//! ```text
//! aivat_build_values \
//!   --checkpoint artifacts/run_dense_lockfree/nlhe_es_mccfr_final_001000000000.ckpt \
//!   --bucket-table artifacts/bucket_table_default_1000_1000_1000_seed_cafebabe_schemav4.bin \
//!   --hands 2000000 --seed 20260530 --out artifacts/aivat_values.bin
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use poker::training::aivat_value::build_value_tables;
use poker::training::nlhe::SimplifiedNlheGame;
use poker::training::nlhe_dense_trainer::DenseNlheEsMccfrTrainer;
use poker::BucketTable;

struct Args {
    checkpoint: PathBuf,
    bucket_table: PathBuf,
    hands: u64,
    seed: u64,
    out: PathBuf,
    max_actions: usize,
}

fn parse_args() -> Result<Args, String> {
    let mut checkpoint: Option<PathBuf> = None;
    let mut bucket_table: Option<PathBuf> = None;
    let mut hands: u64 = 2_000_000;
    let mut seed: u64 = 20_260_530;
    let mut out: Option<PathBuf> = None;
    let mut max_actions: usize = 512;

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut next = |a: &str| -> Result<String, String> {
            it.next().ok_or_else(|| format!("{a} 缺参数值"))
        };
        match arg.as_str() {
            "--checkpoint" => checkpoint = Some(PathBuf::from(next(&arg)?)),
            "--bucket-table" => bucket_table = Some(PathBuf::from(next(&arg)?)),
            "--hands" => hands = next(&arg)?.parse().map_err(|e| format!("--hands: {e}"))?,
            "--seed" => seed = next(&arg)?.parse().map_err(|e| format!("--seed: {e}"))?,
            "--out" => out = Some(PathBuf::from(next(&arg)?)),
            "--max-actions" => {
                max_actions = next(&arg)?
                    .parse()
                    .map_err(|e| format!("--max-actions: {e}"))?
            }
            other => return Err(format!("未知参数 {other}")),
        }
    }
    Ok(Args {
        checkpoint: checkpoint.ok_or("缺 --checkpoint")?,
        bucket_table: bucket_table.ok_or("缺 --bucket-table")?,
        hands,
        seed,
        out: out.ok_or("缺 --out")?,
        max_actions,
    })
}

fn main() -> Result<(), String> {
    let args = parse_args()?;

    eprintln!(
        "[aivat-vf] 加载 bucket table {}",
        args.bucket_table.display()
    );
    let table = Arc::new(
        BucketTable::open(&args.bucket_table)
            .map_err(|e| format!("BucketTable::open 失败: {e:?}"))?,
    );
    let game =
        SimplifiedNlheGame::new(Arc::clone(&table)).map_err(|e| format!("game 构造失败: {e:?}"))?;

    eprintln!("[aivat-vf] 加载 checkpoint {}", args.checkpoint.display());
    let trainer = DenseNlheEsMccfrTrainer::load_checkpoint(&args.checkpoint, game)
        .map_err(|e| format!("load_checkpoint 失败: {e:?}"))?;

    let total_rows = trainer.strategy_sum().indexer().total_rows();
    let vf_bytes = total_rows * (8 + 4) * 2; // f64 mean + u32 count, 2 positions
    eprintln!(
        "[aivat-vf] update_count={} total_rows={} VF 表约 {:.2} GiB（含 count）",
        trainer.update_count(),
        total_rows,
        vf_bytes as f64 / (1u64 << 30) as f64
    );
    eprintln!(
        "[aivat-vf] 自对弈 {} 手，seed={}，max_actions={}",
        args.hands, args.seed, args.max_actions
    );

    let t0 = Instant::now();
    let tables = build_value_tables(&trainer, args.hands, args.seed, args.max_actions);
    let wall = t0.elapsed();

    // 覆盖率统计。
    for (pos, name) in [(0usize, "SB"), (1usize, "BB")] {
        let visited: u64 = tables.vf_count[pos].iter().filter(|&&c| c > 0).count() as u64;
        let total_obs: u64 = tables.vf_count[pos].iter().map(|&c| c as u64).sum();
        eprintln!(
            "[aivat-vf] {name}: 访问到 {visited}/{total_rows} 行（{:.1}%），累计观测 {total_obs}",
            100.0 * visited as f64 / total_rows as f64
        );
    }
    eprintln!(
        "[aivat-vf] 自对弈完成，wall={:.1}s（{:.0} 手/s）",
        wall.as_secs_f64(),
        args.hands as f64 / wall.as_secs_f64()
    );

    tables
        .save(&args.out)
        .map_err(|e| format!("保存 {} 失败: {e}", args.out.display()))?;
    eprintln!("[aivat-vf] 已写 {}", args.out.display());
    Ok(())
}
