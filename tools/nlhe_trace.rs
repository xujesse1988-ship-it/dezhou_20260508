//! 一次 ES-MCCFR update 的 trace 可视化工具（简化 NLHE / heads-up stack profile）。
//!
//! 跑法：
//! ```
//! cargo run --release --bin nlhe_trace -- \
//!     --bucket-table artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin \
//!     --warmup 20 --seed 0x4e4c48455f545241 \
//!     --output artifacts/nlhe_trace.html \
//!     [--stack-bb 100|200]
//! ```
//!
//! 输出与 Leduc 版同型的自包含 HTML（共用 `mccfr_trace_template.html`）；NLHE 没有
//! 独立 chance node（D-308 / D-315：所有 randomness 在 `GameState::with_rng` 时
//! 一次性消费），因此树里只有 Decision / Terminal 两种节点。
//!
//! warmup 默认值偏小（20）—— 单次 NLHE update 比 Leduc 重；如果想看到 regret /
//! sigma 上明显的非均匀分布，可以加大到 100-500。bucket table 加载 ~528 MiB / 1-2 s。

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use poker::abstraction::action::{AbstractAction, BetRatio};
use poker::abstraction::info::{InfoSetId, StreetTag};
use poker::core::{Card, Rank, Suit};
use poker::training::game::{Game, NodeKind, PlayerId};
use poker::training::nlhe::{
    NlheStackProfile, SimplifiedNlheAction, SimplifiedNlheGame, SimplifiedNlheInfoSet,
    SimplifiedNlheState,
};
use poker::training::{RegretTable, StrategyAccumulator};
use poker::{BucketTable, ChaCha20Rng, RngSource};

const DEFAULT_WARMUP: u64 = 20;
const DEFAULT_SEED: u64 = 0x4e4c_4845_5f54_5241; // "NLHE_TRA"

struct Args {
    warmup: u64,
    seed: u64,
    bucket_table: PathBuf,
    output: PathBuf,
    stack_profile: NlheStackProfile,
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("[nlhe_trace] argument error: {e}");
            eprintln!(
                "usage: cargo run --release --bin nlhe_trace -- \
                 --bucket-table PATH \
                 [--warmup N] [--seed N] [--output PATH] [--stack-bb 100|200]"
            );
            return ExitCode::from(2);
        }
    };
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("[nlhe_trace] failed: {e}");
            ExitCode::from(1)
        }
    }
}

fn parse_args() -> Result<Args, String> {
    let mut warmup = DEFAULT_WARMUP;
    let mut seed = DEFAULT_SEED;
    let mut bucket_table = PathBuf::new();
    let mut output = PathBuf::from("artifacts/nlhe_trace.html");
    let mut stack_profile = NlheStackProfile::default();

    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--warmup" => warmup = parse_u64(&it.next().ok_or("--warmup need value")?)?,
            "--seed" => seed = parse_u64(&it.next().ok_or("--seed need value")?)?,
            "--bucket-table" => {
                bucket_table = PathBuf::from(it.next().ok_or("--bucket-table need value")?)
            }
            "--output" => output = PathBuf::from(it.next().ok_or("--output need value")?),
            "--stack-bb" => {
                stack_profile = it.next().ok_or("--stack-bb need value")?.parse()?;
            }
            "-h" | "--help" => {
                println!(
                    "usage: cargo run --release --bin nlhe_trace -- \
                     --bucket-table PATH \
                     [--warmup N] [--seed N] [--output PATH] [--stack-bb 100|200]"
                );
                std::process::exit(0);
            }
            x => return Err(format!("unknown arg: {x}")),
        }
    }
    if bucket_table.as_os_str().is_empty() {
        return Err("--bucket-table is required".into());
    }
    Ok(Args {
        warmup,
        seed,
        bucket_table,
        output,
        stack_profile,
    })
}

fn parse_u64(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).map_err(|e| format!("bad hex u64 {s:?}: {e}"))
    } else {
        s.parse::<u64>().map_err(|e| format!("bad u64 {s:?}: {e}"))
    }
}

