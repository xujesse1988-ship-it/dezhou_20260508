//! Leduc ES-MCCFR LBR proxy 校准工具。
//!
//! 在 Leduc 上 LBR proxy 不是必需的（`exploitability::<LeducGame,
//! LeducBestResponse>` 已能给精确 BR），但本工具同时跑两者，让 LBR proxy
//! 方法学有外部对照：proxy mean BR chips ≤ exact exploitability，并且随训练
//! 进度收敛到同一 0 附近。这是 CLAUDE.md "在已知 Nash 解的小博弈上对照"
//! 给简化 NLHE LBR proxy 趋势提供方法学证据的入口。
//!
//! 输出 Markdown + 同名 JSON：每个 `--curve-update N` 里程碑给一行 `(update,
//! exact_exploitability, lbr_mean ± SE, probes_used)`。

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use blake3::Hasher;
use serde::Serialize;

use poker::training::game::{Game, NodeKind};
use poker::training::leduc::{LeducAction, LeducGame, LeducInfoSet, LeducState};
use poker::training::{
    estimate_lbr, exploitability, EsMccfrTrainer, LbrConfig, LeducBestResponse, Trainer,
};
use poker::{ChaCha20Rng, RngSource};

const DEFAULT_SEED: u64 = 0x4c45_4455_435f_4c42; // "LEDUC_LB"
const DEFAULT_LBR_SEED: u64 = 0x4c42_525f_4c45_4455; // "LBR_LEDU"

/// 与 nlhe_h3_report 同款 fallback：strategy_sum 全零 infoset 退化用 current_strategy。
#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
enum FallbackPolicy {
    Average,
    Current,
    Hybrid,
}

impl FallbackPolicy {
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "average" | "avg" => Ok(FallbackPolicy::Average),
            "current" | "cur" => Ok(FallbackPolicy::Current),
            "hybrid" => Ok(FallbackPolicy::Hybrid),
            other => Err(format!(
                "unknown --fallback-policy {other:?}; expected average|current|hybrid"
            )),
        }
    }

    fn slug(self) -> &'static str {
        match self {
            FallbackPolicy::Average => "average",
            FallbackPolicy::Current => "current",
            FallbackPolicy::Hybrid => "hybrid",
        }
    }
}

#[derive(Debug)]
struct Args {
    updates: u64,
    seed: u64,
    curve_updates: Vec<u64>,
    lbr_probes: u64,
    lbr_rollouts: u64,
    lbr_seed: u64,
    output: PathBuf,
    fallback_policy: FallbackPolicy,
    include_uniform_baseline: bool,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            updates: 1_000_000,
            seed: DEFAULT_SEED,
            curve_updates: vec![10_000, 100_000, 1_000_000],
            lbr_probes: 4_000,
            lbr_rollouts: 32,
            lbr_seed: DEFAULT_LBR_SEED,
            output: PathBuf::from("artifacts/leduc_lbr_calibration.md"),
            fallback_policy: FallbackPolicy::Hybrid,
            include_uniform_baseline: true,
        }
    }
}

#[derive(Serialize, Debug)]
struct CalibrationReport {
    seed: u64,
    updates: u64,
    lbr_seed: u64,
    lbr_probes: u64,
    lbr_rollouts: u64,
    fallback_policy: FallbackPolicy,
    strategy_blake3: String,
    points: Vec<CurvePoint>,
}

#[derive(Serialize, Clone, Debug)]
struct CurvePoint {
    label: String,
    update_count: u64,
    exact_exploitability: f64,
    lbr_mean_chips: f64,
    lbr_standard_error_chips: f64,
    lbr_probes_used: u64,
    lbr_probes_requested: u64,
    strategy_blake3: String,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("[leduc_lbr_report] failed: {e}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    if args.lbr_probes == 0 || args.lbr_rollouts == 0 {
        return Err("--lbr-probes and --lbr-rollouts must be > 0".to_string());
    }
    if args.curve_updates.iter().any(|&n| n > args.updates) {
        return Err(format!(
            "--curve-update {:?} contains a value greater than --updates {}",
            args.curve_updates, args.updates
        ));
    }
    let mut milestones = args.curve_updates.clone();
    milestones.sort();
    milestones.dedup();

    let mut trainer = EsMccfrTrainer::new(LeducGame, args.seed);
    let mut rng = ChaCha20Rng::from_seed(args.seed);
    let started = Instant::now();

