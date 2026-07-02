//! 在线 ML range 预测器接线 smoke 测试（`use_ml_range=true` 决策路径）。
//!
//! ⚠ **EV 未验证**：本测试只证明「接线不 panic、出合法动作、range 分布良定义、
//! off 路径不退化」，**不**证明 EV。远端 sim A/B（mbb/g 不劣于标量路径）通过前
//! **不翻默认**（`use_ml_range` 仍默认 false）。
//!
//! 产物缺失（无 venv / CI）→ 打印并 return（不 fail），镜像 optional-test 约定。

use std::path::PathBuf;

use poker::heuristic::config::HeuristicConfig;
use poker::heuristic::policy::DecisionAction;
use poker::heuristic::view::{GameView, HistAction, Request, ValidActions};
use poker::heuristic::HeuristicBot;

/// 带权威街标签的历史动作。
fn ha(seat: u8, action: &str, to: Option<u64>, street: u8) -> HistAction {
    HistAction {
        seat,
        action: action.to_string(),
        to,
        street: Some(street),
    }
}

/// 多路 flop 决策点（3 handed：BTN=0 / SB=1 / BB=2，sb=1 / bb=2，栈 200=100bb）：
///   preflop: BTN raise→6, SB call, BB call
///   flop AsAh6d：BB check, SB check, hero(BTN) 决策点（valid = check/bet）。
/// hero = BTN(seat0) 持 AcAd，两个仍在场对手（SB=1 / BB=2）→ 多路加权 equity。
/// 四张 A 全为死牌（hero AcAd + board AsAh）→ AA 类（idx 12）存活 combo=0 = 真死类。
fn multiway_flop_request() -> Request {
    Request {
        hole: vec!["Ac".into(), "Ad".into()],
        board: vec!["As".into(), "Ah".into(), "6d".into()],
        button_seat: 0,
        my_seat: 0,
        num_seats: 3,
        small_blind: 1,
        big_blind: 2,
        actions: vec![
            ha(0, "raise", Some(6), 0), // BTN open
            ha(1, "call", None, 0),     // SB call
            ha(2, "call", None, 0),     // BB call
            ha(2, "check", None, 1),    // BB check
            ha(1, "check", None, 1),    // SB check
        ],
        valid: ValidActions {
            can_check: true,
            can_call: false,
            can_raise: true,
            min_raise: Some(2),
            max_raise: Some(194),
        },
        stacks: vec![200, 200, 200],
        names: Default::default(),
        sb_seat: Some(1),
        bb_seat: Some(2),
    }
}

#[test]
fn ml_range_online_smoke() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let strength_path = root.join("artifacts/range_ml/strength169.json");
    let weights_path = root.join("artifacts/range_ml/range_v1_gru_weights.json");
    for (label, p) in [
        ("strength169.json", &strength_path),
        ("range_v1_gru_weights.json", &weights_path),
    ] {
        if !p.exists() {
            eprintln!("[ml-range-online] {label} 缺席 ({}) → skipped", p.display());
            return;
        }
    }

    // --- use_ml_range=true：载入成功 ---
    let mut cfg = HeuristicConfig::default();
    cfg.range.use_ml_range = true;
    cfg.range.ml_strength_path = strength_path.to_string_lossy().into_owned();
    cfg.range.ml_weights_path = weights_path.to_string_lossy().into_owned();
    // 决策 MC 采样调小以提速（不影响接线正确性）。
    cfg.equity.samples = 200;
    let bot = HeuristicBot::new(cfg);
    assert!(
        bot.ml_range.is_some(),
        "use_ml_range=true 且产物存在 → ml_range 应 Some"
    );
    eprintln!("[ml-range-online] bot.ml_range.is_some() = true");

    let req = multiway_flop_request();
    let view = GameView::from_request(&req);

    // --- 直接调 predict：对每个仍在场对手断言 RangeVec 良定义 ---
    let me = view.my_seat as usize;
    let ml = bot.ml_range.as_ref().unwrap();
    let mut checked_opp = 0usize;
    for seat in 0..view.num_seats as usize {
        if seat == me || view.folded[seat] {
            continue;
        }
        let rv = ml.predict(&view, seat);
        let sum: f64 = rv.mass.iter().sum();
        assert!(
            (sum - 1.0).abs() < 1e-6,
            "seat {seat} RangeVec 应归一，∑mass={sum}"
        );
        assert!(
            rv.mass.iter().all(|&m| m.is_finite() && m >= 0.0),
            "seat {seat} RangeVec 应全有限非负"
        );
        // 真死类质量应 ≈ 0：四张 A 全死（hero AcAd + board AsAh）→ AA 类（idx 12）
        // 存活 combo=0 → mlrange::predict 掩 -1e30 → softmax≈0。
        assert!(
            rv.mass[12] < 1e-12,
            "seat {seat} AA 死类质量应 ≈0，got {}",
            rv.mass[12]
        );
        checked_opp += 1;
        eprintln!(
            "[ml-range-online] seat {seat}: ∑mass={sum:.6} (finite, non-neg, AA-dead={:.2e})",
            rv.mass[12]
        );
    }
    assert!(checked_opp >= 1, "场景应至少 1 个仍在场对手");

    // --- respond：ML 路径出合法动作、不 panic ---
    let resp = bot.respond(&req);
    assert!(
        matches!(resp.action.as_str(), "fold" | "check" | "call" | "raise"),
        "ML 路径应出合法动作，got {}",
        resp.action
    );
    let d = bot.decide_view(&view);
    assert!(
        matches!(
            d.action,
            DecisionAction::Fold
                | DecisionAction::Check
                | DecisionAction::Call
                | DecisionAction::Raise(_)
        ),
        "ML 路径 Decision 应合法"
    );
    eprintln!(
        "[ml-range-online] ML decide: action={} source={}",
        resp.action, resp.source
    );

    // --- off 路径回归：use_ml_range=false 仍正常决策（走标量 equity）---
    let mut cfg_off = HeuristicConfig::default();
    cfg_off.equity.samples = 200;
    let bot_off = HeuristicBot::new(cfg_off);
    assert!(
        bot_off.ml_range.is_none(),
        "use_ml_range=false → ml_range 应 None"
    );
    let resp_off = bot_off.respond(&req);
    assert!(
        matches!(
            resp_off.action.as_str(),
            "fold" | "check" | "call" | "raise"
        ),
        "off 路径应出合法动作，got {}",
        resp_off.action
    );
    eprintln!(
        "[ml-range-online] off(use_ml_range=false) decide: action={} source={}",
        resp_off.action, resp_off.source
    );
    eprintln!("[ml-range-online] PASS");
}