// ===========================================================================
// 镜像 src/training/trainer.rs::recurse_es（NLHE 路径无 Chance 分支）
// ===========================================================================

fn step_es(
    update_count: u64,
    n_players: u64,
    game: &SimplifiedNlheGame,
    regret: &mut RegretTable<SimplifiedNlheInfoSet>,
    strategy_sum: &mut StrategyAccumulator<SimplifiedNlheInfoSet>,
    rng: &mut dyn RngSource,
) {
    let traverser = (update_count % n_players) as PlayerId;
    let root = game.root(rng);
    recurse_es(root, traverser, 1.0, regret, strategy_sum, rng);
}

fn recurse_es(
    state: SimplifiedNlheState,
    traverser: PlayerId,
    pi_trav: f64,
    regret: &mut RegretTable<SimplifiedNlheInfoSet>,
    strategy_sum: &mut StrategyAccumulator<SimplifiedNlheInfoSet>,
    rng: &mut dyn RngSource,
) -> f64 {
    match SimplifiedNlheGame::current(&state) {
        NodeKind::Terminal => SimplifiedNlheGame::payoff(&state, traverser),
        NodeKind::Chance => unreachable!("simplified NLHE has no in-game chance nodes"),
        NodeKind::Player(actor) => {
            let info = SimplifiedNlheGame::info_set(&state, actor);
            let actions = SimplifiedNlheGame::legal_actions(&state);
            let n = actions.len();
            regret.get_or_init(info, n);
            let sigma = regret.current_strategy(&info, n);
            if actor == traverser {
                let weighted: Vec<f64> = sigma.iter().map(|s| pi_trav * s).collect();
                strategy_sum.accumulate(info, &weighted);
                let mut cfvs: Vec<f64> = Vec::with_capacity(n);
                for (i, action) in actions.iter().enumerate() {
                    let next_state = SimplifiedNlheGame::next(state.clone(), *action, rng);
                    let cfv = recurse_es(
                        next_state,
                        traverser,
                        pi_trav * sigma[i],
                        regret,
                        strategy_sum,
                        rng,
                    );
                    cfvs.push(cfv);
                }
                let sigma_value: f64 = sigma.iter().zip(&cfvs).map(|(s, c)| s * c).sum();
                let delta: Vec<f64> = cfvs.iter().map(|c| c - sigma_value).collect();
                regret.accumulate(info, &delta);
                sigma_value
            } else {
                let nonzero: Vec<(SimplifiedNlheAction, f64)> = actions
                    .iter()
                    .copied()
                    .zip(sigma.iter().copied())
                    .filter(|(_, p)| *p > 0.0)
                    .collect();
                let sampled = sample_discrete(&nonzero, rng);
                let next_state = SimplifiedNlheGame::next(state, sampled, rng);
                recurse_es(next_state, traverser, pi_trav, regret, strategy_sum, rng)
            }
        }
    }
}

fn sample_discrete<A: Copy>(dist: &[(A, f64)], rng: &mut dyn RngSource) -> A {
    let raw = rng.next_u64();
    let u = (raw >> 11) as f64 / ((1u64 << 53) as f64);
    let mut cum = 0.0;
    for &(a, p) in dist.iter().take(dist.len() - 1) {
        cum += p;
        if u < cum {
            return a;
        }
    }
    dist[dist.len() - 1].0
}

// ===========================================================================
// Trace data
// ===========================================================================

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
enum NodeRecord {
    Decision {
        actor: u8,
        is_traverser: bool,
        info_set_label: String,
        info_set_key: String,
        bucket_id: u32,
        node_id_in_tree: u32,
        street: String,
        actions: Vec<String>,
        pi_trav: f64,
        sigma: Vec<f64>,
        regret_at_visit: Vec<f64>,
        weighted_strategy_added: Option<Vec<f64>>,
        strategy_sum_after_node: Option<Vec<f64>>,
        cfvs: Option<Vec<f64>>,
        sigma_value: Option<f64>,
        regret_delta: Option<Vec<f64>>,
        regret_after_node: Option<Vec<f64>>,
        sampled_action: Option<String>,
        sampled_index: Option<usize>,
    },
    Terminal {
        payoff_traverser: f64,
        committed: [u32; 2],
    },
}

