//! B1 §A 类：`InfoSetId` 64-bit 编码 + `InfoAbstraction` (preflop 169 + postflop
//! stub) 核心 fixed scenario 测试。
//!
//! 覆盖 `pluribus_stage2_workflow.md` §B1 §输出 A 类清单中 7 条
//! preflop_169_* / info_abs_*，外加 D-211-rev1 / IA-006-rev1 / IA-007 收紧后的
//! 不变量断言（API §9 InfoAbstraction::map 配套约束影响 ③ 字面要求 B1 [测试]
//! 落地三种 TableConfig stack_bucket 桶分配断言）。
//!
//! **B1 状态**：A1 阶段 `PreflopLossless169::*` / `InfoAbstraction::map` /
//! `InfoSetId` getter 等全部 `unimplemented!()`，本文件中的 `#[test]` 在第一次
//! 调用对应方法时 panic（与 stage-1 §B1 `tests/scenarios.rs` 同形态）。
//!
//! **B2 状态**：方法落地后断言激活；本文件保持原文，仅 [实现] 侧填充 stub。
//!
//! 角色边界：本文件属 `[测试]` agent 产物。任一断言被 [实现] 反驳必须由决策者
//! review 后由 [测试] agent 修订。

use poker::{
    BettingState, Card, ChipAmount, GameState, InfoAbstraction, InfoSetId, PreflopLossless169,
    Rank, Suit, TableConfig,
};

// ============================================================================
// 通用 fixture
// ============================================================================

/// 6-max 默认 100BB 配置 + seed=0。
fn default_state(seed: u64) -> (GameState, TableConfig) {
    let cfg = TableConfig::default_6max_100bb();
    let state = GameState::new(&cfg, seed);
    (state, cfg)
}

/// 自定义 stacks 的 6-max 配置（D-211-rev1 stack_bucket 来源 = TableConfig
/// initial_stack）。
fn state_with_stacks(stack_bb: u64, seed: u64) -> (GameState, TableConfig) {
    let mut cfg = TableConfig::default_6max_100bb();
    let stack_chips = stack_bb * 100; // BB = 100
    cfg.starting_stacks = vec![ChipAmount::new(stack_chips); 6];
    let state = GameState::new(&cfg, seed);
    (state, cfg)
}

/// 构造手牌：A♠A♥（最强 pocket pair，D-217 hand_class_169 = 12）。
fn aa_spades_hearts() -> [Card; 2] {
    [
        Card::new(Rank::Ace, Suit::Spades),
        Card::new(Rank::Ace, Suit::Hearts),
    ]
}

/// 构造手牌：A♠A♣（与 [A♠, A♥] 不同的 AA 实例，但 D-217 hand_class 相同）。
fn aa_spades_clubs() -> [Card; 2] {
    [
        Card::new(Rank::Ace, Suit::Spades),
        Card::new(Rank::Ace, Suit::Clubs),
    ]
}

/// 构造手牌：A♠K♠（AKs，D-217 hand_class_169 = 90）。
fn aks() -> [Card; 2] {
    [
        Card::new(Rank::Ace, Suit::Spades),
        Card::new(Rank::King, Suit::Spades),
    ]
}

/// 构造手牌：A♠K♥（AKo，D-217 hand_class_169 = 168）。
fn ako() -> [Card; 2] {
    [
        Card::new(Rank::Ace, Suit::Spades),
        Card::new(Rank::King, Suit::Hearts),
    ]
}

// ============================================================================
// 1. preflop_169_aces_canonical
// ============================================================================
//
// D-217 锚点：AA → hand_class = 12（pockets 0..13 段最大）。同 AA 的不同花色
// 组合（A♠A♥ vs A♠A♣）必须映射到相同 hand_class。
#[test]
fn preflop_169_aces_canonical() {
    let abs = PreflopLossless169::new();

    let aa1 = abs.hand_class(aa_spades_hearts());
    let aa2 = abs.hand_class(aa_spades_clubs());
    assert_eq!(
        aa1, 12,
        "D-217 锚点：AA hand_class = 12（pockets 段最大 id）"
    );
    assert_eq!(aa1, aa2, "D-217 / IA-001：AA 不同花色对应同一 169 类");

    // hole_count_in_class(12) = 6（pocket pair 6 组合，C(4,2)）。
    assert_eq!(
        PreflopLossless169::hole_count_in_class(12),
        6,
        "D-217：pocket pair 类内 6 组合"
    );
}

