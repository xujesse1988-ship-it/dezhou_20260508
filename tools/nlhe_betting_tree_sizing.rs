//! 简化 NLHE 抽象 betting tree 决策节点数 sizing 工具。
//!
//! 从 `GameState` root 出发 DFS 枚举所有 reachable 抽象动作序列，针对一组候选
//! `raise_pot_ratios` + 牌桌 profile（座位数 / 起始码量）配置打印决策节点数、
//! infoset 数、按街分布、深度直方图、`node_id` 位宽。与 `PublicBettingTree::build`
//! 走同一抽象 + 同一 root 路径单射性质，节点计数与 tree 实际构造一致。
//!
//! Phase 0（dense infoset table）：另打印 full-prealloc dense 布局 sizing——
//! `total_rows`（Σ bucket_count，应 == infoset 数）、`total_slots`（Σ bucket_count ×
//! action_count，variable-action 布局的 f64 数）、per-street rows/slots、action_count
//! 直方图，以及 regret+strategy 两表在 variable / 固定 stride 6 / stride 8 三种布局下的
//! 内存估算 + visited bitset 体量。用来确认目标 profile 的 variable 两表能否落进
//! 32–64 GB 目标机器。
//!
//! 6-max（S2）：`walk` 本身不假设玩家数，只走 `current_player` / `street` /
//! `abstract_actions` / `apply`，所以换 `default_6max_100bb()` 即枚举 6-max 树。
//! 6-max 树可能远大于 HU（玩家数 2→6 让 preflop 动作序列爆炸），故加 `NODE_CAP`：
//! 决策节点数到上限即停止下探并标记 capped，把"是否大到无法枚举"本身当作 sizing
//! 结论返回，而不是跑到 OOM / 不收敛。
//!
//! 支持 per-street raise 集合（street-dependent action abstraction 的 sizing 探针）：
//! 每条街用各自的 `DefaultActionAbstraction`，按 `state.street()` 分派。
//!
//! B3 摘要探针（`B3_SUMMARY=1`，docs/temp/betting_history_abstraction_options_2026_05_31.md
//! §B3）：除了按 `node_id`（完美回忆完整路径）的计数，另在 walk 中为每个决策节点
//! 计算 betting-state 摘要 key = `(street, actor_position 相对 button, live_players
//! bitmask, raises_this_street{cap3}, facing_size_bucket, spr_bucket, last_aggressor
//! 2 槽 preflop+postflop 相对 button)`，统计 **distinct key 数 / B3 infoset 数 /
//! B3 dense 表体量**，并断言"同 key 的所有节点 legal_actions 一致"（不变量：key 必须
//! 决定合法动作集，否则重蹈 F17）。把"下注历史摘成有界 key 后还剩多大"变成数字。

use std::collections::{BTreeMap, HashMap};
use std::process::ExitCode;

use poker::{
    AbstractAction, ActionAbstraction, ActionAbstractionConfig, BetRatio, ChaCha20Rng,
    DefaultActionAbstraction, GameState, PlayerStatus, RngSource, TableConfig,
};

const WALK_SEED: u64 = 0x4E4C_4845_5F53_5A4E; // "NLHE_SZN"

/// 决策节点枚举上限。到上限即停止下探（标记 capped）。6-max 树可能 ≫ 这个数，
/// 那本身就是结论：该抽象在全宽枚举 / 单机 dense 表下不可行。
const NODE_CAP: u64 = 100_000_000;

/// 每条街的 bucket 数（preflop = lossless hand class，postflop = K-means 桶）。
#[derive(Clone, Copy)]
struct BucketCounts {
    preflop: u64,
    postflop: u64,
}

impl BucketCounts {
    fn for_street(&self, street: u8) -> u64 {
        if street == 0 {
            self.preflop
        } else {
            self.postflop
        }
    }
}

/// B3 摘要 key 的逐 key 元数据（取该 key 首次出现节点的合法动作签名 + 动作数）。
#[derive(Clone, Copy)]
struct B3KeyMeta {
    /// 合法动作集签名（Fold/Check/Call/AllIn + 各 ratio 的 Bet/Raise 位）。
    action_sig: u64,
    /// 合法动作数（dense stride）。
    action_count: u32,
}