#[derive(Debug, Clone)]
struct TraceNode {
    id: usize,
    parent_id: Option<usize>,
    depth: usize,
    state_summary: StateSummary,
    record: NodeRecord,
    children_ids: Vec<usize>,
}

#[derive(Debug, Clone)]
struct StateSummary {
    fields: Vec<(String, String)>,
}

struct Collector {
    nodes: Vec<TraceNode>,
    visited_order: Vec<TouchedInfo>,
    visited_seen: HashSet<u64>,
}

#[derive(Debug, Clone)]
struct TouchedInfo {
    info: SimplifiedNlheInfoSet,
    label: String,
    actions: Vec<String>,
    n_actions: usize,
}

impl Collector {
    fn new() -> Self {
        Self {
            nodes: Vec::new(),
            visited_order: Vec::new(),
            visited_seen: HashSet::new(),
        }
    }
    fn note_infoset(&mut self, info: SimplifiedNlheInfoSet, touched: TouchedInfo) {
        if self.visited_seen.insert(info.raw()) {
            self.visited_order.push(touched);
        }
    }
}

// ===========================================================================
// Labels / formatting
// ===========================================================================

fn rank_char(r: Rank) -> char {
    match r {
        Rank::Two => '2',
        Rank::Three => '3',
        Rank::Four => '4',
        Rank::Five => '5',
        Rank::Six => '6',
        Rank::Seven => '7',
        Rank::Eight => '8',
        Rank::Nine => '9',
        Rank::Ten => 'T',
        Rank::Jack => 'J',
        Rank::Queen => 'Q',
        Rank::King => 'K',
        Rank::Ace => 'A',
    }
}
fn suit_char(s: Suit) -> char {
    match s {
        Suit::Clubs => 'c',
        Suit::Diamonds => 'd',
        Suit::Hearts => 'h',
        Suit::Spades => 's',
    }
}
fn card_label(c: Card) -> String {
    format!("{}{}", rank_char(c.rank()), suit_char(c.suit()))
}
fn cards_label(cards: &[Card]) -> String {
    cards
        .iter()
        .map(|c| card_label(*c))
        .collect::<Vec<_>>()
        .join(" ")
}
fn hole_label(h: Option<[Card; 2]>) -> String {
    match h {
        Some([a, b]) => format!("{}{}", card_label(a), card_label(b)),
        None => "—".into(),
    }
}

/// chips（1 chip = 1/100 BB）→ "N (= X.YY BB)" 字符串。
fn chip_str(chips: u64) -> String {
    let bb = chips as f64 / 100.0;
    format!("{} ({} BB)", chips, format_bb(bb))
}
fn format_bb(bb: f64) -> String {
    if bb == bb.round() {
        format!("{:.0}", bb)
    } else {
        format!("{:.2}", bb)
    }
}

fn ratio_label(r: BetRatio) -> String {
    let m = r.as_milli();
    if m == 500 {
        "½p".into()
    } else if m == 1000 {
        "1p".into()
    } else {
        format!("{:.3}p", m as f64 / 1000.0)
    }
}

fn action_label(a: AbstractAction) -> String {
    match a {
        AbstractAction::Fold => "Fold".into(),
        AbstractAction::Check => "Check".into(),
        AbstractAction::Call { to } => format!("Call→{}", to.as_u64()),
        AbstractAction::Bet { to, ratio_label: r } => {
            format!("Bet {}→{}", ratio_label(r), to.as_u64())
        }
        AbstractAction::Raise { to, ratio_label: r } => {
            format!("Raise {}→{}", ratio_label(r), to.as_u64())
        }
        AbstractAction::AllIn { to } => format!("AllIn→{}", to.as_u64()),
    }
}

fn street_label(s: poker::core::Street) -> &'static str {
    use poker::core::Street;
    match s {
        Street::Preflop => "Preflop",
        Street::Flop => "Flop",
        Street::Turn => "Turn",
        Street::River => "River",
        Street::Showdown => "Showdown",
    }
}

