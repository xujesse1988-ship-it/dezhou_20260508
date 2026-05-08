//! B1：fuzz harness 骨架（C 类）。
//!
//! `pluribus_stage1_workflow.md` §B1 要求：
//!
//! - 随机动作生成器（从 `legal_actions()` 采样）
//! - Invariant 检查器：筹码守恒 / 无负筹码 / 无重复牌 / 未弃牌玩家投入相等 / pot = sum(contributions)
//!
//! **B1 出口**：能生成 1 手并报告 invariant 状态。**当前 GameState 未实现，
//! 实际驱动会 panic**；harness 把驱动包在 [`std::panic::catch_unwind`] 中，让
//! 流程层不崩，仅以 "panicked: not yet implemented" 形式记录。
//!
//! **D1 阶段**升级到 1M 手 + cargo fuzz 配置；本文件作为骨架占位，验证：
//!
//! - 随机动作生成器签名正确（消费 [`LegalActionSet`]，按字段权重采样）
//! - 入口 [`run_one_hand`] 与未来批量入口的接口形状（`fn(seed) -> Report`）
//! - Invariant 调用顺序：每步 apply 后立即检查
//! - panic-safety：单手 panic 不污染整体计数器
//!
//! 角色边界：本文件属 `[测试]` agent。`[实现]` agent 不得修改。

mod common;

use std::panic::{catch_unwind, AssertUnwindSafe};

use poker::{
    Action, ChaCha20Rng, ChipAmount, GameState, LegalActionSet, PlayerStatus, RngSource,
    TableConfig,
};

use common::{expected_total_chips, Invariants};

// ============================================================================
// 单手汇总
// ============================================================================

#[derive(Debug, Default)]
pub struct HandReport {
    pub seed: u64,
    pub actions_applied: usize,
    pub reached_terminal: bool,
    /// 出错原因；`None` 表示无违反。可能值：unimplemented panic / invariant 编号 / RuleError 文本。
    pub failure: Option<String>,
}

#[derive(Debug, Default)]
pub struct FuzzReport {
    pub hands_attempted: usize,
    pub hands_clean: usize,
    pub hands_failed_invariant: usize,
    pub hands_panicked: usize,
    pub first_failure: Option<HandReport>,
}

impl FuzzReport {
    pub fn record(&mut self, h: HandReport) {
        self.hands_attempted += 1;
        match (h.failure.as_deref(), h.reached_terminal) {
            (None, true) => self.hands_clean += 1,
            (Some(msg), _) if msg.starts_with("panic:") => {
                self.hands_panicked += 1;
                if self.first_failure.is_none() {
                    self.first_failure = Some(h);
                }
            }
            _ => {
                self.hands_failed_invariant += 1;
                if self.first_failure.is_none() {
                    self.first_failure = Some(h);
                }
            }
        }
    }
}

// ============================================================================
// 随机动作选择器
// ============================================================================

/// 给定一个 [`LegalActionSet`] 与一个 rng，按 **均匀** 权重在合法字段中采样
/// 一个 [`Action`]。空集合（terminal）时返回 `None`。
///
/// 采样策略（B1 简洁版本）：
///
/// 1. 把"合法"字段全部展平成候选列表：
///    - `fold` → `Action::Fold`（如果允许）
///    - `check` → `Action::Check`
///    - `call` → `Action::Call`
///    - `bet_range = (min,max)` → `Action::Bet { to: random in [min,max] }`
///    - `raise_range = (min,max)` → `Action::Raise { to: random in [min,max] }`
///    - `all_in_amount` → `Action::AllIn`
/// 2. `rng.next_u64() % candidates.len()` 选一个。
///
/// 注意：这里给 fold 与 raise 同等概率会过度集中弃牌。**D1 升级时**会按"实战
/// 类似分布"加权（如 fold 5%、call 50%、raise 5%、check/all-in 余下）。B1 只
/// 需保证流程能跑、合法性正确。
pub fn sample_action(la: &LegalActionSet, rng: &mut dyn RngSource) -> Option<Action> {
    let mut candidates: Vec<Action> = Vec::with_capacity(6);
    if la.fold {
        candidates.push(Action::Fold);
    }
    if la.check {
        candidates.push(Action::Check);
    }
    if la.call.is_some() {
        candidates.push(Action::Call);
    }
    if let Some((min, max)) = la.bet_range {
        let to = sample_chip_in_range(min, max, rng);
        candidates.push(Action::Bet { to });
    }
    if let Some((min, max)) = la.raise_range {
        let to = sample_chip_in_range(min, max, rng);
        candidates.push(Action::Raise { to });
    }
    if la.all_in_amount.is_some() {
        candidates.push(Action::AllIn);
    }
    if candidates.is_empty() {
        return None;
    }
    let idx = (rng.next_u64() as usize) % candidates.len();
    Some(candidates[idx])
}

fn sample_chip_in_range(min: ChipAmount, max: ChipAmount, rng: &mut dyn RngSource) -> ChipAmount {
    let lo = min.as_u64();
    let hi = max.as_u64();
    if lo >= hi {
        return min;
    }
    let span = hi - lo + 1;
    ChipAmount::new(lo + rng.next_u64() % span)
}

// ============================================================================
// 单手驱动
// ============================================================================