    eprintln!(
        "[leduc_lbr_report] updates={} seed=0x{:016x} milestones={:?} fallback={}",
        args.updates,
        args.seed,
        milestones,
        args.fallback_policy.slug()
    );

    let lbr_cfg = LbrConfig {
        probes: args.lbr_probes,
        rollouts_per_action: args.lbr_rollouts,
        seed: args.lbr_seed,
        max_actions_per_probe: 64,
        max_actions_per_rollout: 64,
    };

    let mut points = Vec::new();
    if args.include_uniform_baseline {
        // 注意 LBR proxy 把空 Vec 当作 uniform fallback，但 LeducBestResponse 不会
        // —— 它直接 sigma[i] 索引会 panic。两条路径都要显式 uniform。
        let uniform = |_info: &LeducInfoSet, n: usize| vec![1.0 / n as f64; n];
        let lbr = estimate_lbr::<LeducGame>(&LeducGame, &uniform, &lbr_cfg)
            .map_err(|e| format!("uniform LBR proxy failed: {e:?}"))?;
        let exact = exploitability::<LeducGame, LeducBestResponse>(&LeducGame, &uniform);
        points.push(CurvePoint {
            label: "uniform-0".to_string(),
            update_count: 0,
            exact_exploitability: exact,
            lbr_mean_chips: lbr.mean_best_response_chips,
            lbr_standard_error_chips: lbr.standard_error_chips,
            lbr_probes_used: lbr.probes_used,
            lbr_probes_requested: lbr.probes_requested,
            strategy_blake3: "uniform-empty".to_string(),
        });
    }

    let mut next_milestone_idx = 0usize;
    while trainer.update_count() < args.updates {
        trainer
            .step(&mut rng)
            .map_err(|e| format!("step at update {} failed: {e:?}", trainer.update_count()))?;
        let done = trainer.update_count();
        if next_milestone_idx < milestones.len() && done == milestones[next_milestone_idx] {
            let point = sample_point(&trainer, &lbr_cfg, args.fallback_policy)
                .map_err(|e| format!("sample point @ {done} failed: {e}"))?;
            eprintln!(
                "[leduc_lbr_report] update={done} exact={:.6} lbr={:.6} ± {:.6} probes={}",
                point.exact_exploitability,
                point.lbr_mean_chips,
                point.lbr_standard_error_chips,
                point.lbr_probes_used
            );
            points.push(point);
            next_milestone_idx += 1;
        }
    }
    if next_milestone_idx < milestones.len() {
        // 兜底：最大 milestone == updates 但 strict equality 没命中（不应发生）。
        let final_point = sample_point(&trainer, &lbr_cfg, args.fallback_policy)
            .map_err(|e| format!("final point @ {} failed: {e}", trainer.update_count()))?;
        points.push(final_point);
    }

    let final_strategy_hash = strategy_hash(&trainer, args.fallback_policy);
    let report = CalibrationReport {
        seed: args.seed,
        updates: args.updates,
        lbr_seed: args.lbr_seed,
        lbr_probes: args.lbr_probes,
        lbr_rollouts: args.lbr_rollouts,
        fallback_policy: args.fallback_policy,
        strategy_blake3: final_strategy_hash,
        points,
    };

