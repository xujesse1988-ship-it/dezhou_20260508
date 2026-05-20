//! 简化 NLHE 抽象 betting tree 决策节点数 sizing 工具。
//!
//! 从 `GameState` root 出发 DFS 枚举所有 reachable 抽象动作序列，针对一组候选
//! `raise_pot_ratios` 配置打印决策节点数、按街分布、深度直方图、`node_id` 位宽。
//! 与 `PublicBettingTree::build` 走同一抽象 + 同一 root 路径单射性质，节点计数
//! 与 tree 实际构造一致。

use std::collections::BTreeMap;
use std::process::ExitCode;

use poker::{
    ActionAbstraction, ActionAbstractionConfig, ChaCha20Rng, DefaultActionAbstraction, GameState,
    RngSource, TableConfig,
};

const WALK_SEED: u64 = 0x4E4C_4845_5F53_5A4E; // "NLHE_SZN"

#[derive(Default)]
struct Stats {
    decision_nodes: u64,
    terminal_nodes: u64,
    per_street: BTreeMap<u8, u64>,
    per_street_player: BTreeMap<(u8, u8), u64>,
    depth_histogram: BTreeMap<u32, u64>,
    max_depth: u32,
}

fn walk(state: &GameState, depth: u32, stats: &mut Stats, abs: &DefaultActionAbstraction) {
    if state.is_terminal() {
        stats.terminal_nodes += 1;
        return;
    }

    stats.decision_nodes += 1;
    let street = state.street() as u8;
    let actor = state
        .current_player()
        .expect("non-terminal state must have current_player")
        .0;
    *stats.per_street.entry(street).or_default() += 1;
    *stats.per_street_player.entry((street, actor)).or_default() += 1;
    *stats.depth_histogram.entry(depth).or_default() += 1;
    if depth > stats.max_depth {
        stats.max_depth = depth;
    }

    let legal_set = abs.abstract_actions(state);
    for action in legal_set.iter().copied() {
        let mut next_state = state.clone();
        next_state
            .apply(action.to_concrete())
            .expect("DefaultActionAbstraction must emit legal actions");
        walk(&next_state, depth + 1, stats, abs);
    }
}

fn measure(raise_ratios: &[f64]) -> Stats {
    let cfg = ActionAbstractionConfig::new(raise_ratios.to_vec())
        .expect("raise ratios must satisfy ActionAbstractionConfig::new");
    let abs = DefaultActionAbstraction::new(cfg);
    let table_cfg = TableConfig::default_hu_200bb();
    let mut rng = ChaCha20Rng::from_seed(WALK_SEED);
    let state = GameState::with_rng(&table_cfg, 0, &mut rng as &mut dyn RngSource);

    let mut stats = Stats::default();
    walk(&state, 0, &mut stats, &abs);
    stats
}

fn bits_for(n: u64) -> u32 {
    if n == 0 {
        0
    } else {
        64 - (n - 1).leading_zeros()
    }
}

fn street_label(s: u8) -> &'static str {
    match s {
        0 => "Preflop",
        1 => "Flop",
        2 => "Turn",
        3 => "River",
        _ => "Unknown",
    }
}

fn print_stats(label: &str, ratios: &[f64], stats: &Stats, baseline: Option<u64>) {
    let n = stats.decision_nodes;
    let bits = bits_for(n);

    println!("--- {label} : raise_pot_ratios = {ratios:?} ---");
    let baseline_marker = match baseline {
        Some(b) if b > 0 => format!("  ({:.2}× baseline)", n as f64 / b as f64),
        _ => String::new(),
    };
    println!(
        "Decision nodes  : {n}{baseline_marker}    [node_id {bits} bit → cover {}]",
        1u64 << bits
    );
    println!(
        "Terminal nodes  : {}    Max depth : {}",
        stats.terminal_nodes, stats.max_depth
    );

    print!("Per-street      :");
    for (street, count) in &stats.per_street {
        print!(" {}={}", street_label(*street), count);
    }
    println!();

    print!("Per (street, p) :");
    for ((street, actor), count) in &stats.per_street_player {
        print!(" {}/p{}={}", street_label(*street), actor, count);
    }
    println!();
    println!();
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let configs: &[(&str, &[f64])] = &[
        ("baseline (current default)", &[0.5, 1.0]),
        ("+ 0.75p", &[0.5, 0.75, 1.0]),
        ("+ 2p", &[0.5, 1.0, 2.0]),
        ("+ 0.75p + 2p (Slumbot-like)", &[0.5, 0.75, 1.0, 2.0]),
    ];

    println!("=== Simplified NLHE Abstract Betting Tree Sizing (HU 200BB default) ===");
    println!("RNG seed = 0x{WALK_SEED:016x}");
    println!();

    let mut baseline: Option<u64> = None;
    for (label, ratios) in configs {
        let start = std::time::Instant::now();
        let stats = measure(ratios);
        let elapsed = start.elapsed();
        print_stats(label, ratios, &stats, baseline);
        println!("walk wall time  : {:.3}s", elapsed.as_secs_f64());
        println!();
        if baseline.is_none() {
            baseline = Some(stats.decision_nodes);
        }
    }

    println!("Phase 0 gate (< 10^6 decision nodes per configuration above)");
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