// ============================================================================
// 2. preflop_169_suited_offsuit_distinction
// ============================================================================
//
// D-217 锚点：AKs → hand_class = 90（suited 段最大 id）；AKo → hand_class
// = 168（offsuit 段最大 id）。Suited 与 offsuit 不可混入同一 169 类（IA-001）。
#[test]
fn preflop_169_suited_offsuit_distinction() {
    let abs = PreflopLossless169::new();

    let aks_class = abs.hand_class(aks());
    let ako_class = abs.hand_class(ako());

    assert_eq!(
        aks_class, 90,
        "D-217 锚点：AKs hand_class = 90（suited 段最大 id）"
    );
    assert_eq!(
        ako_class, 168,
        "D-217 锚点：AKo hand_class = 168（offsuit 段最大 id）"
    );
    assert_ne!(aks_class, ako_class, "D-217：suited vs offsuit 必须区分");

    // hole_count：suited 4 组合（4 花色）/ offsuit 12 组合（4×3 花色对）。
    assert_eq!(
        PreflopLossless169::hole_count_in_class(aks_class),
        4,
        "D-217：suited 类内 4 组合"
    );
    assert_eq!(
        PreflopLossless169::hole_count_in_class(ako_class),
        12,
        "D-217：offsuit 类内 12 组合"
    );
}

// ============================================================================
// 3. preflop_169_position_changes_infoset
// ============================================================================
//
// IA-002：同一 hand_class_169 在不同 position_bucket 下产出不同 InfoSetId
// （bit 24..28 编码差异）。最简比对：seat 0 (BTN) vs seat 3 (UTG) 在同一 seed
// 下持有 AA，hand_class 相同但 InfoSetId 不同。
#[test]
fn preflop_169_position_changes_infoset() {
    let abs = PreflopLossless169::new();

    // 6-max default：UTG (seat 3) 起手决策。第一手 InfoSetId.position_bucket
    // = UTG 桶。
    let (s_utg, _cfg) = default_state(0);
    let infoset_utg: InfoSetId = abs.map(&s_utg, aa_spades_hearts());

    // 推进到 BTN (seat 0)：UTG / MP / CO fold，BTN 决策。
    let mut s_btn = s_utg.clone();
    use poker::{Action, SeatId};
    for seat_idx in [3u8, 4, 5] {
        assert_eq!(s_btn.current_player(), Some(SeatId(seat_idx)));
        s_btn.apply(Action::Fold).expect("preflop fold");
    }
    assert_eq!(s_btn.current_player(), Some(SeatId(0)), "BTN 决策");
    let infoset_btn = abs.map(&s_btn, aa_spades_hearts());

    // 同 hand AA，不同 actor position → InfoSetId 必须不同。
    assert_ne!(
        infoset_utg.raw(),
        infoset_btn.raw(),
        "IA-002：position 不同 → InfoSetId 不同"
    );
    assert_eq!(
        infoset_utg.bucket_id(),
        infoset_btn.bucket_id(),
        "D-215 / D-217：bucket_id (= hand_class_169) 同 hand 相同"
    );
    assert_ne!(
        infoset_utg.position_bucket(),
        infoset_btn.position_bucket(),
        "D-210：position_bucket 不同"
    );
}