    write_reports(&args.output, &report)?;
    eprintln!(
        "[leduc_lbr_report] wrote {} ({:.1}s total)",
        args.output.display(),
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

fn sample_point(
    trainer: &EsMccfrTrainer<LeducGame>,
    lbr_cfg: &LbrConfig,
    policy: FallbackPolicy,
) -> Result<CurvePoint, String> {
    let strategy = make_strategy_fn(trainer, policy);
    let exact = exploitability::<LeducGame, LeducBestResponse>(&LeducGame, &strategy);
    let lbr = estimate_lbr::<LeducGame>(&LeducGame, &strategy, lbr_cfg)
        .map_err(|e| format!("LBR proxy failed: {e:?}"))?;
    Ok(CurvePoint {
        label: format!("update-{}", trainer.update_count()),
        update_count: trainer.update_count(),
        exact_exploitability: exact,
        lbr_mean_chips: lbr.mean_best_response_chips,
        lbr_standard_error_chips: lbr.standard_error_chips,
        lbr_probes_used: lbr.probes_used,
        lbr_probes_requested: lbr.probes_requested,
        strategy_blake3: strategy_hash(trainer, policy),
    })
}

fn make_strategy_fn(
    trainer: &EsMccfrTrainer<LeducGame>,
    policy: FallbackPolicy,
) -> impl Fn(&LeducInfoSet, usize) -> Vec<f64> + '_ {
    move |info: &LeducInfoSet, n: usize| {
        let raw = match policy {
            FallbackPolicy::Average => trainer.average_strategy(info),
            FallbackPolicy::Current => trainer.current_strategy(info),
            FallbackPolicy::Hybrid => {
                let degenerate = match trainer.strategy_sum().inner().get(info) {
                    None => true,
                    Some(v) => v.iter().sum::<f64>() <= 0.0,
                };
                if degenerate {
                    trainer.current_strategy(info)
                } else {
                    trainer.average_strategy(info)
                }
            }
        };
        if raw.len() == n {
            raw
        } else {
            vec![1.0 / n as f64; n]
        }
    }
}

fn strategy_hash(trainer: &EsMccfrTrainer<LeducGame>, policy: FallbackPolicy) -> String {
    let probes = collect_strategy_probes();
    let strategy = make_strategy_fn(trainer, policy);
    let mut hasher = Hasher::new();
    hasher.update(&trainer.update_count().to_le_bytes());
    hasher.update(&(probes.len() as u64).to_le_bytes());
    for (info, actions) in probes {
        let probs = strategy(&info, actions.len());
        hasher.update(&[info.actor]);
        hasher.update(&[info.private_card]);
        hasher.update(&[info.public_card.unwrap_or(0xFF)]);
        hasher.update(&(probs.len() as u32).to_le_bytes());
        for p in probs {
            hasher.update(&p.to_le_bytes());
        }
    }
    hex32(&hasher.finalize().into())
}

/// Leduc tree 较小，直接 DFS 一遍枚举所有可达 `(infoset, legal_actions)`
/// 作为 strategy_blake3 的 probe 集合（与 leduc_es_mccfr_report 一致）。
fn collect_strategy_probes() -> Vec<(LeducInfoSet, Vec<LeducAction>)> {
    use std::collections::HashMap;
    use std::collections::HashSet;
    let mut rng = ChaCha20Rng::from_seed(0xCAFE_F00D_DEAD_BEEF);
    let root = LeducGame.root(&mut rng);
    let mut seen: HashSet<LeducInfoSet> = HashSet::new();
    let mut infos: HashMap<LeducInfoSet, Vec<LeducAction>> = HashMap::new();
    collect_dfs(&root, &mut seen, &mut infos, &mut rng);
    let mut out: Vec<_> = infos.into_iter().collect();
    out.sort_by_key(|(i, _)| {
        (
            i.actor,
            i.private_card,
            i.public_card.unwrap_or(0xFF),
            i.preflop_history.len(),
            i.history.len(),
        )
    });
    out
}

fn collect_dfs(
    state: &LeducState,
    seen: &mut std::collections::HashSet<LeducInfoSet>,
    infos: &mut std::collections::HashMap<LeducInfoSet, Vec<LeducAction>>,
    rng: &mut dyn RngSource,
) {
    match LeducGame::current(state) {
        NodeKind::Terminal => {}
        NodeKind::Chance => {
            for (action, _p) in LeducGame::chance_distribution(state) {
                let next = LeducGame::next(state.clone(), action, rng);
                collect_dfs(&next, seen, infos, rng);
            }
        }
        NodeKind::Player(actor) => {
            let info = LeducGame::info_set(state, actor);
            if seen.insert(info.clone()) {
                infos.insert(info.clone(), LeducGame::legal_actions(state).to_vec());
            }
            for action in LeducGame::legal_actions(state) {
                let next = LeducGame::next(state.clone(), action, rng);
                collect_dfs(&next, seen, infos, rng);
            }
        }
    }
}

fn write_reports(path: &PathBuf, report: &CalibrationReport) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create report dir failed: {e}"))?;
    }
    let json_path = path.with_extension("json");
    let json = serde_json::to_string_pretty(report)
        .map_err(|e| format!("serialize JSON report failed: {e}"))?;
    fs::write(&json_path, json)
        .map_err(|e| format!("write {} failed: {e}", json_path.display()))?;
    fs::write(path, markdown_report(report))
        .map_err(|e| format!("write {} failed: {e}", path.display()))?;
    Ok(())
}

