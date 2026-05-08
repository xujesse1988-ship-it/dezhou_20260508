//! B1/B2：PokerKit cross-validation harness（B 类）。
//!
//! `pluribus_stage1_workflow.md` §B1 要求：
//!
//! - 接 PokerKit（Python 子进程或 pyo3）
//! - 接口：给定 `(initial_state, action_sequence)` 比对终局筹码 / pot 划分 / winner / showdown 顺序
//! - B1 第一版只跑 10 手；B2 pass 1 跑 100 手随机牌局
//!
//! 实现选择：**Python 子进程 + JSON stdin/stdout**。pyo3 留待 C1 视性能需求
//! 升级。
//!
//! 与参考实现的语义边界（D-083 / D-086）：
//!
//! - 我方 ≠ PokerKit 时默认 **我方 bug**，需 review 后才能记为 reference 差异。
//! - PokerKit 必须配置为"全程 n_seats 在场、无 sit-in/sit-out、按钮机械每手左移、
//!   SB/BB 机械推导"模式（D-086）。
//!
//! **B2 状态**：
//!
//! - Rust 端随机打一手并输出完整 `HandHistory` JSON。
//! - Python 子进程用 PokerKit 重放同一动作序列。
//! - 比对终局 payouts 与 showdown_order；PokerKit 缺失时保留 skipped fallback。

mod common;

use std::io::Write;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use poker::{
    Action, ChaCha20Rng, ChipAmount, GameState, HandHistory, LegalActionSet, RecordedAction,
    RngSource, SeatId, Street, TableConfig,
};
use serde_json::{json, Value};

use common::{expected_total_chips, Invariants};

// ============================================================================
// 子进程 IO
// ============================================================================

#[derive(Debug)]
pub enum CrossValidationOutcome {
    /// 双方一致。
    Match,
    /// 双方分歧；记录差异详情。我方 bug 优先假设。
    Diverged { reason: String },
    /// 子进程跳过（PokerKit 未安装、B1 stub 等）。不计入分歧总数。
    Skipped { reason: String },
    /// 我方 GameState 路径 panic（保留 legacy fallback，B2 后不应出现）。
    OurPanic { context: String },
    /// IO / 子进程 spawn 错误，与规则正确性无关。
    HarnessError { reason: String },
}

#[derive(Debug, Default)]
pub struct CrossValidationReport {
    pub matches: usize,
    pub diverged: usize,
    pub skipped: usize,
    pub our_panics: usize,
    pub harness_errors: usize,
    pub first_diverged: Option<(u64, String)>,
}

impl CrossValidationReport {
    pub fn record(&mut self, seed: u64, outcome: CrossValidationOutcome) {
        match outcome {
            CrossValidationOutcome::Match => self.matches += 1,
            CrossValidationOutcome::Diverged { reason } => {
                self.diverged += 1;
                if self.first_diverged.is_none() {
                    self.first_diverged = Some((seed, reason));
                }
            }
            CrossValidationOutcome::Skipped { .. } => self.skipped += 1,
            CrossValidationOutcome::OurPanic { .. } => self.our_panics += 1,
            CrossValidationOutcome::HarnessError { .. } => self.harness_errors += 1,
        }
    }
}

// ============================================================================
// 入口
// ============================================================================

