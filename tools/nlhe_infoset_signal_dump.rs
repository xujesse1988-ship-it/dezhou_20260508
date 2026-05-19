//! 简化 NLHE infoset 学习信号强度分析工具。
//!
//! 从 checkpoint 加载 ES-MCCFR trainer，dump 每个 infoset 的
//! `Σ_a strategy_sum[I][a]`（= 该 infoset 上所有 traverser 访问的 π_trav
//! 累计和，即该 infoset 对最终 average strategy 输出的贡献权重）。
//!
//! 输出：
//! - 按 street_tag (preflop/flop/turn/river) 分桶的 percentile 分布
//! - top-k% 占总信号比例（集中度）
//! - Gini 系数（0 = 完全均匀，1 = 全部信号集中在 1 个 infoset）
//! - log10 直方图
//! - 低信号 cutoff 计数（"学习不足" infoset 数）
//! - 按 legal action 数的 infoset 分布
//!
//! 用法：
//! ```
//! cargo run --release --bin nlhe_infoset_signal_dump -- \
//!     --checkpoint <PATH> \
//!     --bucket-table <PATH> \
//!     --output <MD_PATH>
//! ```

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use poker::training::nlhe::SimplifiedNlheGame;
use poker::training::{EsMccfrTrainer, Trainer};
use poker::{BucketTable, InfoSetId, StreetTag};

struct Args {
    checkpoint: PathBuf,
    bucket_table: PathBuf,
    output: PathBuf,
}