/// 驱动一手随机牌局到终局或步数上限。返回 [`HandReport`]。
///
/// 步数上限是 `max_actions`（防止状态机 bug 导致死循环把测试超时）；典型值
/// 4 街 × 6 玩家 × 多次加注 < 200。B1 给 256 上限。
pub fn run_one_hand(seed: u64, max_actions: usize) -> HandReport {
    let mut report = HandReport {
        seed,
        ..Default::default()
    };

    // 用 catch_unwind 包裹整个 GameState 路径（含 cfg 构造），A1 阶段
    // `TableConfig::default_6max_100bb` / `GameState::new` 等都 unimplemented；
    // 任意一步 panic 不污染 batch。
    let outcome = catch_unwind(AssertUnwindSafe(|| -> Result<HandReport, String> {
        let cfg = TableConfig::default_6max_100bb();
        let total = expected_total_chips(&cfg);
        let mut state = GameState::new(&cfg, seed);
        let mut action_rng = ChaCha20Rng::from_seed(seed.wrapping_add(0xDEAD_BEEF));
        let mut applied = 0usize;
        let mut reached_terminal = false;

        // 起手 invariants
        Invariants::check_all(&state, total)?;

        for _ in 0..max_actions {
            if state.is_terminal() {
                reached_terminal = true;
                break;
            }
            let la = state.legal_actions();
            let Some(action) = sample_action(&la, &mut action_rng) else {
                // current_player == None 但 !is_terminal：D-036 跳轮路径正常出现，
                // 状态机应在此前的 apply 内一路推进到 terminal；如果到这里还
                // 不是 terminal，记为 invariant 违反。
                return Err(format!(
                    "current_player=None and !is_terminal at action #{applied}"
                ));
            };
            // current_player 在 sample 之前应为 Some
            let cp = state.current_player();
            if cp.is_none() {
                return Err(format!(
                    "current_player=None but legal_actions returned action {action:?} at #{applied}"
                ));
            }
            let acting = cp.unwrap();
            // 该座位状态必须 Active（不是 AllIn / Folded）
            let active = state
                .players()
                .iter()
                .find(|p| p.seat == acting)
                .map(|p| p.status == PlayerStatus::Active)
                .unwrap_or(false);
            if !active {
                return Err(format!(
                    "current_player {acting:?} is not Active at action #{applied}"
                ));
            }

            state
                .apply(action)
                .map_err(|e| format!("apply failed at #{applied}: {e}"))?;
            applied += 1;
            Invariants::check_all(&state, total)
                .map_err(|e| format!("invariant after #{applied} ({action:?}): {e}"))?;
        }

        Ok(HandReport {
            seed,
            actions_applied: applied,
            reached_terminal,
            failure: None,
        })
    }));

    match outcome {
        Ok(Ok(r)) => return r,
        Ok(Err(msg)) => report.failure = Some(msg),
        Err(_panic) => {
            report.failure = Some(format!(
                "panic: GameState path not yet implemented (seed={seed})"
            ));
        }
    }
    report
}

// ============================================================================
// Smoke test：B1 出口标准要求"能生成 1 手并报告 invariant 状态"
// ============================================================================

/// 1 手 smoke：验证 fuzz 入口可调用、HandReport 可构造、catch_unwind 包裹有效。
///
/// **A1 状态**：实际 GameState 路径会 panic，HandReport.failure 含 "panic:"。
/// 该测试通过的判据是 "harness 没崩溃"，而不是"hand 跑成功"。
#[test]
fn fuzz_smoke_one_hand() {
    let report = run_one_hand(42, 256);
    eprintln!("[fuzz_smoke] {:?}", report);
    // B1 阶段：失败信息预期含 panic 字样。B2 实现落地后改为 `assert!(report.failure.is_none())`。
    assert!(
        report.failure.is_some() || report.reached_terminal,
        "fuzz smoke：要么 panic（A1 期望），要么走到 terminal（B2+）"
    );
}

/// 10 手 mini-batch：验证 [`FuzzReport`] 聚合行为。
#[test]
fn fuzz_smoke_ten_hands_aggregate() {
    let mut report = FuzzReport::default();
    for seed in 0..10u64 {
        report.record(run_one_hand(seed, 256));
    }
    eprintln!("[fuzz_smoke] {:?}", report);
    assert_eq!(report.hands_attempted, 10);
    // A1：10 手都应 panic（unimplemented），hands_panicked == 10。
    // B2 起预期 hands_clean == 10。这里只断言总数，不锁定各分桶。
    assert_eq!(
        report.hands_panicked + report.hands_clean + report.hands_failed_invariant,
        10
    );
}

/// B2 出口验证：随机动作 fuzz 10,000 手，每手每步无 invariant 违反。
#[test]
fn fuzz_b2_10000_hands_no_invariant_violations() {
    let mut report = FuzzReport::default();
    for seed in 0..10_000u64 {
        report.record(run_one_hand(seed, 256));
    }
    eprintln!("[fuzz-b2-10000] {:?}", report);
    assert_eq!(report.hands_attempted, 10_000);
    assert_eq!(
        report.hands_clean, 10_000,
        "B2 fuzz failed: {:?}",
        report.first_failure
    );
    assert_eq!(report.hands_failed_invariant, 0);
    assert_eq!(report.hands_panicked, 0);
}

// ============================================================================
// 入口占位：D1 升级到 cargo fuzz target / 1M 手
// ============================================================================
//
// `cargo fuzz` 通过另设 `fuzz/` 目录与独立 crate 运行 libFuzzer。D1 时新增。
// 当前 [`run_one_hand`] 函数签名兼容 libFuzzer 入口形态（取一个 u64 seed），
// D1 改造时直接 wrap：
//
// ```ignore
// libfuzzer_sys::fuzz_target!(|data: &[u8]| {
//     if data.len() < 8 { return; }
//     let seed = u64::from_le_bytes(data[..8].try_into().unwrap());
//     let _ = fuzz_smoke::run_one_hand(seed, 256);
// });
// ```
