//! 档二′-跨街复用（`--search-unanchored-cross-street`）ON vs OFF **决策级** A/B，跑在**构造的
//! deep off-tree flop→turn 加注战线**上（`unanchored_range_design` §动机 / §4 「仍 pending = 强弱」
//! 指定的「干净标尺 = 构造 off-tree-flop→turn 线」）。
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

use poker::training::game::Game;
use poker::training::nlhe::SimplifiedNlheGame;
use poker::training::nlhe_betting_tree::{
    first_small_6max, first_small_preopen_6max, first_small_preopen_small_6max, AbstractActionTag,
    BettingAbstractionRules, NodeId,
};
use poker::training::nlhe_dense_trainer::DenseNlheEsMccfrTrainer;
use poker::training::subgame::{
    subgame_search_unanchored_cached, subgame_search_unanchored_cached_cross,
    synced_prefix_decisions, PrefixReach, ResolveRoot, SearchTrigger, SubgameSearchConfig,
    SubgameSolveCache,
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
    /// 构造线参数（BB）：UTG 短码 all-in / SB raise-over to / flop SB bet to / flop BB raise to。
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

    // 同步前缀（deal-independent：影子走 abstract all-in + 3 fold → SB 节点 = 断点前）。
    let synced = build_synced_prefix(game_ref)?;

    eprintln!(
        "[cross_street_ab] reshape={} cap={} update_count={} synced_prefix_len={}",
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

    let results: Vec<DealResult> = (0..args.deals)
        .into_par_iter()
        .filter_map(|i| eval_deal(game_ref, &strat, &game_cfg, &synced, &args, i))
        .collect();

    print_report(&results, args.deals, args.top_k);
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

fn print_report(results: &[DealResult], deals_total: u64, top_k: usize) {
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

    println!("\n=== 跨街复用 ON vs OFF 决策级 A/B（构造 off-tree flop→turn 加注战线）===");
    println!("  deal: {solved} 解出 / {skipped} skip（共 {deals_total}）");
    println!(
        "  跨街触发（ON≠OFF, TV>0）: {}/{solved} ({:.1}%)",
        fired.len(),
        100.0 * fired.len() as f64 / solved as f64
    );
    println!(
        "  hero turn 决策 TV（ON vs OFF）: mean={mean:.4} median={median:.4} max={max:.4}；触发子集 mean={fired_mean:.4}"
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
        "\n  top-{} 最大决策改变谱（hero=SB；OFF=档一前缀 / ON=flop σ 后验）:",
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
        println!("       OFF(档一): {}", fmt_dist(&r.off));
        println!("       ON (档二′): {}", fmt_dist(&r.on));
    }
    println!(
        "\n  读法：TV/翻转 = 跨街 range 精化对 turn 决策的**幅度**；方向看 top-K（§动机 = ON 应把档一\n  丢掉的 flop 加注战信号捡回 → 价值线收紧/诈唬下修）。构造谱非真实频率，单数字勿当强弱判决。"
    );
}

fn fmt_cards(cs: &[Card]) -> String {
    cs.iter()
        .map(|c| format!("{c:?}"))
        .collect::<Vec<_>>()
        .join("")
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
        utg_bb,
        sb_raise_bb,
        flop_bet_bb,
        flop_raise_bb,
    })
}