fn street_from_tag(t: StreetTag) -> &'static str {
    match t {
        StreetTag::Preflop => "Preflop",
        StreetTag::Flop => "Flop",
        StreetTag::Turn => "Turn",
        StreetTag::River => "River",
    }
}

fn info_set_label(info: SimplifiedNlheInfoSet, street_tag: StreetTag, node_id_tree: u32) -> String {
    let bucket = info.bucket_id();
    let bucket_tag = match street_tag {
        StreetTag::Preflop => format!("hand_class169={}", bucket),
        _ => format!("cluster={}", bucket),
    };
    format!(
        "{} | {} | tree_node={}",
        street_from_tag(street_tag),
        bucket_tag,
        node_id_tree
    )
}

fn state_summary(state: &SimplifiedNlheState) -> StateSummary {
    let gs = &state.game_state;
    let players = gs.players();
    let p0_hole = players.first().and_then(|p| p.hole_cards);
    let p1_hole = players.get(1).and_then(|p| p.hole_cards);
    let board = gs.board();
    let mut fields: Vec<(String, String)> = Vec::new();

    fields.push(("street".into(), street_label(gs.street()).into()));
    fields.push((
        "P0 hole".into(),
        hole_label(p0_hole)
            + &format!(
                "  stack={}  committed={}",
                players[0].stack.as_u64(),
                players[0].committed_total.as_u64()
            ),
    ));
    fields.push((
        "P1 hole".into(),
        hole_label(p1_hole)
            + &format!(
                "  stack={}  committed={}",
                players[1].stack.as_u64(),
                players[1].committed_total.as_u64()
            ),
    ));
    fields.push((
        "board".into(),
        if board.is_empty() {
            "—".into()
        } else {
            cards_label(board)
        },
    ));
    fields.push(("pot".into(), chip_str(gs.pot().as_u64())));

    let hist: Vec<String> = state
        .action_history
        .iter()
        .copied()
        .map(action_label)
        .collect();
    if !hist.is_empty() {
        fields.push(("abs action hist".into(), format!("[{}]", hist.join(", "))));
    }
    StateSummary { fields }
}

// ===========================================================================
// Traced DFS
// ===========================================================================

