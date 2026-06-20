//! 档二′-跨街复用（`--search-unanchored-cross-street`）ON vs OFF **决策级** A/B，跑在**构造的
//! deep off-tree flop→turn 加注战线**上（`unanchored_range_design` §动机 / §4 「仍 pending = 强弱」
//! 指定的「干净标尺 = 构造 off-tree-flop→turn 线」）。
//!
//! # 两种模式
//!
//! - **默认（脱锚 turn）**：off-tree flop→turn 线，比 turn 决策 ON（flop σ 后验）vs OFF（档一前缀）。
//! - **`--anchored`（锚定 river，`turn_blueprint_trim_cross_street_anchored_2026_06_19` §4.4）**：
//!   **on-tree**（lockstep 100BB）flop→turn→river 线，turn 子树解入缓存，比 **river 决策** ON
//!   （turn 子树后验，跨街复用 turn 解 σ）vs OFF（`estimate_range` 读 **turn blueprint**）。受控配对
//!   差只差 river root range 来源 → **直接量「裁掉 turn blueprint」对 river 决策的影响**（裁剪 §4.4
//!   决策级证据；保留 turn blueprint 跑）。方向应与 §动机一致：turn 子树后验比泛 blueprint estimate
//!   更贴本子博弈。两臂用**同一** turn 解（同 hand_seed/cfg）→ 欠收敛对两臂等量不污染差值。
//! - **`--hitrate`（§4.5 命中率，裁剪 go/no-go 闸）**：**自对弈**（blueprint 驱动全座）生成真分布
//!   on-tree 手，hero 座每手常驻 cache 先解 turn → river 决策 cross=Some(turn_within)，delta cache
//!   跨街计数 → **river 跨街命中率**。命中 = turn 信息经后验恢复（裁 turn 后 range 不退化）；未命中 =
//!   river range 退化为「preflop+flop reach × turn uniform」（§3 兜底）。命中率够高 → 裁近无损。
//!   口径：自对弈全 on-tree（off-menu 失配近 0）+ 固定 iter（生产 12s/24 线程 giveup 更少 → 本读数
//!   是下界）；脱锚 river 尾自对弈触发~0 → 须 live（advisor 已插桩 cross_attempts/cross_hits 待 live）。
//!
//! # 为什么不是 EV / 自对弈
//!
//! - 跨街复用要同一手 flop **和** turn **都**走脱锚搜索（先解 flop 缓存、turn 复用）。连只需单街
//!   触发的档一（`six_max_unanchored_prefix_ab`）自对弈都是 0/~1500 手触发——跨街是其真子集，自对弈
//!   配对 EV 结构上测不动。live EV 功效又太弱（AIVAT 仅 1.1–1.33× SD，救不动强弱）。
//! - 既有判据（档一翻生产默认 ON 的取证方式）= **决策级**：在脱锚搜索点比 ON vs OFF 的 hero 动作
//!   分布 TV 距离 + argmax 翻转 + 方向，不依赖 EV。本工具把它落到一条**确定性构造**的 off-tree
//!   flop→turn 线上（随 deal_seed 换牌）。
//!
//! # 构造的线（off-stack all-in 触发 + flop 加注战，stacks 深到 turn 有真决策）
//!
//! preflop：UTG 短码 all-in → HJ/CO/BTN fold → SB raise-over（**100BB 影子断点**：影子把 UTG
//! all-in 当满栈 shove → SB 在树里无 raise 槽 → 失同步，synced_node = SB 节点）→ BB call → flop。
//! flop：SB bet → BB raise → SB call → turn（SB 首行动 = hero 决策）。UTG 真栈调小 → SB/BB flop/turn
//! 仍深（影子走的是 **abstract** all-in，amount-blind → synced 前缀与 UTG 真栈无关，深浅自由）。
//!
//! 两臂只差一个参数（`cross_street`，= 生产 `decide_search_unanchored` 里 ON/OFF 唯一分叉）：
//! - **OFF（档一）**：turn root range = 同步前缀 reach（preflop 3bettor，**丢掉 flop 加注战**）。
//! - **ON（档二′）**：turn root range = flop 子树 σ 对 flop 加注战实际线条件化的后验（§动机：恰好
//!   把档一丢掉的 flop re-raise 战捡回来）。
//!
//! # 务必随结果一并解读
//!
//! - **场景分布不真实**：flop 线是**强制固定**动作（非策略驱动），故「决策改变率」不是真实牌局频率，
//!   而是「**给定**这些 off-tree flop→turn 谱，range 精化改不改 turn 决策、方向对不对」=机制验证。
//! - **单点单 seed 不是判据**（`project_6max_search_range_prior_overexploit`）：扫多 deal + 报 TV 分布
//!   + dump top-K 谱（hero 手 / board / 两分布）供人读方向，别拿单个数字当强弱判决。
//! - OFF 用**真 blueprint**（checkpoint）当 prefix strategy——uniform 会把档一塌成 uniform、虚高 ON。
//!
//! 用法（vultr）：
//! ```bash
//! cargo run --release --bin six_max_cross_street_ab -- \
//!   --bucket-table artifacts/bucket_table_default_500_500_500_seed_cafebabe_schemav4.bin \
//!   --reshape nolimp --postflop-cap 3 \
//!   --checkpoint artifacts/run_6max_s4_nolimp/nlhe_es_mccfr_final_001000000000.ckpt \
//!   --deals 64 --search-iterations 1000 --top-k 8
//! ```

use std::process::ExitCode;

use rayon::prelude::*;

use poker::training::blueprint_advisor::outgoing_action;
use poker::training::game::Game;
use poker::training::nlhe::{SimplifiedNlheGame, SimplifiedNlheState};
use poker::training::nlhe_betting_tree::{
    first_small_6max, first_small_preopen_6max, first_small_preopen_small_6max, AbstractActionTag,
    BettingAbstractionRules, NodeId,
};
use poker::training::nlhe_dense_trainer::DenseNlheEsMccfrTrainer;
use poker::training::subgame::{
    subgame_search_cached, subgame_search_unanchored_cached,
    subgame_search_unanchored_cached_cross, synced_prefix_decisions, PrefixReach, ResolveRoot,
    SearchTrigger, SubgameSearchConfig, SubgameSolveCache,
};
use poker::{
    AbstractAction, Action, BucketTable, Card, ChaCha20Rng, ChipAmount, GameState, InfoSetId,
    PlayerId, PlayerStatus, RngSource, SeatId, Street, StreetActionAbstraction, TableConfig,
};

