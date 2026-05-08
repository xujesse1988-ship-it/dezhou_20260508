//! C1：跨语言反序列化（Python 读取 Rust 写出的 hand history）。
//!
//! `pluribus_stage1_validation.md` §5：
//!
//! - Rust/C++ 写出的 hand history 必须能被 Python 评测脚本完整读取并验证；
//!   至少 10,000 手牌跨语言回放结果一致。
//!
//! 实现：
//!
//! - Rust 端 [`HandHistory::to_proto`] → base64 → 批量传给 `tools/history_reader.py`；
//!   - Python 端 minimal proto3 decoder（无需 protoc / google.protobuf）解码
//!     字段并产出 JSON 视图；
//!   - Rust 端比对 Python JSON 与"自己写出的 expected JSON"；任何不一致即分歧。
//!
//! 默认 100 手；`--ignored` 提供 10k。
//!
//! Python 子进程不存在 / 失败 → 测试报告 `skipped`，不计为分歧。

mod common;

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use base64::Engine;
use poker::{
    Action, ChaCha20Rng, ChipAmount, GameState, HandHistory, LegalActionSet, RngSource, TableConfig,
};
use serde_json::{json, Value};

use common::{expected_total_chips, Invariants};

// ============================================================================
// 共享：随机一手 + 编码 expected JSON
// ============================================================================

fn play_random_hand(seed: u64) -> Result<HandHistory, String> {
    let cfg = TableConfig::default_6max_100bb();
    let total = expected_total_chips(&cfg);
    let mut state = GameState::new(&cfg, seed);
    let mut rng = ChaCha20Rng::from_seed(seed.wrapping_add(0xC1_DD));
    Invariants::check_all(&state, total)?;
    for _ in 0..256 {
        if state.is_terminal() {
            break;
        }
        let la = state.legal_actions();
        let a = sample(&la, &mut rng).ok_or("no legal action")?;
        state.apply(a).map_err(|e| format!("apply: {e}"))?;
        Invariants::check_all(&state, total)?;
    }
    if !state.is_terminal() {
        return Err(format!("non-terminal after 256 actions, seed={seed}"));
    }
    Ok(state.hand_history().clone())
}

fn sample(la: &LegalActionSet, rng: &mut dyn RngSource) -> Option<Action> {
    let mut cands: Vec<Action> = Vec::with_capacity(6);
    if la.fold {
        cands.push(Action::Fold);
    }
    if la.check {
        cands.push(Action::Check);
    }
    if la.call.is_some() {
        cands.push(Action::Call);
    }
    if let Some((min, max)) = la.bet_range {
        cands.push(Action::Bet {
            to: range(min, max, rng),
        });
    }
    if let Some((min, max)) = la.raise_range {
        cands.push(Action::Raise {
            to: range(min, max, rng),
        });
    }
    if la.all_in_amount.is_some() {
        cands.push(Action::AllIn);
    }
    if cands.is_empty() {
        return None;
    }
    Some(cands[(rng.next_u64() as usize) % cands.len()])
}

fn range(min: ChipAmount, max: ChipAmount, rng: &mut dyn RngSource) -> ChipAmount {
    let lo = min.as_u64();
    let hi = max.as_u64();
    if lo >= hi {
        return min;
    }
    ChipAmount::new(lo + rng.next_u64() % (hi - lo + 1))
}

fn card_to_string(c: poker::Card) -> String {
    let ranks = [
        "2", "3", "4", "5", "6", "7", "8", "9", "T", "J", "Q", "K", "A",
    ];
    let suits = ["c", "d", "h", "s"];
    let v = c.to_u8();
    format!("{}{}", ranks[(v / 4) as usize], suits[(v % 4) as usize])
}

fn street_str(s: poker::Street) -> &'static str {
    match s {
        poker::Street::Preflop => "preflop",
        poker::Street::Flop => "flop",
        poker::Street::Turn => "turn",
        poker::Street::River => "river",
        poker::Street::Showdown => "showdown",
    }
}

fn action_kind_str(a: Action) -> &'static str {
    match a {
        Action::Fold => "fold",
        Action::Check => "check",
        Action::Call => "call",
        Action::Bet { .. } => "bet",
        Action::Raise { .. } => "raise",
        Action::AllIn => unreachable!("history never stores AllIn"),
    }
}

