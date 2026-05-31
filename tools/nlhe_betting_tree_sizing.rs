//! 简化 NLHE 抽象 betting tree 决策节点数 sizing 工具。
//!
//! 从 `GameState` root 出发 DFS 枚举所有 reachable 抽象动作序列，针对一组候选
//! `raise_pot_ratios` + 牌桌 profile（座位数 / 起始码量）配置打印决策节点数、
//! infoset 数、按街分布、深度直方图、`node_id` 位宽。与 `PublicBettingTree::build`
//! 走同一抽象 + 同一 root 路径单射性质，节点计数与 tree 实际构造一致。
//!
//! Phase 0（dense infoset table）：另打印 full-prealloc dense 布局 sizing——
//! `total_rows`（Σ bucket_count，应 == infoset 数）、`total_slots`（Σ bucket_count ×
//! action_count，variable-action 布局的 f64 数）、per-street rows/slots、action_count
//! 直方图，以及 regret+strategy 两表在 variable / 固定 stride 6 / stride 8 三种布局下的
//! 内存估算 + visited bitset 体量。用来确认目标 profile 的 variable 两表能否落进
//! 32–64 GB 目标机器。
//!
//! 6-max（S2）：`walk` 本身不假设玩家数，只走 `current_player` / `street` /
//! `abstract_actions` / `apply`，所以换 `default_6max_100bb()` 即枚举 6-max 树。
//! 6-max 树可能远大于 HU（玩家数 2→6 让 preflop 动作序列爆炸），故加 `NODE_CAP`：
//! 决策节点数到上限即停止下探并标记 capped，把"是否大到无法枚举"本身当作 sizing
//! 结论返回，而不是跑到 OOM / 不收敛。
//!
//! 支持 per-street raise 集合（street-dependent action abstraction 的 sizing 探针）：
//! 每条街用各自的 `DefaultActionAbstraction`，按 `state.street()` 分派。

use std::collections::BTreeMap;
use std::process::ExitCode;

use poker::{
    AbstractAction, ActionAbstraction, ActionAbstractionConfig, ChaCha20Rng,
    DefaultActionAbstraction, GameState, RngSource, TableConfig,
};

const WALK_SEED: u64 = 0x4E4C_4845_5F53_5A4E; // "NLHE_SZN"

/// 决策节点枚举上限。到上限即停止下探（标记 capped）。6-max 树可能 ≫ 这个数，
/// 那本身就是结论：该抽象在全宽枚举 / 单机 dense 表下不可行。
const NODE_CAP: u64 = 100_000_000;

/// 每条街的 bucket 数（preflop = lossless hand class，postflop = K-means 桶）。
#[derive(Clone, Copy)]
struct BucketCounts {
    preflop: u64,
    postflop: u64,
}

impl BucketCounts {
    fn for_street(&self, street: u8) -> u64 {
        if street == 0 {
            self.preflop
        } else {
            self.postflop
        }
    }
}

#[derive(Default)]
struct Stats {
    decision_nodes: u64,
    terminal_nodes: u64,
    per_street: BTreeMap<u8, u64>,
    per_street_player: BTreeMap<(u8, u8), u64>,
    depth_histogram: BTreeMap<u32, u64>,
    max_depth: u32,
    /// 枚举是否因 `NODE_CAP` 被截断（true → 下面所有计数是 lower bound）。
    capped: bool,
    // ---- dense full-prealloc 布局 sizing（Phase 0）----
    /// Σ bucket_count(node)；dense 表的 row 数，应当 == `infosets()`。
    total_rows: u64,
    /// Σ bucket_count(node) × action_count(node)；variable-action 布局的 f64 slot 数。
    total_slots: u64,
    per_street_rows: BTreeMap<u8, u64>,
    per_street_slots: BTreeMap<u8, u64>,
    /// action_count（= legal_actions.len()）→ 节点数直方图。
    action_count_hist: BTreeMap<usize, u64>,
}

impl Stats {
    /// infoset 数 = Σ node_count(street) × bucket_count(street)。
    fn infosets(&self, buckets: &BucketCounts) -> u64 {
        self.per_street
            .iter()
            .map(|(street, count)| count * buckets.for_street(*street))
            .sum()
    }
}