const HERO_SEAT: usize = 1; // SB = turn 首行动 = hero 决策。
const SHADOW_SEED: u64 = 0x5348_4144_5859_5341; // "SHADXYSA"（影子游走 deal-independent 节点）。

/// 同步前缀决策三元组（off-tree 断点前，全 preflop）。
type SyncedPrefix = Vec<(NodeId, AbstractActionTag, PlayerId)>;

/// 构造线产物：(flop round-start, turn auth, flop within-round 实际动作线)。
type Line = (GameState, GameState, Vec<(Action, bool)>);

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("[cross_street_ab] failed: {e}");
            ExitCode::from(2)
        }
    }
}

struct Args {
    bucket_table: String,
    reshape: String,
    checkpoint: String,
    postflop_cap: u8,
    deals: u64,
    seed: u64,
    top_k: usize,
    search: SubgameSearchConfig,
    /// `--anchored`：跑**锚定 river** A/B（on-tree flop→turn→river，turn-posterior vs
    /// estimate(turn blueprint)），而非默认的脱锚 turn A/B。下面 4 个构造线参数仅默认模式用。
    anchored: bool,
    /// `--hitrate`（turn_blueprint_trim §4.5）：跑**自对弈 river 跨街命中率**（裁剪 go/no-go 闸），
    /// 而非 A/B。优先于 `--anchored`。
    hitrate: bool,
    /// 构造线参数（BB，仅默认脱锚模式）：UTG 短码 all-in / SB raise-over to / flop SB bet to / flop BB raise to。
    utg_bb: u64,
    sb_raise_bb: u64,
    flop_bet_bb: u64,
    flop_raise_bb: u64,
}

/// 单 deal 的 ON-vs-OFF 决策级读数。
struct DealResult {
    deal_seed: u64,
    hero_hole: [Card; 2],
    board: Vec<Card>,
    tv: f64,
    flip: bool,
    off: Vec<(AbstractAction, f64)>,
    on: Vec<(AbstractAction, f64)>,
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    if !matches!(args.postflop_cap, 2..=4) {
        return Err(format!(
            "--postflop-cap must be 2..=4, got {}",
            args.postflop_cap
        ));
    }
    let table = open_table(&args.bucket_table)?;
    // game/影子用对称 100BB（= blueprint 训练树，dense ckpt 按它键）；auth 真栈在构造线里按 --utg-bb 调。
    let game_cfg = TableConfig::default_6max_100bb();
    let (abs, rules) = reshape_profile(&args.reshape, args.postflop_cap)?;
    let game = SimplifiedNlheGame::new_with_abstraction(table, game_cfg.clone(), abs, rules)
        .map_err(|e| format!("build game failed: {e:?}"))?;
    let trainer =
        DenseNlheEsMccfrTrainer::load_checkpoint(std::path::Path::new(&args.checkpoint), game)
            .map_err(|e| format!("load checkpoint {} failed: {e:?}", args.checkpoint))?;
    let strat = |info: &InfoSetId, _n: usize| trainer.average_strategy(*info);
    let game_ref = trainer.game();

    // §4.5 命中率模式（自对弈，非 A/B）：早返回，自带报表。
    if args.hitrate {
        eprintln!(
            "[cross_street_ab] mode=HITRATE(self-play river) reshape={} cap={} update_count={} | search: iters={} deep_menu={} range_mix={} max_nodes={} seed=0x{:016x} deals={}",
            args.reshape, args.postflop_cap, trainer.update_count(),
            args.search.iterations, args.search.deep_menu, args.search.range_uniform_mix,
            args.search.max_subtree_nodes, args.seed, args.deals,
        );
        let stats: Vec<HitrateStat> = (0..args.deals)
            .into_par_iter()
            .map(|i| eval_hitrate_deal(game_ref, &strat, &args, i))
            .collect();
        print_hitrate_report(&stats, args.deals);
        return Ok(());
    }

    let results: Vec<DealResult> = if args.anchored {
        // 锚定 river A/B（turn_blueprint_trim §4.4）：on-tree flop→turn→river，无需同步前缀。
        eprintln!(
            "[cross_street_ab] mode=ANCHORED-river reshape={} cap={} update_count={} | search: iters={} deep_menu={} range_mix={} max_nodes={} seed=0x{:016x} deals={}",
            args.reshape, args.postflop_cap, trainer.update_count(),
            args.search.iterations, args.search.deep_menu, args.search.range_uniform_mix,
            args.search.max_subtree_nodes, args.seed, args.deals,
        );
        (0..args.deals)
            .into_par_iter()
            .filter_map(|i| eval_anchored_deal(game_ref, &strat, &args, i))
            .collect()
    } else {
        // 同步前缀（deal-independent：影子走 abstract all-in + 3 fold → SB 节点 = 断点前）。
        let synced = build_synced_prefix(game_ref)?;
        eprintln!(
            "[cross_street_ab] mode=UNANCHORED-turn reshape={} cap={} update_count={} synced_prefix_len={}",
            args.reshape,
            args.postflop_cap,
            trainer.update_count(),
            synced.len()
        );
        eprintln!(
            "[cross_street_ab] line: utg={}bb sb_raise_to={}bb flop_bet_to={}bb flop_raise_to={}bb | search: iters={} deep_menu={} range_mix={} max_nodes={} seed=0x{:016x} deals={}",
            args.utg_bb, args.sb_raise_bb, args.flop_bet_bb, args.flop_raise_bb,
            args.search.iterations, args.search.deep_menu, args.search.range_uniform_mix,
            args.search.max_subtree_nodes, args.seed, args.deals,
        );
        (0..args.deals)
            .into_par_iter()
            .filter_map(|i| eval_deal(game_ref, &strat, &game_cfg, &synced, &args, i))
            .collect()
    };

    print_report(&results, args.deals, args.top_k, args.anchored);
    Ok(())
}