/// 跑一手 cross-validation：用 seed 让我方随机打一手，再交给 PokerKit 重放比对。
///
/// 流程：
/// 1. 我方：`GameState::new(cfg, seed)` + 从 `legal_actions()` 采样随机动作直到终局。
/// 2. PokerKit：把"原始 cfg + seed + 推导的 hole/board + actions"序列化为 JSON，
///    调用 `tools/pokerkit_replay.py`，解析 stdout。
///    - exit 2 → `Skipped`（PokerKit 缺失）
///    - error_kind = BadInput → `HarnessError`
///    - ok = true → 比对终局 payouts / showdown_order
/// 3. 比对：双方均成功 → `Match` 或 `Diverged`。
pub fn validate_one_hand(seed: u64) -> CrossValidationOutcome {
    let our_result = catch_unwind(AssertUnwindSafe(|| -> (TableConfig, OursSnapshot) {
        let (cfg, state) =
            play_random_hand(seed, 256).unwrap_or_else(|e| panic!("our random hand failed: {e}"));
        let snap = snapshot_from_state(&state);
        (cfg, snap)
    }));

    let (cfg, ours) = match our_result {
        Ok(pair) => pair,
        Err(_) => {
            return CrossValidationOutcome::OurPanic {
                context: format!("GameState::new(cfg, seed={seed}) panicked"),
            };
        }
    };

    // ---- PokerKit 路径 ----
    let request = encode_request_json(&cfg, seed, &ours.hand_history_for_request);

    let script = locate_python_helper();
    let mut child = match Command::new("python3")
        .arg(&script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return CrossValidationOutcome::HarnessError {
                reason: format!("spawn python3 {script:?} failed: {e}"),
            };
        }
    };
    if let Some(mut stdin) = child.stdin.take() {
        if let Err(e) = stdin.write_all(request.as_bytes()) {
            return CrossValidationOutcome::HarnessError {
                reason: format!("stdin write failed: {e}"),
            };
        }
    }
    let output = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => {
            return CrossValidationOutcome::HarnessError {
                reason: format!("wait_with_output failed: {e}"),
            };
        }
    };

    // exit code 2 = MissingDependency，跳过该手
    if let Some(code) = output.status.code() {
        if code == 2 {
            return CrossValidationOutcome::Skipped {
                reason: "pokerkit not installed".into(),
            };
        }
        if code == 3 {
            return CrossValidationOutcome::HarnessError {
                reason: format!(
                    "pokerkit_replay reported BadInput: {}",
                    String::from_utf8_lossy(&output.stdout)
                ),
            };
        }
    }

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let last_line = stdout
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("");

    let payload: Value = match serde_json::from_str(last_line) {
        Ok(v) => v,
        Err(e) => {
            return CrossValidationOutcome::HarnessError {
                reason: format!(
                    "invalid pokerkit JSON response: {e}; line={last_line:?}; stderr={stderr}"
                ),
            };
        }
    };

    if payload.get("ok").and_then(Value::as_bool) == Some(false) {
        if payload.get("error_kind").and_then(Value::as_str) == Some("B1Stub") {
            return CrossValidationOutcome::Skipped {
                reason: "pokerkit_replay.py is a B1 stub (translation not yet implemented)".into(),
            };
        }
        return CrossValidationOutcome::Diverged {
            reason: format!("pokerkit returned ok=false: {payload} (stderr: {stderr})"),
        };
    }

    if payload.get("ok").and_then(Value::as_bool) != Some(true) {
        return CrossValidationOutcome::HarnessError {
            reason: format!("unexpected pokerkit response: {payload} (stderr: {stderr})"),
        };
    }

    if strict_snapshot_match(&ours, &payload) {
        CrossValidationOutcome::Match
    } else {
        CrossValidationOutcome::Diverged {
            reason: format!(
                "PokerKit mismatch for seed {seed}: ours_payouts={:?}, ref_payouts={:?}, \
                 ours_showdown={:?}, ref_showdown={:?}",
                ours.final_payouts,
                parse_payouts(&payload),
                ours.showdown_order,
                parse_showdown_order(&payload)
            ),
        }
    }
}

fn play_random_hand(seed: u64, max_actions: usize) -> Result<(TableConfig, GameState), String> {
    let cfg = TableConfig::default_6max_100bb();
    let total = expected_total_chips(&cfg);
    let mut state = GameState::new(&cfg, seed);
    let mut action_rng = ChaCha20Rng::from_seed(seed.wrapping_add(0xC005_CAFE));

    Invariants::check_all(&state, total)?;
    for index in 0..max_actions {
        if state.is_terminal() {
            return Ok((cfg, state));
        }
        let la = state.legal_actions();
        let action = sample_action(&la, &mut action_rng)
            .ok_or_else(|| format!("no legal action at index {index}"))?;
        state
            .apply(action)
            .map_err(|e| format!("apply #{index} {action:?} failed: {e}"))?;
        Invariants::check_all(&state, total)
            .map_err(|e| format!("invariant after #{index} {action:?}: {e}"))?;
    }

    if state.is_terminal() {
        Ok((cfg, state))
    } else {
        Err(format!(
            "hand did not terminate within {max_actions} actions"
        ))
    }
}

