//! 从 dense checkpoint 导出全部 169 个 preflop 起手牌的首次行动 average strategy。
//!
//! 用法：
//! ```
//! cargo run --release --bin nlhe_dense_preflop_169_dump -- \
//!     --checkpoint <PATH> \
//!     --bucket-table <PATH> \
//!     --output <MD_PATH>
//! ```

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use poker::training::nlhe::SimplifiedNlheGame;
use poker::training::nlhe_betting_tree::{AbstractActionTag, Child, NodeId};
use poker::training::nlhe_dense_trainer::DenseNlheEsMccfrTrainer;
use poker::{BetRatio, BucketTable, Card, InfoSetId, PreflopLossless169, Rank, Suit};

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
                    "usage: nlhe_dense_preflop_169_dump --checkpoint PATH \
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
    eprintln!("[nlhe_dense_preflop_169_dump] loading bucket table...");
    let table = Arc::new(
        BucketTable::open(&args.bucket_table)
            .map_err(|e| format!("BucketTable::open failed: {e:?}"))?,
    );
    let game = SimplifiedNlheGame::new(Arc::clone(&table))
        .map_err(|e| format!("SimplifiedNlheGame::new failed: {e:?}"))?;

    eprintln!("[nlhe_dense_preflop_169_dump] loading dense checkpoint...");
    let trainer = DenseNlheEsMccfrTrainer::load_checkpoint(&args.checkpoint, game)
        .map_err(|e| format!("load_checkpoint failed: {e:?}"))?;

    let game = trainer.game();
    let tree = game.tree();
    let root_id = tree.root_id();

    // SB 首次行动 = root node
    // BB facing SB limp = root → Call
    let bb_post_limp_id = walk(tree, root_id, AbstractActionTag::Call)
        .ok_or("root has no Call edge — abstraction broken?")?;

    let spots = [
        ("SB open (首次行动)", root_id, "SB"),
        ("BB vs SB limp", bb_post_limp_id, "BB"),
    ];

    // 构建 169 个 canonical 起手牌的标签和代表牌
    let all_169 = build_all_169_hands();

    let mut out = BufWriter::new(
        File::create(&args.output)
            .map_err(|e| format!("create {} failed: {e}", args.output.display()))?,
    );

    writeln!(out, "# NLHE Dense Preflop 169 Strategy Dump").unwrap();
    writeln!(out).unwrap();
    writeln!(out, "- checkpoint: `{}`", args.checkpoint.display()).unwrap();
    writeln!(out, "- update_count: `{}`", trainer.update_count()).unwrap();
    writeln!(out, "- bucket_table: `{}`", args.bucket_table.display()).unwrap();
    writeln!(out).unwrap();

    for (spot_name, node_id, position) in spots {
        let node = tree.node(node_id);
        writeln!(out, "## {spot_name}").unwrap();
        writeln!(out).unwrap();
        writeln!(out, "- node_id: `{node_id}`").unwrap();
        writeln!(out, "- position: `{position}`").unwrap();
        writeln!(out, "- player_acting: `p{}`", node.player_acting).unwrap();
        let action_labels = node
            .legal_actions
            .iter()
            .map(label_action_tag)
            .collect::<Vec<_>>();
        writeln!(out, "- legal_actions: {}", action_labels.join(" | ")).unwrap();
        writeln!(out).unwrap();

        // 表头
        write!(out, "| hand | class |").unwrap();
        for a in &action_labels {
            write!(out, " {a} |").unwrap();
        }
        writeln!(out).unwrap();
        write!(out, "|---|---:|").unwrap();
        for _ in &action_labels {
            write!(out, "---:|").unwrap();
        }
        writeln!(out).unwrap();

        for (hand_label, hole, class_id) in &all_169 {
            let info: InfoSetId = game.preflop_info_set_for_hand(node_id, *hole);
            let avg = trainer.average_strategy(info);
            if avg.len() != action_labels.len() {
                // unseen
                let unseen_cells = " - |".repeat(action_labels.len());
                writeln!(out, "| {hand_label} | {class_id} |{unseen_cells}").unwrap();
                continue;
            }
            write!(out, "| {hand_label} | {class_id} |").unwrap();
            for v in &avg {
                write!(out, " {:.3} |", v).unwrap();
            }
            writeln!(out).unwrap();
        }
        writeln!(out).unwrap();
    }

    out.flush().map_err(|e| format!("flush failed: {e}"))?;
    eprintln!(
        "[nlhe_dense_preflop_169_dump] wrote {}",
        args.output.display()
    );
    Ok(())
}