#[allow(clippy::too_many_arguments)]
fn recurse_traced(
    state: SimplifiedNlheState,
    traverser: PlayerId,
    pi_trav: f64,
    regret: &mut RegretTable<SimplifiedNlheInfoSet>,
    strategy_sum: &mut StrategyAccumulator<SimplifiedNlheInfoSet>,
    rng: &mut dyn RngSource,
    parent_id: Option<usize>,
    depth: usize,
    collector: &mut Collector,
) -> f64 {
    let summary = state_summary(&state);

    match SimplifiedNlheGame::current(&state) {
        NodeKind::Chance => unreachable!("simplified NLHE has no in-game chance nodes"),
        NodeKind::Terminal => {
            let payoff = SimplifiedNlheGame::payoff(&state, traverser);
            let committed = [
                state.game_state.players()[0].committed_total.as_u64() as u32,
                state.game_state.players()[1].committed_total.as_u64() as u32,
            ];
            let id = collector.nodes.len();
            collector.nodes.push(TraceNode {
                id,
                parent_id,
                depth,
                state_summary: summary,
                record: NodeRecord::Terminal {
                    payoff_traverser: payoff,
                    committed,
                },
                children_ids: vec![],
            });
            if let Some(pid) = parent_id {
                collector.nodes[pid].children_ids.push(id);
            }
            payoff
        }
        NodeKind::Player(actor) => {
            let info = SimplifiedNlheGame::info_set(&state, actor);
            let actions = SimplifiedNlheGame::legal_actions(&state);
            let n = actions.len();
            let node_id_tree: u32 = ((info.raw() >> 38) & ((1u64 << 26) - 1)) as u32;
            let street_tag = info.street_tag();
            let label = info_set_label(info, street_tag, node_id_tree);
            let action_labels: Vec<String> = actions.iter().copied().map(action_label).collect();
            collector.note_infoset(
                info,
                TouchedInfo {
                    info,
                    label: label.clone(),
                    actions: action_labels.clone(),
                    n_actions: n,
                },
            );

            regret.get_or_init(info, n);
            let regret_at_visit = regret
                .inner()
                .get(&info)
                .cloned()
                .unwrap_or_else(|| vec![0.0; n]);
            let sigma: Vec<f64> = regret.current_strategy(&info, n);

            let id = collector.nodes.len();
            let is_traverser = actor == traverser;

            let record = NodeRecord::Decision {
                actor,
                is_traverser,
                info_set_label: label,
                info_set_key: format!("0x{:016x}", info.raw()),
                bucket_id: info.bucket_id(),
                node_id_in_tree: node_id_tree,
                street: street_from_tag(street_tag).into(),
                actions: action_labels,
                pi_trav,
                sigma: sigma.clone(),
                regret_at_visit,
                weighted_strategy_added: None,
                strategy_sum_after_node: None,
                cfvs: None,
                sigma_value: None,
                regret_delta: None,
                regret_after_node: None,
                sampled_action: None,
                sampled_index: None,
            };
            collector.nodes.push(TraceNode {
                id,
                parent_id,
                depth,
                state_summary: summary,
                record,
                children_ids: vec![],
            });
            if let Some(pid) = parent_id {
                collector.nodes[pid].children_ids.push(id);
            }

            if is_traverser {
                let weighted: Vec<f64> = sigma.iter().map(|s| pi_trav * s).collect();
                strategy_sum.accumulate(info, &weighted);
                let strategy_sum_after_node = strategy_sum
                    .inner()
                    .get(&info)
                    .cloned()
                    .unwrap_or_else(|| vec![0.0; n]);

                let mut cfvs: Vec<f64> = Vec::with_capacity(n);
                for (i, action) in actions.iter().enumerate() {
                    let next_state = SimplifiedNlheGame::next(state.clone(), *action, rng);
                    let cfv = recurse_traced(
                        next_state,
                        traverser,
                        pi_trav * sigma[i],
                        regret,
                        strategy_sum,
                        rng,
                        Some(id),
                        depth + 1,
                        collector,
                    );
                    cfvs.push(cfv);
                }
                let sigma_value: f64 = sigma.iter().zip(&cfvs).map(|(s, c)| s * c).sum();
                let delta: Vec<f64> = cfvs.iter().map(|c| c - sigma_value).collect();
                regret.accumulate(info, &delta);
                let regret_after_node = regret
                    .inner()
                    .get(&info)
                    .cloned()
                    .unwrap_or_else(|| vec![0.0; n]);

                if let NodeRecord::Decision {
                    weighted_strategy_added,
                    strategy_sum_after_node: ssan,
                    cfvs: cf_slot,
                    sigma_value: sv_slot,
                    regret_delta: rd_slot,
                    regret_after_node: ran,
                    ..
                } = &mut collector.nodes[id].record
                {
                    *weighted_strategy_added = Some(weighted);
                    *ssan = Some(strategy_sum_after_node);
                    *cf_slot = Some(cfvs);
                    *sv_slot = Some(sigma_value);
                    *rd_slot = Some(delta);
                    *ran = Some(regret_after_node);
                }
                sigma_value
            } else {
                let nonzero: Vec<(SimplifiedNlheAction, f64)> = actions
                    .iter()
                    .copied()
                    .zip(sigma.iter().copied())
                    .filter(|(_, p)| *p > 0.0)
                    .collect();
                let raw = rng.next_u64();
                let u = (raw >> 11) as f64 / ((1u64 << 53) as f64);
                let mut cum = 0.0;
                let mut chosen_idx_in_nz = nonzero.len() - 1;
                for (i, (_, p)) in nonzero.iter().enumerate().take(nonzero.len() - 1) {
                    cum += p;
                    if u < cum {
                        chosen_idx_in_nz = i;
                        break;
                    }
                }
                let sampled = nonzero[chosen_idx_in_nz].0;
                let sampled_orig_idx = actions.iter().position(|a| *a == sampled).unwrap_or(0);

                if let NodeRecord::Decision {
                    sampled_action: sa,
                    sampled_index: si,
                    ..
                } = &mut collector.nodes[id].record
                {
                    *sa = Some(action_label(sampled));
                    *si = Some(sampled_orig_idx);
                }

                let next_state = SimplifiedNlheGame::next(state, sampled, rng);
                recurse_traced(
                    next_state,
                    traverser,
                    pi_trav,
                    regret,
                    strategy_sum,
                    rng,
                    Some(id),
                    depth + 1,
                    collector,
                )
            }
        }
    }
}