fn parse_args() -> Result<Args, String> {
    let mut checkpoint: Option<PathBuf> = None;
    let mut bucket_table: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut take = || -> Result<String, String> {
            args.next()
                .ok_or_else(|| format!("missing value for {arg}"))
        };
        match arg.as_str() {
            "--checkpoint" => checkpoint = Some(PathBuf::from(take()?)),
            "--bucket-table" | "--artifact" => bucket_table = Some(PathBuf::from(take()?)),
            "--output" => output = Some(PathBuf::from(take()?)),
            "--help" | "-h" => {
                eprintln!(
                    "usage: nlhe_infoset_signal_dump --checkpoint PATH \
                     --bucket-table PATH --output PATH"
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown arg: {other}")),
        }
    }
    Ok(Args {
        checkpoint: checkpoint.ok_or("--checkpoint required")?,
        bucket_table: bucket_table.ok_or("--bucket-table required")?,
        output: output.ok_or("--output required")?,
    })
}

fn run(args: Args) -> Result<(), String> {
    let table = Arc::new(
        BucketTable::open(&args.bucket_table)
            .map_err(|e| format!("BucketTable::open failed: {e:?}"))?,
    );
    let game = SimplifiedNlheGame::new(Arc::clone(&table))
        .map_err(|e| format!("SimplifiedNlheGame::new failed: {e:?}"))?;
    let trainer =
        <EsMccfrTrainer<SimplifiedNlheGame> as Trainer<SimplifiedNlheGame>>::load_checkpoint(
            &args.checkpoint,
            game,
        )
        .map_err(|e| format!("load_checkpoint failed: {e:?}"))?;

    // Collect per-infoset signal weight = Σ_a strategy_sum[I][a].
    let inner = trainer.strategy_sum().inner();
    let mut by_street: BTreeMap<u8, Vec<f64>> = BTreeMap::new();
    let mut all_weights: Vec<f64> = Vec::with_capacity(inner.len());
    let mut action_count: BTreeMap<usize, usize> = BTreeMap::new();
    for (info, vec) in inner {
        let weight: f64 = vec.iter().sum();
        let info: InfoSetId = *info;
        let street = info.street_tag() as u8;
        by_street.entry(street).or_default().push(weight);
        all_weights.push(weight);
        *action_count.entry(vec.len()).or_insert(0) += 1;
    }
    all_weights.sort_by(|a, b| a.partial_cmp(b).unwrap());

    let total: f64 = all_weights.iter().sum();
    let n_all = all_weights.len();

    let mut out = BufWriter::new(
        File::create(&args.output)
            .map_err(|e| format!("create {} failed: {e}", args.output.display()))?,
    );

    writeln!(out, "# Simplified NLHE Infoset Signal Strength Dump").unwrap();
    writeln!(out).unwrap();
    writeln!(out, "- checkpoint: `{}`", args.checkpoint.display()).unwrap();
    writeln!(out, "- update_count: `{}`", trainer.update_count()).unwrap();
    writeln!(out, "- bucket_table: `{}`", args.bucket_table.display()).unwrap();
    writeln!(out, "- n_visited_infosets: `{}`", n_all).unwrap();
    writeln!(
        out,
        "- total_signal (Σ_I Σ_a strategy_sum[I][a]): `{total:.6e}`"
    )
    .unwrap();
    writeln!(out).unwrap();
    writeln!(
        out,
        "信号权重 = `Σ_a strategy_sum[I][a]` = 该 infoset 上所有 traverser 访问的 \
         π_trav 累计和。值越大表示该 infoset 上的训练对最终 average strategy \
         输出贡献越大；值接近 0 表示访问稀少或 π_trav 几乎为 0 的尾部节点。"
    )
    .unwrap();
    writeln!(out).unwrap();

    // ------------------------------------------------------------------
    // 1) 按街 percentile 分布
    // ------------------------------------------------------------------
    writeln!(out, "## 1. 信号强度按街分布").unwrap();
    writeln!(out).unwrap();
    writeln!(
        out,
        "| street | n_infosets | sum | min | p10 | p50 | p90 | p99 | max | mean |"
    )
    .unwrap();
    writeln!(out, "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|").unwrap();
    for (street, mut weights) in by_street.iter().map(|(k, v)| (*k, v.clone())) {
        weights.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let n = weights.len();
        let sum: f64 = weights.iter().sum();
        let pct = |q: f64| -> f64 {
            let idx = ((n as f64 - 1.0) * q).round() as usize;
            weights[idx.min(n - 1)]
        };
        writeln!(
            out,
            "| {} | {} | {:.3e} | {:.3e} | {:.3e} | {:.3e} | {:.3e} | {:.3e} | {:.3e} | {:.3e} |",
            street_name(street),
            n,
            sum,
            weights[0],
            pct(0.10),
            pct(0.50),
            pct(0.90),
            pct(0.99),
            weights[n - 1],
            sum / n as f64
        )
        .unwrap();
    }
    // overall
    {
        let pct = |q: f64| -> f64 {
            let idx = ((n_all as f64 - 1.0) * q).round() as usize;
            all_weights[idx.min(n_all - 1)]
        };
        writeln!(
            out,
            "| **all** | {} | {:.3e} | {:.3e} | {:.3e} | {:.3e} | {:.3e} | {:.3e} | {:.3e} | {:.3e} |",
            n_all,
            total,
            all_weights[0],
            pct(0.10),
            pct(0.50),
            pct(0.90),
            pct(0.99),
            all_weights[n_all - 1],
            total / n_all as f64
        )
        .unwrap();
    }
    writeln!(out).unwrap();

    // ------------------------------------------------------------------
    // 2) 集中度
    // ------------------------------------------------------------------
    writeln!(out, "## 2. 集中度（top-k% 占总信号比例）").unwrap();
    writeln!(out).unwrap();
    writeln!(
        out,
        "完全均匀分布 → top-k% 占 k% 信号；越偏离对角线越长尾。"
    )
    .unwrap();
    writeln!(out).unwrap();
    writeln!(out, "| top % of infosets | n | share of total signal |").unwrap();
    writeln!(out, "|---:|---:|---:|").unwrap();
    for &pct in &[0.1_f64, 0.5, 1.0, 5.0, 10.0, 25.0, 50.0] {
        let k = ((pct / 100.0) * n_all as f64).ceil() as usize;
        let top_sum: f64 = all_weights.iter().rev().take(k).sum();
        let share = if total > 0.0 {
            100.0 * top_sum / total
        } else {
            0.0
        };
        writeln!(out, "| top {pct:.1}% | {k} | {share:.2}% |").unwrap();
    }
    writeln!(out).unwrap();

    // Gini coefficient: ascending sort; Gini = (Σ (2i - n + 1) * x_i) / (n * Σ x_i)
    // 索引 i 从 1 起。0 = 完全均匀；1 = 全集中。
    let gini = if total > 0.0 {
        let mut acc = 0.0;
        for (i, w) in all_weights.iter().enumerate() {
            let one_indexed = i as f64 + 1.0;
            acc += (2.0 * one_indexed - n_all as f64 - 1.0) * w;
        }
        acc / (n_all as f64 * total)
    } else {
        0.0
    };
    writeln!(
        out,
        "**Gini 系数: `{gini:.4}`**（0 = 完全均匀；1 = 全部信号集中在 1 个 infoset）"
    )
    .unwrap();
    writeln!(out).unwrap();

    // 按街 Gini
    writeln!(out, "按街 Gini：").unwrap();
    writeln!(out).unwrap();
    writeln!(out, "| street | gini |").unwrap();
    writeln!(out, "|---|---:|").unwrap();
    for (street, weights_unsorted) in &by_street {
        let mut weights = weights_unsorted.clone();
        weights.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let n = weights.len();
        let sum: f64 = weights.iter().sum();
        let g = if sum > 0.0 && n > 0 {
            let mut acc = 0.0;
            for (i, w) in weights.iter().enumerate() {
                let one_indexed = i as f64 + 1.0;
                acc += (2.0 * one_indexed - n as f64 - 1.0) * w;
            }
            acc / (n as f64 * sum)
        } else {
            0.0
        };
        writeln!(out, "| {} | {g:.4} |", street_name(*street)).unwrap();
    }
    writeln!(out).unwrap();

    // ------------------------------------------------------------------
    // 3) log10 直方图
    // ------------------------------------------------------------------
    writeln!(out, "## 3. log10 直方图（按信号权重）").unwrap();
    writeln!(out).unwrap();
    writeln!(out, "| bucket (signal weight) | n_infosets | pct |").unwrap();
    writeln!(out, "|---|---:|---:|").unwrap();
    let mut buckets: BTreeMap<i32, usize> = BTreeMap::new();
    for &w in &all_weights {
        let key = if w <= 0.0 {
            i32::MIN
        } else {
            w.log10().floor() as i32
        };
        *buckets.entry(key).or_insert(0) += 1;
    }
    for (key, count) in &buckets {
        let label = if *key == i32::MIN {
            "≤ 0".to_string()
        } else {
            format!("[10^{}, 10^{})", key, key + 1)
        };
        writeln!(
            out,
            "| {label} | {count} | {:.2}% |",
            100.0 * *count as f64 / n_all as f64
        )
        .unwrap();
    }
    writeln!(out).unwrap();

    // ------------------------------------------------------------------
    // 4) 学习不足 infoset 计数
    // ------------------------------------------------------------------
    writeln!(out, "## 4. 学习不足 infoset 计数").unwrap();
    writeln!(out).unwrap();
    writeln!(out, "| threshold | n_infosets below | pct |").unwrap();
    writeln!(out, "|---:|---:|---:|").unwrap();
    for &th in &[1e-6_f64, 1e-4, 1e-2, 1.0, 100.0, 1e4] {
        let count = all_weights.iter().filter(|w| **w < th).count();
        writeln!(
            out,
            "| < {th:.0e} | {count} | {:.2}% |",
            100.0 * count as f64 / n_all as f64
        )
        .unwrap();
    }
    writeln!(out).unwrap();

    // ------------------------------------------------------------------
    // 5) 按 legal action 数分布
    // ------------------------------------------------------------------
    writeln!(out, "## 5. 按 legal action 数分布").unwrap();
    writeln!(out).unwrap();
    writeln!(out, "| n_actions | n_infosets |").unwrap();
    writeln!(out, "|---:|---:|").unwrap();
    for (k, v) in &action_count {
        writeln!(out, "| {k} | {v} |").unwrap();
    }
    writeln!(out).unwrap();

    out.flush().map_err(|e| format!("flush failed: {e}"))?;
    eprintln!("[nlhe_infoset_signal_dump] wrote {}", args.output.display());
    Ok(())
}

fn street_name(s: u8) -> &'static str {
    match s {
        x if x == StreetTag::Preflop as u8 => "preflop",
        x if x == StreetTag::Flop as u8 => "flop",
        x if x == StreetTag::Turn as u8 => "turn",
        x if x == StreetTag::River as u8 => "river",
        _ => "?",
    }
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("[nlhe_infoset_signal_dump] argument error: {e}");
            return ExitCode::from(2);
        }
    };
    if let Some(parent) = args.output.parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(e) = fs::create_dir_all(parent) {
                eprintln!("[nlhe_infoset_signal_dump] create dir failed: {e}");
                return ExitCode::from(1);
            }
        }
    }
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("[nlhe_infoset_signal_dump] error: {e}");
            ExitCode::from(1)
        }
    }
}
