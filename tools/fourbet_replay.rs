//! PM-17 识别器离线回放（`docs/temp/pm17_preflop_4bet_exploit_design_2026_10_22.md` §8 第 8 条）。
//!
//! 读 poker_manager 的 hh JSONL（driver `--hh-log`，每行一手、`result.actions` 带名字和金额），
//! 每个文件新建一个 [`FourBetTracker`]（= 每个进程从零统计），逐手：先对「我方开局、恰好一人非全下
//! 3bet、其余都弃、轮到我方」的点判门，再把整手喂进识别器。每个点输出一行 JSON：
//! `{"hand_id","opp","open","p3_tail","f_tail","fstar","p3_mean","f_mean","opp3","faced4"}`，与
//! `artifacts/pm17_replay_2026_10_22/replay_v1.py --dump` 逐条比对（后四项不依赖 fstar，可直接对实盘
//! `resp.fourbet` 遥测）。4bet 额按回放口径取 3bet 额的 2.8 倍（advisor 实盘用 raise1pot 的真实出牌额）。
//!
//! 用法：`fourbet_replay [--load-state <in.json>] [--save-state <out.json>] <hh.jsonl>...`
//! - 不带 `--*-state`：多个文件按给出顺序处理，各自从零统计。
//! - 带任一 `--*-state`：所有文件串成一个识别器（= 跨进程续用）；`--load-state` 从已有状态接着喂
//!   （文件必须存在），`--save-state` 在喂完后写出状态文件（advisor `--exploit-4bet-state` 同一格式，
//!   可用历史牌局给实盘生成初始状态）。

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;

use poker::training::fourbet_exploit::{FourBetConfig, FourBetTracker};
use poker::training::opponent_profile::{ObsAction, ObserveHand};
use serde::Deserialize;

const S_MULT: f64 = 2.8;

#[derive(Deserialize)]
struct HhAction {
    seat: u8,
    name: String,
    action: String,
    street: String,
    #[serde(default)]
    contribution_delta: Option<u64>,
    #[serde(default)]
    to_call_before: Option<u64>,
    #[serde(default)]
    pot_before: Option<u64>,
    #[serde(default)]
    stack_before: Option<u64>,
}

#[derive(Deserialize)]
struct HhResult {
    actions: Vec<HhAction>,
}

#[derive(Deserialize)]
struct HhLine {
    hand_id: String,
    my_seat: u8,
    initial_stacks: BTreeMap<String, u64>,
    result: HhResult,
}

fn aggressive(a: &HhAction) -> bool {
    matches!(a.action.as_str(), "raise" | "all_in" | "bet")
        && a.contribution_delta.unwrap_or(0) > a.to_call_before.unwrap_or(0)
}

/// 返回 (3bet 者名字, 3bet 者座位, 我方面对 3bet 的那个动作)。
fn spot(x: &HhLine) -> Option<(String, u8, &HhAction)> {
    let me = x.my_seat;
    let mut opened = false;
    let mut three: Option<(String, u8)> = None;
    for a in x.result.actions.iter().filter(|a| a.street == "preflop") {
        if !opened {
            if a.seat == me {
                if !aggressive(a) {
                    return None;
                }
                opened = true;
            } else if a.action != "fold" {
                return None;
            }
            continue;
        }
        if a.seat == me {
            let (name, seat) = three?;
            return Some((name, seat, a));
        }
        if aggressive(a) && three.is_none() && a.action != "all_in" {
            three = Some((a.name.clone(), a.seat));
        } else if a.action != "fold" {
            return None;
        }
    }
    None
}

fn observe(x: &HhLine) -> ObserveHand {
    let mut names = BTreeMap::new();
    for a in &x.result.actions {
        names.insert(a.seat, a.name.clone());
    }
    ObserveHand {
        names,
        actions: x
            .result
            .actions
            .iter()
            .map(|a| ObsAction {
                seat: a.seat,
                street: a.street.clone(),
                action: a.action.clone(),
                contribution_delta: a.contribution_delta,
                to_call_before: a.to_call_before,
            })
            .collect(),
    }
}

const USAGE: &str =
    "用法：fourbet_replay [--load-state <in.json>] [--save-state <out.json>] <hh.jsonl>...";

fn main() -> Result<(), String> {
    let mut files: Vec<String> = Vec::new();
    let mut load: Option<PathBuf> = None;
    let mut save: Option<PathBuf> = None;
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--load-state" | "--save-state" => {
                let v = PathBuf::from(it.next().ok_or_else(|| format!("{arg} 缺值；{USAGE}"))?);
                if arg == "--load-state" {
                    load = Some(v);
                } else {
                    save = Some(v);
                }
            }
            a if a.starts_with("--") => return Err(format!("unknown arg {a}；{USAGE}")),
            _ => files.push(arg),
        }
    }
    if files.is_empty() {
        return Err(USAGE.into());
    }
    let cfg = FourBetConfig::default();
    let chained = load.is_some() || save.is_some();
    let mut tracker = match &load {
        Some(p) => {
            let (t, warnings) = FourBetTracker::load_state(cfg.clone(), p)?
                .ok_or_else(|| format!("{}: 不存在", p.display()))?;
            for w in warnings {
                eprintln!("[fourbet_replay] 告警：{w}");
            }
            t
        }
        None => FourBetTracker::new(cfg.clone()),
    };
    let mut out = std::io::stdout().lock();
    for f in &files {
        if !chained {
            tracker = FourBetTracker::new(cfg.clone());
        }
        let file = std::fs::File::open(f).map_err(|e| format!("{f}: {e}"))?;
        for line in std::io::BufReader::new(file).lines() {
            let line = line.map_err(|e| e.to_string())?;
            let x: HhLine = serde_json::from_str(&line).map_err(|e| format!("{f}: {e}"))?;
            if let Some((name, seat, a)) = spot(&x) {
                let stack0 = |s: u8| x.initial_stacks.get(&s.to_string()).copied().unwrap_or(0);
                let hero_start = stack0(x.my_seat);
                let hero_in = hero_start - a.stack_before.unwrap_or(hero_start);
                let t = (hero_in + a.to_call_before.unwrap_or(0)) as f64;
                let s = S_MULT * t;
                let risk = s - hero_in as f64;
                let pot = a.pot_before.unwrap_or(0) as f64;
                let fstar = risk / (risk + pot);
                let deep = hero_start as f64 >= cfg.min_depth_mult * s
                    && stack0(seat) as f64 >= cfg.min_depth_mult * s;
                if deep {
                    let g = tracker.gate(&name, fstar);
                    writeln!(
                        out,
                        "{}",
                        serde_json::json!({
                            "hand_id": x.hand_id, "opp": name, "open": g.open,
                            "p3_tail": g.p3_tail, "f_tail": g.f_tail, "fstar": fstar,
                            "p3_mean": g.p3_mean, "f_mean": g.f_mean, "opp3": g.opp3,
                            "faced4": g.faced4,
                        })
                    )
                    .map_err(|e| e.to_string())?;
                }
            }
            tracker.observe_hand(&observe(&x));
        }
    }
    if let Some(p) = &save {
        tracker.save_state(p)?;
        eprintln!(
            "[fourbet_replay] 状态写到 {}：累计 {} 手、{} 名玩家",
            p.display(),
            tracker.hands(),
            tracker.players()
        );
    }
    Ok(())
}
