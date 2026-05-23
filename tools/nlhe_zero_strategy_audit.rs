//! 简化 NLHE checkpoint「strategy_sum 全 0 节点」审计。
//!
//! ES-MCCFR 里 `StrategyAccumulator::accumulate` 在 traverser 访问 infoset 时
//! 加 `σ(I, a)`；σ 由 regret matching 归一化，要么是某一 action 的 1.0、要么是
//! 多 action 的非零概率，永远不是全 0。所以 entry 一旦被创建 → strategy_sum 至少
//! 有一个 a 上 > 0。
//!
//! 反过来「strategy_sum 全 0」等价于「这个 infoset 一次都没被 traverser 命中
//! 过，根本没进 HashMap」。average strategy fallback 到 uniform → 实战在这个
//! infoset 上是均匀乱打。
//!
//! 分母 = 完整 abstract betting tree × 该街 bucket_count：
//! - preflop nodes × 169（preflop hand class）
//! - flop / turn / river nodes × 500（postflop cluster）
//!
//! 分子 = `strategy_sum.inner()` 实际持有的 entry 数。InfoSetId v2 layout 高 26 bit
//! 是 node_id；按 node_id 分桶后，每个 node 上 present_count = 该节点实际访问到
//! 的 distinct bucket 数。
//!
//! 输出 5 张表（markdown）：
//! 1. 全局
//! 2. 按 street
//! 3. 按 first preflop action（root edge → 大致代表 betting line 起手）
//! 4. 按 node-level reach prob bin（uniform 自我 + uniform 对手 proxy，
//!    chance 隐含在 node→node 边里 → 不出现在 prob 里；用来近似 on-path / off-path）
//! 5. Top-N 最 starved 的 node（按"未覆盖 bucket 数 × node reach prob"排序），
//!    附 betting line path。
//!
//! 用法：
//! ```
//! cargo run --release --bin nlhe_zero_strategy_audit -- \
//!     --checkpoint <PATH> \
//!     --bucket-table <PATH> \
//!     --output <MD_PATH> \
//!     [--top-n 50]
//! ```
//!
//! 注：1.6 GB checkpoint 加载约 1-2 min，约 8 GB RAM peak（regret + strategy
//! 两张表 deserialize 后驻留内存）。本机内存不够时跑 vultr。

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use poker::training::nlhe::SimplifiedNlheGame;
use poker::training::nlhe_betting_tree::{AbstractActionTag, Child, NodeId, PublicBettingTree};
use poker::training::{EsMccfrTrainer, Trainer};
use poker::{BucketTable, StreetTag};

struct Args {
    checkpoint: PathBuf,
    bucket_table: PathBuf,
    output: PathBuf,
    top_n: usize,
}

fn parse_args() -> Result<Args, String> {
    let mut checkpoint: Option<PathBuf> = None;
    let mut bucket_table: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut top_n: usize = 50;
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
            "--top-n" => {
                top_n = take()?
                    .parse()
                    .map_err(|e| format!("--top-n must be usize: {e}"))?
            }
            "--help" | "-h" => {
                eprintln!(
                    "usage: nlhe_zero_strategy_audit --checkpoint PATH \
                     --bucket-table PATH --output PATH [--top-n N]"
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
        top_n,
    })
}

/// 该街 bucket 抽象基数（与 `expected_bucket_config` 500/500/500 + preflop 169 对齐）。
fn bucket_count(street: StreetTag) -> u32 {
    match street {
        StreetTag::Preflop => 169,
        StreetTag::Flop | StreetTag::Turn | StreetTag::River => 500,
    }
}

/// v2 layout: bits 38..64 = node_id (26 bit)。`src/training/nlhe.rs` `pack_info_set_v2`。
const NLHE_V2_NODE_ID_SHIFT: u32 = 38;
const NLHE_V2_NODE_ID_BITS: u32 = 26;

fn node_id_from_raw(raw: u64) -> u32 {
    ((raw >> NLHE_V2_NODE_ID_SHIFT) & ((1u64 << NLHE_V2_NODE_ID_BITS) - 1)) as u32
}

