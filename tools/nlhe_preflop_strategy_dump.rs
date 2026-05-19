//! 简化 NLHE preflop 策略抽样工具（Phase 5 诊断）。
//!
//! 从 checkpoint 加载 ES-MCCFR trainer 后，在 3 个关键 preflop spot 上枚举
//! 代表手牌，dump average_strategy + current_strategy。用来判断 `docs/h3_500m
//! _checkpoint_investigation.md` 报告的"BB after SB limp 拿 AKo 100% AllIn"
//! 这条结构性病态是否在新 InfoSet v2 编码 + 同 update budget 下消失。
//!
//! 用法：
//! ```
//! cargo run --release --bin nlhe_preflop_strategy_dump -- \
//!     --checkpoint <PATH> \
//!     --bucket-table <PATH> \
//!     --output <MD_PATH> \
//!     [--stack-bb 100|200]
//! ```

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use poker::training::nlhe::{NlheStackProfile, SimplifiedNlheGame};
use poker::training::nlhe_betting_tree::{AbstractActionTag, Child, NodeId};
use poker::training::{EsMccfrTrainer, Trainer};
use poker::{BetRatio, BucketTable, Card, InfoSetId, Rank, Suit};

struct Args {
    checkpoint: PathBuf,
    bucket_table: PathBuf,
    output: PathBuf,
    stack_profile: NlheStackProfile,
}

fn parse_args() -> Result<Args, String> {
    let mut checkpoint: Option<PathBuf> = None;
    let mut bucket_table: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut stack_profile = NlheStackProfile::default();
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
            "--stack-bb" => stack_profile = take()?.parse()?,
            "--help" | "-h" => {
                eprintln!(
                    "usage: nlhe_preflop_strategy_dump --checkpoint PATH \
                     --bucket-table PATH --output PATH [--stack-bb 100|200]"
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
        stack_profile,
    })
}

