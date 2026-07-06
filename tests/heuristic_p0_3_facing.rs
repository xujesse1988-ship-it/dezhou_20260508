//! P0-3 阶段1（采集基建）针对性测试：facing 上下文重建正确性 + 收缩数学 + 后向兼容。
//!
//! 阶段1 = 纯增采集、决策路径不读（sim byte-equal 另由 `heuristic9_sim` 回归门守）。本文件
//! 自证「采到的计数符合构造序列」「fold-to-cbet 方向 = ThreebetFolder 高 / WideOpener 低」
//! 「Beta-Binomial 收缩数学正确」「旧 profile（无 facing 字段）仍加载、旧字段不丢」。

use poker::heuristic::opponent::{
    attack_showdown_node_key, facing_raise_node_key, node_field_prior, FacingCounts,
    ObservedAction, OpponentTable, ShowdownObs,
};

/// 构造一条观测动作（非盲位）。
fn act<'a>(name: &'a str, street: u8, action: &'a str) -> ObservedAction<'a> {
    ObservedAction {
        name,
        street,
        action,
        forced_blind: false,
    }
}

/// P0-3 §D：构造一条摊牌观测（`made_rank`=MadeClass::rank，`nonnut`=monster_non_nut 同花+顺，
/// `nonnut_flush`=flush-only 非坚果同花 = 候选① 真杠杆）。
fn sd(name: &str, made_rank: u8, nonnut: bool, nonnut_flush: bool) -> ShowdownObs<'_> {
    ShowdownObs {
        name,
        made_rank,
        nonnut,
        nonnut_flush,
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
    // 旧字段完整（抽查 GenesisBlock）+ facing / attack_showdown 默认空。
    if let Some(st) = t.by_name.get("GenesisBlock") {
        assert_eq!(st.hands, 592);
        assert!(st.facing.is_empty());
        assert!(st.attack_showdown.is_empty());
    }
    // round-trip 不含 facing / attack_showdown（现有画像无翻后 facing/§D 数据 → §D 新字段
    // skip_serializing_if 空表不写 → 逐字节兼容旧 profile）。
    let round = t.to_json_string();
    assert!(!round.contains("\"facing\""));
    assert!(!round.contains("attack_showdown"));
}

// ===========================================================================
// P0-3 fold-to-raise 采集扩展（villain 作进攻方被 re-raise，§B；pre.facing_3bet 翻后镜像）
// ===========================================================================

// ---------------------------------------------------------------------------
// 8. fold-to-raise 计数 + actor/时点正确性：开火者（donk-lead 方）被 hero 加注后弃牌
//    → `flop.srp.facing_raise`；并证 fold-to-cbet 采集互不干扰。
// ---------------------------------------------------------------------------
#[test]
fn flop_fold_to_raise_counts_and_actor() {
    let mut t = OpponentTable::new();
    // SRP（hero open, villain call → pot_type=srp, PFA=hero）。
    // flop：villain donk-lead（开火者）→ hero raise → villain fold（fold-to-raise）。
    let actions = [
        act("hero", 0, "raise"),
        act("villain", 0, "call"),
        act("villain", 1, "bet"), // villain 领打 = 街内首个 raiser = 开火者/进攻方
        act("hero", 1, "raise"),  // hero re-raise villain 的 lead
        act("villain", 1, "fold"), // 开火者面对 re-raise → fold（fold-to-raise 命中）
    ];
    observe(&mut t, &actions, &["hero", "villain"]);

    // villain 的 fold-to-raise 命中（进攻方被加注弃牌）。
    let fr = t.by_name["villain"]
        .facing
        .get("flop.srp.facing_raise")
        .expect("villain 应有 flop.srp.facing_raise");
    assert_eq!((fr.n, fr.folds, fr.calls, fr.raises), (1, 1, 0, 0));
    assert_eq!(fr.raw_fold_rate(), Some(1.0));

    // actor 正确性：villain 是**开火者**（进攻方）→ 不入 fold-to-cbet（防守方）节点。
    assert!(
        !t.by_name["villain"].facing.contains_key("flop.srp"),
        "开火者不应进 fold-to-cbet 表"
    );
    // hero 是防守方（面对 villain 的 lead）且加注了 → 进既有 fold-to-cbet 节点 raises 桶（未变）。
    let hero_ftc = t.by_name["hero"]
        .facing
        .get("flop.srp")
        .expect("hero 面对 villain lead 应入 fold-to-cbet 表");
    assert_eq!((hero_ftc.n, hero_ftc.raises), (1, 1));
    // hero 不是开火者 → 无 fold-to-raise。
    assert!(!t.by_name["hero"]
        .facing
        .contains_key("flop.srp.facing_raise"));
}

