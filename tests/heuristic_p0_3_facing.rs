//! P0-3 阶段1（采集基建）针对性测试：facing 上下文重建正确性 + 收缩数学 + 后向兼容。
//!
//! 阶段1 = 纯增采集、决策路径不读（sim byte-equal 另由 `heuristic9_sim` 回归门守）。本文件
//! 自证「采到的计数符合构造序列」「fold-to-cbet 方向 = ThreebetFolder 高 / WideOpener 低」
//! 「Beta-Binomial 收缩数学正确」「旧 profile（无 facing 字段）仍加载、旧字段不丢」。

use poker::heuristic::opponent::{node_field_prior, FacingCounts, ObservedAction, OpponentTable};

/// 构造一条观测动作（非盲位）。
fn act<'a>(name: &'a str, street: u8, action: &'a str) -> ObservedAction<'a> {
    ObservedAction {
        name,
        street,
        action,
        forced_blind: false,
    }
}

/// 便捷：喂一手观测（names = 出现的所有名）。
fn observe(t: &mut OpponentTable, actions: &[ObservedAction], names: &[&str]) {
    let owned: Vec<String> = names.iter().map(|s| s.to_string()).collect();
    t.observe_hand(actions, &owned);
}

// ---------------------------------------------------------------------------
// 1. fold-to-cbet 采集：villain 面对 flop c-bet 弃牌 → flop.<pot> folds+1, n+1
// ---------------------------------------------------------------------------
#[test]
fn flop_fold_to_cbet_counts() {
    let mut t = OpponentTable::new();
    // SRP：hero 开池（1 个翻前 raise）→ pot_type=srp。flop hero c-bet，villain fold。
    let actions = [
        act("hero", 0, "raise"),
        act("villain", 0, "call"),
        act("villain", 1, "check"),
        act("hero", 1, "bet"),
        act("villain", 1, "fold"),
    ];
    observe(&mut t, &actions, &["hero", "villain"]);
    let fc = t.by_name["villain"].facing.get("flop.srp").unwrap();
    assert_eq!(fc.n, 1);
    assert_eq!(fc.folds, 1);
    assert_eq!(fc.calls, 0);
    assert_eq!(fc.raises, 0);
    assert_eq!(fc.raw_fold_rate(), Some(1.0));
    // c-better（hero）不入 facing（他是进攻方）。
    assert!(
        t.by_name["hero"]
            .facing
            .get("flop.srp")
            .map(|f| f.n)
            .unwrap_or(0)
            == 0
    );
}

// ---------------------------------------------------------------------------
// 2. call / raise 桶 + 池型 3bp + 逐街（turn/river）
// ---------------------------------------------------------------------------
#[test]
fn call_raise_and_pot_type_and_streets() {
    let mut t = OpponentTable::new();
    // 3bet 池（2 个翻前 raise）：hero open, villain 3bet, hero call。
    // flop：hero(非 PFA 侧... 这里 villain 是 PFA) — 用「villain 领打，hero 面对」构造 hero 防守。
    // 但我们要测 villain 作防守方：让 hero flop 领打、villain call；turn hero 领打、villain raise。
    let actions = [
        act("hero", 0, "raise"),
        act("villain", 0, "raise"),
        act("hero", 0, "call"),
        // flop：hero bet，villain call
        act("hero", 1, "bet"),
        act("villain", 1, "call"),
        // turn：hero bet，villain raise
        act("hero", 2, "bet"),
        act("villain", 2, "raise"),
        act("hero", 2, "call"),
        // river：hero bet，villain fold
        act("hero", 3, "bet"),
        act("villain", 3, "fold"),
    ];
    observe(&mut t, &actions, &["hero", "villain"]);
    let f = &t.by_name["villain"].facing;
    let flop = f.get("flop.3bp").unwrap();
    assert_eq!((flop.n, flop.folds, flop.calls, flop.raises), (1, 0, 1, 0));
    let turn = f.get("turn.3bp").unwrap();
    assert_eq!((turn.n, turn.folds, turn.calls, turn.raises), (1, 0, 0, 1));
    let river = f.get("river.3bp").unwrap();
    assert_eq!(
        (river.n, river.folds, river.calls, river.raises),
        (1, 1, 0, 0)
    );
}

// ---------------------------------------------------------------------------
// 3. fold-to-3bet（翻前）：opener 面对站住的 3bet 弃牌
// ---------------------------------------------------------------------------
#[test]
fn preflop_fold_to_3bet() {
    let mut t = OpponentTable::new();
    let actions = [
        act("opener", 0, "raise"),
        act("threebettor", 0, "raise"),
        act("opener", 0, "fold"),
    ];
    observe(&mut t, &actions, &["opener", "threebettor"]);
    let fc = t.by_name["opener"].facing.get("pre.facing_3bet").unwrap();
    assert_eq!((fc.n, fc.folds), (1, 1));
    // 3bettor 不是 opener → 不记 pre.facing_3bet。
    assert!(!t.by_name["threebettor"]
        .facing
        .contains_key("pre.facing_3bet"));
}