fn walk(
    tree: &poker::training::nlhe_betting_tree::PublicBettingTree,
    from: NodeId,
    via: AbstractActionTag,
) -> Option<NodeId> {
    let node = tree.node(from);
    let idx = node.legal_actions.iter().position(|t| *t == via)?;
    match node.children[idx] {
        Child::Decision(id) => Some(id),
        Child::Terminal => None,
    }
}

fn label_action_tag(tag: &AbstractActionTag) -> String {
    match tag {
        AbstractActionTag::Fold => "Fold".to_string(),
        AbstractActionTag::Check => "Check".to_string(),
        AbstractActionTag::Call => "Call/Limp".to_string(),
        AbstractActionTag::Bet(r) => format!("Bet({})", ratio_label(*r)),
        AbstractActionTag::Raise(r) => format!("Raise({})", ratio_label(*r)),
        AbstractActionTag::AllIn => "AllIn".to_string(),
    }
}

fn ratio_label(r: BetRatio) -> String {
    match r.as_milli() {
        500 => "0.5x".to_string(),
        1000 => "1x".to_string(),
        2000 => "2x".to_string(),
        other => format!("{}‰", other),
    }
}

/// 构建 169 个 canonical 起手牌，按"常规扑克手牌表格"顺序排列：
/// 行列按 A, K, Q, J, T, 9, 8, 7, 6, 5, 4, 3, 2 排，
/// 上三角 = suited，下三角 = offsuit，对角线 = pair。
/// 同时返回 hand_class id 便于排序/调试。
fn build_all_169_hands() -> Vec<(&'static str, [Card; 2], u8)> {
    use Rank::*;
    use Suit::*;

    // 13 ranks from A down to 2
    let ranks = [Ace, King, Queen, Jack, Ten, Nine, Eight, Seven, Six, Five, Four, Three, Two];
    let rank_chars = ['A', 'K', 'Q', 'J', 'T', '9', '8', '7', '6', '5', '4', '3', '2'];

    let preflop = PreflopLossless169::new();
    let mut result = Vec::with_capacity(169);

    // 生成顺序：先 pair (AA..22), 再 suited (AKs..32s), 再 offsuit (AKo..32o)
    // 但用户更习惯按"13x13 方格表"看——我们这里按行（高牌）-> 列（低牌）顺序排列

    // 1) Pairs: AA, KK, ..., 22
    for (i, &r) in ranks.iter().enumerate() {
        let hole = [Card::new(r, Spades), Card::new(r, Hearts)];
        let class = preflop.hand_class(hole);
        let label = format!("{}{}", rank_chars[i], rank_chars[i]);
        result.push((leak_string(label), hole, class));
    }

    // 2) Suited: AKs, AQs, ..., ATs, ..., 32s
    for (i, &hi) in ranks.iter().enumerate() {
        for (j, &lo) in ranks.iter().enumerate() {
            if j <= i {
                continue; // only hi > lo
            }
            let hole = [Card::new(hi, Spades), Card::new(lo, Spades)];
            let class = preflop.hand_class(hole);
            let label = format!("{}{}s", rank_chars[i], rank_chars[j]);
            result.push((leak_string(label), hole, class));
        }
    }

    // 3) Offsuit: AKo, AQo, ..., 32o
    for (i, &hi) in ranks.iter().enumerate() {
        for (j, &lo) in ranks.iter().enumerate() {
            if j <= i {
                continue; // only hi > lo
            }
            let hole = [Card::new(hi, Spades), Card::new(lo, Hearts)];
            let class = preflop.hand_class(hole);
            let label = format!("{}{}o", rank_chars[i], rank_chars[j]);
            result.push((leak_string(label), hole, class));
        }
    }

    assert_eq!(result.len(), 169);
    result
}

fn leak_string(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("[nlhe_dense_preflop_169_dump] argument error: {e}");
            return ExitCode::from(2);
        }
    };
    if let Some(parent) = args.output.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            eprintln!("[nlhe_dense_preflop_169_dump] create dir failed: {e}");
            return ExitCode::from(1);
        }
    }
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("[nlhe_dense_preflop_169_dump] error: {e}");
            ExitCode::from(1)
        }
    }
}