fn sample_action(la: &LegalActionSet, rng: &mut dyn RngSource) -> Option<Action> {
    let mut candidates: Vec<Action> = Vec::with_capacity(6);
    // PokerKit rejects fold when check is available as a redundant fold. The
    // repo API keeps LA-003 (`fold` always legal), so cross-validation samples
    // only the overlapping action domain.
    if la.fold && !la.check {
        candidates.push(Action::Fold);
    }
    if la.check {
        candidates.push(Action::Check);
    }
    if la.call.is_some() {
        candidates.push(Action::Call);
    }
    if let Some((min, max)) = la.bet_range {
        candidates.push(Action::Bet {
            to: sample_chip_in_range(min, max, rng),
        });
    }
    if let Some((min, max)) = la.raise_range {
        candidates.push(Action::Raise {
            to: sample_chip_in_range(min, max, rng),
        });
    }
    if la.all_in_amount.is_some() {
        candidates.push(Action::AllIn);
    }
    if candidates.is_empty() {
        return None;
    }
    Some(candidates[(rng.next_u64() as usize) % candidates.len()])
}

fn sample_chip_in_range(min: ChipAmount, max: ChipAmount, rng: &mut dyn RngSource) -> ChipAmount {
    let lo = min.as_u64();
    let hi = max.as_u64();
    if lo >= hi {
        return min;
    }
    ChipAmount::new(lo + rng.next_u64() % (hi - lo + 1))
}

// ============================================================================
// 我方快照 / JSON encoding（极简）
// ============================================================================

#[derive(Debug, Default, Clone)]
struct OursSnapshot {
    /// `final_payouts` 拷贝；A1 阶段 GameState 不会到这里。B2 起被 [`naive_payouts_match`] 读取。
    #[allow(dead_code)]
    final_payouts: Vec<(SeatId, i64)>,
    showdown_order: Vec<SeatId>,
    /// 我方的 HandHistory（B2 起完整；A1 该字段无法构造，因此整个 snapshot 的
    /// 收集函数会先 panic — 见 [`validate_one_hand`] 的 `catch_unwind`）。
    hand_history_for_request: Option<HandHistory>,
}

fn snapshot_from_state(state: &GameState) -> OursSnapshot {
    let history = state.hand_history().clone();
    OursSnapshot {
        final_payouts: state.payouts().unwrap_or_default(),
        showdown_order: history.showdown_order.clone(),
        hand_history_for_request: Some(history),
    }
}

/// JSON encoder：输出完整 hand history，让 Python/PokerKit 可独立重放。
fn encode_request_json(cfg: &TableConfig, seed: u64, hh: &Option<HandHistory>) -> String {
    let h = hh.as_ref().expect("B2 cross-validation requires history");
    json!({
        "schema_version": 1,
        "n_seats": cfg.n_seats,
        "starting_stacks": cfg.starting_stacks.iter().map(|c| c.as_u64()).collect::<Vec<_>>(),
        "small_blind": cfg.small_blind.as_u64(),
        "big_blind": cfg.big_blind.as_u64(),
        "ante": cfg.ante.as_u64(),
        "button_seat": cfg.button_seat.0,
        "seed": seed,
        "hole_cards": h.hole_cards.iter().map(|hole| {
            hole.map(|cards| vec![card_to_string(cards[0]), card_to_string(cards[1])])
        }).collect::<Vec<_>>(),
        "board": h.board.iter().map(|&card| card_to_string(card)).collect::<Vec<_>>(),
        "actions": h.actions.iter().map(action_to_json).collect::<Vec<_>>(),
        "final_payouts": h.final_payouts.iter().map(|(seat, net)| {
            json!({"seat": seat.0, "net": net})
        }).collect::<Vec<_>>(),
        "showdown_order": h.showdown_order.iter().map(|seat| seat.0).collect::<Vec<_>>(),
    })
    .to_string()
}

fn action_to_json(action: &RecordedAction) -> Value {
    let (kind, to) = match action.action {
        Action::Fold => ("fold", 0),
        Action::Check => ("check", 0),
        Action::Call => ("call", action.committed_after.as_u64()),
        Action::Bet { to } => ("bet", to.as_u64()),
        Action::Raise { to } => ("raise", to.as_u64()),
        Action::AllIn => unreachable!("history must normalize AllIn"),
    };
    json!({
        "seq": action.seq,
        "seat": action.seat.0,
        "street": street_to_string(action.street),
        "kind": kind,
        "to": to,
        "committed_after": action.committed_after.as_u64(),
    })
}

