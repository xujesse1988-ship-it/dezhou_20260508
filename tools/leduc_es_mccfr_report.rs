use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use blake3::Hasher;
use poker::training::game::{Game, NodeKind, PlayerId};
use poker::training::leduc::{LeducAction, LeducGame, LeducInfoSet, LeducState, LeducStreet};
use poker::training::{exploitability, EsMccfrTrainer, LeducBestResponse, Trainer};
use poker::{ChaCha20Rng, RngSource};

const DEFAULT_UPDATES: u64 = 100_000_000;
const DEFAULT_SEED: u64 = 0x4c_45_44_55_43_5f_45_53; // "LEDUC_ES"

#[derive(Debug)]
struct Args {
    updates: u64,
    seed: u64,
    report_every: u64,
    output: PathBuf,
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(e) => {
            eprintln!("[leduc_es_mccfr_report] argument error: {e}");
            eprintln!(
                "usage: cargo run --release --bin leduc_es_mccfr_report -- \
                 [--updates N] [--seed N] [--report-every N] [--output PATH]"
            );
            return ExitCode::from(2);
        }
    };

    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("[leduc_es_mccfr_report] failed: {e}");
            ExitCode::from(1)
        }
    }
}

fn parse_args() -> Result<Args, String> {
    let mut updates = DEFAULT_UPDATES;
    let mut seed = DEFAULT_SEED;
    let mut report_every = 10_000_000;
    let mut output = PathBuf::from("artifacts/leduc_es_mccfr_100m_strategy.txt");

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--updates" => {
                updates = parse_u64(&args.next().ok_or("--updates requires a value")?)?;
            }
            "--seed" => {
                seed = parse_u64(&args.next().ok_or("--seed requires a value")?)?;
            }
            "--report-every" => {
                report_every = parse_u64(&args.next().ok_or("--report-every requires a value")?)?;
            }
            "--output" => {
                output = PathBuf::from(args.next().ok_or("--output requires a value")?);
            }
            "--help" | "-h" => {
                println!(
                    "usage: cargo run --release --bin leduc_es_mccfr_report -- \
                     [--updates N] [--seed N] [--report-every N] [--output PATH]"
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    Ok(Args {
        updates,
        seed,
        report_every,
        output,
    })
}

fn parse_u64(s: &str) -> Result<u64, String> {
    if let Some(hex) = s.strip_prefix("0x") {
        u64::from_str_radix(hex, 16).map_err(|e| format!("invalid hex u64 `{s}`: {e}"))
    } else {
        s.parse::<u64>()
            .map_err(|e| format!("invalid u64 `{s}`: {e}"))
    }
}

fn run(args: Args) -> Result<(), String> {
    if let Some(parent) = args.output.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("create output directory `{}` failed: {e}", parent.display()))?;
    }

    let mut trainer = EsMccfrTrainer::new(LeducGame, args.seed);
    let mut rng = ChaCha20Rng::from_seed(args.seed);
    let started = Instant::now();
    let report_every = args.report_every.max(1);

    eprintln!(
        "[leduc_es_mccfr_report] updates={} seed=0x{:016x}",
        args.updates, args.seed
    );

    for i in 0..args.updates {
        trainer
            .step(&mut rng)
            .map_err(|e| format!("step #{i} failed: {e:?}"))?;
        let done = trainer.update_count();
        if done == args.updates || done % report_every == 0 {
            let elapsed = started.elapsed().as_secs_f64();
            let throughput = done as f64 / elapsed.max(1e-9);
            eprintln!(
                "[leduc_es_mccfr_report] update {done} / {} elapsed={elapsed:.1}s throughput={throughput:.0}/s",
                args.updates
            );
        }
    }

    let infos = enumerate_reachable_infosets();
    let strategy = |info: &LeducInfoSet, n: usize| strategy_or_uniform(&trainer, info, n);
    let exploit = exploitability::<LeducGame, LeducBestResponse>(&LeducGame, &strategy);
    let ev0 = expected_value(&strategy, 0);
    let ev1 = expected_value(&strategy, 1);
    let hash = strategy_hash(&trainer, &infos);

    let file = File::create(&args.output)
        .map_err(|e| format!("create output `{}` failed: {e}", args.output.display()))?;
    let mut out = BufWriter::new(file);

    writeln!(out, "Leduc ES-MCCFR strategy report").map_err(write_err)?;
    writeln!(out, "updates: {}", trainer.update_count()).map_err(write_err)?;
    writeln!(out, "seed: 0x{:016x}", args.seed).map_err(write_err)?;
    writeln!(out, "wall_seconds: {:.3}", started.elapsed().as_secs_f64()).map_err(write_err)?;
    writeln!(
        out,
        "throughput_updates_per_second: {:.0}",
        trainer.update_count() as f64 / started.elapsed().as_secs_f64().max(1e-9)
    )
    .map_err(write_err)?;
    writeln!(out, "reachable_infosets: {}", infos.len()).map_err(write_err)?;
    writeln!(out, "average_strategy_blake3: {}", hex32(&hash)).map_err(write_err)?;
    writeln!(out, "exploitability_chips_per_game: {exploit:.9}").map_err(write_err)?;
    writeln!(out, "ev_p0: {ev0:.9}").map_err(write_err)?;
    writeln!(out, "ev_p1: {ev1:.9}").map_err(write_err)?;
    writeln!(out, "ev_sum: {:.9}", ev0 + ev1).map_err(write_err)?;
    writeln!(out).map_err(write_err)?;
    writeln!(
        out,
        "format: actor private public street preflop_history/current_history | actions = probabilities"
    )
    .map_err(write_err)?;

    for (info, actions) in &infos {
        let probs = strategy_or_uniform(&trainer, info, actions.len());
        writeln!(
            out,
            "P{} {} {} {:?} {} | {}",
            info.actor,
            card_label(info.private_card),
            info.public_card.map(card_label).unwrap_or("-".to_string()),
            info.street,
            full_history_label(info),
            action_probs_label(actions, &probs)
        )
        .map_err(write_err)?;
    }
    out.flush().map_err(write_err)?;

    eprintln!(
        "[leduc_es_mccfr_report] wrote {} (exploitability={exploit:.9}, hash={})",
        args.output.display(),
        hex32(&hash)
    );
    Ok(())
}