#[derive(Default)]
struct Stats {
    decision_nodes: u64,
    terminal_nodes: u64,
    per_street: BTreeMap<u8, u64>,
    per_street_player: BTreeMap<(u8, u8), u64>,
    depth_histogram: BTreeMap<u32, u64>,
    max_depth: u32,
    /// 枚举是否因 `NODE_CAP` 被截断（true → 下面所有计数是 lower bound）。
    capped: bool,
    // ---- dense full-prealloc 布局 sizing（Phase 0）----
    /// Σ bucket_count(node)；dense 表的 row 数，应当 == `infosets()`。
    total_rows: u64,
    /// Σ bucket_count(node) × action_count(node)；variable-action 布局的 f64 slot 数。
    total_slots: u64,
    per_street_rows: BTreeMap<u8, u64>,
    per_street_slots: BTreeMap<u8, u64>,
    /// action_count（= legal_actions.len()）→ 节点数直方图。
    action_count_hist: BTreeMap<usize, u64>,
    // ---- B3 摘要探针（仅 b3_enabled 时填）----
    /// distinct 摘要 key → meta（key 低 2 bit 编码 street，便于按街还原）。
    b3_keys: HashMap<u64, B3KeyMeta>,
    /// 同一 key 出现过不同 legal_actions 签名的次数（> 0 = 不变量被破，key 不够决定动作集）。
    b3_violations: u64,
    /// 首个违规示例 `(key, 已存签名, 新签名)`，便于定位。
    b3_first_violation: Option<(u64, u64, u64)>,
}

impl Stats {
    /// infoset 数 = Σ node_count(street) × bucket_count(street)。
    fn infosets(&self, buckets: &BucketCounts) -> u64 {
        self.per_street
            .iter()
            .map(|(street, count)| count * buckets.for_street(*street))
            .sum()
    }

    /// 记录一个决策节点的 B3 摘要 key + 合法动作签名。首见即存；重见则校验签名一致
    /// （不一致 = 不变量违规，累计并记首例）。
    fn record_b3(&mut self, key: u64, sig: u64, action_count: u32) {
        match self.b3_keys.get(&key).map(|m| m.action_sig) {
            Some(existing) if existing != sig => {
                self.b3_violations += 1;
                if self.b3_first_violation.is_none() {
                    self.b3_first_violation = Some((key, existing, sig));
                }
            }
            Some(_) => {}
            None => {
                self.b3_keys.insert(
                    key,
                    B3KeyMeta {
                        action_sig: sig,
                        action_count,
                    },
                );
            }
        }
    }
}

/// 跨街 aggressor 追踪（B3 `last_aggressor` 2 槽）。值 = 相对 button 的座位 relpos，
/// `NONE`(7) 表示该街/该手尚无 voluntary 进攻。preflop 槽只在 preflop 更新、postflop
/// 槽在 flop/turn/river 任一进攻更新（postflop 线最近一个 aggressor，不随街清零）。
#[derive(Clone, Copy)]
struct Aggr {
    pre: u8,
    post: u8,
}

const AGGR_NONE: u8 = 7;

impl Default for Aggr {
    fn default() -> Self {
        Aggr {
            pre: AGGR_NONE,
            post: AGGR_NONE,
        }
    }
}

/// 相对 button 的座位号 = `(seat + n - button) % n`，范围 `0..n`。
fn relpos(seat: u8, button: u8, n: u8) -> u8 {
    (seat + n - button) % n
}

/// SPR 桶（有效剩余筹码 / 当前底池），log 间距 12 桶（0..11）。
fn spr_bucket(eff_stack: u64, pot: u64) -> u8 {
    if pot == 0 {
        return 11;
    }
    let spr = eff_stack as f64 / pot as f64;
    const BOUNDS: [f64; 11] = [0.25, 0.5, 1.0, 2.0, 3.0, 4.0, 6.0, 9.0, 13.0, 20.0, 35.0];
    let mut idx = 0u8;
    for &b in BOUNDS.iter() {
        if spr > b {
            idx += 1;
        } else {
            break;
        }
    }
    idx
}