fn run(args: Args) -> Result<(), String> {
    let table = Arc::new(
        BucketTable::open(&args.bucket_table)
            .map_err(|e| format!("BucketTable::open failed: {e:?}"))?,
    );
    let load_game =
        SimplifiedNlheGame::new_with_stack_profile(Arc::clone(&table), args.stack_profile)
            .map_err(|e| format!("SimplifiedNlheGame::new failed: {e:?}"))?;
    let shared_tree = load_game.tree_arc();
    let trainer =
        <EsMccfrTrainer<SimplifiedNlheGame> as Trainer<SimplifiedNlheGame>>::load_checkpoint(
            &args.checkpoint,
            load_game,
        )
        .map_err(|e| format!("load_checkpoint failed: {e:?}"))?;
    let game = SimplifiedNlheGame::new_sharing_tree(table, args.stack_profile, shared_tree)
        .map_err(|e| format!("SimplifiedNlheGame::new (probe) failed: {e:?}"))?;

    let tree = game.tree();
    let root_id = tree.root_id();

    // Spot 2: BB facing SB limp = root → Call edge.
    let bb_post_limp_id = walk(tree, root_id, AbstractActionTag::Call)
        .ok_or("root has no Call edge — abstraction broken?")?;

    // Spot 3: SB facing BB 3bet after own limp = root → Call → Raise(FULL_POT).
    let sb_facing_3bet_id = walk(
        tree,
        bb_post_limp_id,
        AbstractActionTag::Raise(BetRatio::FULL_POT),
    )
    .ok_or("BB after limp has no Raise(FULL_POT) edge")?;

    let spots = [
        ("SB at root (preflop, facing BB blind)", root_id, 0),
        ("BB facing SB limp", bb_post_limp_id, 1),
        ("SB facing BB 3bet after own limp", sb_facing_3bet_id, 0),
    ];

    let hands = sample_hands();

    let mut out = BufWriter::new(
        File::create(&args.output)
            .map_err(|e| format!("create {} failed: {e}", args.output.display()))?,
    );
    writeln!(out, "# Simplified NLHE Preflop Strategy Dump").unwrap();
    writeln!(out).unwrap();
    writeln!(out, "- checkpoint: `{}`", args.checkpoint.display()).unwrap();
    writeln!(out, "- update_count: `{}`", trainer.update_count()).unwrap();
    writeln!(out, "- bucket_table: `{}`", args.bucket_table.display()).unwrap();
    writeln!(out, "- stack_profile: `{}`", args.stack_profile).unwrap();
    writeln!(out).unwrap();

    for (spot_name, node_id, actor) in spots {
        let node = tree.node(node_id);
        writeln!(out, "## {spot_name}").unwrap();
        writeln!(out).unwrap();
        writeln!(out, "- node_id: `{node_id}`").unwrap();
        writeln!(out, "- player_acting: `p{}`", node.player_acting).unwrap();
        let action_labels = node
            .legal_actions
            .iter()
            .map(label_action_tag)
            .collect::<Vec<_>>();
        writeln!(out, "- legal_actions: {}", action_labels.join(" | ")).unwrap();
        writeln!(out).unwrap();
        writeln!(
            out,
            "| hand | {} |",
            action_labels
                .iter()
                .flat_map(|a| [format!("{a} avg"), format!("{a} cur")])
                .collect::<Vec<_>>()
                .join(" | ")
        )
        .unwrap();
        writeln!(out, "|---|{}", "---:|".repeat(action_labels.len() * 2)).unwrap();
        for (hand_label, hole) in &hands {
            let info: InfoSetId = game.preflop_info_set_for_hand(node_id, *hole);
            let avg = trainer.average_strategy(&info);
            let cur = trainer.current_strategy(&info);
            // 应当与 node.legal_actions 长度一致；trainer 返回空 vec 意味着 InfoSet
            // 从未被访问，dump 出 "(unseen)" 提示。
            if avg.len() != action_labels.len() || cur.len() != action_labels.len() {
                let _ = actor; // suppress unused warning
                let unseen_cells = "(unseen) |".repeat(action_labels.len() * 2);
                writeln!(out, "| {hand_label} | {unseen_cells}").unwrap();
                continue;
            }
            let mut row = format!("| {hand_label} |");
            for i in 0..action_labels.len() {
                row.push_str(&format!(" {:.3} | {:.3} |", avg[i], cur[i]));
            }
            writeln!(out, "{row}").unwrap();
        }
        writeln!(out).unwrap();
    }

    out.flush().map_err(|e| format!("flush failed: {e}"))?;
    eprintln!(
        "[nlhe_preflop_strategy_dump] wrote {}",
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
        AbstractActionTag::Fold => "F".to_string(),
        AbstractActionTag::Check => "X".to_string(),
        AbstractActionTag::Call => "C".to_string(),
        AbstractActionTag::Bet(r) => format!("B{}", r.as_milli()),
        AbstractActionTag::Raise(r) => format!("R{}", r.as_milli()),
        AbstractActionTag::AllIn => "A".to_string(),
    }
}

fn sample_hands() -> Vec<(&'static str, [Card; 2])> {
    use Rank::*;
    use Suit::*;
    vec![
        ("AA", [Card::new(Ace, Spades), Card::new(Ace, Hearts)]),
        ("KK", [Card::new(King, Spades), Card::new(King, Hearts)]),
        ("QQ", [Card::new(Queen, Spades), Card::new(Queen, Hearts)]),
        ("JJ", [Card::new(Jack, Spades), Card::new(Jack, Hearts)]),
        ("TT", [Card::new(Ten, Spades), Card::new(Ten, Hearts)]),
        ("88", [Card::new(Eight, Spades), Card::new(Eight, Hearts)]),
        ("AKs", [Card::new(Ace, Spades), Card::new(King, Spades)]),
        ("AKo", [Card::new(Ace, Spades), Card::new(King, Hearts)]),
        ("AQs", [Card::new(Ace, Spades), Card::new(Queen, Spades)]),
        ("AQo", [Card::new(Ace, Spades), Card::new(Queen, Hearts)]),
        ("KQs", [Card::new(King, Spades), Card::new(Queen, Spades)]),
        ("22", [Card::new(Two, Spades), Card::new(Two, Hearts)]),
        ("72o", [Card::new(Seven, Spades), Card::new(Two, Hearts)]),
    ]
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("[nlhe_preflop_strategy_dump] argument error: {e}");
            return ExitCode::from(2);
        }
    };
    if let Some(parent) = args.output.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            eprintln!("[nlhe_preflop_strategy_dump] create dir failed: {e}");
            return ExitCode::from(1);
        }
    }
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("[nlhe_preflop_strategy_dump] error: {e}");
            ExitCode::from(1)
        }
    }
}