fn markdown_report(r: &CalibrationReport) -> String {
    let mut out = String::new();
    out.push_str("# Leduc LBR Proxy Calibration\n\n");
    out.push_str(&format!("- seed: `0x{:016x}`\n", r.seed));
    out.push_str(&format!("- updates: `{}`\n", r.updates));
    out.push_str(&format!("- lbr_seed: `0x{:016x}`\n", r.lbr_seed));
    out.push_str(&format!("- lbr_probes: `{}`\n", r.lbr_probes));
    out.push_str(&format!("- lbr_rollouts: `{}`\n", r.lbr_rollouts));
    out.push_str(&format!(
        "- fallback_policy: `{}`\n",
        r.fallback_policy.slug()
    ));
    out.push_str(&format!(
        "- final_strategy_blake3: `{}`\n\n",
        r.strategy_blake3
    ));
    out.push_str("## Calibration Curve\n\n");
    out.push_str(
        "`exact_exploitability` 走 `exploitability::<LeducGame, LeducBestResponse>` 全树 PI BR；\n",
    );
    out.push_str("`lbr_mean_chips` 走 game-generic `estimate_lbr` 同 NLHE proxy 路径，每 probe 走 blueprint self-play 到 target 决策点后枚举 legal actions 取 rollout EV max。\n\n");
    out.push_str(
        "| label | updates | exact_exploit | lbr_mean | lbr_SE | probes | strategy_blake3 |\n",
    );
    out.push_str("|---|---:|---:|---:|---:|---:|---|\n");
    for p in &r.points {
        out.push_str(&format!(
            "| {} | {} | {:.6} | {:.6} | {:.6} | {}/{} | `{}` |\n",
            p.label,
            p.update_count,
            p.exact_exploitability,
            p.lbr_mean_chips,
            p.lbr_standard_error_chips,
            p.lbr_probes_used,
            p.lbr_probes_requested,
            short_hash(&p.strategy_blake3)
        ));
    }
    out
}

fn short_hash(h: &str) -> String {
    if h.len() > 12 {
        format!("{}…", &h[..12])
    } else {
        h.to_string()
    }
}

fn hex32(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn parse_args() -> Result<Args, String> {
    let mut out = Args::default();
    let mut overrode_curve = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--updates" => out.updates = parse_u64(&next_value(&mut args, &arg)?)?,
            "--seed" => out.seed = parse_u64(&next_value(&mut args, &arg)?)?,
            "--curve-update" => {
                if !overrode_curve {
                    out.curve_updates.clear();
                    overrode_curve = true;
                }
                let raw = next_value(&mut args, &arg)?;
                for s in raw.split(',') {
                    out.curve_updates.push(parse_u64(s)?);
                }
            }
            "--lbr-probes" => out.lbr_probes = parse_u64(&next_value(&mut args, &arg)?)?,
            "--lbr-rollouts" => out.lbr_rollouts = parse_u64(&next_value(&mut args, &arg)?)?,
            "--lbr-seed" => out.lbr_seed = parse_u64(&next_value(&mut args, &arg)?)?,
            "--output" => out.output = PathBuf::from(next_value(&mut args, &arg)?),
            "--fallback-policy" => {
                out.fallback_policy = FallbackPolicy::from_str(&next_value(&mut args, &arg)?)?
            }
            "--no-uniform-baseline" => out.include_uniform_baseline = false,
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(out)
}

fn next_value(args: &mut impl Iterator<Item = String>, name: &str) -> Result<String, String> {
    args.next()
        .ok_or_else(|| format!("{name} requires a value"))
}

fn parse_u64(raw: &str) -> Result<u64, String> {
    let trimmed = raw.trim();
    if let Some(hex) = trimmed.strip_prefix("0x") {
        u64::from_str_radix(hex, 16).map_err(|e| format!("invalid hex integer {raw}: {e}"))
    } else {
        trimmed
            .parse::<u64>()
            .map_err(|e| format!("invalid integer {raw}: {e}"))
    }
}

fn print_usage() {
    eprintln!(
        "usage: cargo run --release --bin leduc_lbr_report -- \\\n\
         \t--updates N --curve-update 10000,100000,1000000\n\n\
         options:\n\
         \t--updates <N>                 default 1_000_000 (training upper bound)\n\
         \t--seed <N|0xHEX>              ES-MCCFR training seed\n\
         \t--curve-update <N[,N,...]>   ascending milestones for the proxy / exploit curve\n\
         \t--lbr-probes <N>              default 4000\n\
         \t--lbr-rollouts <N>            default 32\n\
         \t--lbr-seed <N|0xHEX>          LBR sampling seed\n\
         \t--fallback-policy <m>         average|current|hybrid (default hybrid)\n\
         \t--no-uniform-baseline         skip the update=0 uniform-policy reference row\n\
         \t--output <path>               default artifacts/leduc_lbr_calibration.md"
    );
}