/// 面对的下注尺寸桶：0 = 无活注（可 check）、1 = ≤0.5p、2 = ~1p、3 = ≥~2p、
/// 4 = 跟注即 all-in（to_call ≥ 自己 stack）。
fn facing_bucket(state: &GameState, actor_idx: usize) -> u8 {
    let legal = state.legal_actions();
    if legal.check {
        return 0;
    }
    let Some(call_to) = legal.call else {
        return 0;
    };
    let p = &state.players()[actor_idx];
    let committed = p.committed_this_round.as_u64();
    let to_call = call_to.as_u64().saturating_sub(committed);
    let stack = p.stack.as_u64();
    if to_call >= stack {
        return 4;
    }
    let pot = state.pot().as_u64();
    if pot == 0 {
        return 2;
    }
    let r = to_call as f64 / pot as f64;
    if r <= 0.5 {
        1
    } else if r <= 1.5 {
        2
    } else {
        3
    }
}

/// 在场 bitmask（相对 button，`Active∪AllIn`=1，`Folded`=0），6-max → 6 bit。
fn live_mask(state: &GameState, button: u8, n: u8) -> u8 {
    let mut m = 0u8;
    for p in state.players() {
        if matches!(p.status, PlayerStatus::Active | PlayerStatus::AllIn) {
            m |= 1 << relpos(p.seat.0, button, n);
        }
    }
    m
}

/// 组装 B3 摘要 key（u64，仅探针用：字段笛卡尔积的单射打包，不必对齐生产位布局）。
/// 低 2 bit = street，便于 report 按街还原。
fn b3_key(state: &GameState, actor_seat: u8, street: u8, raises_on_street: u32, aggr: Aggr) -> u64 {
    let cfg = state.config();
    let n = cfg.n_seats;
    let button = cfg.button_seat.0;
    let apos = relpos(actor_seat, button, n) as u64; // 3 bit
    let lmask = live_mask(state, button, n) as u64; // 6 bit
    let raises = raises_on_street.min(3) as u64; // 2 bit
    let facing = facing_bucket(state, actor_seat as usize) as u64; // 3 bit
    let eff = state
        .players()
        .iter()
        .filter(|p| matches!(p.status, PlayerStatus::Active))
        .map(|p| p.stack.as_u64())
        .min()
        .unwrap_or(0);
    let spr = spr_bucket(eff, state.pot().as_u64()) as u64; // 4 bit
    let pre = aggr.pre as u64; // 3 bit (7=none)
    let post = aggr.post as u64; // 3 bit
    (street as u64 & 0b11)
        | (apos << 2)
        | (lmask << 5)
        | (raises << 11)
        | (facing << 13)
        | (spr << 16)
        | (pre << 20)
        | (post << 23)
}

/// 把 ratio 标签压成 0..3 的小下标（500/1000/2000 → 0/1/2，其它 → 3）。
fn ratio_idx(r: BetRatio) -> u32 {
    match r.as_milli() {
        500 => 0,
        1000 => 1,
        2000 => 2,
        _ => 3,
    }
}

/// 合法动作集签名（位掩码）：Fold=bit0 Check=bit1 Call=bit2 AllIn=bit3，
/// Bet(ratio)=bit(4+idx)、Raise(ratio)=bit(10+idx)。用于 B3 不变量校验。
fn action_sig(actions: &[AbstractAction]) -> u64 {
    let mut s = 0u64;
    for a in actions {
        match a {
            AbstractAction::Fold => s |= 1 << 0,
            AbstractAction::Check => s |= 1 << 1,
            AbstractAction::Call { .. } => s |= 1 << 2,
            AbstractAction::AllIn { .. } => s |= 1 << 3,
            AbstractAction::Bet { ratio_label, .. } => s |= 1 << (4 + ratio_idx(*ratio_label)),
            AbstractAction::Raise { ratio_label, .. } => s |= 1 << (10 + ratio_idx(*ratio_label)),
        }
    }
    s
}