// ============================================================================
// 4. preflop_169_stack_bucket_changes_infoset
// ============================================================================
//
// D-211-rev1：stack_bucket 来源钉到 `TableConfig::initial_stack(seat) /
// big_blind`。100 BB / 200 BB / 50 BB 三种配置下 actor 落入不同桶（D-211 5 桶
// `[0,20)/[20,50)/[50,100)/[100,200)/[200,+∞)`，桶号 0..4）：
//
// - 100 BB → bucket id 3（`[100, 200)`）
// - 200 BB → bucket id 4（`[200, +∞)`）
// - 50 BB → bucket id 2（`[50, 100)`）
//
// 同 hand_class_169 + position_bucket + betting_state 下，stack_bucket 不同 →
// InfoSetId.raw() 不同。F21 / API §9 InfoAbstraction::map 配套约束影响 ③
// 字面要求 B1 [测试] 落地此断言。
#[test]
fn preflop_169_stack_bucket_changes_infoset() {
    let abs = PreflopLossless169::new();

    let (s_100bb, _) = state_with_stacks(100, 0);
    let (s_200bb, _) = state_with_stacks(200, 0);
    let (s_50bb, _) = state_with_stacks(50, 0);

    let info_100 = abs.map(&s_100bb, aa_spades_hearts());
    let info_200 = abs.map(&s_200bb, aa_spades_hearts());
    let info_50 = abs.map(&s_50bb, aa_spades_hearts());

    // D-211 5 桶映射（左闭右开）：
    assert_eq!(
        info_100.stack_bucket(),
        3,
        "D-211：100 BB → stack_bucket 3 ([100, 200))"
    );
    assert_eq!(
        info_200.stack_bucket(),
        4,
        "D-211：200 BB → stack_bucket 4 ([200, +∞))"
    );
    assert_eq!(
        info_50.stack_bucket(),
        2,
        "D-211：50 BB → stack_bucket 2 ([50, 100))"
    );

    // 三种 stack_bucket → 三种不同 InfoSetId。
    assert_ne!(info_100.raw(), info_200.raw(), "stack 100 vs 200 不同");
    assert_ne!(info_100.raw(), info_50.raw(), "stack 100 vs 50 不同");
    assert_ne!(info_200.raw(), info_50.raw(), "stack 200 vs 50 不同");
}

// ============================================================================
// 5. preflop_169_prior_action_changes_infoset
// ============================================================================
//
// D-212：betting_state 5 状态展开，preflop 局面区分 `FacingBetNoRaise`
// （非 BB 位首次面对 BB 强制下注）vs `FacingRaise1`（已发生 1 次 voluntary
// raise）。同 actor / hand / stack 下，betting_state 不同 → InfoSetId 不同。
//
// 比对路径：seed=0 同手内 UTG 决策（FacingBetNoRaise，盲注后无 voluntary
// raise）→ UTG raise → MP 决策（FacingRaise1）。两个决策点 actor 不同，但
// 我们用 seed=0 + 同 hand_class 强制对照（人造同 hand_class 不同 betting_state
// 路径）。
//
// 简化版：UTG 决策点 betting_state ∈ {FacingBetNoRaise}，UTG-raise → MP 决策点
// betting_state ∈ {FacingRaise1}。
#[test]
fn preflop_169_prior_action_changes_infoset() {
    let abs = PreflopLossless169::new();
    let (mut s, _cfg) = default_state(0);

    use poker::{Action, SeatId};

    // UTG 决策点 betting_state = FacingBetNoRaise（盲注后无 voluntary raise）。
    assert_eq!(s.current_player(), Some(SeatId(3)));
    let info_utg_first = abs.map(&s, aa_spades_hearts());
    assert_eq!(
        info_utg_first.betting_state(),
        BettingState::FacingBetNoRaise,
        "D-212：UTG 开局面对 BB 盲注 = FacingBetNoRaise"
    );

    // UTG raise → MP 决策点 betting_state = FacingRaise1。
    s.apply(Action::Raise {
        to: ChipAmount::new(300),
    })
    .expect("UTG open raise");
    assert_eq!(s.current_player(), Some(SeatId(4)));
    let info_mp_after_raise = abs.map(&s, aa_spades_hearts());
    assert_eq!(
        info_mp_after_raise.betting_state(),
        BettingState::FacingRaise1,
        "D-212：UTG raise 后 MP = FacingRaise1"
    );

    // 两个 InfoSetId.raw() 必须不同（betting_state 字段差异）。
    assert_ne!(
        info_utg_first.raw(),
        info_mp_after_raise.raw(),
        "D-212：betting_state 不同 → InfoSetId 不同"
    );
}