fn write_err(e: std::io::Error) -> String {
    e.to_string()
}

fn strategy_or_uniform(
    trainer: &EsMccfrTrainer<LeducGame>,
    info: &LeducInfoSet,
    n: usize,
) -> Vec<f64> {
    let strategy = trainer.average_strategy(info);
    if strategy.len() == n {
        strategy
    } else {
        vec![1.0 / n as f64; n]
    }
}

fn enumerate_reachable_infosets() -> Vec<(LeducInfoSet, Vec<LeducAction>)> {
    let mut rng = ChaCha20Rng::from_seed(0xCAFE_F00D_DEAD_BEEF);
    let root = LeducGame.root(&mut rng);
    let mut seen = HashSet::new();
    let mut infos = HashMap::new();
    collect_infos(&root, &mut seen, &mut infos, &mut rng);
    let mut out: Vec<_> = infos.into_iter().collect();
    out.sort_by(|a, b| info_sort_key(&a.0).cmp(&info_sort_key(&b.0)));
    out
}

fn collect_infos(
    state: &LeducState,
    seen: &mut HashSet<LeducInfoSet>,
    infos: &mut HashMap<LeducInfoSet, Vec<LeducAction>>,
    rng: &mut dyn RngSource,
) {
    match LeducGame::current(state) {
        NodeKind::Terminal => {}
        NodeKind::Chance => {
            for (action, _p) in LeducGame::chance_distribution(state) {
                let next = LeducGame::next(state.clone(), action, rng);
                collect_infos(&next, seen, infos, rng);
            }
        }
        NodeKind::Player(actor) => {
            let info = LeducGame::info_set(state, actor);
            if seen.insert(info.clone()) {
                infos.insert(info.clone(), LeducGame::legal_actions(state));
            }
            for action in LeducGame::legal_actions(state) {
                let next = LeducGame::next(state.clone(), action, rng);
                collect_infos(&next, seen, infos, rng);
            }
        }
    }
}

fn info_sort_key(info: &LeducInfoSet) -> (u8, u8, u8, u8, Vec<u8>) {
    (
        info.actor,
        street_key(info.street),
        info.private_card,
        info.public_card.unwrap_or(0xFF),
        history_sort_bytes(info),
    )
}

fn history_sort_bytes(info: &LeducInfoSet) -> Vec<u8> {
    let mut out: Vec<u8> = info.preflop_history.iter().map(|a| *a as u8).collect();
    out.push(0xFF);
    out.extend(info.history.iter().map(|a| *a as u8));
    out
}