/// 某动作是否进攻（用于 raise 计数 + aggressor 追踪）。`raises_this_street` 沿用
/// 工具原口径只数 Bet/Raise（AllIn 不计入 cap）；aggressor 追踪则把 AllIn 也算进攻
/// （all-in 是把筹码推进去的攻击，对 range 不对称有意义）。
fn is_bet_or_raise(a: &AbstractAction) -> bool {
    matches!(a, AbstractAction::Bet { .. } | AbstractAction::Raise { .. })
}
fn is_aggression_for_aggressor(a: &AbstractAction) -> bool {
    matches!(
        a,
        AbstractAction::Bet { .. } | AbstractAction::Raise { .. } | AbstractAction::AllIn { .. }
    )
}

/// A1 raise cap（每街 (Bet+Raise) 聚合上限）：`raises_on_street` 是到达本节点前
/// 本街已发生的 voluntary 进攻动作（`Bet` + `Raise`，对齐 `BettingState` 的
/// `FacingBetNoRaise`/`FacingRaise{1,2,3+}` 计数）次数。到达 `raise_cap` 后，
/// 本节点合法集里只留 `Fold/Check/Call/AllIn`——`AllIn` 始终保留（escape hatch，
/// 不计入 cap），砍掉的只是 sized `Bet`/`Raise` 这条组合爆炸链。`raise_cap = u32::MAX`
/// 等价无 cap（与历史行为逐字节一致，见 run() 的 huge-cap self-check）。
/// `drop_small_reraise`：first-bet-small 规则——0.5pot 只许作开池 `Bet`，禁掉
/// 任何 `Raise { ratio_label = HALF_POT }`（re-raise 一律走 1pot）。preflop 集为
/// {1.0} 时该过滤天然不触发（无 0.5），只在 postflop {0.5,1.0} 处生效。与
/// `raise_cap` 可叠加。
/// `b3_summary`：开 B3 摘要 key 探针（见文件头）。
/// `b3_pin_actions`：把合法动作集签名折进 B3 key（bits 26+）。不开时 key 只含字段元组，
/// 用于暴露"字段是否足以决定合法动作集"（不变量校验）；开时 key 按构造决定动作集
/// （生产修法：dense stride 要求同 key 同动作集），测的是修法后的真实有界规模。
/// 递归过程中不变的配置（打包以免 `walk` 参数过多）。
struct WalkCfg<'a> {
    abs_by_street: &'a [DefaultActionAbstraction; 4],
    buckets: &'a BucketCounts,
    raise_cap: u32,
    drop_small_reraise: bool,
    b3_summary: bool,
    b3_pin_actions: bool,
}

fn walk(
    state: &GameState,
    depth: u32,
    raises_on_street: u32,
    aggr: Aggr,
    cfg: &WalkCfg,
    stats: &mut Stats,
) {
    if state.is_terminal() {
        stats.terminal_nodes += 1;
        return;
    }

    // NODE_CAP：到上限停止下探，把 capped 当结论返回。
    if stats.decision_nodes >= NODE_CAP {
        stats.capped = true;
        return;
    }

    stats.decision_nodes += 1;
    let street = state.street() as u8;
    let actor = state
        .current_player()
        .expect("non-terminal state must have current_player")
        .0;
    *stats.per_street.entry(street).or_default() += 1;
    *stats.per_street_player.entry((street, actor)).or_default() += 1;
    *stats.depth_histogram.entry(depth).or_default() += 1;
    if depth > stats.max_depth {
        stats.max_depth = depth;
    }

    let abs = &cfg.abs_by_street[street as usize];
    let legal_set = abs.abstract_actions(state);

    // 动作过滤：① A1 raise cap 到顶剔 sized Bet/Raise；② first-bet-small 剔 0.5pot 的 Raise。
    // 二者只砍 sized 进攻，Fold/Check/Call/AllIn 永远保留。
    let cap_reached = raises_on_street >= cfg.raise_cap;
    let actions: Vec<AbstractAction> = legal_set
        .iter()
        .copied()
        .filter(|a| {
            if cap_reached && is_bet_or_raise(a) {
                return false;
            }
            if cfg.drop_small_reraise {
                if let AbstractAction::Raise { ratio_label, .. } = a {
                    if *ratio_label == BetRatio::HALF_POT {
                        return false;
                    }
                }
            }
            true
        })
        .collect();

    // dense 布局累加：本节点贡献 bucket_count 行、bucket_count × action_count 个 slot。
    let action_count = actions.len() as u64;
    let rows = cfg.buckets.for_street(street);
    stats.total_rows += rows;
    stats.total_slots += rows * action_count;
    *stats.per_street_rows.entry(street).or_default() += rows;
    *stats.per_street_slots.entry(street).or_default() += rows * action_count;
    *stats.action_count_hist.entry(actions.len()).or_default() += 1;

    // B3 摘要 key：用本节点 incoming 状态（aggr 反映到达此节点前的攻击）。
    if cfg.b3_summary {
        let sig = action_sig(&actions);
        let mut key = b3_key(state, actor, street, raises_on_street, aggr);
        if cfg.b3_pin_actions {
            // 把合法动作集签名折进高位（生产修法：key 必须决定动作集，否则 dense
            // stride 与 regret 槽错位 = F17）。此后 record_b3 的不变量恒成立，
            // distinct key 数即修法后的真实规模。
            key |= sig << 26;
        }
        stats.record_b3(key, sig, actions.len() as u32);
    }

    for action in actions {
        let mut next_state = state.clone();
        next_state
            .apply(action.to_concrete())
            .expect("DefaultActionAbstraction must emit legal actions");
        // 街切换则进攻计数清零；否则 Bet/Raise +1，其它（Call/Check/AllIn）不变。
        let next_street = next_state.street() as u8;
        let next_raises = if next_street != street {
            0
        } else if is_bet_or_raise(&action) {
            raises_on_street + 1
        } else {
            raises_on_street
        };
        // aggressor 追踪：本动作若进攻，更新对应槽（相对 button）。postflop 槽跨街不清零。
        let mut next_aggr = aggr;
        if is_aggression_for_aggressor(&action) {
            let rp = relpos(actor, state.config().button_seat.0, state.config().n_seats);
            if street == 0 {
                next_aggr.pre = rp;
            } else {
                next_aggr.post = rp;
            }
        }
        walk(&next_state, depth + 1, next_raises, next_aggr, cfg, stats);
        if stats.capped {
            return;
        }
    }
}