/// 影子走 abstract `AllIn`（UTG）+ 3× `Fold` → SB 决策节点 = 同步前缀末（off-stack 断点前）。
/// node_id 与发牌无关 → 一次算定。
fn build_synced_prefix(game: &SimplifiedNlheGame) -> Result<SyncedPrefix, String> {
    let mut rng = ChaCha20Rng::from_seed(SHADOW_SEED);
    let drng: &mut dyn RngSource = &mut rng;
    let mut abs = game.root(drng);
    if abs.game_state.current_player() != Some(SeatId(3)) {
        return Err("影子根首行动非 UTG(seat 3)".to_string());
    }
    let allin = SimplifiedNlheGame::legal_actions(&abs)
        .into_iter()
        .find(|a| AbstractActionTag::of(a) == AbstractActionTag::AllIn)
        .ok_or("UTG 根无 AllIn 抽象档")?;
    abs = SimplifiedNlheGame::next(abs, allin, drng);
    for who in ["HJ", "CO", "BTN"] {
        let fold = SimplifiedNlheGame::legal_actions(&abs)
            .into_iter()
            .find(|a| matches!(a, AbstractAction::Fold))
            .ok_or_else(|| format!("{who} 无 Fold 档"))?;
        abs = SimplifiedNlheGame::next(abs, fold, drng);
    }
    if abs.game_state.current_player() != Some(SeatId(HERO_SEAT as u8)) {
        return Err("3 fold 后非 SB 决策点".to_string());
    }
    let prefix = synced_prefix_decisions(game, abs.current_node_id);
    if prefix.is_empty() {
        return Err("同步前缀为空（off-stack 前缀应含 preflop 决策）".to_string());
    }
    Ok(prefix)
}

/// 构造 deep off-tree flop→turn 加注战线（确定性动作，随 `deal_seed` 换牌）。
/// 返回 (flop_round_start, turn_auth, flop_within)；hero = SB。
fn build_line(game_cfg: &TableConfig, deal_seed: u64, args: &Args) -> Result<Line, String> {
    let bb = game_cfg.big_blind.as_u64();
    let mut acfg = game_cfg.clone();
    acfg.starting_stacks[3] = ChipAmount::new(args.utg_bb * bb); // UTG 短码 → 深 flop/turn。
    let mut st = GameState::new(&acfg, deal_seed);
    if st.current_player() != Some(SeatId(3)) {
        return Err("preflop 首行动非 UTG".to_string());
    }
    apply(&mut st, Action::AllIn)?; // UTG 短码 shove
    apply(&mut st, Action::Fold)?; // HJ
    apply(&mut st, Action::Fold)?; // CO
    apply(&mut st, Action::Fold)?; // BTN
    apply(
        &mut st,
        Action::Raise {
            to: ChipAmount::new(args.sb_raise_bb * bb),
        },
    )?; // SB raise-over
    apply(&mut st, Action::Call)?; // BB
    if st.street() != Street::Flop || st.current_player() != Some(SeatId(HERO_SEAT as u8)) {
        return Err(format!(
            "preflop 后非 flop/SB 首行动（street={:?}）",
            st.street()
        ));
    }
    let flop_rs = st.clone();
    // flop 加注战：SB bet → BB raise → SB call。each within = (动作, 该动作是否令行动者 all-in)。
    let mut within = Vec::with_capacity(3);
    apply_within(
        &mut st,
        Action::Bet {
            to: ChipAmount::new(args.flop_bet_bb * bb),
        },
        HERO_SEAT,
        &mut within,
    )?;
    apply_within(
        &mut st,
        Action::Raise {
            to: ChipAmount::new(args.flop_raise_bb * bb),
        },
        2,
        &mut within,
    )?;
    apply_within(&mut st, Action::Call, HERO_SEAT, &mut within)?;
    if st.street() != Street::Turn || st.current_player() != Some(SeatId(HERO_SEAT as u8)) {
        return Err(format!(
            "flop 后非 turn/SB 首行动（street={:?}）",
            st.street()
        ));
    }
    Ok((flop_rs, st, within))
}

fn eval_deal(
    game: &SimplifiedNlheGame,
    strat: &(dyn Fn(&InfoSetId, usize) -> Vec<f64> + Sync),
    game_cfg: &TableConfig,
    synced: &SyncedPrefix,
    args: &Args,
    deal_idx: u64,
) -> Option<DealResult> {
    let deal_seed = mix2(args.seed, deal_idx);
    let (flop_rs, turn_auth, flop_within) = build_line(game_cfg, deal_seed, args).ok()?;
    let hand_seed = mix2(args.seed ^ 0xC505, deal_idx);
    let mk_prefix = || PrefixReach {
        strategy: strat,
        decisions: synced,
    };

    // OFF 臂（档一）：flop 解入缓存 → turn cross=None（同步前缀 reach）。
    let mut cache_off = SubgameSolveCache::new();
    solve_flop(
        &mut cache_off,
        &flop_rs,
        game,
        &args.search,
        mk_prefix(),
        hand_seed,
    )?;
    let off = subgame_search_unanchored_cached(
        Some(&mut cache_off),
        &turn_auth,
        &turn_auth,
        game,
        &[],
        &args.search,
        None,
        Some(mk_prefix()),
        hand_seed,
    )
    .ok()?;

    // ON 臂（档二′）：同一 flop 解入缓存 → turn cross=Some(flop_within)（flop σ 后验覆盖前缀）。
    let mut cache_on = SubgameSolveCache::new();
    solve_flop(
        &mut cache_on,
        &flop_rs,
        game,
        &args.search,
        mk_prefix(),
        hand_seed,
    )?;
    let on = subgame_search_unanchored_cached_cross(
        Some(&mut cache_on),
        &turn_auth,
        &turn_auth,
        game,
        &[],
        &args.search,
        None,
        Some(mk_prefix()),
        Some(&flop_within),
        hand_seed,
    )
    .ok()?;

    let (tv, flip) = dist_tv_and_flip(&on, &off);
    let hero_hole = turn_auth.players()[HERO_SEAT].hole_cards?;
    Some(DealResult {
        deal_seed,
        hero_hole,
        board: turn_auth.board().to_vec(),
        tv,
        flip,
        off,
        on,
    })
}

/// 把 flop round-start 子树解进缓存（首 postflop 街 = 无跨街，用同步前缀 reach）。turn 跨街 peek 它。
fn solve_flop(
    cache: &mut SubgameSolveCache,
    flop_rs: &GameState,
    game: &SimplifiedNlheGame,
    cfg: &SubgameSearchConfig,
    prefix: PrefixReach,
    hand_seed: u64,
) -> Option<()> {
    subgame_search_unanchored_cached(
        Some(cache),
        flop_rs,
        flop_rs,
        game,
        &[],
        cfg,
        None,
        Some(prefix),
        hand_seed,
    )
    .ok()
    .map(|_| ())
}

