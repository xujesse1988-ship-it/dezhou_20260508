//! 简化 NLHE 抽象 betting tree 决策节点数 sizing 工具。
//!
//! 从 `GameState` root 出发 DFS 枚举所有 reachable 抽象动作序列，针对一组候选
//! `raise_pot_ratios` 配置打印决策节点数、infoset 数、按街分布、深度直方图、
//! `node_id` 位宽。与 `PublicBettingTree::build` 走同一抽象 + 同一 root 路径单射
//! 性质，节点计数与 tree 实际构造一致。
//!
//! 支持 per-street raise 集合（street-dependent action abstraction 的 sizing 探针）：
//! 每条街用各自的 `DefaultActionAbstraction`，按 `state.street()` 分派。这只在本
//! 工具内成立，**不改 production `nlhe_betting_tree.rs` 的全局 `default_6_action`
//! 路径**——纯粹是"如果 preflop/flop 加细 size、turn/river 不动会多大"的离线估算。

use std::collections::BTreeMap;
use std::process::ExitCode;

use poker::{
    ActionAbstraction, ActionAbstractionConfig, ChaCha20Rng, DefaultActionAbstraction, GameState,
    RngSource, TableConfig,
};

const WALK_SEED: u64 = 0x4E4C_4845_5F53_5A4E; // "NLHE_SZN"

// preflop=169 lossless hand class；postflop K=500（v3 cafebabe profile）。
const PREFLOP_BUCKETS: u64 = 169;
const POSTFLOP_BUCKETS: u64 = 500;

#[derive(Default)]
struct Stats {
    decision_nodes: u64,
    terminal_nodes: u64,
    per_street: BTreeMap<u8, u64>,
    per_street_player: BTreeMap<(u8, u8), u64>,
    depth_histogram: BTreeMap<u32, u64>,
    max_depth: u32,
}

impl Stats {
    /// infoset 数 = Σ node_count(street) × bucket_count(street)。
    fn infosets(&self) -> u64 {
        self.per_street
            .iter()
            .map(|(street, count)| {
                let buckets = if *street == 0 {
                    PREFLOP_BUCKETS
                } else {
                    POSTFLOP_BUCKETS
                };
                count * buckets
            })
            .sum()
    }
}

fn walk(
    state: &GameState,
    depth: u32,
    stats: &mut Stats,
    abs_by_street: &[DefaultActionAbstraction; 4],
) {
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

    let abs = &abs_by_street[street as usize];
    let legal_set = abs.abstract_actions(state);
    for action in legal_set.iter().copied() {
        let mut next_state = state.clone();
        next_state
            .apply(action.to_concrete())
            .expect("DefaultActionAbstraction must emit legal actions");
        walk(&next_state, depth + 1, stats, abs_by_street);
    }
}

fn make_abs(raise_ratios: &[f64]) -> DefaultActionAbstraction {
    let cfg = ActionAbstractionConfig::new(raise_ratios.to_vec())
        .expect("raise ratios must satisfy ActionAbstractionConfig::new");
    DefaultActionAbstraction::new(cfg)
}

/// `per_street` = [preflop, flop, turn, river] 各自的 raise ratio 集合。
fn measure(per_street: [&[f64]; 4]) -> Stats {
    let abs_by_street = [
        make_abs(per_street[0]),
        make_abs(per_street[1]),
        make_abs(per_street[2]),
        make_abs(per_street[3]),
    ];
    let table_cfg = TableConfig::default_hu_200bb();
    let mut rng = ChaCha20Rng::from_seed(WALK_SEED);
    let state = GameState::with_rng(&table_cfg, 0, &mut rng as &mut dyn RngSource);

    let mut stats = Stats::default();
    walk(&state, 0, &mut stats, &abs_by_street);
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

/// 把 per-street ratio 集合压成一行展示；全街相同则只印一组。
fn ratios_desc(per_street: [&[f64]; 4]) -> String {
    let all_same = per_street.iter().all(|r| r == &per_street[0]);
    if all_same {
        format!("{:?} (all streets)", per_street[0])
    } else {
        format!(
            "pre={:?} flop={:?} turn={:?} river={:?}",
            per_street[0], per_street[1], per_street[2], per_street[3]
        )
    }
}

fn print_stats(label: &str, desc: &str, stats: &Stats, baseline: Option<u64>) {
    let n = stats.decision_nodes;
    let bits = bits_for(n);
    let infosets = stats.infosets();

    println!("--- {label} : raise_pot_ratios = {desc} ---");
    let baseline_marker = match baseline {
        Some(b) if b > 0 => format!("  ({:.2}× baseline)", n as f64 / b as f64),
        _ => String::new(),
    };
    println!(
        "Decision nodes  : {n}{baseline_marker}    [node_id {bits} bit → cover {}]",
        1u64 << bits
    );
    let infoset_marker = match baseline {
        Some(_) => format!("  ({:.1}M)", infosets as f64 / 1e6),
        None => String::new(),
    };
    println!("Infosets        : {infosets}{infoset_marker}");
    println!(
        "Terminal nodes  : {}    Max depth : {}",
        stats.terminal_nodes, stats.max_depth
    );

    print!("Per-street      :");
    for (street, count) in &stats.per_street {
        print!(" {}={}", street_label(*street), count);
    }
    println!();
    println!();
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    // raise 集合常量，方便复用。
    const R3: &[f64] = &[0.5, 1.0, 2.0]; // 现在的 6-action {0.5p,1p,2p}
    const R4: &[f64] = &[0.5, 0.75, 1.0, 2.0]; // 均匀 +0.75p
    const PF5: &[f64] = &[0.33, 0.5, 0.75, 1.0, 2.0]; // 早街加细：+0.33p +0.75p
    const PF4: &[f64] = &[0.5, 0.75, 1.0, 2.0]; // 早街只 +0.75p

    // [preflop, flop, turn, river]
    let configs: &[(&str, [&[f64]; 4])] = &[
        ("baseline 全街 {0.5,1,2}", [R3, R3, R3, R3]),
        ("均匀 +0.75p 全街", [R4, R4, R4, R4]),
        (
            "按街: pre/flop +{0.33,0.75} / turn,river 不动",
            [PF5, PF5, R3, R3],
        ),
        (
            "按街: pre/flop +0.75 only / turn,river 不动",
            [PF4, PF4, R3, R3],
        ),
        ("按街: flop-only +{0.33,0.75} / 其它不动", [R3, PF5, R3, R3]),
    ];

    println!("=== Simplified NLHE Abstract Betting Tree Sizing (HU 200BB default) ===");
    println!("RNG seed = 0x{WALK_SEED:016x}");
    println!("(infoset = preflop_nodes×169 + postflop_nodes×500)");
    println!();

    let mut baseline: Option<u64> = None;
    for (label, per_street) in configs {
        let start = std::time::Instant::now();
        let stats = measure(*per_street);
        let elapsed = start.elapsed();
        print_stats(label, &ratios_desc(*per_street), &stats, baseline);
        println!("walk wall time  : {:.3}s", elapsed.as_secs_f64());
        println!();
        if baseline.is_none() {
            baseline = Some(stats.decision_nodes);
        }
    }

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