// ---------------------------------------------------------------------------
// 4. 累积多手 + 方向自证（ThreebetFolder 高 fold-to-cbet / WideOpener 低）
// ---------------------------------------------------------------------------
#[test]
fn fold_to_cbet_direction_high_vs_low() {
    let mut t = OpponentTable::new();
    // "folder"：10 手全弃 c-bet。 "sticky"：10 手全跟。SRP，hero c-bet。
    for _ in 0..10 {
        observe(
            &mut t,
            &[
                act("hero", 0, "raise"),
                act("folder", 0, "call"),
                act("folder", 1, "check"),
                act("hero", 1, "bet"),
                act("folder", 1, "fold"),
            ],
            &["hero", "folder"],
        );
        observe(
            &mut t,
            &[
                act("hero", 0, "raise"),
                act("sticky", 0, "call"),
                act("sticky", 1, "check"),
                act("hero", 1, "bet"),
                act("sticky", 1, "call"),
            ],
            &["hero", "sticky"],
        );
    }
    let folder = t.by_name["folder"].facing.get("flop.srp").unwrap();
    let sticky = t.by_name["sticky"].facing.get("flop.srp").unwrap();
    assert_eq!(folder.raw_fold_rate(), Some(1.0));
    assert_eq!(sticky.raw_fold_rate(), Some(0.0));
    // 方向：folder 显著高于 sticky。
    assert!(folder.raw_fold_rate().unwrap() > sticky.raw_fold_rate().unwrap());
    // 收缩后仍保持方向（n=10, k=20）。
    let r_folder = t.node_readout("folder", "flop.srp", 20.0);
    let r_sticky = t.node_readout("sticky", "flop.srp", 20.0);
    assert!(r_folder.shrunk_fold > r_sticky.shrunk_fold);
    assert_eq!(r_folder.n, 10);
}

// ---------------------------------------------------------------------------
// 5. Beta-Binomial 收缩数学正确
// ---------------------------------------------------------------------------
#[test]
fn shrinkage_math() {
    // n=3, folds=3, prior=0.456, k=20 → (3 + 0.456*20)/(3+20) = 12.12/23 = 0.526956...
    let fc = FacingCounts {
        n: 3,
        folds: 3,
        calls: 0,
        raises: 0,
    };
    let s = fc.shrunk_fold_rate(0.456, 20.0);
    assert!((s - (3.0 + 0.456 * 20.0) / 23.0).abs() < 1e-12);
    assert!((s - 0.526956_5).abs() < 1e-4);
    // n=0 → 返回 prior。
    let empty = FacingCounts::default();
    assert!((empty.shrunk_fold_rate(0.456, 20.0) - 0.456).abs() < 1e-12);
    assert_eq!(empty.raw_fold_rate(), None);
    // field 先验查表。
    assert!((node_field_prior("flop.srp").fold - 0.456).abs() < 1e-12);
    assert!((node_field_prior("pre.facing_3bet").fold - 0.151).abs() < 1e-12);
    // 未知节点 → 中性占位（不 panic）。
    let _ = node_field_prior("weird.node");
}

// ---------------------------------------------------------------------------
// 6. 后向兼容：旧 profile（无 facing 字段）加载 → 旧字段不丢、facing 空
// ---------------------------------------------------------------------------
#[test]
fn backward_compat_old_profile_loads() {
    // 旧格式（7 字段，无 facing），逐字节仿 h9_live_prof.json 的 OppStats。
    let old = r#"{"by_name":{"GenesisBlock":{"hands":592,"vpip_opp":385,"vpip_cnt":183,"pfr_opp":385,"pfr_cnt":127,"aggr_actions":147,"call_actions":92}}}"#;
    let t = OpponentTable::from_json_str(old);
    let st = t.by_name.get("GenesisBlock").expect("旧 profile 应加载");
    assert_eq!(st.hands, 592);
    assert_eq!(st.vpip_cnt, 183);
    assert_eq!(st.pfr_cnt, 127);
    assert_eq!(st.aggr_actions, 147);
    assert_eq!(st.call_actions, 92);
    assert!(st.facing.is_empty(), "旧字段无 facing → 默认空表");
    // node_readout 对无样本节点返回先验（不 panic）。
    let r = t.node_readout("GenesisBlock", "flop.srp", 20.0);
    assert_eq!(r.n, 0);
    assert_eq!(r.raw_fold, None);
    assert!((r.shrunk_fold - 0.456).abs() < 1e-12);
    // 空 facing 不序列化（skip_serializing_if）→ 输出与旧格式等价（byte-stable）。
    let round = t.to_json_string();
    assert!(!round.contains("facing"), "空 facing 不应出现在序列化输出");
}

// ---------------------------------------------------------------------------
// 7. 后向兼容：加载仓库现有真实 profile 文件（44 名画像）不炸
// ---------------------------------------------------------------------------
#[test]
fn backward_compat_real_profile_file() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/h9_live_prof.json");
    let Ok(s) = std::fs::read_to_string(path) else {
        // 文件可能在某些 CI 环境缺失 → 跳过（非硬失败）。
        eprintln!("跳过：{path} 不存在");
        return;
    };
    let t = OpponentTable::from_json_str(&s);
    assert!(
        t.by_name.len() >= 40,
        "现有 profile 应加载 ≥40 名，实得 {}",
        t.by_name.len()
    );
    // 旧字段完整（抽查 GenesisBlock）+ facing 默认空。
    if let Some(st) = t.by_name.get("GenesisBlock") {
        assert_eq!(st.hands, 592);
        assert!(st.facing.is_empty());
    }
    // round-trip 不含 facing（现有画像无翻后 facing 数据）。
    let round = t.to_json_string();
    assert!(!round.contains("\"facing\""));
}