// ===========================================================================
// JSON encoding（手写，与 mccfr_trace.rs 同型）
// ===========================================================================

fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn f64_json(x: f64) -> String {
    if !x.is_finite() {
        return "null".to_string();
    }
    let s = format!("{x:.9}");
    let trimmed = s.trim_end_matches('0').trim_end_matches('.').to_string();
    if trimmed.is_empty() || trimmed == "-" {
        "0".to_string()
    } else {
        trimmed
    }
}

fn vec_f64_json(v: &[f64]) -> String {
    let inner = v.iter().map(|x| f64_json(*x)).collect::<Vec<_>>().join(",");
    format!("[{inner}]")
}

fn vec_str_json(v: &[String]) -> String {
    let inner = v.iter().map(|s| esc(s)).collect::<Vec<_>>().join(",");
    format!("[{inner}]")
}

fn state_summary_json(s: &StateSummary) -> String {
    let inner = s
        .fields
        .iter()
        .map(|(k, v)| format!("{{\"k\":{},\"v\":{}}}", esc(k), esc(v)))
        .collect::<Vec<_>>()
        .join(",");
    format!("{{\"fields\":[{inner}]}}")
}

fn record_json(r: &NodeRecord) -> String {
    match r {
        NodeRecord::Decision {
            actor,
            is_traverser,
            info_set_label,
            info_set_key,
            bucket_id,
            node_id_in_tree,
            street,
            actions,
            pi_trav,
            sigma,
            regret_at_visit,
            weighted_strategy_added,
            strategy_sum_after_node,
            cfvs,
            sigma_value,
            regret_delta,
            regret_after_node,
            sampled_action,
            sampled_index,
        } => {
            let mut parts = vec![
                "\"kind\":\"Decision\"".to_string(),
                format!("\"actor\":{}", actor),
                format!("\"is_traverser\":{}", is_traverser),
                format!("\"info_set_label\":{}", esc(info_set_label)),
                format!("\"info_set_key\":{}", esc(info_set_key)),
                format!("\"bucket_id\":{}", bucket_id),
                format!("\"node_id_in_tree\":{}", node_id_in_tree),
                format!("\"street\":{}", esc(street)),
                format!("\"actions\":{}", vec_str_json(actions)),
                format!("\"pi_trav\":{}", f64_json(*pi_trav)),
                format!("\"sigma\":{}", vec_f64_json(sigma)),
                format!("\"regret_at_visit\":{}", vec_f64_json(regret_at_visit)),
            ];
            if let Some(v) = weighted_strategy_added {
                parts.push(format!("\"weighted_strategy_added\":{}", vec_f64_json(v)));
            }
            if let Some(v) = strategy_sum_after_node {
                parts.push(format!("\"strategy_sum_after_node\":{}", vec_f64_json(v)));
            }
            if let Some(v) = cfvs {
                parts.push(format!("\"cfvs\":{}", vec_f64_json(v)));
            }
            if let Some(v) = sigma_value {
                parts.push(format!("\"sigma_value\":{}", f64_json(*v)));
            }
            if let Some(v) = regret_delta {
                parts.push(format!("\"regret_delta\":{}", vec_f64_json(v)));
            }
            if let Some(v) = regret_after_node {
                parts.push(format!("\"regret_after_node\":{}", vec_f64_json(v)));
            }
            if let Some(s) = sampled_action {
                parts.push(format!("\"sampled_action\":{}", esc(s)));
            }
            if let Some(i) = sampled_index {
                parts.push(format!("\"sampled_index\":{}", i));
            }
            format!("{{{}}}", parts.join(","))
        }
        NodeRecord::Terminal {
            payoff_traverser,
            committed,
        } => format!(
            "{{\"kind\":\"Terminal\",\"payoff_traverser\":{},\"committed\":[{},{}]}}",
            f64_json(*payoff_traverser),
            committed[0],
            committed[1],
        ),
    }
}