// ===========================================================================
// 锚定 river A/B（turn_blueprint_trim §4.4）：on-tree flop→turn→river，turn 解入缓存，river 决策
// ON（cross=Some(turn_within)=turn 子树后验）vs OFF（cross=None=estimate_range 读 turn blueprint）。
// ===========================================================================

/// 想要的抽象动作类别（在 legal 集里按 tag 取首个匹配 → 确定性走 on-tree 线）。
#[derive(Clone, Copy, Debug)]
enum Want {
    Raise,
    Fold,
    Call,
    Check,
    Bet,
}

fn pick_want(st: &SimplifiedNlheState, want: Want) -> Option<AbstractAction> {
    SimplifiedNlheGame::legal_actions(st).into_iter().find(|a| {
        matches!(
            (want, AbstractActionTag::of(a)),
            (Want::Fold, AbstractActionTag::Fold)
                | (Want::Check, AbstractActionTag::Check)
                | (Want::Call, AbstractActionTag::Call)
                | (Want::Raise, AbstractActionTag::Raise(_))
                | (Want::Bet, AbstractActionTag::Bet(_))
        )
    })
}

/// 在 blueprint 树上走一步抽象动作（`SimplifiedNlheState` 同步推进 game_state + node_id）；`rec=Some`
/// 时把该步 **concrete** 动作记入（turn_within 跨街导航用）——Bet/Raise 的 `to` = 行动者 apply 后的
/// committed_this_round（= 下注/加注到的总额），与 advisor `build_real_auth` 记 prev_within 同口径。
fn nav(
    st: SimplifiedNlheState,
    want: Want,
    rng: &mut dyn RngSource,
    rec: Option<&mut Vec<(Action, bool)>>,
) -> Result<SimplifiedNlheState, String> {
    let actor = st.game_state.current_player().ok_or("nav: 非决策点")?.0 as usize;
    let chosen = pick_want(&st, want).ok_or_else(|| format!("nav: legal 集无 {want:?}"))?;
    let tag = AbstractActionTag::of(&chosen);
    let next = SimplifiedNlheGame::next(st, chosen, rng);
    if let Some(rec) = rec {
        let p = &next.game_state.players()[actor];
        let became_all_in = p.status == PlayerStatus::AllIn;
        let concrete = match tag {
            AbstractActionTag::Fold => Action::Fold,
            AbstractActionTag::Check => Action::Check,
            AbstractActionTag::Call => Action::Call,
            AbstractActionTag::AllIn => Action::AllIn,
            AbstractActionTag::Bet(_) => Action::Bet {
                to: p.committed_this_round,
            },
            AbstractActionTag::Raise(_) => Action::Raise {
                to: p.committed_this_round,
            },
        };
        rec.push((concrete, became_all_in));
    }
    Ok(next)
}

/// 构造 on-tree（lockstep 100BB）flop→turn→river 加注线（betting 线固定、随 `deal_seed` 换牌）：
/// UTG raise → HJ/CO/BTN/SB fold → BB call → flop BB/UTG check-check → turn BB check / UTG bet /
/// BB call → river（BB 首行动 = hero）。返回 (turn round-start, turn_within 完整真实动作线, river
/// round-start, hero seat)；turn/river 各带 blueprint 树 node_id（锚定路径建子树 + 导航）。
type AnchoredLine = (
    SimplifiedNlheState,
    Vec<(Action, bool)>,
    SimplifiedNlheState,
    usize,
);

fn build_anchored_line(game: &SimplifiedNlheGame, deal_seed: u64) -> Result<AnchoredLine, String> {
    let mut rng = ChaCha20Rng::from_seed(deal_seed);
    let rng: &mut dyn RngSource = &mut rng;
    let mut st = game.root(rng);
    if st.game_state.current_player() != Some(SeatId(3)) {
        return Err("preflop 首行动非 UTG(seat 3)".to_string());
    }
    st = nav(st, Want::Raise, rng, None)?; // UTG open
    for _ in 0..4 {
        st = nav(st, Want::Fold, rng, None)?; // HJ/CO/BTN/SB fold
    }
    st = nav(st, Want::Call, rng, None)?; // BB call → flop
    if st.game_state.street() != Street::Flop {
        return Err(format!("preflop 后非 flop（{:?}）", st.game_state.street()));
    }
    st = nav(st, Want::Check, rng, None)?; // flop BB check
    st = nav(st, Want::Check, rng, None)?; // flop UTG check → turn
    if st.game_state.street() != Street::Turn {
        return Err(format!("flop 后非 turn（{:?}）", st.game_state.street()));
    }
    let turn_rs = st.clone();
    let mut within = Vec::with_capacity(3);
    st = nav(st, Want::Check, rng, Some(&mut within))?; // turn BB check
    st = nav(st, Want::Bet, rng, Some(&mut within))?; // turn UTG bet
    st = nav(st, Want::Call, rng, Some(&mut within))?; // turn BB call → river
    if st.game_state.street() != Street::River {
        return Err(format!("turn 后非 river（{:?}）", st.game_state.street()));
    }
    let hero = st.game_state.current_player().ok_or("river 非决策点")?.0 as usize;
    Ok((turn_rs, within, st, hero))
}

/// turn round-start 子树解入缓存（cross=None：turn 自身用 estimate，两臂一致）→ river 跨街 peek 它。
fn solve_turn(
    cache: &mut SubgameSolveCache,
    turn_rs: &SimplifiedNlheState,
    turn_legal: &[AbstractAction],
    game: &SimplifiedNlheGame,
    strat: &(dyn Fn(&InfoSetId, usize) -> Vec<f64> + Sync),
    cfg: &SubgameSearchConfig,
    hand_seed: u64,
) -> Option<()> {
    subgame_search_cached(
        Some(cache),
        &turn_rs.game_state,
        &turn_rs.game_state,
        game,
        turn_legal,
        turn_rs.current_node_id,
        strat,
        cfg,
        None,
        None,
        None,
        None, // cross=None
        hand_seed,
        0,
    )
    .ok()
    .map(|_| ())
}