fn make_abs(raise_ratios: &[f64]) -> DefaultActionAbstraction {
    let cfg = ActionAbstractionConfig::new(raise_ratios.to_vec())
        .expect("raise ratios must satisfy ActionAbstractionConfig::new");
    DefaultActionAbstraction::new(cfg)
}

/// `per_street` = [preflop, flop, turn, river] 各自的 raise ratio 集合。
/// `raise_cap` = 每街 (Bet+Raise) 聚合上限（`u32::MAX` = 无 cap）。
/// `drop_small_reraise` = first-bet-small 规则（见 `walk`）。
/// `b3_summary` = 开 B3 摘要 key 探针；`b3_pin_actions` = 把动作集签名折进 key。
fn measure(
    table_cfg: &TableConfig,
    per_street: [&[f64]; 4],
    buckets: &BucketCounts,
    raise_cap: u32,
    drop_small_reraise: bool,
    b3_summary: bool,
    b3_pin_actions: bool,
) -> Stats {
    let abs_by_street = [
        make_abs(per_street[0]),
        make_abs(per_street[1]),
        make_abs(per_street[2]),
        make_abs(per_street[3]),
    ];
    let mut rng = ChaCha20Rng::from_seed(WALK_SEED);
    let state = GameState::with_rng(table_cfg, 0, &mut rng as &mut dyn RngSource);

    let cfg = WalkCfg {
        abs_by_street: &abs_by_street,
        buckets,
        raise_cap,
        drop_small_reraise,
        b3_summary,
        b3_pin_actions,
    };
    let mut stats = Stats::default();
    walk(&state, 0, 0, Aggr::default(), &cfg, &mut stats);
    stats
}

fn bits_for(n: u64) -> u32 {
    if n == 0 {
        0
    } else {
        64 - (n - 1).leading_zeros()
    }
}

fn street_label(s: u8) -> &'static str {
    match s {
        0 => "Preflop",
        1 => "Flop",
        2 => "Turn",
        3 => "River",
        _ => "Unknown",
    }
}

/// 把 per-street ratio 集合压成一行展示；全街相同则只印一组。
fn ratios_desc(per_street: [&[f64]; 4]) -> String {
    let all_same = per_street.iter().all(|r| r == &per_street[0]);
    if all_same {
        format!("{:?} (all streets)", per_street[0])
    } else {
        format!(
            "pre={:?} flop={:?} turn={:?} river={:?}",
            per_street[0], per_street[1], per_street[2], per_street[3]
        )
    }
}