// ---------------------------------------------------------------------------
// 9. fold-to-raise 方向自证（DonkFold 型高 / DonkStick 型低）+ 收缩保持方向
// ---------------------------------------------------------------------------
#[test]
fn fold_to_raise_direction_high_vs_low() {
    let mut t = OpponentTable::new();
    // "folder"：10 手全部 lead→被加注→弃（高 fold-to-raise，DonkFold 型）。
    // "sticker"：10 手全部 lead→被加注→跟（低 fold-to-raise，DonkStick 型）。
    for _ in 0..10 {
        observe(
            &mut t,
            &[
                act("hero", 0, "raise"),
                act("folder", 0, "call"),
                act("folder", 1, "bet"),
                act("hero", 1, "raise"),
                act("folder", 1, "fold"),
            ],
            &["hero", "folder"],
        );
        observe(
            &mut t,
            &[
                act("hero", 0, "raise"),
                act("sticker", 0, "call"),
                act("sticker", 1, "bet"),
                act("hero", 1, "raise"),
                act("sticker", 1, "call"),
            ],
            &["hero", "sticker"],
        );
    }
    let folder = t.by_name["folder"]
        .facing
        .get("flop.srp.facing_raise")
        .unwrap();
    let sticker = t.by_name["sticker"]
        .facing
        .get("flop.srp.facing_raise")
        .unwrap();
    assert_eq!(folder.raw_fold_rate(), Some(1.0));
    assert_eq!(sticker.raw_fold_rate(), Some(0.0));
    assert!(folder.raw_fold_rate().unwrap() > sticker.raw_fold_rate().unwrap());
    // 收缩后仍保持方向（n=10, k=20, prior=0.30）。
    let r_folder = t.node_readout("folder", "flop.srp.facing_raise", 20.0);
    let r_sticker = t.node_readout("sticker", "flop.srp.facing_raise", 20.0);
    assert!(r_folder.shrunk_fold > r_sticker.shrunk_fold);
    assert_eq!(r_folder.n, 10);
    // 收缩数学：folder (10 folds/10) 向 0.30 收缩 = (10 + 0.30*20)/(10+20) = 16/30 = 0.5333…
    assert!((r_folder.shrunk_fold - 16.0 / 30.0).abs() < 1e-9);
}

// ---------------------------------------------------------------------------
// 10. 前缀碰撞铁证：`*.facing_raise` 先验 = 0.30 占位（**不是** fold-to-cbet 的 0.456）
// ---------------------------------------------------------------------------
#[test]
fn facing_raise_prior_no_prefix_collision() {
    // 专用分支前置 → 不被 starts_with("flop.") 抢先匹配到 0.456。
    assert!((node_field_prior("flop.srp.facing_raise").fold - 0.30).abs() < 1e-12);
    assert!((node_field_prior("turn.3bp.facing_raise").fold - 0.30).abs() < 1e-12);
    assert!((node_field_prior("river.srp.facing_raise").fold - 0.30).abs() < 1e-12);
    // 对照：基座 fold-to-cbet 节点仍取各自先验（未被扰动）。
    assert!((node_field_prior("flop.srp").fold - 0.456).abs() < 1e-12);
    assert!((node_field_prior("turn.srp").fold - 0.456).abs() < 1e-12);
    assert!((node_field_prior("river.srp").fold - 0.50).abs() < 1e-12);
    // key 构造器与前缀检测自洽。
    assert_eq!(facing_raise_node_key(1, "srp"), "flop.srp.facing_raise");
    assert_eq!(facing_raise_node_key(2, "3bp"), "turn.3bp.facing_raise");
    assert_eq!(
        facing_raise_node_key(3, "limped"),
        "river.limped.facing_raise"
    );
}