fn expected_view(h: &HandHistory) -> Value {
    json!({
        "schema_version": h.schema_version,
        "config": {
            "n_seats": h.config.n_seats,
            "starting_stacks": h.config.starting_stacks.iter().map(|c| c.as_u64()).collect::<Vec<_>>(),
            "small_blind": h.config.small_blind.as_u64(),
            "big_blind": h.config.big_blind.as_u64(),
            "ante": h.config.ante.as_u64(),
            "button_seat": h.config.button_seat.0,
        },
        "seed": h.seed,
        "actions": h.actions.iter().map(|a| {
            let to_field = match a.action {
                Action::Fold | Action::Check => 0,
                Action::Call => a.committed_after.as_u64(),
                Action::Bet { to } => to.as_u64(),
                Action::Raise { to } => to.as_u64(),
                Action::AllIn => unreachable!(),
            };
            json!({
                "seq": a.seq,
                "seat": a.seat.0,
                "street": street_str(a.street),
                "kind": action_kind_str(a.action),
                "to": to_field,
                "committed_after": a.committed_after.as_u64(),
            })
        }).collect::<Vec<_>>(),
        "board": h.board.iter().map(|c| card_to_string(*c)).collect::<Vec<_>>(),
        "hole_cards": h.hole_cards.iter().map(|hc| match hc {
            Some([a, b]) => json!([card_to_string(*a), card_to_string(*b)]),
            None => json!(null),
        }).collect::<Vec<_>>(),
        "final_payouts": h.final_payouts.iter().map(|(seat, net)| json!({
            "seat": seat.0,
            "net": *net,
        })).collect::<Vec<_>>(),
        "showdown_order": h.showdown_order.iter().map(|s| s.0).collect::<Vec<_>>(),
    })
}

// ============================================================================
// Subprocess driver
// ============================================================================

fn locate_python_helper() -> PathBuf {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into());
    PathBuf::from(manifest)
        .join("tools")
        .join("history_reader.py")
}

#[derive(Debug, Default)]
struct Report {
    matched: usize,
    diverged: usize,
    skipped: bool,
    first_diff: Option<(usize, String)>,
}

fn cross_lang_batch(samples: usize, base_seed: u64) -> Report {
    let mut report = Report::default();
    let mut histories: Vec<HandHistory> = Vec::with_capacity(samples);
    for s in 0..samples as u64 {
        let h = play_random_hand(base_seed.wrapping_add(s))
            .unwrap_or_else(|e| panic!("play_random_hand failed: {e}"));
        histories.push(h);
    }

    let blobs: Vec<String> = histories
        .iter()
        .map(|h| base64::engine::general_purpose::STANDARD.encode(h.to_proto()))
        .collect();

    let payload = json!({"blobs_b64": blobs});

    let script = locate_python_helper();
    let mut child = match Command::new("python3")
        .arg(&script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => {
            report.skipped = true;
            return report;
        }
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(payload.to_string().as_bytes());
    }
    let output = child.wait_with_output().expect("wait_with_output");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let line = stdout
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("");
    let resp: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            report.skipped = true;
            report.first_diff = Some((
                usize::MAX,
                format!("invalid JSON from python: {e}; line={line:?}"),
            ));
            return report;
        }
    };
    if resp.get("ok").and_then(Value::as_bool) != Some(true) {
        report.skipped = true;
        report.first_diff = Some((usize::MAX, format!("python ok != true: {resp}")));
        return report;
    }
    let decoded = match resp.get("decoded").and_then(Value::as_array) {
        Some(a) => a,
        None => {
            report.skipped = true;
            return report;
        }
    };
    if decoded.len() != samples {
        report.skipped = true;
        report.first_diff = Some((
            decoded.len(),
            format!("expected {samples} decoded, got {}", decoded.len()),
        ));
        return report;
    }

    for (i, py_view) in decoded.iter().enumerate() {
        let rust_view = expected_view(&histories[i]);
        if py_view == &rust_view {
            report.matched += 1;
        } else {
            report.diverged += 1;
            if report.first_diff.is_none() {
                let detail = pretty_diff(&rust_view, py_view);
                report.first_diff = Some((i, detail));
            }
        }
    }
    report
}

fn pretty_diff(a: &Value, b: &Value) -> String {
    format!(
        "rust={}\npython={}",
        serde_json::to_string(a).unwrap_or_default(),
        serde_json::to_string(b).unwrap_or_default()
    )
}

// ============================================================================
// 测试
// ============================================================================

#[test]
fn cross_lang_default_100() {
    let report = cross_lang_batch(100, 0xC1_CC_AA);
    eprintln!("[cross-lang-100] {report:?}");
    if report.skipped {
        eprintln!("[cross-lang-100] python3 / history_reader.py 不可用 → skipped");
        return;
    }
    assert_eq!(
        report.diverged, 0,
        "first divergence: {:?}",
        report.first_diff
    );
    assert_eq!(report.matched, 100);
}

#[ignore = "C1 full-volume — 10k cross-language; opt-in via -- --ignored"]
#[test]
fn cross_lang_full_10k() {
    let report = cross_lang_batch(10_000, 0xC1_CC_AA);
    eprintln!("[cross-lang-10k] {report:?}");
    if report.skipped {
        panic!("python3 path unavailable: {:?}", report.first_diff);
    }
    assert_eq!(
        report.diverged, 0,
        "first divergence: {:?}",
        report.first_diff
    );
    assert_eq!(report.matched, 10_000);
}