// ============================================================================
// 6. info_abs_postflop_bucket_id_in_range
// ============================================================================
//
// IA-003：`PostflopBucketAbstraction::bucket_id(...)` 返回值 ∈
// `[0, BucketConfig.{street})` 且 `< 2^24`。**B2 阶段** PostflopBucketAbstraction
// 用 stub 实现（每条街固定返回 bucket_id = 0）；C2 才接入真实 mmap。
//
// **B1 状态**：本测试 `#[ignore]`——构造 PostflopBucketAbstraction 需要 mmap
// 文件，B2 [实现] 决定 stub 路径（典型：内存中假 BucketTable 或 test-only
// 构造器）；B1 测试结构与断言落位但不进默认 cargo test。详见
// `pluribus_stage2_workflow.md` §B1 line 222 "info_abs_postflop_bucket_id_in_range
// （C2 前用 stub bucket）" + §B2 line 274 "PostflopBucketAbstraction 占位实现
// （C2 才完整）：每条街固定返回 bucket_id = 0"。
#[test]
#[ignore = "B2: PostflopBucketAbstraction stub 构造路径未定，留 B2 [实现] 决定"]
fn info_abs_postflop_bucket_id_in_range() {
    // 占位断言结构：构造 flop street state + AA 起手 + PostflopBucketAbstraction
    // (stub) → 断言 bucket_id < bucket_count(StreetTag::Flop) ≤ 2^24。
    //
    // B2 [实现] 落地后取消 #[ignore]：测试体应类似下方伪代码（具体获取
    // PostflopBucketAbstraction 的路径由 B2 决定，B1 不锁定）：
    //
    // ```ignore
    // let postflop_abs: PostflopBucketAbstraction = /* B2 提供 stub 构造路径 */;
    // let cfg = postflop_abs.config();
    // let state_on_flop = drive_to_flop(/* ... */);
    // let bucket = postflop_abs.bucket_id(&state_on_flop, aa_spades_hearts());
    // assert!(bucket < cfg.flop, "IA-003: bucket_id < bucket_count(flop)");
    // assert!(bucket < (1u32 << 24), "IA-003: bucket_id < 2^24 (D-215 字段宽度)");
    // ```
    panic!("B1 placeholder：B2 [实现] 落地 PostflopBucketAbstraction stub 后取消 #[ignore]");
}

// ============================================================================
// 7. info_abs_determinism_repeat_smoke
// ============================================================================
//
// IA-004 deterministic：`map(state, hole)` 重复调用结果 byte-equal。**B1 默认
// 1k**；full 1M 留 D1（与 stage-1 §B1 同形态：B1 1k smoke，D1 才接 1M）。
#[test]
fn info_abs_determinism_repeat_smoke() {
    let abs = PreflopLossless169::new();
    let (s, _cfg) = default_state(0);
    let hole = aa_spades_hearts();

    let baseline = abs.map(&s, hole);
    for i in 0..1_000 {
        let other = abs.map(&s, hole);
        assert_eq!(baseline.raw(), other.raw(), "IA-004 iter {i}: byte-equal");
    }
}

// ============================================================================
// 8. IA-007 reserved 位为零
// ============================================================================
//
// IA-007：`InfoSetId.raw()` 的 bit 38..64（26 bit）必须全为 0。任一非零 bit
// 写入是 P0 阻塞 bug。
#[test]
fn info_id_reserved_bits_must_be_zero() {
    let abs = PreflopLossless169::new();
    let (s, _cfg) = default_state(0);

    let info = abs.map(&s, aa_spades_hearts());
    let raw = info.raw();
    let reserved_mask: u64 = !((1u64 << 38) - 1); // bit 38..64
    assert_eq!(
        raw & reserved_mask,
        0,
        "IA-007：bit 38..64 reserved 位必须全为 0（raw = {raw:#016x}）"
    );
}