// ---------------------------------------------------------------------------
// 11. 止步首次响应：开火者被加注后 re-raise（记 raise 桶）→ 后续 4bet 链不再记
// ---------------------------------------------------------------------------
#[test]
fn fold_to_raise_stops_at_first_response() {
    let mut t = OpponentTable::new();
    let actions = [
        act("hero", 0, "raise"),
        act("villain", 0, "call"),
        act("villain", 1, "bet"),   // villain 开火（raises=1）
        act("hero", 1, "raise"),    // hero re-raise（raises=2）
        act("villain", 1, "raise"), // 开火者面对 re-raise 的**首次响应** = 3bet-back（记 raise）
        act("hero", 1, "raise"),    // hero 4bet（raises=4）
        act("villain", 1, "fold"),  // 开火者后续弃 → **不再记**（止步首次响应）
    ];
    observe(&mut t, &actions, &["hero", "villain"]);
    let fr = t.by_name["villain"]
        .facing
        .get("flop.srp.facing_raise")
        .unwrap();
    // 只记首次响应（re-raise），后续 fold 不入 → n=1, raises=1, folds=0。
    assert_eq!((fr.n, fr.folds, fr.calls, fr.raises), (1, 0, 0, 1));
}

// ---------------------------------------------------------------------------
// 12. 后向兼容：含 facing_raise key 的 profile 反序列化 + round-trip（无新结构字段，
//     复用既有 facing BTreeMap → serde 逐字节兼容）
// ---------------------------------------------------------------------------
#[test]
fn backward_compat_facing_raise_key_roundtrip() {
    // 旧 profile（无 facing）仍加载（新增采集不加结构字段）。
    let old = r#"{"by_name":{"A":{"hands":10,"vpip_opp":5,"vpip_cnt":3,"pfr_opp":5,"pfr_cnt":2,"aggr_actions":4,"call_actions":3}}}"#;
    let t_old = OpponentTable::from_json_str(old);
    assert!(t_old.by_name["A"].facing.is_empty());

    // 含 fold-to-cbet + fold-to-raise 双节点的 profile 反序列化。
    let json = r#"{"by_name":{"villain":{"hands":50,"vpip_opp":30,"vpip_cnt":15,"pfr_opp":30,"pfr_cnt":10,"aggr_actions":20,"call_actions":15,"facing":{"flop.srp":{"n":10,"folds":4,"calls":5,"raises":1},"flop.srp.facing_raise":{"n":6,"folds":5,"calls":0,"raises":1}}}}}"#;
    let t = OpponentTable::from_json_str(json);
    let fr = t.by_name["villain"]
        .facing
        .get("flop.srp.facing_raise")
        .expect("facing_raise 节点应反序列化");
    assert_eq!((fr.n, fr.folds, fr.calls, fr.raises), (6, 5, 0, 1));
    // fold-to-cbet 节点并存不受影响。
    let ftc = t.by_name["villain"].facing.get("flop.srp").unwrap();
    assert_eq!((ftc.n, ftc.folds, ftc.calls, ftc.raises), (10, 4, 5, 1));
    // node_readout 用 facing_raise 先验（0.30），不误用 0.456。
    let r = t.node_readout("villain", "flop.srp.facing_raise", 20.0);
    assert_eq!(r.n, 6);
    assert!((r.raw_fold.unwrap() - 5.0 / 6.0).abs() < 1e-9);
    // round-trip 保留 facing_raise key。
    let round = t.to_json_string();
    assert!(round.contains("flop.srp.facing_raise"));
    let t2 = OpponentTable::from_json_str(&round);
    assert_eq!(
        t2.by_name["villain"]
            .facing
            .get("flop.srp.facing_raise")
            .unwrap()
            .folds,
        5
    );
}

// ===========================================================================
// P0-3 §D 摊牌采集（villain 作进攻方且摊牌，final-board 成手牌力构成；候选① range 极性真信号）
// MadeClass::rank：Air=0 WeakPair=1 MidPair=2 TopPair=3 Strong=4 Monster=5
// ===========================================================================