/// 计算每个 node 在「双方 uniform 策略 + chance 隐含」下的 reach prob。
/// chance 转街不出现在 PublicBettingTree 边上，因此不进 prob 计算；
/// 这个 prob 只衡量「betting line geometry」上的可达性。
fn compute_node_reach(tree: &PublicBettingTree) -> Vec<f64> {
    let n = tree.num_nodes();
    let mut reach = vec![0.0f64; n];
    reach[tree.root_id() as usize] = 1.0;
    // DFS-order: 父节点 id < 子节点 id (`walk` 先 push self 再递归), 顺序 propagate 即可。
    for id in 0..n as NodeId {
        let p = reach[id as usize];
        if p == 0.0 {
            continue;
        }
        let node = tree.node(id);
        let n_children = node.children.len();
        if n_children == 0 {
            continue;
        }
        let per_child = p / n_children as f64;
        for child in node.children.iter() {
            if let Child::Decision(cid) = child {
                reach[*cid as usize] += per_child;
            }
            // Terminal 不分配 node_id，吸收概率，不再传播。
        }
    }
    reach
}

fn action_label(tag: AbstractActionTag) -> String {
    match tag {
        AbstractActionTag::Fold => "Fold".to_string(),
        AbstractActionTag::Check => "Check".to_string(),
        AbstractActionTag::Call => "Call".to_string(),
        AbstractActionTag::Bet(r) => format!("Bet({})", ratio_label(r)),
        AbstractActionTag::Raise(r) => format!("Raise({})", ratio_label(r)),
        AbstractActionTag::AllIn => "AllIn".to_string(),
    }
}

fn ratio_label(r: poker::BetRatio) -> String {
    // BetRatio 是 ratio over pot 的命名常量；输出原始 raw 数值即可。
    format!("{:?}", r)
}

fn street_name(s: StreetTag) -> &'static str {
    match s {
        StreetTag::Preflop => "preflop",
        StreetTag::Flop => "flop",
        StreetTag::Turn => "turn",
        StreetTag::River => "river",
    }
}

/// log10 bin label。0 → "0"，否则 floor(log10(x))。
fn reach_bin(p: f64) -> i32 {
    if p <= 0.0 {
        i32::MIN
    } else {
        p.log10().floor() as i32
    }
}