/// 单 deal 的锚定 river ON-vs-OFF 读数。两臂用**同一** turn 解（同 hand_seed/cfg/子树），只 river
/// 的 cross 不同（ON=Some(turn_within) 复用 turn σ 后验 / OFF=None 走 estimate_range 读 turn
/// blueprint）→ TV 纯由 river root range 来源驱动。
fn eval_anchored_deal(
    game: &SimplifiedNlheGame,
    strat: &(dyn Fn(&InfoSetId, usize) -> Vec<f64> + Sync),
    args: &Args,
    deal_idx: u64,
) -> Option<DealResult> {
    let deal_seed = mix2(args.seed, deal_idx);
    let (turn_rs, turn_within, river_rs, hero) = build_anchored_line(game, deal_seed).ok()?;
    let hand_seed = mix2(args.seed ^ 0xC505, deal_idx);
    let turn_legal = SimplifiedNlheGame::legal_actions(&turn_rs);
    let river_legal = SimplifiedNlheGame::legal_actions(&river_rs);
    let river_decision = |cache: &mut SubgameSolveCache,
                          cross: Option<&[(Action, bool)]>|
     -> Option<Vec<(AbstractAction, f64)>> {
        subgame_search_cached(
            Some(cache),
            &river_rs.game_state,
            &river_rs.game_state,
            game,
            &river_legal,
            river_rs.current_node_id,
            strat,
            &args.search,
            None,
            None,
            None, // within_round_real：river 首决策（within 空）不需。
            cross,
            hand_seed,
            0,
        )
        .ok()
    };

    // OFF 臂：turn 解入缓存 → river cross=None（estimate_range 读 turn blueprint σ）。
    let mut cache_off = SubgameSolveCache::new();
    solve_turn(
        &mut cache_off,
        &turn_rs,
        &turn_legal,
        game,
        strat,
        &args.search,
        hand_seed,
    )?;
    let off = river_decision(&mut cache_off, None)?;

    // ON 臂：同一 turn 解 → river cross=Some(turn_within)（turn 子树 σ 后验覆盖 estimate）。
    let mut cache_on = SubgameSolveCache::new();
    solve_turn(
        &mut cache_on,
        &turn_rs,
        &turn_legal,
        game,
        strat,
        &args.search,
        hand_seed,
    )?;
    let on = river_decision(&mut cache_on, Some(&turn_within))?;

    let (tv, flip) = dist_tv_and_flip(&on, &off);
    let hero_hole = river_rs.game_state.players()[hero].hole_cards?;
    Some(DealResult {
        deal_seed,
        hero_hole,
        board: river_rs.game_state.board().to_vec(),
        tv,
        flip,
        off,
        on,
    })
}

// ===========================================================================
// §4.5 跨街复用命中率（turn_blueprint_trim §4.5）：自对弈真分布 river 决策里 cross 命中占比 =
// 「river range 由 turn 子树后验恢复（不退化）」的比例。miss = turn 信息退化为 uniform（§3 兜底
// 「preflop+flop reach × turn uniform」）。**关键口径**：`estimate_range`（读 turn σ）在 cross 前
// **无条件**跑——裁掉 turn blueprint 后它对缺失 turn key 退 uniform（§5.1），cross **命中即用后验
// 覆盖、恢复 turn 信息**；cross miss → 留 uniform = 退化。故命中率 = turn 信息被恢复的占比 = 裁剪
// 无损度。命中率够高 → 裁近无损（go），低 → 裁会实质劣化 river range（no-go）。
// ===========================================================================

/// blueprint 分布（空 / 全零 / 长度不符 → uniform 兜底，与 advisor blueprint_distribution 同口径）。
fn blueprint_dist(
    info: &InfoSetId,
    legal_abs: &[AbstractAction],
    strat: &(dyn Fn(&InfoSetId, usize) -> Vec<f64> + Sync),
) -> Vec<(AbstractAction, f64)> {
    let raw = strat(info, legal_abs.len());
    if raw.len() == legal_abs.len() && raw.iter().any(|p| p.is_finite() && *p > 0.0) {
        let sum: f64 = raw.iter().filter(|p| p.is_finite() && **p > 0.0).sum();
        legal_abs
            .iter()
            .copied()
            .zip(raw)
            .filter(|(_, p)| p.is_finite() && *p > 0.0)
            .map(|(a, p)| (a, p / sum))
            .collect()
    } else {
        let p = 1.0 / legal_abs.len() as f64;
        legal_abs.iter().copied().map(|a| (a, p)).collect()
    }
}