// ---------------------------------------------------------------------------
// 13. §D 计数 + actor/时点正确性：river 攻击方摊牌 → river.srp.attack_showdown；非攻击者 / 独立 map
// ---------------------------------------------------------------------------
#[test]
fn river_attack_showdown_counts_and_actor() {
    let mut t = OpponentTable::new();
    // SRP（hero open, villain call → pot_type=srp）。river：villain bet（进攻方）→ hero call → 摊牌。
    // villain 亮非坚果 Monster（rank=5, nonnut=true）；hero 只跟（非攻击方）亮 Strong。
    let actions = [
        act("hero", 0, "raise"),
        act("villain", 0, "call"),
        act("villain", 3, "bet"), // villain river 攻击（进攻方）
        act("hero", 3, "call"),   // hero 只跟（防守方，非攻击）
    ];
    t.observe_showdown(
        &actions,
        &[sd("villain", 5, true, true), sd("hero", 4, false, false)],
    );

    // villain river attack_showdown 命中（进攻方摊牌）。
    let r = t.by_name["villain"]
        .attack_showdown
        .get("river.srp.attack_showdown")
        .expect("villain 应有 river.srp.attack_showdown");
    assert_eq!((r.showdowns, r.nonnut, r.nonnut_flush), (1, 1, 1));
    assert_eq!(r.by_made_class[5], 1); // Monster 直方 +1
    assert_eq!(r.raw_nonnut_rate(), Some(1.0));
    assert_eq!(r.raw_nonnut_flush_rate(), Some(1.0));

    // actor：villain 只在 river 攻击 → 无 flop/turn 节点。
    assert!(!t.by_name["villain"]
        .attack_showdown
        .contains_key("flop.srp.attack_showdown"));
    assert!(!t.by_name["villain"]
        .attack_showdown
        .contains_key("turn.srp.attack_showdown"));

    // hero 未攻击任何街（只跟）→ observe_showdown 从不为其建条目（进攻方专属，非攻击者不入表）。
    assert!(!t.by_name.contains_key("hero"));

    // 结构分野：§D 独立 map（attack_showdown），不进 facing map。
    assert!(t.by_name["villain"].facing.is_empty());
}

// ---------------------------------------------------------------------------
// 14. §D 非坚果率方向分化（纯坚果攻击 FlushTrap 型 0% ≪ 混合价值 Tag 型 60%）——§B 频率代理做不到
// ---------------------------------------------------------------------------
#[test]
fn showdown_nonnut_direction_nutonly_vs_mixed() {
    let mut t = OpponentTable::new();
    // "nutonly"（FlushTrap 型）：10 手 river 攻击摊牌全坚果 Monster（nonnut=false）。
    // "mixed"（Tag 型）：10 手 river 攻击摊牌里 6 手非坚果 Monster（nonnut=true）+ 4 手坚果。
    for i in 0..10u8 {
        t.observe_showdown(
            &[
                act("hero", 0, "raise"),
                act("nutonly", 0, "call"),
                act("nutonly", 3, "bet"),
                act("hero", 3, "call"),
            ],
            &[sd("nutonly", 5, false, false), sd("hero", 4, false, false)],
        );
        t.observe_showdown(
            &[
                act("hero", 0, "raise"),
                act("mixed", 0, "call"),
                act("mixed", 3, "bet"),
                act("hero", 3, "call"),
            ],
            &[sd("mixed", 5, i < 6, i < 6), sd("hero", 4, false, false)],
        );
    }
    let nut = t.showdown_readout("nutonly", "river.srp.attack_showdown");
    let mix = t.showdown_readout("mixed", "river.srp.attack_showdown");
    assert_eq!(nut.showdowns, 10);
    assert_eq!(mix.showdowns, 10);
    assert_eq!(nut.raw_nonnut, Some(0.0)); // 纯坚果 → 0%
    assert_eq!(mix.raw_nonnut, Some(0.6)); // 混合 → 60%
    assert_eq!(nut.raw_nonnut_flush, Some(0.0)); // flush-only 同向（候选① 真信号）
    assert_eq!(mix.raw_nonnut_flush, Some(0.6));
    // 这正是 §B raise 频率代理做不到的分化（FlushTrap 44.9% ≈ Tag 45.2% 撞车）。
    assert!(mix.raw_nonnut.unwrap() > nut.raw_nonnut.unwrap());
    assert!(mix.raw_nonnut_flush.unwrap() > nut.raw_nonnut_flush.unwrap());
}