fn card_to_string(card: poker::Card) -> String {
    let ranks = [
        "2", "3", "4", "5", "6", "7", "8", "9", "T", "J", "Q", "K", "A",
    ];
    let suits = ["c", "d", "h", "s"];
    let v = card.to_u8();
    format!("{}{}", ranks[(v / 4) as usize], suits[(v % 4) as usize])
}

fn street_to_string(street: Street) -> &'static str {
    match street {
        Street::Preflop => "preflop",
        Street::Flop => "flop",
        Street::Turn => "turn",
        Street::River => "river",
        Street::Showdown => "showdown",
    }
}

fn strict_snapshot_match(ours: &OursSnapshot, payload: &Value) -> bool {
    parse_payouts(payload).as_deref() == Some(ours.final_payouts.as_slice())
        && parse_showdown_order(payload).as_deref() == Some(ours.showdown_order.as_slice())
}

fn parse_payouts(payload: &Value) -> Option<Vec<(SeatId, i64)>> {
    let arr = payload.get("final_payouts")?.as_array()?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let seat = item.get("seat")?.as_u64()?;
        let net = item.get("net")?.as_i64()?;
        out.push((SeatId(u8::try_from(seat).ok()?), net));
    }
    out.sort_by_key(|(seat, _)| seat.0);
    Some(out)
}

fn parse_showdown_order(payload: &Value) -> Option<Vec<SeatId>> {
    let arr = payload.get("showdown_order")?.as_array()?;
    arr.iter()
        .map(|value| Some(SeatId(u8::try_from(value.as_u64()?).ok()?)))
        .collect()
}

fn locate_python_helper() -> PathBuf {
    // tests 在 cargo 运行下，CARGO_MANIFEST_DIR 指向 crate 根目录。
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into());
    PathBuf::from(manifest)
        .join("tools")
        .join("pokerkit_replay.py")
}

// ============================================================================
// Smoke tests
// ============================================================================

/// 1 手 smoke：harness 能 spawn python3、写 stdin、读 stdout，不崩溃。
#[test]
fn cross_validation_smoke_one_hand() {
    let outcome = validate_one_hand(0);
    eprintln!("[xvalidate] seed=0 outcome={outcome:?}");
    // Smoke 层只要求 harness 不产生规则分歧；环境缺失仍通过 100 手出口测试处理。
    let acceptable = matches!(
        outcome,
        CrossValidationOutcome::OurPanic { .. }
            | CrossValidationOutcome::Skipped { .. }
            | CrossValidationOutcome::HarnessError { .. }
            | CrossValidationOutcome::Match
    );
    assert!(
        acceptable,
        "cross-validation smoke 不应出现 Diverged：{outcome:?}"
    );
}

/// 10 手 mini-batch：基础 smoke，PokerKit 缺失时允许 skipped。
/// 聚合到 [`CrossValidationReport`]，验证统计接口。
#[test]
fn cross_validation_smoke_ten_hands() {
    let mut report = CrossValidationReport::default();
    for seed in 0..10u64 {
        report.record(seed, validate_one_hand(seed));
    }
    eprintln!("[xvalidate] {report:?}");
    assert_eq!(
        report.matches
            + report.diverged
            + report.skipped
            + report.our_panics
            + report.harness_errors,
        10,
        "10 手累加必须等于 10"
    );
    assert_eq!(report.diverged, 0, "10 手 smoke 不应出现 Diverged");
}

/// B2 出口验证：PokerKit 可用时，100 手随机牌局必须全部 match。
#[test]
fn cross_validation_pokerkit_100_random_hands() {
    let mut report = CrossValidationReport::default();
    for seed in 0..100u64 {
        report.record(seed, validate_one_hand(seed));
    }
    eprintln!("[xvalidate-100] {report:?}");
    assert_eq!(report.our_panics, 0);
    assert_eq!(report.harness_errors, 0);
    assert_eq!(
        report.diverged, 0,
        "first divergence: {:?}",
        report.first_diverged
    );
    if report.skipped > 0 {
        eprintln!("[xvalidate-100] skipped because PokerKit is unavailable");
    } else {
        assert_eq!(report.matches, 100);
    }
}