fn street_key(street: LeducStreet) -> u8 {
    match street {
        LeducStreet::Preflop => 0,
        LeducStreet::Postflop => 1,
    }
}

fn expected_value(strategy: &dyn Fn(&LeducInfoSet, usize) -> Vec<f64>, player: PlayerId) -> f64 {
    let mut rng = ChaCha20Rng::from_seed(0);
    let root = LeducGame.root(&mut rng);
    ev_recurse(&root, strategy, player, &mut rng)
}

fn ev_recurse(
    state: &LeducState,
    strategy: &dyn Fn(&LeducInfoSet, usize) -> Vec<f64>,
    player: PlayerId,
    rng: &mut dyn RngSource,
) -> f64 {
    match LeducGame::current(state) {
        NodeKind::Terminal => LeducGame::payoff(state, player),
        NodeKind::Chance => LeducGame::chance_distribution(state)
            .into_iter()
            .map(|(action, p)| {
                let next = LeducGame::next(state.clone(), action, rng);
                p * ev_recurse(&next, strategy, player, rng)
            })
            .sum(),
        NodeKind::Player(actor) => {
            let info = LeducGame::info_set(state, actor);
            let actions = LeducGame::legal_actions(state);
            let probs = strategy(&info, actions.len());
            actions
                .into_iter()
                .zip(probs)
                .map(|(action, p)| {
                    let next = LeducGame::next(state.clone(), action, rng);
                    p * ev_recurse(&next, strategy, player, rng)
                })
                .sum()
        }
    }
}

fn strategy_hash(
    trainer: &EsMccfrTrainer<LeducGame>,
    infos: &[(LeducInfoSet, Vec<LeducAction>)],
) -> [u8; 32] {
    let mut hasher = Hasher::new();
    hasher.update(&trainer.update_count().to_le_bytes());
    hasher.update(&(infos.len() as u64).to_le_bytes());
    for (info, actions) in infos {
        hasher.update(&[info.actor]);
        hasher.update(&[info.private_card]);
        hasher.update(&[info.public_card.unwrap_or(0xFF)]);
        hasher.update(&[street_key(info.street)]);
        hasher.update(&(info.preflop_history.len() as u32).to_le_bytes());
        for action in &info.preflop_history {
            hasher.update(&[*action as u8]);
        }
        hasher.update(&(info.history.len() as u32).to_le_bytes());
        for action in &info.history {
            hasher.update(&[*action as u8]);
        }
        hasher.update(&(actions.len() as u32).to_le_bytes());
        for action in actions {
            hasher.update(&[*action as u8]);
        }
        let probs = strategy_or_uniform(trainer, info, actions.len());
        hasher.update(&(probs.len() as u32).to_le_bytes());
        for p in probs {
            hasher.update(&p.to_le_bytes());
        }
    }
    hasher.finalize().into()
}

fn action_probs_label(actions: &[LeducAction], probs: &[f64]) -> String {
    actions
        .iter()
        .zip(probs)
        .map(|(a, p)| format!("{}={p:.6}", action_label(*a)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn history_label(history: &[LeducAction]) -> String {
    if history.is_empty() {
        return "-".to_string();
    }
    history
        .iter()
        .map(|a| action_label(*a))
        .collect::<Vec<_>>()
        .join("/")
}

fn full_history_label(info: &LeducInfoSet) -> String {
    match info.street {
        LeducStreet::Preflop => history_label(&info.history),
        LeducStreet::Postflop => format!(
            "{}/{}",
            history_label(&info.preflop_history),
            history_label(&info.history)
        ),
    }
}

fn action_label(action: LeducAction) -> &'static str {
    match action {
        LeducAction::Check => "check",
        LeducAction::Bet => "bet",
        LeducAction::Call => "call",
        LeducAction::Fold => "fold",
        LeducAction::Raise => "raise",
        LeducAction::Deal0
        | LeducAction::Deal1
        | LeducAction::Deal2
        | LeducAction::Deal3
        | LeducAction::Deal4
        | LeducAction::Deal5 => "deal",
    }
}

fn card_label(card: u8) -> String {
    if (11..=13).contains(&card) {
        return match card {
            11 => "J".to_string(),
            12 => "Q".to_string(),
            13 => "K".to_string(),
            _ => unreachable!(),
        };
    }

    let rank = match card / 2 {
        0 => "J",
        1 => "Q",
        2 => "K",
        _ => "?",
    };
    format!("{rank}{}", card % 2)
}

fn hex32(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
