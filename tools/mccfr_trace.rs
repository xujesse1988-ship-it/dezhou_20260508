//! 一次 ES-MCCFR update 的 trace 可视化工具（Leduc）。
//!
//! 跑法：
//! ```
//! cargo run --release --bin mccfr_trace -- \
//!     --warmup 200 --seed 0x4c454455435f5452 \
//!     --output artifacts/mccfr_trace.html
//! ```
//!
//! 输出一个自包含的 HTML 文件，内嵌：
//! - DFS 递归树（chance / traverser-decision / opponent-decision / terminal）
//! - 每个 traverser 决策点的 sigma / cfvs / regret_delta / regret(before/after)
//!   / strategy_sum(before/after)
//! - 每个 opponent 决策点的 sigma / sampled action
//! - 触达 InfoSet 的 before/after snapshot（regret / current_strategy / strategy_sum
//!   / average_strategy）
//!
//! 注意：本工具自带 `RegretTable + StrategyAccumulator`，warmup 复用与
//! `src/training/trainer.rs::recurse_es` 同型的本地 `step_es` 实现，traced step 由
//! `recurse_traced` 在同一份表上直接 mutate。算法路径（采样、regret/strategy_sum
//! 累积顺序）与 trainer 内部完全一致；本工具不消费 `EsMccfrTrainer`（其内部字段
//! `pub(crate)`，binary crate 无法触达）。

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use poker::training::game::{Game, NodeKind, PlayerId};
use poker::training::leduc::{LeducAction, LeducGame, LeducInfoSet, LeducState, LeducStreet};
use poker::training::{RegretTable, StrategyAccumulator};
use poker::{ChaCha20Rng, RngSource};

const DEFAULT_WARMUP: u64 = 200;
const DEFAULT_SEED: u64 = 0x4c45_4455_435f_5452; // "LEDUC_TR"

struct Args {
    warmup: u64,
    seed: u64,
    output: PathBuf,
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("[mccfr_trace] argument error: {e}");
            eprintln!(
                "usage: cargo run --release --bin mccfr_trace -- \
                 [--warmup N] [--seed N] [--output PATH]"
            );
            return ExitCode::from(2);
        }
    };
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("[mccfr_trace] failed: {e}");
            ExitCode::from(1)
        }
    }
}