fn node_json(n: &TraceNode) -> String {
    let parent = n
        .parent_id
        .map(|p| p.to_string())
        .unwrap_or_else(|| "null".to_string());
    let kids = n
        .children_ids
        .iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"id\":{},\"parent_id\":{},\"depth\":{},\"children_ids\":[{}],\"state_summary\":{},\"record\":{}}}",
        n.id,
        parent,
        n.depth,
        kids,
        state_summary_json(&n.state_summary),
        record_json(&n.record),
    )
}

#[allow(clippy::too_many_arguments)]
fn snapshot_json(
    touched: &TouchedInfo,
    regret_before: &[f64],
    regret_after: &[f64],
    sigma_before: &[f64],
    sigma_after: &[f64],
    ssum_before: &[f64],
    ssum_after: &[f64],
    avg_before: &[f64],
    avg_after: &[f64],
) -> String {
    let key = format!("0x{:016x}", touched.info.raw());
    format!(
        "{{\"label\":{},\"key\":{},\"n_actions\":{},\"actions\":{},\"regret_before\":{},\"regret_after\":{},\"sigma_before\":{},\"sigma_after\":{},\"strategy_sum_before\":{},\"strategy_sum_after\":{},\"average_strategy_before\":{},\"average_strategy_after\":{}}}",
        esc(&touched.label),
        esc(&key),
        touched.n_actions,
        vec_str_json(&touched.actions),
        vec_f64_json(regret_before),
        vec_f64_json(regret_after),
        vec_f64_json(sigma_before),
        vec_f64_json(sigma_after),
        vec_f64_json(ssum_before),
        vec_f64_json(ssum_after),
        vec_f64_json(avg_before),
        vec_f64_json(avg_after),
    )
}

// ===========================================================================
// run
// ===========================================================================