// ---------------------------------------------------------------------------
// 14b. flush-only 与 monster 分开跟踪：非坚果**顺子**攻击命中 nonnut 但不命中 nonnut_flush
//      （monster_non_nut 被 FlushTrap 成顺板非坚果顺子污染、唯 flush-only 给净信号的机理自证）
// ---------------------------------------------------------------------------
#[test]
fn showdown_flush_only_isolates_straight() {
    let mut t = OpponentTable::new();
    let actions = [
        act("hero", 0, "raise"),
        act("v", 0, "call"),
        act("v", 3, "bet"),
        act("hero", 3, "call"),
    ];
    // 非坚果**顺子**攻击：Monster(rank=5) + nonnut=true + nonnut_flush=false。
    t.observe_showdown(
        &actions,
        &[sd("v", 5, true, false), sd("hero", 4, false, false)],
    );
    let r = t.showdown_readout("v", "river.srp.attack_showdown");
    assert_eq!((r.showdowns, r.nonnut, r.nonnut_flush), (1, 1, 0));
    assert_eq!(r.raw_nonnut, Some(1.0)); // monster_non_nut 命中（含顺子）
    assert_eq!(r.raw_nonnut_flush, Some(0.0)); // flush-only 不命中 → 候选① 不误判为极化攻击
}

// ---------------------------------------------------------------------------
// 15. §D node key 构造器 + 独立 readout 不经 node_field_prior（无前缀碰撞污染）
// ---------------------------------------------------------------------------
#[test]
fn attack_showdown_key_and_no_field_prior_path() {
    // key 构造器（复用 facing_node_key 基座 + `.attack_showdown` 后缀）。
    assert_eq!(
        attack_showdown_node_key(3, "srp"),
        "river.srp.attack_showdown"
    );
    assert_eq!(
        attack_showdown_node_key(1, "3bp"),
        "flop.3bp.attack_showdown"
    );
    assert_eq!(
        attack_showdown_node_key(2, "limped"),
        "turn.limped.attack_showdown"
    );
    // §D 走独立 map + 独立 readout（showdown_readout），**不经 node_field_prior** → 空样本返回
    // showdowns=0 / raw_nonnut=None（非某 fold 先验数值污染，比 fold-to-raise 共用 facing map 更干净）。
    let t = OpponentTable::new();
    let r = t.showdown_readout("ghost", "river.srp.attack_showdown");
    assert_eq!((r.showdowns, r.nonnut), (0, 0));
    assert_eq!(r.raw_nonnut, None);
    assert_eq!(r.by_made_class, [0u32; 6]);
    // 对照：基座 fold-to-cbet 节点的 node_field_prior 未被 §D 扰动。
    assert!((node_field_prior("river.srp").fold - 0.50).abs() < 1e-12);
}

// ---------------------------------------------------------------------------
// 16. §D 多街归属：triple-barrel 摊牌 → flop/turn/river 三节点各记一次（同 final-board 牌力）
// ---------------------------------------------------------------------------
#[test]
fn showdown_multistreet_attribution() {
    let mut t = OpponentTable::new();
    let actions = [
        act("hero", 0, "raise"),
        act("villain", 0, "call"),
        act("villain", 1, "bet"),
        act("hero", 1, "call"),
        act("villain", 2, "bet"),
        act("hero", 2, "call"),
        act("villain", 3, "bet"),
        act("hero", 3, "call"),
    ];
    t.observe_showdown(
        &actions,
        &[sd("villain", 5, true, true), sd("hero", 4, false, false)],
    );
    for node in [
        "flop.srp.attack_showdown",
        "turn.srp.attack_showdown",
        "river.srp.attack_showdown",
    ] {
        let r = t.by_name["villain"]
            .attack_showdown
            .get(node)
            .unwrap_or_else(|| panic!("villain 应有 {node}"));
        assert_eq!((r.showdowns, r.nonnut), (1, 1));
        assert_eq!(r.by_made_class[5], 1); // 同一 final-board 牌力（Monster）
    }
    // hero 只跟 → observe_showdown 从不为其建条目。
    assert!(!t.by_name.contains_key("hero"));
}