fn parse_args() -> Result<Args, String> {
    let mut warmup = DEFAULT_WARMUP;
    let mut seed = DEFAULT_SEED;
    let mut output = PathBuf::from("artifacts/mccfr_trace.html");

    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--warmup" => warmup = parse_u64(&it.next().ok_or("--warmup need value")?)?,
            "--seed" => seed = parse_u64(&it.next().ok_or("--seed need value")?)?,
            "--output" => output = PathBuf::from(it.next().ok_or("--output need value")?),
            "-h" | "--help" => {
                println!(
                    "usage: cargo run --release --bin mccfr_trace -- \
                     [--warmup N] [--seed N] [--output PATH]"
                );
                std::process::exit(0);
            }
            x => return Err(format!("unknown arg: {x}")),
        }
    }
    Ok(Args {
        warmup,
        seed,
        output,
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
// 与 src/training/trainer.rs::recurse_es 同型的本地 step_es（不带 trace）
// ===========================================================================

fn step_es(
    update_count: u64,
    n_players: u64,
    regret: &mut RegretTable<LeducInfoSet>,
    strategy_sum: &mut StrategyAccumulator<LeducInfoSet>,
    rng: &mut dyn RngSource,
) {
    let traverser = (update_count % n_players) as PlayerId;
    let root = LeducGame.root(rng);
    recurse_es(root, traverser, 1.0, regret, strategy_sum, rng);
}

fn recurse_es(
    state: LeducState,
    traverser: PlayerId,
    pi_trav: f64,
    regret: &mut RegretTable<LeducInfoSet>,
    strategy_sum: &mut StrategyAccumulator<LeducInfoSet>,
    rng: &mut dyn RngSource,
) -> f64 {
    match LeducGame::current(&state) {
        NodeKind::Terminal => LeducGame::payoff(&state, traverser),
        NodeKind::Chance => {
            let dist = LeducGame::chance_distribution(&state);
            let action = sample_discrete(&dist, rng);
            let next_state = LeducGame::next(state, action, rng);
            recurse_es(next_state, traverser, pi_trav, regret, strategy_sum, rng)
        }
        NodeKind::Player(actor) => {
            let info = LeducGame::info_set(&state, actor);
            let actions = LeducGame::legal_actions(&state);
            let n = actions.len();
            regret.get_or_init(info.clone(), n);
            let sigma = regret.current_strategy(&info, n);
            if actor == traverser {
                let weighted: Vec<f64> = sigma.iter().map(|s| pi_trav * s).collect();
                strategy_sum.accumulate(info.clone(), &weighted);
                let mut cfvs: Vec<f64> = Vec::with_capacity(n);
                for (i, action) in actions.iter().enumerate() {
                    let next_state = LeducGame::next(state.clone(), *action, rng);
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
                let nonzero: Vec<(LeducAction, f64)> = actions
                    .iter()
                    .copied()
                    .zip(sigma.iter().copied())
                    .filter(|(_, p)| *p > 0.0)
                    .collect();
                let sampled = sample_discrete(&nonzero, rng);
                let next_state = LeducGame::next(state, sampled, rng);
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
#[allow(clippy::large_enum_variant)] // one-off trace tool；总节点 < 100，Box 化无收益
enum NodeRecord {
    Chance {
        deal_target: String,
        distribution: Vec<(String, f64)>,
        sampled_action: String,
        sampled_index: usize,
    },
    Decision {
        actor: u8,
        is_traverser: bool,
        info_set_label: String,
        info_set_key: String,
        private_card_rank: u8,
        public_card_rank: Option<u8>,
        street: String,
        preflop_history: Vec<String>,
        history: Vec<String>,
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
    cards: [Option<u8>; 2],
    public_card: Option<u8>,
    street: String,
    preflop_history: Vec<String>,
    history: Vec<String>,
    committed: [u32; 2],
}

struct Collector {
    nodes: Vec<TraceNode>,
    visited_order: Vec<LeducInfoSet>,
    visited_seen: HashSet<String>,
}

impl Collector {
    fn new() -> Self {
        Self {
            nodes: Vec::new(),
            visited_order: Vec::new(),
            visited_seen: HashSet::new(),
        }
    }
    fn note_infoset(&mut self, info: &LeducInfoSet) {
        let key = format!("{info:?}");
        if self.visited_seen.insert(key) {
            self.visited_order.push(info.clone());
        }
    }
}

// ===========================================================================
// 标签 helpers
// ===========================================================================

fn card_label(c: u8) -> String {
    let rank = match c / 2 {
        0 => "J",
        1 => "Q",
        2 => "K",
        _ => "?",
    };
    let suit = if c % 2 == 0 { "s" } else { "h" };
    format!("{rank}{suit}")
}

fn rank_label(r: u8) -> &'static str {
    match r {
        11 => "J",
        12 => "Q",
        13 => "K",
        _ => "?",
    }
}

fn action_label(a: LeducAction) -> String {
    match a {
        LeducAction::Check => "Check",
        LeducAction::Bet => "Bet",
        LeducAction::Call => "Call",
        LeducAction::Fold => "Fold",
        LeducAction::Raise => "Raise",
        LeducAction::Deal0 => "Deal(Js)",
        LeducAction::Deal1 => "Deal(Jh)",
        LeducAction::Deal2 => "Deal(Qs)",
        LeducAction::Deal3 => "Deal(Qh)",
        LeducAction::Deal4 => "Deal(Ks)",
        LeducAction::Deal5 => "Deal(Kh)",
    }
    .to_string()
}

fn street_label(s: LeducStreet) -> &'static str {
    match s {
        LeducStreet::Preflop => "Preflop",
        LeducStreet::Postflop => "Postflop",
    }
}

fn state_summary(state: &LeducState) -> StateSummary {
    let to_opt = |c: u8| if c == 0xFF { None } else { Some(c) };
    StateSummary {
        cards: [to_opt(state.cards[0]), to_opt(state.cards[1])],
        public_card: state.public_card,
        street: street_label(state.street).into(),
        preflop_history: state
            .preflop_history
            .iter()
            .copied()
            .map(action_label)
            .collect(),
        history: state.history.iter().copied().map(action_label).collect(),
        committed: state.committed,
    }
}

fn info_set_label(info: &LeducInfoSet) -> String {
    let pub_s = info
        .public_card
        .map(|r| rank_label(r).to_string())
        .unwrap_or_else(|| "?".into());
    let prv = rank_label(info.private_card);
    let preflop = info
        .preflop_history
        .iter()
        .copied()
        .map(action_label)
        .collect::<Vec<_>>()
        .join(",");
    let history = info
        .history
        .iter()
        .copied()
        .map(action_label)
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "P{} | {} | board={} | {} | pre=[{}] | this=[{}]",
        info.actor,
        prv,
        pub_s,
        street_label(info.street),
        preflop,
        history
    )
}

fn deal_target_label(state: &LeducState) -> String {
    if state.cards[0] == 0xFF {
        "deal private card → P0".into()
    } else if state.cards[1] == 0xFF {
        "deal private card → P1".into()
    } else {
        "deal public board card".into()
    }
}

// ===========================================================================
// Traced DFS
// ===========================================================================

#[allow(clippy::too_many_arguments)]
fn recurse_traced(
    state: LeducState,
    traverser: PlayerId,
    pi_trav: f64,
    regret: &mut RegretTable<LeducInfoSet>,
    strategy_sum: &mut StrategyAccumulator<LeducInfoSet>,
    rng: &mut dyn RngSource,
    parent_id: Option<usize>,
    depth: usize,
    collector: &mut Collector,
) -> f64 {
    let summary = state_summary(&state);

    match LeducGame::current(&state) {
        NodeKind::Terminal => {
            let payoff = LeducGame::payoff(&state, traverser);
            let id = collector.nodes.len();
            collector.nodes.push(TraceNode {
                id,
                parent_id,
                depth,
                state_summary: summary,
                record: NodeRecord::Terminal {
                    payoff_traverser: payoff,
                    committed: state.committed,
                },
                children_ids: vec![],
            });
            if let Some(pid) = parent_id {
                collector.nodes[pid].children_ids.push(id);
            }
            payoff
        }
        NodeKind::Chance => {
            let dist = LeducGame::chance_distribution(&state);
            // 必须与 sample_discrete 路径同消费 1 次 rng（保持算法等价）
            let raw = rng.next_u64();
            let u = (raw >> 11) as f64 / ((1u64 << 53) as f64);
            let mut cum = 0.0;
            let mut chosen_idx = dist.len() - 1;
            for (i, (_, p)) in dist.iter().enumerate().take(dist.len() - 1) {
                cum += p;
                if u < cum {
                    chosen_idx = i;
                    break;
                }
            }
            let chosen_action = dist[chosen_idx].0;

            let id = collector.nodes.len();
            collector.nodes.push(TraceNode {
                id,
                parent_id,
                depth,
                state_summary: summary,
                record: NodeRecord::Chance {
                    deal_target: deal_target_label(&state),
                    distribution: dist.iter().map(|(a, p)| (action_label(*a), *p)).collect(),
                    sampled_action: action_label(chosen_action),
                    sampled_index: chosen_idx,
                },
                children_ids: vec![],
            });
            if let Some(pid) = parent_id {
                collector.nodes[pid].children_ids.push(id);
            }

            let next_state = LeducGame::next(state, chosen_action, rng);
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
        NodeKind::Player(actor) => {
            let info = LeducGame::info_set(&state, actor);
            collector.note_infoset(&info);
            let actions = LeducGame::legal_actions(&state);
            let n = actions.len();
            regret.get_or_init(info.clone(), n);
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
                info_set_label: info_set_label(&info),
                info_set_key: format!("{info:?}"),
                private_card_rank: info.private_card,
                public_card_rank: info.public_card,
                street: street_label(info.street).into(),
                preflop_history: info
                    .preflop_history
                    .iter()
                    .copied()
                    .map(action_label)
                    .collect(),
                history: info.history.iter().copied().map(action_label).collect(),
                actions: actions.iter().copied().map(action_label).collect(),
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
                strategy_sum.accumulate(info.clone(), &weighted);
                let strategy_sum_after_node = strategy_sum
                    .inner()
                    .get(&info)
                    .cloned()
                    .unwrap_or_else(|| vec![0.0; n]);

                let mut cfvs: Vec<f64> = Vec::with_capacity(n);
                for (i, action) in actions.iter().enumerate() {
                    let next_state = LeducGame::next(state.clone(), *action, rng);
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
                regret.accumulate(info.clone(), &delta);
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
                let nonzero: Vec<(LeducAction, f64)> = actions
                    .iter()
                    .copied()
                    .zip(sigma.iter().copied())
                    .filter(|(_, p)| *p > 0.0)
                    .collect();
                // 与 sample_discrete 同消费 1 次 rng
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

                let next_state = LeducGame::next(state, sampled, rng);
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
// 手写 JSON encoding（避开给 InfoSet 引入 Serialize 实现的依赖）
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

fn opt_card_json(c: Option<u8>) -> String {
    match c {
        Some(c) => esc(&card_label(c)),
        None => "null".to_string(),
    }
}

fn state_summary_json(s: &StateSummary) -> String {
    format!(
        "{{\"cards\":[{},{}],\"public_card\":{},\"street\":{},\"preflop_history\":{},\"history\":{},\"committed\":[{},{}]}}",
        opt_card_json(s.cards[0]),
        opt_card_json(s.cards[1]),
        opt_card_json(s.public_card),
        esc(&s.street),
        vec_str_json(&s.preflop_history),
        vec_str_json(&s.history),
        s.committed[0],
        s.committed[1],
    )
}

fn record_json(r: &NodeRecord) -> String {
    match r {
        NodeRecord::Chance {
            deal_target,
            distribution,
            sampled_action,
            sampled_index,
        } => {
            let dist = distribution
                .iter()
                .map(|(a, p)| format!("{{\"action\":{},\"prob\":{}}}", esc(a), f64_json(*p)))
                .collect::<Vec<_>>()
                .join(",");
            format!(
                "{{\"kind\":\"Chance\",\"deal_target\":{},\"distribution\":[{}],\"sampled_action\":{},\"sampled_index\":{}}}",
                esc(deal_target),
                dist,
                esc(sampled_action),
                sampled_index,
            )
        }
        NodeRecord::Decision {
            actor,
            is_traverser,
            info_set_label,
            info_set_key,
            private_card_rank,
            public_card_rank,
            street,
            preflop_history,
            history,
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
                format!("\"private_card_rank\":{}", private_card_rank),
                format!(
                    "\"public_card_rank\":{}",
                    public_card_rank
                        .map(|r| r.to_string())
                        .unwrap_or_else(|| "null".to_string())
                ),
                format!("\"street\":{}", esc(street)),
                format!("\"preflop_history\":{}", vec_str_json(preflop_history)),
                format!("\"history\":{}", vec_str_json(history)),
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
    info: &LeducInfoSet,
    n: usize,
    actions: &[LeducAction],
    regret_before: &[f64],
    regret_after: &[f64],
    sigma_before: &[f64],
    sigma_after: &[f64],
    ssum_before: &[f64],
    ssum_after: &[f64],
    avg_before: &[f64],
    avg_after: &[f64],
) -> String {
    let label = info_set_label(info);
    let key = format!("{info:?}");
    let actions_str: Vec<String> = actions.iter().copied().map(action_label).collect();
    format!(
        "{{\"label\":{},\"key\":{},\"n_actions\":{},\"actions\":{},\"regret_before\":{},\"regret_after\":{},\"sigma_before\":{},\"sigma_after\":{},\"strategy_sum_before\":{},\"strategy_sum_after\":{},\"average_strategy_before\":{},\"average_strategy_after\":{}}}",
        esc(&label),
        esc(&key),
        n,
        vec_str_json(&actions_str),
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
    let n_players = LeducGame.n_players() as u64;
    let mut regret = RegretTable::<LeducInfoSet>::new();
    let mut strategy_sum = StrategyAccumulator::<LeducInfoSet>::new();
    let mut rng = ChaCha20Rng::from_seed(args.seed);

    let mut update_count: u64 = 0;
    for _ in 0..args.warmup {
        step_es(
            update_count,
            n_players,
            &mut regret,
            &mut strategy_sum,
            &mut rng,
        );
        update_count += 1;
    }

    let regret_before_table: HashMap<LeducInfoSet, Vec<f64>> = regret
        .inner()
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let strategy_sum_before_table: HashMap<LeducInfoSet, Vec<f64>> = strategy_sum
        .inner()
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let traverser = (update_count % n_players) as PlayerId;

    let mut collector = Collector::new();
    let root = LeducGame.root(&mut rng);
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

    let regret_after_table: HashMap<LeducInfoSet, Vec<f64>> = regret
        .inner()
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let strategy_sum_after_table: HashMap<LeducInfoSet, Vec<f64>> = strategy_sum
        .inner()
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    let mut snapshots: Vec<String> = Vec::new();
    for info in &collector.visited_order {
        let regret_after = regret_after_table.get(info).cloned().unwrap_or_default();
        let n = regret_after.len();
        let actions = leduc_actions_from_info(info);
        let regret_before = regret_before_table
            .get(info)
            .cloned()
            .unwrap_or_else(|| vec![0.0; n]);
        let ssum_before = strategy_sum_before_table
            .get(info)
            .cloned()
            .unwrap_or_else(|| vec![0.0; n]);
        let ssum_after = strategy_sum_after_table
            .get(info)
            .cloned()
            .unwrap_or_else(|| vec![0.0; n]);
        let sigma_before = sigma_from_regret(&regret_before, n);
        let sigma_after = sigma_from_regret(&regret_after, n);
        let avg_before = avg_from_ssum(&ssum_before, n);
        let avg_after = avg_from_ssum(&ssum_after, n);
        snapshots.push(snapshot_json(
            info,
            n,
            &actions,
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
        "{{\"metadata\":{{\"game\":\"Leduc\",\"seed_hex\":\"0x{:016x}\",\"warmup_updates\":{},\"update_index\":{},\"traverser\":{},\"n_nodes\":{},\"n_infosets_touched\":{}}},\"nodes\":[{}],\"infoset_snapshots\":[{}]}}",
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
    // 模板里占位符写成 `/*__TRACE_DATA__*/ null`，让未替换的模板也是合法 JS
    // （直接在浏览器打开模板只显示提示页，不抛 ReferenceError）。
    let html = HTML_TEMPLATE.replace("/*__TRACE_DATA__*/ null", &payload);
    fs::write(&args.output, html.as_bytes())
        .map_err(|e| format!("write {} failed: {e}", args.output.display()))?;

    println!(
        "[mccfr_trace] wrote {} ({} nodes, {} info sets touched, traverser=P{}, update#={})",
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

fn leduc_actions_from_info(info: &LeducInfoSet) -> Vec<LeducAction> {
    let has_outstanding = info
        .history
        .iter()
        .rev()
        .find_map(|a| match a {
            LeducAction::Bet | LeducAction::Raise => Some(true),
            LeducAction::Call | LeducAction::Check | LeducAction::Fold => Some(false),
            _ => None,
        })
        .unwrap_or(false);
    if has_outstanding {
        let raises = info
            .history
            .iter()
            .filter(|a| matches!(a, LeducAction::Bet | LeducAction::Raise))
            .count();
        let mut out = vec![LeducAction::Fold, LeducAction::Call];
        if raises < 2 {
            out.push(LeducAction::Raise);
        }
        out
    } else {
        vec![LeducAction::Check, LeducAction::Bet]
    }
}

const HTML_TEMPLATE: &str = include_str!("mccfr_trace_template.html");