fn run(args: Args) -> Result<(), String> {
    eprintln!(
        "[nlhe_trace] loading bucket table from {} ...",
        args.bucket_table.display()
    );
    let bucket_table = Arc::new(
        BucketTable::open(&args.bucket_table)
            .map_err(|e| format!("BucketTable::open failed: {e:?}"))?,
    );
    let game =
        SimplifiedNlheGame::new_with_stack_profile(Arc::clone(&bucket_table), args.stack_profile)
            .map_err(|e| format!("SimplifiedNlheGame::new failed: {e:?}"))?;
    let n_players = game.n_players() as u64;

    let mut regret = RegretTable::<SimplifiedNlheInfoSet>::new();
    let mut strategy_sum = StrategyAccumulator::<SimplifiedNlheInfoSet>::new();
    let mut rng = ChaCha20Rng::from_seed(args.seed);

    let mut update_count: u64 = 0;
    eprintln!("[nlhe_trace] running {} warmup updates ...", args.warmup);
    for _ in 0..args.warmup {
        step_es(
            update_count,
            n_players,
            &game,
            &mut regret,
            &mut strategy_sum,
            &mut rng,
        );
        update_count += 1;
    }

    let regret_before_table: HashMap<InfoSetId, Vec<f64>> = regret
        .inner()
        .iter()
        .map(|(k, v)| (*k, v.clone()))
        .collect();
    let strategy_sum_before_table: HashMap<InfoSetId, Vec<f64>> = strategy_sum
        .inner()
        .iter()
        .map(|(k, v)| (*k, v.clone()))
        .collect();
    let traverser = (update_count % n_players) as PlayerId;

    let mut collector = Collector::new();
    let root = game.root(&mut rng);
    eprintln!("[nlhe_trace] running traced update (traverser=P{traverser})...");
    let _ = recurse_traced(
        root,
        traverser,
        1.0,
        &mut regret,
        &mut strategy_sum,
        &mut rng,
        None,
        0,
        &mut collector,
    );
    let traced_update_index = update_count;

    let regret_after_table: HashMap<InfoSetId, Vec<f64>> = regret
        .inner()
        .iter()
        .map(|(k, v)| (*k, v.clone()))
        .collect();
    let strategy_sum_after_table: HashMap<InfoSetId, Vec<f64>> = strategy_sum
        .inner()
        .iter()
        .map(|(k, v)| (*k, v.clone()))
        .collect();

    let mut snapshots: Vec<String> = Vec::new();
    for t in &collector.visited_order {
        let n = t.n_actions;
        let regret_before = regret_before_table
            .get(&t.info)
            .cloned()
            .unwrap_or_else(|| vec![0.0; n]);
        let regret_after = regret_after_table
            .get(&t.info)
            .cloned()
            .unwrap_or_else(|| vec![0.0; n]);
        let ssum_before = strategy_sum_before_table
            .get(&t.info)
            .cloned()
            .unwrap_or_else(|| vec![0.0; n]);
        let ssum_after = strategy_sum_after_table
            .get(&t.info)
            .cloned()
            .unwrap_or_else(|| vec![0.0; n]);
        let sigma_before = sigma_from_regret(&regret_before, n);
        let sigma_after = sigma_from_regret(&regret_after, n);
        let avg_before = avg_from_ssum(&ssum_before, n);
        let avg_after = avg_from_ssum(&ssum_after, n);
        snapshots.push(snapshot_json(
            t,
            &regret_before,
            &regret_after,
            &sigma_before,
            &sigma_after,
            &ssum_before,
            &ssum_after,
            &avg_before,
            &avg_after,
        ));
    }

    let nodes_json: Vec<String> = collector.nodes.iter().map(node_json).collect();

    let payload = format!(
        "{{\"metadata\":{{\"game\":\"Simplified NLHE\",\"stack_profile\":\"{}\",\"seed_hex\":\"0x{:016x}\",\"warmup_updates\":{},\"update_index\":{},\"traverser\":{},\"n_nodes\":{},\"n_infosets_touched\":{}}},\"nodes\":[{}],\"infoset_snapshots\":[{}]}}",
        args.stack_profile,
        args.seed,
        args.warmup,
        traced_update_index,
        traverser,
        collector.nodes.len(),
        collector.visited_order.len(),
        nodes_json.join(","),
        snapshots.join(","),
    );

    if let Some(parent) = args.output.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {} failed: {e}", parent.display()))?;
        }
    }
    let html = HTML_TEMPLATE.replace("/*__TRACE_DATA__*/ null", &payload);
    fs::write(&args.output, html.as_bytes())
        .map_err(|e| format!("write {} failed: {e}", args.output.display()))?;

    println!(
        "[nlhe_trace] wrote {} ({} nodes, {} info sets touched, traverser=P{}, update#={})",
        args.output.display(),
        collector.nodes.len(),
        collector.visited_order.len(),
        traverser,
        traced_update_index,
    );
    Ok(())
}

fn sigma_from_regret(r: &[f64], n: usize) -> Vec<f64> {
    if n == 0 {
        return vec![];
    }
    let mut out: Vec<f64> = r.iter().map(|x| x.max(0.0)).collect();
    let sum: f64 = out.iter().sum();
    if sum > 0.0 {
        for v in &mut out {
            *v /= sum;
        }
        out
    } else {
        vec![1.0 / n as f64; n]
    }
}

fn avg_from_ssum(s: &[f64], n: usize) -> Vec<f64> {
    if n == 0 {
        return vec![];
    }
    let sum: f64 = s.iter().sum();
    if sum > 0.0 {
        s.iter().map(|x| x / sum).collect()
    } else {
        vec![1.0 / n as f64; n]
    }
}

const HTML_TEMPLATE: &str = include_str!("mccfr_trace_template.html");
