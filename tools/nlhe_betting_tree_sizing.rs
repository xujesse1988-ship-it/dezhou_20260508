//! 简化 NLHE 抽象 betting tree 决策节点数 sizing 工具
//! （`docs/nlhe_infoset_history_investigation.md` 方案 A Phase 0b）。
//!
//! 从 `SimplifiedNlheGame::root` DFS 枚举所有 reachable 抽象动作序列，
//! 统计：决策节点总数、按街分布、按 (街, player_acting) 分布、深度直方图、
//! 节点数对应 node_id 位宽。本工具仅 sizing 用，不构造正式 `PublicBettingTree`
//! 数据结构（Phase 1 落地时再写真树）。

use std::collections::BTreeMap;
use std::process::ExitCode;
use std::sync::Arc;

use poker::training::game::{Game, NodeKind};
use poker::training::nlhe::{NlheStackProfile, SimplifiedNlheGame, SimplifiedNlheState};
use poker::{BucketConfig, BucketTable, ChaCha20Rng};

const SEED: u64 = 0x4E4C_4845_5F53_5A4E; // "NLHE_SZN"

#[derive(Default)]
struct Stats {
    /// 决策节点总数（Terminal 不计）。
    decision_nodes: u64,
    /// Terminal 节点总数（fold/showdown）。
    terminal_nodes: u64,
    /// 按街分布。
    per_street: BTreeMap<u8, u64>,
    /// 按 (街, player_acting) 分布。
    per_street_player: BTreeMap<(u8, u8), u64>,
    /// 深度直方图（depth = root 到当前节点经过的边数）。
    depth_histogram: BTreeMap<u32, u64>,
    /// 最大深度。
    max_depth: u32,
}

fn walk(state: &SimplifiedNlheState, depth: u32, stats: &mut Stats) {
    match SimplifiedNlheGame::current(state) {
        NodeKind::Terminal => {
            stats.terminal_nodes += 1;
            return;
        }
        NodeKind::Chance => {
            // 简化 NLHE 没有独立 chance node（D-308 / D-315），见 nlhe.rs 模块注释。
            // 真触发说明 Game::current 实现走漏了，立即停下让调用方看到。
            panic!("simplified NLHE should not surface Chance node; got one at depth {depth}");
        }
        NodeKind::Player(_actor) => {}
    }

    stats.decision_nodes += 1;
    let street = state.game_state.street();
    let actor = state
        .game_state
        .current_player()
        .expect("Player node must have current_player")
        .0;
    *stats.per_street.entry(street as u8).or_default() += 1;
    *stats
        .per_street_player
        .entry((street as u8, actor))
        .or_default() += 1;
    *stats.depth_histogram.entry(depth).or_default() += 1;
    if depth > stats.max_depth {
        stats.max_depth = depth;
    }

    let legal = SimplifiedNlheGame::legal_actions(state);
    for action in legal {
        // next 不消费 rng（见 nlhe.rs:next 注释）；递归用 dummy。
        let mut dummy_rng = ChaCha20Rng::from_seed(SEED);
        let child = SimplifiedNlheGame::next(state.clone(), action, &mut dummy_rng);
        walk(&child, depth + 1, stats);
    }
}

fn parse_stack_profile() -> Result<NlheStackProfile, String> {
    let mut profile = NlheStackProfile::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--stack-bb" => {
                let raw = args
                    .next()
                    .ok_or_else(|| "--stack-bb requires a value".to_string())?;
                profile = raw.parse()?;
            }
            "--help" | "-h" => {
                eprintln!(
                    "usage: cargo run --release --bin nlhe_betting_tree_sizing -- \
                     [--stack-bb 100|200]"
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(profile)
}

fn run() -> Result<(), String> {
    let stack_profile = parse_stack_profile()?;
    let table = Arc::new(BucketTable::stub_for_postflop(
        BucketConfig::default_500_500_500(),
    ));
    let game = SimplifiedNlheGame::new_with_stack_profile(table, stack_profile)
        .map_err(|e| format!("SimplifiedNlheGame::new_with_stack_profile failed: {e:?}"))?;
    let mut rng = ChaCha20Rng::from_seed(SEED);
    let root = game.root(&mut rng);

    let mut stats = Stats::default();
    let start = std::time::Instant::now();
    walk(&root, 0, &mut stats);
    let elapsed = start.elapsed();

    println!("=== Simplified NLHE Abstract Betting Tree Sizing ===");
    println!("RNG seed              : 0x{SEED:016x}");
    println!("stack profile         : {stack_profile}");
    println!("walk wall time        : {:.3}s", elapsed.as_secs_f64());
    println!();
    println!("Decision node total   : {}", stats.decision_nodes);
    println!("Terminal node total   : {}", stats.terminal_nodes);
    println!("Max depth             : {}", stats.max_depth);
    println!();

    let n = stats.decision_nodes;
    let bits_needed = if n == 0 {
        0
    } else {
        64 - (n - 1).leading_zeros()
    };
    println!(
        "node_id bit width     : {} bit  (covers up to {} ids)",
        bits_needed,
        1u64 << bits_needed
    );
    println!();

    let street_label = |s: u8| -> &'static str {
        match s {
            0 => "Preflop",
            1 => "Flop",
            2 => "Turn",
            3 => "River",
            _ => "Unknown",
        }
    };

    println!("Per-street decision nodes:");
    for (street, count) in &stats.per_street {
        println!("  {:<10} : {}", street_label(*street), count);
    }
    println!();

    println!("Per (street, player_acting) decision nodes:");
    for ((street, actor), count) in &stats.per_street_player {
        println!("  {:<10} p{} : {}", street_label(*street), actor, count);
    }
    println!();

    println!("Depth histogram:");
    for (depth, count) in &stats.depth_histogram {
        println!("  d={:<3} : {}", depth, count);
    }

    println!();
    let viable = bits_needed <= 26;
    println!(
        "NodeId 26-bit gate     : {}",
        if viable { "PASS" } else { "FAIL" }
    );

    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("[nlhe_betting_tree_sizing] error: {e}");
            ExitCode::from(1)
        }
    }
}