fn print_stats(label: &str, desc: &str, stats: &Stats, buckets: &BucketCounts) {
    let n = stats.decision_nodes;
    let bits = bits_for(n);
    let infosets = stats.infosets(buckets);

    println!("--- {label} : raise_pot_ratios = {desc} ---");
    println!(
        "Buckets         : preflop={} postflop={}",
        buckets.preflop, buckets.postflop
    );
    if stats.capped {
        println!("⚠ CAPPED        : 枚举到 NODE_CAP={NODE_CAP} 被截断 → 下面计数是 LOWER BOUND，真实树更大");
    }
    println!(
        "Decision nodes  : {n}    [node_id {bits} bit → cover {}]",
        1u64 << bits
    );
    println!(
        "Infosets        : {infosets}  ({:.1}M)",
        infosets as f64 / 1e6
    );
    println!(
        "Terminal nodes  : {}    Max depth : {}",
        stats.terminal_nodes, stats.max_depth
    );

    print!("Per-street      :");
    for (street, count) in &stats.per_street {
        print!(" {}={}", street_label(*street), count);
    }
    println!();

    print_dense_layout(stats, infosets);
    if !stats.b3_keys.is_empty() {
        print_b3_summary(stats, buckets);
    }
    println!();
}

const GIB: f64 = (1u64 << 30) as f64;
const MIB: f64 = (1u64 << 20) as f64;

/// dense full-prealloc 布局 sizing + 内存估算（Phase 0 决策门：variable 两表是否
/// 落得进目标机器）。
fn print_dense_layout(stats: &Stats, infosets: u64) {
    let rows = stats.total_rows;
    let slots = stats.total_slots;
    let avg_ac = slots as f64 / rows as f64;

    // 自洽校验：dense row 数必须等于 infoset 数（每 (node,bucket) 一行）。
    assert_eq!(
        rows, infosets,
        "total_rows {rows} != infosets {infosets}（dense row 与 infoset 应一一对应）"
    );

    println!("Dense rows      : {rows}  (== infosets ✓)");
    println!("Dense slots     : {slots}  (variable-action, avg action_count {avg_ac:.3})");

    print!("Per-street rows :");
    for (s, r) in &stats.per_street_rows {
        print!(" {}={}", street_label(*s), r);
    }
    println!();
    print!("Per-street slots:");
    for (s, sl) in &stats.per_street_slots {
        print!(" {}={}", street_label(*s), sl);
    }
    println!();
    print!("action_count    :");
    for (ac, nodes) in &stats.action_count_hist {
        print!(" {ac}→{nodes}");
    }
    println!();

    // 两张 f64 表（regret + strategy）。variable = 真实布局；stride 6/8 = 固定 stride 对照。
    let var_one = slots * 8;
    let var_two = var_one * 2;
    let stride6_two = rows * 6 * 8 * 2;
    let stride8_two = rows * 8 * 8 * 2;
    let bitset_two = rows.div_ceil(8) * 2;
    println!(
        "Mem variable    : one table {:.2} GiB / regret+strategy {:.2} GiB",
        var_one as f64 / GIB,
        var_two as f64 / GIB
    );
    println!(
        "Mem stride=6    : regret+strategy {:.2} GiB   stride=8 : {:.2} GiB",
        stride6_two as f64 / GIB,
        stride8_two as f64 / GIB
    );
    println!(
        "Visited bitset  : {:.1} MiB (两表合计)",
        bitset_two as f64 / MIB
    );
}