// ---------------------------------------------------------------------------
// 17. 后向兼容：旧 profile（无 attack_showdown）仍加载 + 含 §D key 的 profile round-trip + 缺 by_made_class 默认
// ---------------------------------------------------------------------------
#[test]
fn backward_compat_attack_showdown_key_roundtrip() {
    // 旧 profile（无 facing / attack_showdown）仍加载，且 round-trip 不写空 §D 字段（逐字节兼容）。
    let old = r#"{"by_name":{"A":{"hands":10,"vpip_opp":5,"vpip_cnt":3,"pfr_opp":5,"pfr_cnt":2,"aggr_actions":4,"call_actions":3}}}"#;
    let t_old = OpponentTable::from_json_str(old);
    assert!(t_old.by_name["A"].attack_showdown.is_empty());
    assert!(t_old.by_name["A"].facing.is_empty());
    let round_old = t_old.to_json_string();
    assert!(!round_old.contains("attack_showdown"));

    // 含 facing + attack_showdown 双结构的 profile 反序列化。
    let json = r#"{"by_name":{"villain":{"hands":50,"vpip_opp":30,"vpip_cnt":15,"pfr_opp":30,"pfr_cnt":10,"aggr_actions":20,"call_actions":15,"facing":{"flop.srp":{"n":10,"folds":4,"calls":5,"raises":1}},"attack_showdown":{"river.srp.attack_showdown":{"showdowns":8,"nonnut":3,"by_made_class":[0,0,1,2,2,3]}}}}}"#;
    let t = OpponentTable::from_json_str(json);
    let sc = t.by_name["villain"]
        .attack_showdown
        .get("river.srp.attack_showdown")
        .expect("attack_showdown 节点应反序列化");
    assert_eq!((sc.showdowns, sc.nonnut), (8, 3));
    // 早期 §D profile 只写 nonnut（无 nonnut_flush）→ #[serde(default)] 补 0（后向兼容）。
    assert_eq!(sc.nonnut_flush, 0);
    assert_eq!(sc.by_made_class, [0, 0, 1, 2, 2, 3]);
    assert!((sc.raw_nonnut_rate().unwrap() - 3.0 / 8.0).abs() < 1e-9);
    // facing 并存不受影响。
    assert_eq!(t.by_name["villain"].facing.get("flop.srp").unwrap().n, 10);
    // round-trip 保留 attack_showdown key。
    let round = t.to_json_string();
    assert!(round.contains("river.srp.attack_showdown"));
    let t2 = OpponentTable::from_json_str(&round);
    assert_eq!(
        t2.by_name["villain"]
            .attack_showdown
            .get("river.srp.attack_showdown")
            .unwrap()
            .nonnut,
        3
    );

    // 缺 nonnut_flush/by_made_class 的 ShowdownCounts 也反序列化（#[serde(default)] 兜底；OppStats
    // 基础字段仍须齐全）。
    let partial = r#"{"by_name":{"v":{"hands":1,"vpip_opp":0,"vpip_cnt":0,"pfr_opp":0,"pfr_cnt":0,"aggr_actions":0,"call_actions":0,"attack_showdown":{"river.srp.attack_showdown":{"showdowns":2,"nonnut":1}}}}}"#;
    let tp = OpponentTable::from_json_str(partial);
    let scp = tp.by_name["v"]
        .attack_showdown
        .get("river.srp.attack_showdown")
        .unwrap();
    assert_eq!((scp.showdowns, scp.nonnut, scp.nonnut_flush), (2, 1, 0));
    assert_eq!(scp.by_made_class, [0u32; 6]);
}
