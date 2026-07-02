//! 启发式 9-max OpenPoker 常驻 advisor（IPC drop-in）。
//!
//! 协议（与 `openpoker_advisor` 兼容）：启动打印一行 `{"ready":true}`；之后每行 stdin =
//! 一个决策请求（[`poker::heuristic::Request`]），回一行 [`poker::heuristic::Response`]。
//! 信封 `{"observe":true,...}` = 每手结束的对手观测（喂画像，回 ack）；`{"prewarm":true,...}`
//! = 预热（启发式无 solve 缓存，直接 ack）。**永不 panic**：解析失败回安全 fold。
//!
//! crate 零网络依赖：网络/token/重连全在 Python driver（`tools/heuristic9_play.py`）。
//!
//! 用法：
//! ```text
//! heuristic9_advisor [--config cfg.json] [--opp-profile prof.json] \
//!                    [--samples N] [--seed S] [--debug]
//! ```
//! 为与现有 6-max driver drop-in，未知 `--flag [val]` 一律忽略。

use std::io::{BufRead, Write};
use std::process::ExitCode;

use serde::Deserialize;

use poker::heuristic::config::HeuristicConfig;
use poker::heuristic::opponent::{ObservedAction, OpponentTable};
use poker::heuristic::{HeuristicBot, Request, Response};

/// observe 信封（driver 每手结束发）。`actions[].street`：0=preflop。
#[derive(Deserialize, Default)]
struct ObserveRecord {
    #[serde(default)]
    button_seat: u8,
    #[serde(default)]
    num_seats: u8,
    #[serde(default)]
    names: std::collections::BTreeMap<u8, String>,
    #[serde(default)]
    actions: Vec<ObserveAction>,
}

#[derive(Deserialize)]
struct ObserveAction {
    seat: u8,
    #[serde(default)]
    street: u8,
    action: String,
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(default)]
    observe: bool,
    #[serde(default)]
    prewarm: bool,
}

struct Args {
    config: Option<String>,
    opp_profile: Option<String>,
    samples: Option<u32>,
    seed: Option<u64>,
    debug: bool,
    no_ml_range: bool,
}

fn parse_args() -> Args {
    let mut a = Args {
        config: None,
        opp_profile: None,
        samples: None,
        seed: None,
        debug: false,
        no_ml_range: false,
    };
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--config" => {
                a.config = argv.get(i + 1).cloned();
                i += 2;
            }
            "--opp-profile" => {
                a.opp_profile = argv.get(i + 1).cloned();
                i += 2;
            }
            "--samples" => {
                a.samples = argv.get(i + 1).and_then(|s| s.parse().ok());
                i += 2;
            }
            "--seed" => {
                a.seed = argv.get(i + 1).and_then(|s| s.parse().ok());
                i += 2;
            }
            "--debug" => {
                a.debug = true;
                i += 1;
            }
            // live 默认开 ML range；此 flag 退回标量路径（A/B 或应急兜底用）。
            "--no-ml-range" => {
                a.no_ml_range = true;
                i += 1;
            }
            // drop-in 兼容：未知 flag 跳过（带值的也跳过其值）。
            other if other.starts_with("--") => {
                let has_val = argv
                    .get(i + 1)
                    .map(|v| !v.starts_with("--"))
                    .unwrap_or(false);
                i += if has_val { 2 } else { 1 };
            }
            _ => i += 1,
        }
    }
    a
}

fn build_config(args: &Args) -> HeuristicConfig {
    let mut cfg = match &args.config {
        Some(path) => match std::fs::read_to_string(path) {
            Ok(s) => HeuristicConfig::from_json_str(&s).unwrap_or_else(|e| {
                eprintln!("[heuristic9] config 解析失败（{e}），用默认。");
                HeuristicConfig::default()
            }),
            Err(e) => {
                eprintln!("[heuristic9] config 读取失败（{e}），用默认。");
                HeuristicConfig::default()
            }
        },
        None => HeuristicConfig::default(),
    };
    if let Some(s) = args.samples {
        cfg.equity.samples = s;
    }
    if let Some(s) = args.seed {
        cfg.equity.seed = s;
        cfg.mixing.seed ^= s;
    }
    // live advisor 默认开 ML range（对手 169 类 range 预测喂加权 equity）；
    // `--no-ml-range` 退回标量阈值路径。缺 artifacts 时 HeuristicBot::new 优雅回落。
    cfg.range.use_ml_range = !args.no_ml_range;
    cfg
}