/// A1 raise cap（每街 (Bet+Raise) 聚合上限）：`raises_on_street` 是到达本节点前
/// 本街已发生的 voluntary 进攻动作（`Bet` + `Raise`，对齐 `BettingState` 的
/// `FacingBetNoRaise`/`FacingRaise{1,2,3+}` 计数）次数。到达 `raise_cap` 后，
/// 本节点合法集里只留 `Fold/Check/Call/AllIn`——`AllIn` 始终保留（escape hatch，
/// 不计入 cap），砍掉的只是 sized `Bet`/`Raise` 这条组合爆炸链。`raise_cap = u32::MAX`
/// 等价无 cap（与历史行为逐字节一致，见 run() 的 huge-cap self-check）。
fn walk(
    state: &GameState,
    depth: u32,
    raises_on_street: u32,
    raise_cap: u32,
    stats: &mut Stats,
    abs_by_street: &[DefaultActionAbstraction; 4],
    buckets: &BucketCounts,
) {
    if state.is_terminal() {
        stats.terminal_nodes += 1;
        return;
    }

    // NODE_CAP：到上限停止下探，把 capped 当结论返回。
    if stats.decision_nodes >= NODE_CAP {
        stats.capped = true;
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

    // A1 raise cap：本街进攻数到顶后剔除 sized Bet/Raise，仅留 Fold/Check/Call/AllIn。
    let cap_reached = raises_on_street >= raise_cap;
    let is_aggressive =
        |a: &AbstractAction| matches!(a, AbstractAction::Bet { .. } | AbstractAction::Raise { .. });
    let actions: Vec<AbstractAction> = legal_set
        .iter()
        .copied()
        .filter(|a| !(cap_reached && is_aggressive(a)))
        .collect();

    // dense 布局累加：本节点贡献 bucket_count 行、bucket_count × action_count 个 slot。
    let action_count = actions.len() as u64;
    let rows = buckets.for_street(street);
    stats.total_rows += rows;
    stats.total_slots += rows * action_count;
    *stats.per_street_rows.entry(street).or_default() += rows;
    *stats.per_street_slots.entry(street).or_default() += rows * action_count;
    *stats.action_count_hist.entry(actions.len()).or_default() += 1;

    for action in actions {
        let mut next_state = state.clone();
        next_state
            .apply(action.to_concrete())
            .expect("DefaultActionAbstraction must emit legal actions");
        // 街切换则进攻计数清零；否则 Bet/Raise +1，其它（Call/Check/AllIn）不变。
        let next_street = next_state.street() as u8;
        let next_raises = if next_street != street {
            0
        } else if is_aggressive(&action) {
            raises_on_street + 1
        } else {
            raises_on_street
        };
        walk(
            &next_state,
            depth + 1,
            next_raises,
            raise_cap,
            stats,
            abs_by_street,
            buckets,
        );
        if stats.capped {
            return;
        }
    }
}

fn make_abs(raise_ratios: &[f64]) -> DefaultActionAbstraction {
    let cfg = ActionAbstractionConfig::new(raise_ratios.to_vec())
        .expect("raise ratios must satisfy ActionAbstractionConfig::new");
    DefaultActionAbstraction::new(cfg)
}

/// `per_street` = [preflop, flop, turn, river] 各自的 raise ratio 集合。
/// `raise_cap` = 每街 (Bet+Raise) 聚合上限（`u32::MAX` = 无 cap）。
fn measure(
    table_cfg: &TableConfig,
    per_street: [&[f64]; 4],
    buckets: &BucketCounts,
    raise_cap: u32,
) -> Stats {
    let abs_by_street = [
        make_abs(per_street[0]),
        make_abs(per_street[1]),
        make_abs(per_street[2]),
        make_abs(per_street[3]),
    ];
    let mut rng = ChaCha20Rng::from_seed(WALK_SEED);
    let state = GameState::with_rng(table_cfg, 0, &mut rng as &mut dyn RngSource);

    let mut stats = Stats::default();
    walk(&state, 0, 0, raise_cap, &mut stats, &abs_by_street, buckets);
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

fn print_stats(label: &str, desc: &str, stats: &Stats, buckets: &BucketCounts) {
    let n = stats.decision_nodes;
    let bits = bits_for(n);
    let infosets = stats.infosets(buckets);

    println!("--- {label} : raise_pot_ratios = {desc} ---");
    println!(
        "Buckets         : preflop={} postflop={}",
        buckets.preflop, buckets.postflop
    );
    if stats.capped {
        println!("⚠ CAPPED        : 枚举到 NODE_CAP={NODE_CAP} 被截断 → 下面计数是 LOWER BOUND，真实树更大");
    }
    println!(
        "Decision nodes  : {n}    [node_id {bits} bit → cover {}]",
        1u64 << bits
    );
    println!(
        "Infosets        : {infosets}  ({:.1}M)",
        infosets as f64 / 1e6
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

    print_dense_layout(stats, infosets);
    println!();
}

const GIB: f64 = (1u64 << 30) as f64;
const MIB: f64 = (1u64 << 20) as f64;

/// dense full-prealloc 布局 sizing + 内存估算（Phase 0 决策门：variable 两表是否
/// 落得进目标机器）。
fn print_dense_layout(stats: &Stats, infosets: u64) {
    let rows = stats.total_rows;
    let slots = stats.total_slots;
    let avg_ac = slots as f64 / rows as f64;

    // 自洽校验：dense row 数必须等于 infoset 数（每 (node,bucket) 一行）。
    assert_eq!(
        rows, infosets,
        "total_rows {rows} != infosets {infosets}（dense row 与 infoset 应一一对应）"
    );

    println!("Dense rows      : {rows}  (== infosets ✓)");
    println!("Dense slots     : {slots}  (variable-action, avg action_count {avg_ac:.3})");

    print!("Per-street rows :");
    for (s, r) in &stats.per_street_rows {
        print!(" {}={}", street_label(*s), r);
    }
    println!();
    print!("Per-street slots:");
    for (s, sl) in &stats.per_street_slots {
        print!(" {}={}", street_label(*s), sl);
    }
    println!();
    print!("action_count    :");
    for (ac, nodes) in &stats.action_count_hist {
        print!(" {ac}→{nodes}");
    }
    println!();

    // 两张 f64 表（regret + strategy）。variable = 真实布局；stride 6/8 = 固定 stride 对照。
    let var_one = slots * 8;
    let var_two = var_one * 2;
    let stride6_two = rows * 6 * 8 * 2;
    let stride8_two = rows * 8 * 8 * 2;
    let bitset_two = rows.div_ceil(8) * 2;
    println!(
        "Mem variable    : one table {:.2} GiB / regret+strategy {:.2} GiB",
        var_one as f64 / GIB,
        var_two as f64 / GIB
    );
    println!(
        "Mem stride=6    : regret+strategy {:.2} GiB   stride=8 : {:.2} GiB",
        stride6_two as f64 / GIB,
        stride8_two as f64 / GIB
    );
    println!(
        "Visited bitset  : {:.1} MiB (两表合计)",
        bitset_two as f64 / MIB
    );
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    const R3: &[f64] = &[0.5, 1.0, 2.0]; // HU 现 6-action {0.5p,1p,2p}（self-check 用）

    // 6-max raise 集合从 argv 读（f64 列表，全街同集）；无参默认 {1.0}。
    // postflop 桶数从 env XV_POSTFLOP 读（默认 200）；preflop 固定 169 lossless。
    // 例：cargo run --release --bin nlhe_betting_tree_sizing -- 0.5 1.0
    //     XV_POSTFLOP=500 cargo run ... -- 1.0
    let argv: Vec<f64> = std::env::args()
        .skip(1)
        .map(|a| {
            a.parse::<f64>()
                .map_err(|e| format!("argv raise ratio '{a}' 不是 f64: {e}"))
        })
        .collect::<Result<_, _>>()?;
    let six_ratios: Vec<f64> = if argv.is_empty() { vec![1.0] } else { argv };
    let postflop_buckets: u64 = std::env::var("XV_POSTFLOP")
        .ok()
        .map(|s| s.parse::<u64>())
        .transpose()
        .map_err(|e| format!("XV_POSTFLOP 不是 u64: {e}"))?
        .unwrap_or(200);
    // A1 raise cap：每街 (Bet+Raise) 聚合上限。env 不设 = 无 cap（u32::MAX）。
    // 例：RAISE_CAP=2 cargo run ... -- 0.5 1.0  →  含 0.5pot 小注但每街最多 2 次进攻。
    let raise_cap: u32 = std::env::var("RAISE_CAP")
        .ok()
        .map(|s| s.parse::<u32>())
        .transpose()
        .map_err(|e| format!("RAISE_CAP 不是 u32: {e}"))?
        .unwrap_or(u32::MAX);

    println!("=== Simplified NLHE Abstract Betting Tree Sizing ===");
    let cap_desc = if raise_cap == u32::MAX {
        "none".to_string()
    } else {
        raise_cap.to_string()
    };
    println!("RNG seed = 0x{WALK_SEED:016x}   NODE_CAP = {NODE_CAP}   RAISE_CAP = {cap_desc}");
    println!();

    // (1) HU self-check：复现 240,096 节点 / 119.7M infoset（验证 refactor 没改计数）。
    {
        let hu = BucketCounts {
            preflop: 169,
            postflop: 500,
        };
        let cfg = TableConfig::default_hu_200bb();
        let start = std::time::Instant::now();
        // HU self-check 永远不加 cap，守住 240,096 节点 / 119.7M 这个 refactor 不变量。
        let stats = measure(&cfg, [R3, R3, R3, R3], &hu, u32::MAX);
        print_stats(
            "HU self-check (期望 240,096 节点 / 119.7M)",
            &ratios_desc([R3, R3, R3, R3]),
            &stats,
            &hu,
        );
        println!("walk wall time  : {:.3}s\n", start.elapsed().as_secs_f64());
    }

    // (2) 6-max S2 探针：argv raise 集 / env postflop 桶数 / preflop 169。
    {
        let six = BucketCounts {
            preflop: 169,
            postflop: postflop_buckets,
        };
        let cfg = TableConfig::default_6max_100bb();
        let r: &[f64] = &six_ratios;
        let label = format!(
            "6-max 100BB / {} bet size(s) / preflop 169 / postflop {postflop_buckets} / raise_cap {cap_desc}",
            six_ratios.len()
        );
        let start = std::time::Instant::now();
        let stats = measure(&cfg, [r, r, r, r], &six, raise_cap);
        print_stats(&label, &ratios_desc([r, r, r, r]), &stats, &six);
        println!("walk wall time  : {:.3}s\n", start.elapsed().as_secs_f64());
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