/// B3 摘要 key 探针报告：distinct key（总 + 按街）、B3 infoset、B3 dense 两表体量、
/// 不变量（同 key legal_actions 一致）校验结果。与上面 node_id 计数对照看压缩比。
fn print_b3_summary(stats: &Stats, buckets: &BucketCounts) {
    let mut keys_per_street = [0u64; 4];
    let mut slots_per_street = [0u64; 4];
    for (key, meta) in &stats.b3_keys {
        let street = (key & 0b11) as usize;
        keys_per_street[street] += 1;
        slots_per_street[street] += buckets.for_street(street as u8) * u64::from(meta.action_count);
    }
    let total_keys: u64 = keys_per_street.iter().sum();
    let b3_infosets: u64 = (0..4)
        .map(|s| keys_per_street[s] * buckets.for_street(s as u8))
        .sum();
    let b3_slots: u64 = slots_per_street.iter().sum();
    let avg_ac = b3_slots as f64 / b3_infosets.max(1) as f64;
    let var_two = b3_slots * 8 * 2;

    println!("--- B3 摘要 key 探针 ---");
    if stats.capped {
        println!(
            "⚠ 树枚举 CAPPED → B3 key/infoset 也是 LOWER BOUND（但 key 空间有界，可能已近饱和）"
        );
    }
    print!("B3 distinct key : {total_keys}   按街:");
    for (s, count) in keys_per_street.iter().enumerate() {
        print!(" {}={}", street_label(s as u8), count);
    }
    println!();
    println!(
        "B3 infosets     : {b3_infosets}  ({:.2}M)   [vs node_id infosets {} ({:.1}M)]",
        b3_infosets as f64 / 1e6,
        stats.infosets(buckets),
        stats.infosets(buckets) as f64 / 1e6
    );
    println!(
        "B3 dense slots  : {b3_slots}  (avg action_count {avg_ac:.3})   两表 {:.3} GiB",
        var_two as f64 / GIB
    );
    let node_infosets = stats.infosets(buckets);
    if b3_infosets > 0 {
        println!(
            "压缩比          : node_id/B3 infoset = {:.1}×",
            node_infosets as f64 / b3_infosets as f64
        );
    }
    if stats.b3_violations == 0 {
        println!("不变量          : ✓ 同 key 所有节点 legal_actions 一致（key 决定合法动作集）");
    } else {
        println!(
            "不变量          : ✗ {} 次违规（同 key 不同 legal_actions → key 不足以决定动作集）",
            stats.b3_violations
        );
        if let Some((k, a, b)) = stats.b3_first_violation {
            println!("  首例: key=0x{k:07x}  已存签名=0b{a:014b}  冲突签名=0b{b:014b}");
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    const R3: &[f64] = &[0.5, 1.0, 2.0]; // HU 现 6-action {0.5p,1p,2p}（self-check 用）

    // 6-max raise 集合从 argv 读（f64 列表，全街同集）；无参默认 {1.0}。
    // postflop 桶数从 env XV_POSTFLOP 读（默认 200）；preflop 固定 169 lossless。
    // 例：cargo run --release --bin nlhe_betting_tree_sizing -- 0.5 1.0
    //     XV_POSTFLOP=500 cargo run ... -- 1.0
    let argv: Vec<f64> = std::env::args()
        .skip(1)
        .map(|a| {
            a.parse::<f64>()
                .map_err(|e| format!("argv raise ratio '{a}' 不是 f64: {e}"))
        })
        .collect::<Result<_, _>>()?;
    let six_ratios: Vec<f64> = if argv.is_empty() { vec![1.0] } else { argv };
    let postflop_buckets: u64 = std::env::var("XV_POSTFLOP")
        .ok()
        .map(|s| s.parse::<u64>())
        .transpose()
        .map_err(|e| format!("XV_POSTFLOP 不是 u64: {e}"))?
        .unwrap_or(200);
    // A1 raise cap：每街 (Bet+Raise) 聚合上限。env 不设 = 无 cap（u32::MAX）。
    // 例：RAISE_CAP=2 cargo run ... -- 0.5 1.0  →  含 0.5pot 小注但每街最多 2 次进攻。
    let raise_cap: u32 = std::env::var("RAISE_CAP")
        .ok()
        .map(|s| s.parse::<u32>())
        .transpose()
        .map_err(|e| format!("RAISE_CAP 不是 u32: {e}"))?
        .unwrap_or(u32::MAX);
    // FIRST_SMALL=1：preflop {1.0}、postflop {0.5,1.0}，且 0.5pot 只许作开池 Bet、
    // re-raise 一律 1pot（禁 Raise{0.5}）。设此 flag 时忽略 argv raise 集（per-street 固定）。
    let first_small: bool = matches!(
        std::env::var("FIRST_SMALL").ok().as_deref(),
        Some("1") | Some("true")
    );
    // B3_SUMMARY=1：开 B3 betting-state 摘要 key 探针（distinct key / B3 infoset / 不变量）。
    // 纯 B3 摘要应在无 RAISE_CAP / 无 FIRST_SMALL 下跑（这两者改的是动作集，不是摘要本身）。
    let b3_summary: bool = matches!(
        std::env::var("B3_SUMMARY").ok().as_deref(),
        Some("1") | Some("true")
    );
    // B3_PIN_ACTIONS=1：把合法动作集签名折进 B3 key（生产修法）。需同时 B3_SUMMARY=1。
    // 关时暴露"字段是否足以决定动作集"（不变量校验）；开时测修法后真实有界规模。
    let b3_pin_actions: bool = matches!(
        std::env::var("B3_PIN_ACTIONS").ok().as_deref(),
        Some("1") | Some("true")
    );

    println!("=== Simplified NLHE Abstract Betting Tree Sizing ===");
    let cap_desc = if raise_cap == u32::MAX {
        "none".to_string()
    } else {
        raise_cap.to_string()
    };
    println!(
        "RNG seed = 0x{WALK_SEED:016x}   NODE_CAP = {NODE_CAP}   RAISE_CAP = {cap_desc}   FIRST_SMALL = {first_small}   B3_SUMMARY = {b3_summary}   B3_PIN_ACTIONS = {b3_pin_actions}"
    );
    println!();

    // (1) HU self-check：复现 240,096 节点 / 119.7M infoset（验证 refactor 没改计数）。
    {
        let hu = BucketCounts {
            preflop: 169,
            postflop: 500,
        };
        let cfg = TableConfig::default_hu_200bb();
        let start = std::time::Instant::now();
        // HU self-check 永远不加 cap / 不加 first-small，守住 240,096 节点 / 119.7M 这个不变量。
        // B3 探针随 flag 开关（不影响 node 计数，只多算摘要 key）。
        let stats = measure(
            &cfg,
            [R3, R3, R3, R3],
            &hu,
            u32::MAX,
            false,
            b3_summary,
            b3_pin_actions,
        );
        print_stats(
            "HU self-check (期望 240,096 节点 / 119.7M)",
            &ratios_desc([R3, R3, R3, R3]),
            &stats,
            &hu,
        );
        println!("walk wall time  : {:.3}s\n", start.elapsed().as_secs_f64());
    }

    // (2) 6-max S2 探针：argv raise 集 / env postflop 桶数 / preflop 169。
    {
        let six = BucketCounts {
            preflop: 169,
            postflop: postflop_buckets,
        };
        let cfg = TableConfig::default_6max_100bb();
        // FIRST_SMALL 时 per-street 固定 preflop{1.0} / postflop{0.5,1.0} + drop Raise{0.5}；
        // 否则沿用 argv raise 集（全街同集）。
        const PRE1: &[f64] = &[1.0];
        const POST05_1: &[f64] = &[0.5, 1.0];
        let r: &[f64] = &six_ratios;
        let per_street: [&[f64]; 4] = if first_small {
            [PRE1, POST05_1, POST05_1, POST05_1]
        } else {
            [r, r, r, r]
        };
        let label = if first_small {
            format!(
                "6-max 100BB / preflop{{1.0}} postflop{{0.5,1.0}} first-bet-small / preflop 169 / postflop {postflop_buckets} / raise_cap {cap_desc}"
            )
        } else {
            format!(
                "6-max 100BB / {} bet size(s) / preflop 169 / postflop {postflop_buckets} / raise_cap {cap_desc}",
                six_ratios.len()
            )
        };
        let start = std::time::Instant::now();
        let stats = measure(
            &cfg,
            per_street,
            &six,
            raise_cap,
            first_small,
            b3_summary,
            b3_pin_actions,
        );
        print_stats(&label, &ratios_desc(per_street), &stats, &six);
        println!("walk wall time  : {:.3}s\n", start.elapsed().as_secs_f64());
    }

    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("[nlhe_betting_tree_sizing] error: {e}");
            ExitCode::from(1)
        }
    }
}