fn reach_bin_label(k: i32) -> String {
    if k == i32::MIN {
        "0 (unreachable)".to_string()
    } else {
        format!("[10^{}, 10^{})", k, k + 1)
    }
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

    let probe_game = SimplifiedNlheGame::new(table)
        .map_err(|e| format!("SimplifiedNlheGame::new (probe) failed: {e:?}"))?;
    let tree = probe_game.tree();
    let n_nodes = tree.num_nodes();

    // 每个 node 上实际出现在 strategy_sum 表里的 distinct bucket 数。
    let mut present_per_node: Vec<u32> = vec![0; n_nodes];
    let mut entries_out_of_range: u64 = 0;
    let strategy_sum = trainer.strategy_sum();
    for (info, vec) in strategy_sum.inner() {
        // sanity: ES-MCCFR 下 entry 创建后 σ-累积必非全 0；遇到反例记录但不打断。
        debug_assert!(
            vec.iter().any(|&x| x > 0.0),
            "strategy_sum entry exists but Σ=0 — accumulate 调用栈违反 D-304 (ES-MCCFR)"
        );
        let nid = node_id_from_raw(info.raw()) as usize;
        if nid < n_nodes {
            present_per_node[nid] += 1;
        } else {
            entries_out_of_range += 1;
        }
    }

    let reach = compute_node_reach(tree);

    // 全局
    let mut total_expected: u64 = 0;
    let mut total_present: u64 = 0;
    for (id, pres_raw) in present_per_node.iter().enumerate() {
        let st = tree.node(id as NodeId).street;
        let exp = bucket_count(st) as u64;
        total_expected += exp;
        total_present += (*pres_raw).min(bucket_count(st)) as u64;
    }
    let total_starved = total_expected.saturating_sub(total_present);

    // 按 street
    let mut by_street: BTreeMap<u8, (u32, u64, u64)> = BTreeMap::new(); // street_tag → (n_nodes, expected_infosets, present_infosets)
    for (id, pres_raw) in present_per_node.iter().enumerate() {
        let node = tree.node(id as NodeId);
        let exp = bucket_count(node.street) as u64;
        let pres = (*pres_raw).min(bucket_count(node.street)) as u64;
        let entry = by_street.entry(node.street as u8).or_insert((0, 0, 0));
        entry.0 += 1;
        entry.1 += exp;
        entry.2 += pres;
    }

    // 按 first preflop action（root 自身归入 "(root)"，root 直接子节点按其 action_from_parent
    // 分组；root 自身没有 first action 但只占 1 个 node × 169 = 169 infosets）
    let mut by_first_action: BTreeMap<String, (u32, u64, u64)> = BTreeMap::new();
    let root_id = tree.root_id();
    for (id, pres_raw) in present_per_node.iter().enumerate() {
        let nid = id as NodeId;
        let label = if nid == root_id {
            "(root SB-preflop)".to_string()
        } else {
            let path = tree.path_to_root(nid);
            match path.first() {
                Some(first) => action_label(*first),
                None => "(orphan)".to_string(),
            }
        };
        let node = tree.node(nid);
        let exp = bucket_count(node.street) as u64;
        let pres = (*pres_raw).min(bucket_count(node.street)) as u64;
        let entry = by_first_action.entry(label).or_insert((0, 0, 0));
        entry.0 += 1;
        entry.1 += exp;
        entry.2 += pres;
    }

    // 按 reach bin
    let mut by_reach_bin: BTreeMap<i32, (u32, u64, u64, f64)> = BTreeMap::new(); // bin → (n_nodes, expected, present, sum_reach)
    for (id, pres_raw) in present_per_node.iter().enumerate() {
        let node = tree.node(id as NodeId);
        let exp = bucket_count(node.street) as u64;
        let pres = (*pres_raw).min(bucket_count(node.street)) as u64;
        let bin = reach_bin(reach[id]);
        let entry = by_reach_bin.entry(bin).or_insert((0, 0, 0, 0.0));
        entry.0 += 1;
        entry.1 += exp;
        entry.2 += pres;
        entry.3 += reach[id];
    }

    // Top-N 最 starved（按 starved_count × reach 排序——既要"很多 bucket 缺"
    // 也要"这条线在 uniform 下真的容易到"才上榜）
    let mut nodes_ranked: Vec<(NodeId, u32, f64, f64)> = (0..n_nodes as NodeId)
        .map(|id| {
            let node = tree.node(id);
            let exp = bucket_count(node.street);
            let pres = present_per_node[id as usize].min(exp);
            let starved = exp - pres;
            let r = reach[id as usize];
            let score = starved as f64 * r;
            (id, starved, r, score)
        })
        .filter(|(_, starved, _, _)| *starved > 0)
        .collect();
    nodes_ranked.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal));
    nodes_ranked.truncate(args.top_n);

    // ============== 输出 ==============
    let mut out = BufWriter::new(
        File::create(&args.output)
            .map_err(|e| format!("create {} failed: {e}", args.output.display()))?,
    );

    writeln!(out, "# Simplified NLHE Zero-strategy_sum Infoset Audit").unwrap();
    writeln!(out).unwrap();
    writeln!(out, "- checkpoint: `{}`", args.checkpoint.display()).unwrap();
    writeln!(out, "- bucket_table: `{}`", args.bucket_table.display()).unwrap();
    writeln!(out, "- update_count: `{}`", trainer.update_count()).unwrap();
    writeln!(out, "- tree node count: `{n_nodes}`").unwrap();
    writeln!(
        out,
        "- strategy_sum entries: `{}`",
        trainer.strategy_sum_len()
    )
    .unwrap();
    if entries_out_of_range > 0 {
        writeln!(
            out,
            "- ⚠ strategy_sum entries with node_id ≥ {n_nodes}: `{entries_out_of_range}`"
        )
        .unwrap();
    }
    writeln!(out).unwrap();
    writeln!(
        out,
        "「strategy_sum 全 0」≡「未被任何 traverser 采样访问到」（ES-MCCFR 下 \
         accumulate += σ(I,a)，σ regret-matching 归一化非零，因此 entry 一旦 \
         存在则 Σ>0；反之 entry 不存在 = 全 0 = uniform fallback）。"
    )
    .unwrap();
    writeln!(out).unwrap();

    // 1. 全局
    writeln!(out, "## 1. 全局").unwrap();
    writeln!(out).unwrap();
    let pct = if total_expected > 0 {
        100.0 * total_starved as f64 / total_expected as f64
    } else {
        0.0
    };
    writeln!(out, "| metric | value |").unwrap();
    writeln!(out, "|---|---:|").unwrap();
    writeln!(
        out,
        "| 全部 abstract infoset 数 (Σ_node bucket_count) | {total_expected} |"
    )
    .unwrap();
    writeln!(out, "| 训练表里存在的 infoset 数 | {total_present} |").unwrap();
    writeln!(
        out,
        "| **starved (strategy_sum 全 0) infoset 数** | **{total_starved}** |"
    )
    .unwrap();
    writeln!(out, "| starved 比例 | **{pct:.2}%** |").unwrap();
    writeln!(out).unwrap();

    // 2. 按 street
    writeln!(out, "## 2. 按 street").unwrap();
    writeln!(out).unwrap();
    writeln!(
        out,
        "| street | n_nodes | expected (n_nodes × bucket_count) | present | starved | starved % |"
    )
    .unwrap();
    writeln!(out, "|---|---:|---:|---:|---:|---:|").unwrap();
    for (st_tag, (n, exp, pres)) in &by_street {
        let st = match *st_tag {
            x if x == StreetTag::Preflop as u8 => StreetTag::Preflop,
            x if x == StreetTag::Flop as u8 => StreetTag::Flop,
            x if x == StreetTag::Turn as u8 => StreetTag::Turn,
            x if x == StreetTag::River as u8 => StreetTag::River,
            _ => continue,
        };
        let starved = exp.saturating_sub(*pres);
        let p = if *exp > 0 {
            100.0 * starved as f64 / *exp as f64
        } else {
            0.0
        };
        writeln!(
            out,
            "| {} | {} | {} | {} | {} | {p:.2}% |",
            street_name(st),
            n,
            exp,
            pres,
            starved
        )
        .unwrap();
    }
    writeln!(out).unwrap();

    // 3. 按 first preflop action
    writeln!(out, "## 3. 按 first preflop action (root edge)").unwrap();
    writeln!(out).unwrap();
    writeln!(
        out,
        "first action = root → child 那一步选的 abstract action。\
         root SB-preflop 节点本身归 `(root SB-preflop)`。"
    )
    .unwrap();
    writeln!(out).unwrap();
    writeln!(
        out,
        "| first preflop action | n_nodes | expected | present | starved | starved % |"
    )
    .unwrap();
    writeln!(out, "|---|---:|---:|---:|---:|---:|").unwrap();
    // 按 starved 数倒序输出
    let mut first_action_sorted: Vec<(&String, &(u32, u64, u64))> =
        by_first_action.iter().collect();
    first_action_sorted.sort_by(|a, b| {
        let sa = a.1 .1.saturating_sub(a.1 .2);
        let sb = b.1 .1.saturating_sub(b.1 .2);
        sb.cmp(&sa)
    });
    for (label, (n, exp, pres)) in first_action_sorted {
        let starved = exp.saturating_sub(*pres);
        let p = if *exp > 0 {
            100.0 * starved as f64 / *exp as f64
        } else {
            0.0
        };
        writeln!(
            out,
            "| `{label}` | {n} | {exp} | {pres} | {starved} | {p:.2}% |"
        )
        .unwrap();
    }
    writeln!(out).unwrap();

    // 4. 按 reach bin
    writeln!(out, "## 4. 按 node reach prob bin (uniform 双方策略 proxy)").unwrap();
    writeln!(out).unwrap();
    writeln!(
        out,
        "reach prob = 双方在每个决策点 uniform 随机选 action 时，到达该 node \
         的概率（chance 转街不出现在 PublicBettingTree 边上，因此不进 prob \
         计算 —— 这个 bin 只刻画 betting line geometry 的可达性，不含 \
         hand bucket 先验）。reach ≪ 1 的 node 上 starved 多是正常的（采样 \
         本来就不容易到）；reach ~ 1e-1 / 1e-2 的 bin 仍有大量 starved 才 \
         是问题。"
    )
    .unwrap();
    writeln!(out).unwrap();
    writeln!(
        out,
        "| reach bin | n_nodes | Σ reach | expected | present | starved | starved % |"
    )
    .unwrap();
    writeln!(out, "|---|---:|---:|---:|---:|---:|---:|").unwrap();
    // 倒序：reach 高 → 低
    for (bin, (n, exp, pres, sum_reach)) in by_reach_bin.iter().rev() {
        let starved = exp.saturating_sub(*pres);
        let p = if *exp > 0 {
            100.0 * starved as f64 / *exp as f64
        } else {
            0.0
        };
        writeln!(
            out,
            "| {} | {} | {sum_reach:.3e} | {exp} | {pres} | {starved} | {p:.2}% |",
            reach_bin_label(*bin),
            n
        )
        .unwrap();
    }
    writeln!(out).unwrap();

    // 5. Top-N 最 starved 节点
    writeln!(
        out,
        "## 5. Top {} 最 starved 节点（starved_buckets × node_reach 排序）",
        args.top_n
    )
    .unwrap();
    writeln!(out).unwrap();
    writeln!(
        out,
        "排序 score = starved_buckets × node_reach_prob。\
         同时筛掉了 starved=0 的节点。`betting_line` = root → 该 node 的 \
         abstract action 序列。"
    )
    .unwrap();
    writeln!(out).unwrap();
    writeln!(
        out,
        "| rank | node_id | street | actor | reach | starved / expected | score | betting_line |"
    )
    .unwrap();
    writeln!(out, "|---:|---:|---|---:|---:|---|---:|---|").unwrap();
    for (rank, (nid, starved, r, score)) in nodes_ranked.iter().enumerate() {
        let node = tree.node(*nid);
        let exp = bucket_count(node.street);
        let path = tree.path_to_root(*nid);
        let path_str = if path.is_empty() {
            "(root)".to_string()
        } else {
            path.iter()
                .map(|t| action_label(*t))
                .collect::<Vec<_>>()
                .join(" → ")
        };
        writeln!(
            out,
            "| {} | {} | {} | {} | {r:.3e} | {} / {} | {score:.3e} | {path_str} |",
            rank + 1,
            nid,
            street_name(node.street),
            node.player_acting,
            starved,
            exp
        )
        .unwrap();
    }
    writeln!(out).unwrap();

    out.flush().map_err(|e| format!("flush failed: {e}"))?;
    eprintln!(
        "[nlhe_zero_strategy_audit] wrote {} ({} starved / {} expected, {:.2}%)",
        args.output.display(),
        total_starved,
        total_expected,
        pct
    );
    Ok(())
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("[nlhe_zero_strategy_audit] argument error: {e}");
            return ExitCode::from(2);
        }
    };
    if let Some(parent) = args.output.parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(e) = fs::create_dir_all(parent) {
                eprintln!("[nlhe_zero_strategy_audit] create dir failed: {e}");
                return ExitCode::from(1);
            }
        }
    }
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("[nlhe_zero_strategy_audit] error: {e}");
            ExitCode::from(1)
        }
    }
}