/// 从归一分布按 rng 抽一个下标（累积分布；next_u64 → [0,1) double）。
fn sample_idx(dist: &[(AbstractAction, f64)], rng: &mut dyn RngSource) -> usize {
    let u = (rng.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
    let mut acc = 0.0;
    for (i, (_, p)) in dist.iter().enumerate() {
        acc += p;
        if u < acc {
            return i;
        }
    }
    dist.len() - 1
}

fn sample_rng_dyn(rng: &mut ChaCha20Rng) -> &mut dyn RngSource {
    rng
}

/// 自对弈（blueprint 驱动全座，含 hero——search 仅测量不驱动 → 保持 on-tree = 锚定路径）生成
/// on-tree flop→turn→river 真分布线，捕 hero 的 turn→river 转移：返回 (turn round-start, turn
/// **完整**真实动作线, hero river 决策态, hero)。hero river 前手结束 / hero 未到 river / 偏离
/// on-tree → `None`（skip）。concrete 动作经 [`outgoing_action`] 在 **pre-state** 算（避免 post-`next`
/// committed 复位）。
fn selfplay_anchored_line(
    game: &SimplifiedNlheGame,
    strat: &(dyn Fn(&InfoSetId, usize) -> Vec<f64> + Sync),
    deal_seed: u64,
    hero: usize,
) -> Option<AnchoredLine> {
    let mut rng = ChaCha20Rng::from_seed(deal_seed);
    let mut st = game.root(sample_rng_dyn(&mut rng));
    let mut turn_rs: Option<SimplifiedNlheState> = None;
    let mut turn_line: Vec<(Action, bool)> = Vec::new();
    let mut cur_within: Vec<(Action, bool)> = Vec::new();
    let mut cur_street = st.game_state.street();
    for _ in 0..256usize {
        if st.game_state.is_terminal() {
            return None; // 手在 hero river 决策前结束。
        }
        let actor = st.game_state.current_player()?.0 as usize;
        if actor == hero && st.game_state.street() == Street::River {
            // hero river 决策点（on-tree）。turn_rs 须已捕（hero 经 turn 到 river）。
            return Some((turn_rs?, turn_line, st, hero));
        }
        let legal = SimplifiedNlheGame::legal_actions(&st);
        if legal.is_empty() {
            return None;
        }
        let hole = st.game_state.players()[actor].hole_cards?;
        let board = st.game_state.board().to_vec();
        let info = game.info_set_for_cards(st.current_node_id, hole, &board);
        let dist = blueprint_dist(&info, &legal, strat);
        let chosen = dist[sample_idx(&dist, sample_rng_dyn(&mut rng))].0;
        let concrete = outgoing_action(&st.game_state, game.abstraction(), chosen).ok()?;
        st = SimplifiedNlheGame::next(st, chosen, sample_rng_dyn(&mut rng));
        let became_all_in = st.game_state.players()[actor].status == PlayerStatus::AllIn;
        if st.game_state.street() != cur_street {
            // 收街动作属上一街（同 build_real_auth）。刚结束的街 = cur_street。
            cur_within.push((concrete, became_all_in));
            let completed = std::mem::take(&mut cur_within);
            if cur_street == Street::Turn {
                turn_line = completed; // turn 完整线（含收街动作）→ river 跨街沿它读 turn σ。
            }
            cur_street = st.game_state.street();
            if cur_street == Street::Turn {
                turn_rs = Some(st.clone()); // turn round-start 快照。
            }
        } else {
            cur_within.push((concrete, became_all_in));
        }
    }
    None
}

/// 单 deal 的 river 跨街命中读数（hero 座轮转覆盖各位置）。
#[derive(Default, Clone, Copy)]
struct HitrateStat {
    river_reached: bool,       // hero 到 river on-tree（= 分母）。
    turn_giveup: bool,         // turn 解 giveup（→ 缓存无 turn → river cross 必 miss）。
    cross_attempt: bool,       // river 决策有跨街尝试（cross_street=Some + cache 传入）。
    cross_hit: bool,           // river cross 命中（turn 信息经后验恢复，不退化）。
    river_search_giveup: bool, // river hero search 本身 giveup（正交：advisor check-when-free）。
}

/// 自对弈 river 跨街命中率单点：生成 on-tree 线 → 同手一个常驻 cache 先解 turn（giveup → 后续
/// cross 必 miss）→ river 决策 cross=Some(turn_within)，delta cache 跨街计数 → 命中/未命中。
fn eval_hitrate_deal(
    game: &SimplifiedNlheGame,
    strat: &(dyn Fn(&InfoSetId, usize) -> Vec<f64> + Sync),
    args: &Args,
    deal_idx: u64,
) -> HitrateStat {
    let mut s = HitrateStat::default();
    let deal_seed = mix2(args.seed, deal_idx);
    let hero = (deal_idx % 6) as usize; // 轮转 hero 座 → 覆盖 BTN/SB/BB/UTG/HJ/CO。
    let Some((turn_rs, turn_within, river_rs, _)) =
        selfplay_anchored_line(game, strat, deal_seed, hero)
    else {
        return s; // 未到 hero river → skip（不计分母）。
    };
    s.river_reached = true;
    let hand_seed = mix2(args.seed ^ 0xC505, deal_idx);
    let turn_legal = SimplifiedNlheGame::legal_actions(&turn_rs);
    let river_legal = SimplifiedNlheGame::legal_actions(&river_rs);
    let mut cache = SubgameSolveCache::new();
    // turn 解入缓存（cross=None；river 命中率只看 turn 解是否在缓存 + 可导航，与 turn 自身 range 来源
    // 无关 → solve_turn 复用 §4.4 的 cross=None，简洁等价）。giveup → 缓存无 turn → river cross 必 miss。
    s.turn_giveup = solve_turn(
        &mut cache,
        &turn_rs,
        &turn_legal,
        game,
        strat,
        &args.search,
        hand_seed,
    )
    .is_none();
    // river 决策（cross=Some(turn_within)）：record_cross 在 solve 前记 → Ok/Err 都已计数，delta 有效。
    let att0 = cache.cross_attempts();
    let hit0 = cache.cross_hits();
    let river = subgame_search_cached(
        Some(&mut cache),
        &river_rs.game_state,
        &river_rs.game_state,
        game,
        &river_legal,
        river_rs.current_node_id,
        strat,
        &args.search,
        None,
        None,
        None,
        Some(&turn_within),
        hand_seed,
        0,
    );
    s.cross_attempt = cache.cross_attempts() > att0;
    s.cross_hit = cache.cross_hits() > hit0;
    s.river_search_giveup = river.is_err();
    s
}

fn print_hitrate_report(stats: &[HitrateStat], deals_total: u64) {
    let reached: Vec<&HitrateStat> = stats.iter().filter(|s| s.river_reached).collect();
    let n = reached.len();
    println!("\n=== §4.5 跨街复用命中率（自对弈 on-tree river 决策；锚定路径）===");
    println!(
        "  deal: {n} 到 hero river / {} skip（共 {deals_total}）",
        deals_total as usize - n
    );
    if n == 0 {
        println!("  无 river 决策——检查 reshape/checkpoint。");
        return;
    }
    let hits = reached.iter().filter(|s| s.cross_hit).count();
    let attempts = reached.iter().filter(|s| s.cross_attempt).count();
    let turn_giveup = reached.iter().filter(|s| s.turn_giveup).count();
    let miss_nav = reached
        .iter()
        .filter(|s| s.cross_attempt && !s.cross_hit && !s.turn_giveup)
        .count();
    let river_giveup = reached.iter().filter(|s| s.river_search_giveup).count();
    println!(
        "  跨街命中（turn 信息经后验恢复、不退化）: {hits}/{n} = {:.1}%",
        100.0 * hits as f64 / n as f64
    );
    println!(
        "  未命中（river range 退化为「preflop+flop reach × turn uniform」，§3 兜底）: {}/{n} = {:.1}%",
        n - hits,
        100.0 * (n - hits) as f64 / n as f64
    );
    println!("    ├─ turn 解 giveup（缓存无 turn 子树）: {turn_giveup}");
    println!("    └─ turn 已解但导航/身份失配（off-menu）: {miss_nav}");
    println!(
        "  [参考] cross attempt 计数: {attempts}/{n}（应 ≈ n：cross_street=Some + cache 传入）；\
         river hero search 自身 giveup（正交，advisor check-when-free）: {river_giveup}"
    );
    println!(
        "\n  读法（go/no-go 闸）：命中率 = 裁掉 turn blueprint 后 river range **不退化**的占比。够高 →\n  裁近无损（极少 river 退 turn-uniform，§3 兜底接住、不崩）；低 → 裁会实质劣化 → 别裁 / 知情接受。\n  口径：自对弈全 on-tree（off-menu 失配近 0）+ 固定 iter（giveup 随 iter，生产 12s/24线程更少 →\n  命中率本读数是**下界**）。脱锚 river（off-tree 尾）自对弈触发~0，须 live 测（已插桩 advisor 待 live）。"
    );
}

fn apply(st: &mut GameState, a: Action) -> Result<(), String> {
    st.apply(a).map_err(|e| format!("apply({a:?}): {e:?}"))
}

/// apply + 记 within（动作, 该动作是否令 `actor` all-in）。
fn apply_within(
    st: &mut GameState,
    a: Action,
    actor: usize,
    within: &mut Vec<(Action, bool)>,
) -> Result<(), String> {
    apply(st, a)?;
    let became_all_in = st.players()[actor].status == PlayerStatus::AllIn;
    within.push((a, became_all_in));
    Ok(())
}

/// 两个子树自身合法集分布的 TV 距离 + argmax 翻转（仅 root range 先验不同 → 同 legal 集逐位对齐）。
fn dist_tv_and_flip(a: &[(AbstractAction, f64)], b: &[(AbstractAction, f64)]) -> (f64, bool) {
    let argmax = |d: &[(AbstractAction, f64)]| -> Option<AbstractAction> {
        d.iter()
            .max_by(|x, y| x.1.partial_cmp(&y.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(act, _)| *act)
    };
    let flip = argmax(a) != argmax(b);
    if a.len() != b.len() {
        return (1.0, flip);
    }
    let mut tv = 0.0_f64;
    for ((aa, pa), (ab, pb)) in a.iter().zip(b.iter()) {
        if aa != ab {
            return (1.0, flip);
        }
        tv += (pa - pb).abs();
    }
    (0.5 * tv, flip)
}

fn print_report(results: &[DealResult], deals_total: u64, top_k: usize, anchored: bool) {
    let (decision, line_desc, off_label, on_label) = if anchored {
        (
            "river",
            "on-tree flop→turn→river 线",
            "estimate(turn blueprint)",
            "turn σ 后验",
        )
    } else {
        (
            "turn",
            "构造 off-tree flop→turn 加注战线",
            "档一前缀",
            "flop σ 后验",
        )
    };
    let solved = results.len();
    let skipped = deals_total as usize - solved;
    if solved == 0 {
        println!("\n=== 跨街 ON vs OFF 决策级 A/B ===");
        println!("  全部 {deals_total} deal 都 skip（构造线/解失败）——检查 stacks/触发。");
        return;
    }
    let fired: Vec<&DealResult> = results.iter().filter(|r| r.tv > 1e-9).collect();
    let flips = results.iter().filter(|r| r.flip).count();
    let mut tvs: Vec<f64> = results.iter().map(|r| r.tv).collect();
    tvs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mean = tvs.iter().sum::<f64>() / tvs.len() as f64;
    let median = tvs[tvs.len() / 2];
    let max = *tvs.last().unwrap();
    let fired_mean = if fired.is_empty() {
        0.0
    } else {
        fired.iter().map(|r| r.tv).sum::<f64>() / fired.len() as f64
    };

    println!("\n=== 跨街复用 ON vs OFF 决策级 A/B（{line_desc}）===");
    println!("  deal: {solved} 解出 / {skipped} skip（共 {deals_total}）");
    println!(
        "  跨街触发（ON≠OFF, TV>0）: {}/{solved} ({:.1}%)",
        fired.len(),
        100.0 * fired.len() as f64 / solved as f64
    );
    println!(
        "  hero {decision} 决策 TV（ON vs OFF）: mean={mean:.4} median={median:.4} max={max:.4}；触发子集 mean={fired_mean:.4}"
    );
    println!(
        "  argmax 翻转: {flips}/{solved} ({:.1}%)",
        100.0 * flips as f64 / solved as f64
    );
    // TV 直方图。
    let buckets = [
        (0.0, 1e-9, "=0"),
        (1e-9, 0.05, "(0,.05]"),
        (0.05, 0.15, "(.05,.15]"),
        (0.15, 0.30, "(.15,.30]"),
        (0.30, 0.50, "(.30,.50]"),
        (0.50, 1.01, "(.50,1]"),
    ];
    print!("  TV 直方图:");
    for (lo, hi, label) in buckets {
        let c = results
            .iter()
            .filter(|r| r.tv > lo - 1e-12 && r.tv <= hi)
            .count();
        print!(" {label}={c}");
    }
    println!();

    // top-K 最大 TV 谱（人读方向）。
    let mut by_tv: Vec<&DealResult> = results.iter().collect();
    by_tv.sort_by(|a, b| b.tv.partial_cmp(&a.tv).unwrap_or(std::cmp::Ordering::Equal));
    println!(
        "\n  top-{} 最大决策改变谱（OFF={off_label} / ON={on_label}）:",
        top_k.min(by_tv.len())
    );
    for r in by_tv.iter().take(top_k) {
        println!(
            "  ── deal=0x{:016x} hero={} board={} | TV={:.4}{}",
            r.deal_seed,
            fmt_cards(&r.hero_hole),
            fmt_cards(&r.board),
            r.tv,
            if r.flip { " [argmax FLIP]" } else { "" }
        );
        println!("       OFF({off_label}): {}", fmt_dist(&r.off));
        println!("       ON ({on_label}): {}", fmt_dist(&r.on));
    }
    let reading = if anchored {
        "\n  读法：TV/翻转 = 「裁掉 turn blueprint、river range 改走 turn 子树后验」对 river 决策的**幅度**；\n  方向看 top-K（§动机 = turn 子树后验比泛 blueprint estimate 更贴本子博弈）。TV 小 → 裁 turn 近无损；\n  TV 大且方向合理 → 后验确有信息。构造谱非真实频率，单数字勿当判决；命中率（§4.5）才是裁剪 go/no-go。"
    } else {
        "\n  读法：TV/翻转 = 跨街 range 精化对 turn 决策的**幅度**；方向看 top-K（§动机 = ON 应把档一\n  丢掉的 flop 加注战信号捡回 → 价值线收紧/诈唬下修）。构造谱非真实频率，单数字勿当强弱判决。"
    };
    println!("{reading}");
}

fn fmt_cards(cs: &[Card]) -> String {
    const RANKS: &[u8; 13] = b"23456789TJQKA";
    const SUITS: &[u8; 4] = b"cdhs";
    cs.iter()
        .map(|c| {
            let v = c.to_u8();
            format!(
                "{}{}",
                RANKS[(v / 4) as usize] as char,
                SUITS[(v % 4) as usize] as char
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn fmt_dist(d: &[(AbstractAction, f64)]) -> String {
    d.iter()
        .map(|(a, p)| format!("{a:?}={p:.3}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn mix2(a: u64, b: u64) -> u64 {
    let mut x = a ^ 0x9E37_79B9_7F4A_7C15;
    x ^= b;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 31;
    x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

fn open_table(path: &str) -> Result<std::sync::Arc<BucketTable>, String> {
    BucketTable::open(std::path::Path::new(path))
        .map(std::sync::Arc::new)
        .map_err(|e| format!("BucketTable::open({path}) failed: {e:?}"))
}

fn reshape_profile(
    reshape: &str,
    cap: u8,
) -> Result<(StreetActionAbstraction, BettingAbstractionRules), String> {
    Ok(match reshape {
        "none" => first_small_6max(cap),
        "nolimp" => {
            let (a, mut r) = first_small_6max(cap);
            r.no_open_limp = true;
            (a, r)
        }
        "preopen" => first_small_preopen_6max(cap),
        "preopen-small" => first_small_preopen_small_6max(cap),
        other => return Err(format!("unknown reshape {other}")),
    })
}

fn parse_args() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let mut bucket_table: Option<String> = None;
    let mut reshape = "nolimp".to_string();
    let mut checkpoint: Option<String> = None;
    let mut postflop_cap = 3u8;
    let mut deals = 64u64;
    let mut seed = 0xC505_A11C_u64;
    let mut top_k = 8usize;
    let mut iterations = 1000u64;
    let mut trigger = SearchTrigger::AllPostflop;
    let mut deep_menu = true;
    let mut max_nodes = 1_000_000usize;
    let mut range_mix = 0.25f64;
    let mut anchored = false;
    let mut hitrate = false;
    let mut utg_bb = 10u64;
    let mut sb_raise_bb = 20u64;
    let mut flop_bet_bb = 12u64;
    let mut flop_raise_bb = 30u64;
    let next = |it: &mut std::iter::Skip<std::env::Args>, name: &str| -> Result<String, String> {
        it.next().ok_or_else(|| format!("{name} 需要一个值"))
    };
    let parse_u64 = |s: String| -> Result<u64, String> {
        s.strip_prefix("0x")
            .map(|h| u64::from_str_radix(h, 16))
            .unwrap_or_else(|| s.parse())
            .map_err(|e| format!("bad u64 {s}: {e}"))
    };
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--bucket-table" => bucket_table = Some(next(&mut it, &arg)?),
            "--reshape" => reshape = next(&mut it, &arg)?,
            "--checkpoint" => checkpoint = Some(next(&mut it, &arg)?),
            "--postflop-cap" => {
                postflop_cap = next(&mut it, &arg)?
                    .parse()
                    .map_err(|e| format!("bad cap: {e}"))?
            }
            "--deals" => deals = parse_u64(next(&mut it, &arg)?)?,
            "--seed" => seed = parse_u64(next(&mut it, &arg)?)?,
            "--top-k" => {
                top_k = next(&mut it, &arg)?
                    .parse()
                    .map_err(|e| format!("bad top-k: {e}"))?
            }
            "--search-iterations" => iterations = parse_u64(next(&mut it, &arg)?)?,
            "--trigger" => {
                trigger = match next(&mut it, &arg)?.as_str() {
                    "all-postflop" => SearchTrigger::AllPostflop,
                    "flop-first-unraised" => SearchTrigger::FlopFirstUnraised,
                    other => return Err(format!("unknown --trigger {other}")),
                };
            }
            "--no-deep-menu" => deep_menu = false,
            "--search-max-nodes" => {
                max_nodes = next(&mut it, &arg)?
                    .parse()
                    .map_err(|e| format!("bad max-nodes: {e}"))?
            }
            "--range-uniform-mix" => {
                range_mix = next(&mut it, &arg)?
                    .parse()
                    .map_err(|e| format!("bad range-mix: {e}"))?;
                if !(0.0..=1.0).contains(&range_mix) {
                    return Err(format!("--range-uniform-mix 须在 [0,1]，得 {range_mix}"));
                }
            }
            "--anchored" => anchored = true,
            "--hitrate" => hitrate = true,
            "--utg-bb" => utg_bb = parse_u64(next(&mut it, &arg)?)?,
            "--sb-raise-bb" => sb_raise_bb = parse_u64(next(&mut it, &arg)?)?,
            "--flop-bet-bb" => flop_bet_bb = parse_u64(next(&mut it, &arg)?)?,
            "--flop-raise-bb" => flop_raise_bb = parse_u64(next(&mut it, &arg)?)?,
            other => return Err(format!("unknown arg {other}")),
        }
    }
    let search = SubgameSearchConfig {
        iterations,
        max_subtree_nodes: max_nodes,
        trigger,
        resolve_root: ResolveRoot::RoundStart,
        deep_menu,
        range_uniform_mix: range_mix,
        ..SubgameSearchConfig::default()
    };
    Ok(Args {
        bucket_table: bucket_table.ok_or("缺 --bucket-table")?,
        reshape,
        checkpoint: checkpoint.ok_or("缺 --checkpoint")?,
        postflop_cap,
        deals,
        seed,
        top_k,
        search,
        anchored,
        hitrate,
        utg_bb,
        sb_raise_bb,
        flop_bet_bb,
        flop_raise_bb,
    })
}