/// 把 observe 记录转成对手画像更新。
fn feed_observe(bot: &mut HeuristicBot, rec: &ObserveRecord) {
    let n = rec.num_seats.max(2);
    let btn = rec.button_seat % n;
    let (sb_seat, bb_seat) = if n == 2 {
        (btn, (btn + 1) % n)
    } else {
        ((btn + 1) % n, (btn + 2) % n)
    };
    let names_in_hand: Vec<String> = rec.names.values().cloned().collect();
    // 收集（name, street, action, forced_blind）。
    let mut owned: Vec<(String, u8, String, bool)> = Vec::new();
    for a in &rec.actions {
        if let Some(name) = rec.names.get(&a.seat) {
            let forced = a.street == 0 && (a.seat == sb_seat || a.seat == bb_seat);
            owned.push((
                name.clone(),
                a.street,
                a.action.to_ascii_lowercase(),
                forced,
            ));
        }
    }
    let observed: Vec<ObservedAction> = owned
        .iter()
        .map(|(nm, st, act, fb)| ObservedAction {
            name: nm,
            street: *st,
            action: act,
            forced_blind: *fb,
        })
        .collect();
    bot.observe_hand(&observed, &names_in_hand);
}

fn run() -> Result<(), String> {
    let args = parse_args();
    let want_ml = !args.no_ml_range;
    let cfg = build_config(&args);
    let mut bot = HeuristicBot::new(cfg);
    // 明确 ML range 状态：想开但没载入（缺 artifacts）→ 静默退化成标量，须让 live 操作者看见。
    match (want_ml, bot.ml_range.is_some()) {
        (true, true) => eprintln!("[heuristic9] ML range: ON（对手 range 预测已载入）"),
        (true, false) => {
            eprintln!("[heuristic9] ⚠ ML range 想开但未载入（artifacts 缺失？）→ 退回标量 equity")
        }
        (false, _) => eprintln!("[heuristic9] ML range: OFF（--no-ml-range）"),
    }

    // 加载持久对手画像（累积经验）。
    if let Some(path) = &args.opp_profile {
        if let Ok(s) = std::fs::read_to_string(path) {
            bot.opponents = OpponentTable::from_json_str(&s);
            eprintln!(
                "[heuristic9] 载入对手画像 {path}（{} 名）",
                bot.opponents.by_name.len()
            );
        }
    }

    let mut stdout = std::io::stdout();
    writeln!(
        stdout,
        "{}",
        serde_json::json!({"ready": true, "engine": "heuristic9", "samples": bot.config.equity.samples})
    )
    .map_err(|e| e.to_string())?;
    stdout.flush().map_err(|e| e.to_string())?;

    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let env = serde_json::from_str::<Envelope>(&line).ok();
        let is_observe = env.as_ref().map(|e| e.observe).unwrap_or(false);
        let is_prewarm = env.as_ref().map(|e| e.prewarm).unwrap_or(false);

        let resp: Response = if is_observe {
            if let Ok(rec) = serde_json::from_str::<ObserveRecord>(&line) {
                feed_observe(&mut bot, &rec);
            }
            Response {
                action: "none".into(),
                source: "observe:ack".into(),
                ..Default::default()
            }
        } else if is_prewarm {
            Response {
                action: "none".into(),
                source: "prewarm:ack".into(),
                ..Default::default()
            }
        } else {
            match serde_json::from_str::<Request>(&line) {
                Ok(req) => {
                    let r = bot.respond(&req);
                    if args.debug {
                        eprintln!(
                            "[heuristic9] seat{} {} board{} -> {} {} (eq={:?}) [{}]",
                            req.my_seat,
                            req.hole.join(""),
                            req.board.len(),
                            r.action,
                            r.amount.map(|a| a.to_string()).unwrap_or_default(),
                            r.equity,
                            r.source,
                        );
                    }
                    r
                }
                Err(e) => Response {
                    action: "fold".into(),
                    source: format!("fallback:bad_request_json:{e}"),
                    ..Default::default()
                },
            }
        };
        writeln!(
            stdout,
            "{}",
            serde_json::to_string(&resp).map_err(|e| e.to_string())?
        )
        .map_err(|e| e.to_string())?;
        stdout.flush().map_err(|e| e.to_string())?;
    }

    // 退出保存画像。
    if let Some(path) = &args.opp_profile {
        let _ = std::fs::write(path, bot.opponents.to_json_string());
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("[heuristic9_advisor] fatal: {e}");
            ExitCode::FAILURE
        }
    }
}
