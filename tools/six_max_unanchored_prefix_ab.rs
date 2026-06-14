//! 脱锚搜索**档一**（同步前缀 reach）相对强度 A/B：**uniform vs 前缀 reach**。
//!
//! 回答 `docs/temp/unanchored_range_design_2026_06_10.md` §3 的强度验收——「前缀 reach 比
//! uniform 强吗」。脱锚搜索（`subgame_search_unanchored`）只在 100BB 影子**失同步**时触发
//! （off-stack all-in / 真 4+way / limp 池），而既有 h2h harness（`evaluate_cross_abstraction_h2h`）
//! 用**常驻影子**、失同步即 `HandError::Desync` 排除整手——**结构上到不了脱锚路径**，也表达不了
//! 前缀 reach。故本探针自带自对弈环：单影子追 auth，失同步后**所有座**改走脱锚搜索（postflop），
//! hero = 前缀 reach、field = uniform，配对差量「前缀 reach 的净 EV」。
//!
//! # 设计（务必随结果一并解读）
//!
//! - **场景生成**：从 preflop 自对弈（blueprint 策略 + 单影子）；失同步前的决策用 blueprint
//!   （两臂逐字相同 = 配对基底），失同步即转脱锚模式。为让 off-tree pot **真到 flop**（前缀 reach
//!   只活在 postflop 脱锚搜索），失同步后的**非搜索**决策（剩余 preflop / 未触发）走「stay」策略
//!   （能 check 则 check、否则 call、否则 fold）——**人为**保 pot 不塌、两臂逐字相同。**故场景分布
//!   不真实**（真人不会都 call），但配对 A/B 的**相对**比较仍成立：「给定这些 off-tree flop，前缀
//!   reach 帮不帮」。
//! - **触发**：用**不等码深**（默认混合短码栈）逼出 off-stack 线；对称栈则靠 4+way。无触发 → 报告
//!   `hands_unanchored=0`、A/B 无意义（需调栈型）。
//! - **决策改变率（便宜读数）**：前缀臂里每个 hero 脱锚搜索点，**额外**算一遍 uniform 分布，记
//!   TV 距离 + argmax 是否翻转——直接量「前缀 reach 改不改决策」，不依赖 EV（若几乎不改，EV A/B 即
//!   moot）。仅前缀臂算（成本 ×当点 1 次额外 solve）。
//! - **统计**：两臂同 seed/同手 → 逐手 hero PnL 配对差 CI（消去发牌共同方差，§11.5 同口径）。效应
//!   可能小到判不动（per-bucket 噪声 ≈ 效应，见 `project_6max_search_range_prior_overexploit`）——
//!   smoke 先看触发率 + 决策改变率 + 方向，再决定要不要上预算大跑。
//!
//! 用法（vultr smoke）：
//! ```bash
//! cargo run --release --bin six_max_unanchored_prefix_ab -- \
//!   --bucket-table artifacts/bucket_table_200_200_200_seed_cafebabe_schemav4.bin \
//!   --reshape nolimp --postflop-cap 3 \
//!   --checkpoint artifacts/run_6max_s4_nolimp/nlhe_es_mccfr_final_001000000000.ckpt \
//!   --hands-per-seat 200 --search-iterations 1000 \
//!   --stacks 100,100,100,40,30,100   # 不等栈逼 off-stack；省略 = 默认混合
//! ```

use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use rayon::prelude::*;

use poker::training::blueprint_advisor::{advance_shadow_by_applied, outgoing_action};
use poker::training::game::Game;
use poker::training::nlhe::{SimplifiedNlheGame, SimplifiedNlheState};
use poker::training::nlhe_betting_tree::{
    deep_menu_for, first_small_6max, first_small_preopen_6max, first_small_preopen_small_6max,
    BettingAbstractionRules, NodeId,
};
use poker::training::nlhe_dense_trainer::DenseNlheEsMccfrTrainer;
use poker::training::sampling::sample_discrete;
use poker::training::subgame::{
    should_search, subgame_search_unanchored_cached, synced_prefix_decisions, PrefixReach,
    ResolveRoot, SearchTrigger, SubgameSearchConfig,
};
use poker::{
    AbstractAction, Action, BucketTable, Card, ChaCha20Rng, ChipAmount, GameState, InfoSetId,
    PlayerStatus, RngSource, Street, StreetActionAbstraction, TableConfig,
};

const N_SEATS: usize = 6;
const SHADOW_SEED: u64 = 0x5348_4144_5541_4231; // "SHADUAB1"
const SAMPLE_SEED: u64 = 0x5341_4d50_5541_4232; // "SAMPUAB2"

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("[unanchored_prefix_ab] failed: {e}");
            ExitCode::from(2)
        }
    }
}

struct Args {
    bucket_table: String,
    reshape: String,
    checkpoint: String,
    postflop_cap: u8,
    hands_per_seat: u64,
    seed: u64,
    search: SubgameSearchConfig,
    /// per-seat 起始码深（BB）。`None` = 默认混合短码栈（逼 off-stack 线）。
    stacks_bb: Option<[u64; N_SEATS]>,
}

/// 自对弈遥测（跨 rayon 线程原子累加）。
#[derive(Default)]
struct ProbeObs {
    hands_total: AtomicU64,          // 跑过的手数
    hands_reached_flop: AtomicU64,   // 至少一个 postflop 决策的手数
    postflop_decisions: AtomicU64,   // postflop 决策总数（同步 + 脱锚）
    hands_unanchored: AtomicU64,     // 进入脱锚模式的手数
    unanchored_decisions: AtomicU64, // 脱锚模式下的决策总数
    search_fired: AtomicU64,         // 脱锚 postflop 触发的搜索次数
    search_giveup: AtomicU64,        // 搜索失败 → stay 的次数
    hero_dc_measured: AtomicU64,     // 量了决策改变的 hero 搜索点数（仅前缀臂）
    hero_dc_flipped: AtomicU64,      // 其中 argmax 翻转的点数
    hero_tv_micro_sum: AtomicU64,    // Σ TV·1e6（算均 TV）
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    if !matches!(args.postflop_cap, 2..=4) {
        return Err(format!(
            "--postflop-cap must be 2..=4, got {}",
            args.postflop_cap
        ));
    }
    let table = Arc::new(
        BucketTable::open(std::path::Path::new(&args.bucket_table))
            .map_err(|e| format!("BucketTable::open({}) failed: {e:?}", args.bucket_table))?,
    );
    // **game/shadow 用对称 100BB**（= blueprint 训练树，dense ckpt layout 按它键，不对称会
    // fingerprint mismatch）；**auth（真自对弈）才用 --stacks 不等码深**（逼 off-stack 线）。
    // 这正是生产口径：blueprint 100BB 对称、真局不等栈、影子 100BB、auth 真栈（advisor lockstep）。
    let game_cfg = TableConfig::default_6max_100bb();
    let bb = game_cfg.big_blind.as_u64();
    let stacks = args.stacks_bb.unwrap_or([100, 100, 100, 40, 30, 100]);
    let mut auth_cfg = game_cfg.clone();
    for (seat, &s_bb) in stacks.iter().enumerate() {
        auth_cfg.starting_stacks[seat] = ChipAmount::new(s_bb * bb);
    }

    let (abs, rules) = reshape_profile(&args.reshape, args.postflop_cap)?;
    let game =
        SimplifiedNlheGame::new_with_abstraction(Arc::clone(&table), game_cfg.clone(), abs, rules)
            .map_err(|e| format!("build game failed: {e:?}"))?;
    let trainer =
        DenseNlheEsMccfrTrainer::load_checkpoint(std::path::Path::new(&args.checkpoint), game)
            .map_err(|e| format!("load checkpoint {} failed: {e:?}", args.checkpoint))?;
    let strat = |info: &InfoSetId, _n: usize| trainer.average_strategy(*info);

    eprintln!(
        "[unanchored_prefix_ab] reshape={} cap={} update_count={} stacks_bb={:?}",
        args.reshape,
        args.postflop_cap,
        trainer.update_count(),
        stacks
    );
    eprintln!(
        "[unanchored_prefix_ab] search: iters={} trigger={:?} deep_menu={} range_mix={} max_nodes={} seed=0x{:016x} hands/seat={} (×{} 座)",
        args.search.iterations, args.search.trigger, args.search.deep_menu,
        args.search.range_uniform_mix, args.search.max_subtree_nodes, args.seed,
        args.hands_per_seat, N_SEATS
    );

    let game_ref = trainer.game();
    // 前缀臂（hero 前缀 reach + 量决策改变） + uniform 臂（hero uniform），同 seed/同手 → 配对差。
    let obs = ProbeObs::default();
    let arm_prefix = run_arm(game_ref, &strat, &auth_cfg, &args, true, &obs);
    let arm_uniform = run_arm(
        game_ref,
        &strat,
        &auth_cfg,
        &args,
        false,
        &ProbeObs::default(),
    );

    print_arm("前缀 reach 臂", &arm_prefix, &auth_cfg);
    print_arm("uniform 臂", &arm_uniform, &auth_cfg);
    print_obs(&obs);
    print_paired_diff(&arm_prefix, &arm_uniform, &auth_cfg);
    Ok(())
}

struct ArmReport {
    per_hand_pnl: Vec<Option<f64>>, // 对齐完整 task 列表（Some=计入 / None=skip）
    counted: u64,
    skipped: u64,
}

/// 跑一臂：hero 轮坐全部 6 座（每座 hands_per_seat 手），其余座 field。`hero_prefix` = hero 脱锚
/// 搜索是否用前缀 reach（field 恒 uniform）。每手 (seat, hand) 并行、seed 确定派生 → 两臂逐下标同手。
fn run_arm(
    game: &SimplifiedNlheGame,
    strat: &(dyn Fn(&InfoSetId, usize) -> Vec<f64> + Sync),
    cfg: &TableConfig,
    args: &Args,
    hero_prefix: bool,
    obs: &ProbeObs,
) -> ArmReport {
    let tasks: Vec<(usize, u64)> = (0..N_SEATS)
        .flat_map(|hero_seat| (0..args.hands_per_seat).map(move |h| (hero_seat, h)))
        .collect();
    let outcomes: Vec<Option<f64>> = tasks
        .par_iter()
        .map(|&(hero_seat, hand_idx)| {
            let hand_seed = mix3(args.seed, hero_seat as u64, hand_idx);
            match play_hand(
                game,
                strat,
                cfg,
                hero_seat,
                hero_prefix,
                &args.search,
                hand_seed,
                hero_prefix,
                obs,
            ) {
                Ok(pnls) => Some(pnls[hero_seat]),
                Err(_) => None,
            }
        })
        .collect();
    let counted = outcomes.iter().filter(|o| o.is_some()).count() as u64;
    let skipped = outcomes.len() as u64 - counted;
    ArmReport {
        per_hand_pnl: outcomes,
        counted,
        skipped,
    }
}

/// stay 策略：脱锚模式下**非搜索**决策（剩余 preflop / 未触发）—— 保 pot 不塌、两臂逐字相同。
/// 能 check 则 Check、否则能 call 则 Call、否则 Fold（同 advisor check-when-free 的扩展）。
fn stay_action(auth: &GameState) -> Action {
    let la = auth.legal_actions();
    if la.check {
        Action::Check
    } else if la.call.is_some() {
        Action::Call
    } else {
        Action::Fold
    }
}

/// blueprint 分布（空 / 全零 / 长度不符 → uniform 兜底）；与 advisor blueprint_distribution 同口径。
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

/// 一手自对弈：单影子追 auth，失同步前 blueprint（场景基底），失同步后所有座脱锚搜索（postflop）/
/// stay（preflop）。返回 per-seat PnL。`record_dc` = 是否量 hero 决策改变（仅前缀臂传 true）。
#[allow(clippy::too_many_arguments)]
fn play_hand(
    game: &SimplifiedNlheGame,
    strat: &(dyn Fn(&InfoSetId, usize) -> Vec<f64> + Sync),
    config: &TableConfig,
    hero_seat: usize,
    hero_prefix: bool,
    search_cfg: &SubgameSearchConfig,
    hand_seed: u64,
    record_dc: bool,
    obs: &ProbeObs,
) -> Result<Vec<f64>, String> {
    let n = config.n_seats as usize;
    let mut auth = GameState::new(config, hand_seed);
    let mut shadow_rng = ChaCha20Rng::from_seed(SHADOW_SEED ^ hand_seed);
    let mut shadow: SimplifiedNlheState = game.root(&mut shadow_rng);
    let mut sample_rng = ChaCha20Rng::from_seed(SAMPLE_SEED ^ hand_seed);

    obs.hands_total.fetch_add(1, Ordering::Relaxed);
    let mut unanchored = false;
    let mut counted_unanchored = false; // 本手是否已计入 hands_unanchored
    let mut reached_flop = false; // 本手是否已计入 hands_reached_flop
    let mut synced_node: NodeId = shadow.current_node_id;
    let mut round_start: Option<GameState> = None;
    let mut round_start_street: Option<Street> = None;
    let mut round_within: Vec<(Action, bool)> = Vec::new();

    for _ in 0..512usize {
        if auth.is_terminal() {
            return payoffs(&auth, n);
        }
        let Some(actor) = auth.current_player() else {
            return payoffs(&auth, n);
        };
        let actor_idx = actor.0 as usize;

        // round-start 快照（loop 顶、apply 前 = 本街轮起点；街变重 snapshot + 清 within）。
        if round_start_street != Some(auth.street()) {
            round_start = Some(auth.clone());
            round_start_street = Some(auth.street());
            round_within.clear();
        }

        // 同步守门：影子行动者 / 街须与 auth 一致；不一致 = 漂移失同步（synced_node 留上一同步点）。
        if !unanchored {
            if shadow.game_state.current_player() == Some(actor)
                && shadow.game_state.street() == auth.street()
            {
                synced_node = shadow.current_node_id; // 本决策的同步节点（off-tree 动作前 = 前缀末）
            } else {
                unanchored = true;
            }
        }

        let hole = auth.players()[actor_idx]
            .hole_cards
            .ok_or_else(|| format!("actor {actor:?} 无手牌"))?;
        let board: Vec<Card> = auth.board().to_vec();
        if board.len() >= 3 {
            obs.postflop_decisions.fetch_add(1, Ordering::Relaxed);
            if !reached_flop {
                reached_flop = true;
                obs.hands_reached_flop.fetch_add(1, Ordering::Relaxed);
            }
        }

        // 决策 → applied Action（+ outgoing 抽象）。
        let applied: Action = if !unanchored {
            // 同步：blueprint（场景基底，两臂逐字相同）。
            let node_id = shadow.current_node_id;
            let legal_abs = SimplifiedNlheGame::legal_actions(&shadow);
            if legal_abs.is_empty() {
                unanchored = true;
            }
            if unanchored {
                unanchored_applied(
                    game,
                    strat,
                    &auth,
                    round_start.as_ref().unwrap_or(&auth),
                    &round_within,
                    search_cfg,
                    actor_idx == hero_seat,
                    hero_prefix,
                    synced_node,
                    hand_seed,
                    record_dc,
                    &mut sample_rng,
                    obs,
                )?
            } else {
                let info = game.info_set_for_cards(node_id, hole, &board);
                let dist = blueprint_dist(&info, &legal_abs, strat);
                let chosen = sample_discrete(&dist, sample_rng_dyn(&mut sample_rng));
                outgoing_action(&auth, game.abstraction(), chosen)
                    .map_err(|e| format!("synced outgoing: {e}"))?
            }
        } else {
            unanchored_applied(
                game,
                strat,
                &auth,
                round_start.as_ref().unwrap_or(&auth),
                &round_within,
                search_cfg,
                actor_idx == hero_seat,
                hero_prefix,
                synced_node,
                hand_seed,
                record_dc,
                &mut sample_rng,
                obs,
            )?
        };

        if unanchored && !counted_unanchored {
            obs.hands_unanchored.fetch_add(1, Ordering::Relaxed);
            counted_unanchored = true;
        }

        // apply 到 auth + 维护 within（收街动作属上一街 → 不入序）。
        auth.apply(applied)
            .map_err(|e| format!("auth apply({applied:?}): {e:?}"))?;
        let became_all_in = auth.players()[actor_idx].status == PlayerStatus::AllIn;
        if round_start_street == Some(auth.street()) {
            round_within.push((applied, became_all_in));
        }

        // 推进影子（仅同步时）：失败 → 转脱锚（synced_node 已 = 本决策节点 = off-tree 动作前）。
        if !unanchored
            && advance_shadow_by_applied(
                &mut shadow,
                applied,
                became_all_in,
                sample_rng_dyn(&mut sample_rng),
            )
            .is_err()
        {
            unanchored = true;
        }
    }
    Err("max_actions 超限（非终局）".into())
}

/// 脱锚模式单决策 → applied Action。postflop 触发 → 脱锚搜索（hero 前缀 / field uniform）；否则
/// stay。`is_hero` + `hero_prefix` 决定本座是否用前缀；`record_dc` 时 hero 搜索点额外算 uniform 分布
/// 记 TV / argmax flip。
#[allow(clippy::too_many_arguments)]
fn unanchored_applied(
    game: &SimplifiedNlheGame,
    strat: &(dyn Fn(&InfoSetId, usize) -> Vec<f64> + Sync),
    auth: &GameState,
    round_start: &GameState,
    round_within: &[(Action, bool)],
    search_cfg: &SubgameSearchConfig,
    is_hero: bool,
    hero_prefix: bool,
    synced_node: NodeId,
    hand_seed: u64,
    record_dc: bool,
    sample_rng: &mut ChaCha20Rng,
    obs: &ProbeObs,
) -> Result<Action, String> {
    obs.unanchored_decisions.fetch_add(1, Ordering::Relaxed);
    let use_prefix = if is_hero { hero_prefix } else { false };
    if auth.board().len() >= 3 && should_search(auth, search_cfg.trigger) {
        obs.search_fired.fetch_add(1, Ordering::Relaxed);
        let prefix_dec = use_prefix.then(|| synced_prefix_decisions(game, synced_node));
        let prefix = prefix_dec.as_ref().map(|d| PrefixReach {
            strategy: strat,
            decisions: d,
        });
        let solved = subgame_search_unanchored_cached(
            None,
            auth,
            round_start,
            game,
            round_within,
            search_cfg,
            None,
            prefix,
            hand_seed,
        );
        if let Ok(dist) = solved {
            // 决策改变率（仅前缀臂 hero 搜索点）：额外算 uniform 分布，记 TV / argmax flip。
            if record_dc && is_hero && hero_prefix {
                if let Ok(uni) = subgame_search_unanchored_cached(
                    None,
                    auth,
                    round_start,
                    game,
                    round_within,
                    search_cfg,
                    None,
                    None,
                    hand_seed,
                ) {
                    let (tv, flip) = dist_tv_and_flip(&dist, &uni);
                    obs.hero_dc_measured.fetch_add(1, Ordering::Relaxed);
                    obs.hero_tv_micro_sum
                        .fetch_add((tv * 1.0e6) as u64, Ordering::Relaxed);
                    if flip {
                        obs.hero_dc_flipped.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            let outgoing_abs: StreetActionAbstraction = if search_cfg.deep_menu {
                deep_menu_for(round_start).0
            } else {
                game.abstraction().clone()
            };
            let chosen = sample_discrete(&dist, sample_rng_dyn(sample_rng));
            return outgoing_action(auth, &outgoing_abs, chosen)
                .map_err(|e| format!("unanchored outgoing: {e}"));
        }
        obs.search_giveup.fetch_add(1, Ordering::Relaxed);
    }
    // 非 postflop / 未触发 / 搜索失败 → stay（保 pot，两臂相同）。
    Ok(stay_action(auth))
}

/// 两个子树自身合法集分布的 TV 距离 + argmax 是否翻转。两分布**同子树同抽象**（仅 root range 先验
/// 不同）→ 动作集同序、逐位对齐。维度不符（不应发生）→ TV=1 / flip=true 上界。
fn dist_tv_and_flip(a: &[(AbstractAction, f64)], b: &[(AbstractAction, f64)]) -> (f64, bool) {
    let argmax = |d: &[(AbstractAction, f64)]| -> Option<AbstractAction> {
        d.iter()
            .max_by(|x, y| x.1.partial_cmp(&y.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(act, _)| *act)
    };
    let flip = argmax(a) != argmax(b);
    // TV：按 AbstractActionTag 对齐求 0.5·Σ|p−q|。两分布同 legal 集 → 直接逐位（防御维度不符）。
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

fn payoffs(auth: &GameState, n: usize) -> Result<Vec<f64>, String> {
    let payouts = auth.payouts().ok_or("终局但 payouts()==None")?;
    let mut out = vec![0.0_f64; n];
    for (seat, pnl) in payouts {
        out[seat.0 as usize] = pnl as f64;
    }
    Ok(out)
}

fn sample_rng_dyn(rng: &mut ChaCha20Rng) -> &mut dyn RngSource {
    rng
}

fn mix3(a: u64, b: u64, c: u64) -> u64 {
    let mut x = a ^ 0x9E37_79B9_7F4A_7C15;
    for v in [b, c] {
        x ^= v;
        x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x ^= x >> 31;
    }
    x
}

// ===========================================================================
// 报表
// ===========================================================================

fn arm_mean_se(per_hand: &[Option<f64>]) -> (f64, f64, usize) {
    let v: Vec<f64> = per_hand.iter().filter_map(|o| *o).collect();
    let n = v.len();
    if n == 0 {
        return (0.0, 0.0, 0);
    }
    let mean = v.iter().sum::<f64>() / n as f64;
    let var = if n > 1 {
        v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1) as f64
    } else {
        0.0
    };
    (mean, (var / n as f64).sqrt(), n)
}

fn print_arm(label: &str, r: &ArmReport, cfg: &TableConfig) {
    let (mean, se, n) = arm_mean_se(&r.per_hand_pnl);
    let scale = 1000.0 / cfg.big_blind.as_u64() as f64;
    let (m, s) = (mean * scale, se * scale);
    println!(
        "== {label} ==  hero mbb/g = {:+.2}  SE = {:.2}  CI95 = [{:+.2}, {:+.2}]  ({n} 手计入 / {} skip)",
        m, s, m - 1.96 * s, m + 1.96 * s, r.skipped
    );
}

fn print_obs(obs: &ProbeObs) {
    let ht = obs.hands_total.load(Ordering::Relaxed);
    let hf = obs.hands_reached_flop.load(Ordering::Relaxed);
    let pd = obs.postflop_decisions.load(Ordering::Relaxed);
    let hu = obs.hands_unanchored.load(Ordering::Relaxed);
    let ud = obs.unanchored_decisions.load(Ordering::Relaxed);
    let sf = obs.search_fired.load(Ordering::Relaxed);
    let sg = obs.search_giveup.load(Ordering::Relaxed);
    let dcm = obs.hero_dc_measured.load(Ordering::Relaxed);
    let dcf = obs.hero_dc_flipped.load(Ordering::Relaxed);
    let tv = obs.hero_tv_micro_sum.load(Ordering::Relaxed);
    println!("\n== 触发遥测（前缀臂）==");
    println!(
        "  手数 = {ht}, 到 flop 手 = {hf} ({:.1}%), postflop 决策 = {pd}",
        if ht > 0 {
            100.0 * hf as f64 / ht as f64
        } else {
            0.0
        }
    );
    println!("  失同步手 = {hu}（脱锚模式手数；=0 → 无 off-tree 触发，A/B 无意义、需调栈型）");
    println!("  脱锚决策 = {ud}, 其中 postflop 触发搜索 = {sf}, giveup→stay = {sg}");
    if dcm > 0 {
        println!(
            "  hero 决策改变（前缀 vs uniform，同子树）: {dcm} 点量得, argmax 翻转 {dcf} ({:.1}%), 均 TV = {:.4}",
            100.0 * dcf as f64 / dcm as f64,
            (tv as f64 / dcm as f64) / 1.0e6
        );
        println!(
            "  → 决策改变 ≈0 ⇒ 前缀 reach 实战不改决策、EV A/B moot；改变大 ⇒ 值得上预算大跑量 EV"
        );
    } else {
        println!(
            "  ⚠ 无 hero 脱锚搜索点（决策改变未量）—— 触发面 / 栈型 / hero 是否进 postflop 池"
        );
    }
}

fn print_paired_diff(prefix: &ArmReport, uniform: &ArmReport, cfg: &TableConfig) {
    if prefix.per_hand_pnl.len() != uniform.per_hand_pnl.len() {
        println!("\n⚠ 配对差：两臂 task 数不一致 —— 无法配对");
        return;
    }
    let diffs: Vec<f64> = prefix
        .per_hand_pnl
        .iter()
        .zip(&uniform.per_hand_pnl)
        .filter_map(|(a, b)| match (a, b) {
            (Some(x), Some(y)) => Some(x - y),
            _ => None,
        })
        .collect();
    if diffs.is_empty() {
        println!("\n⚠ 配对差：无双方都计入的手");
        return;
    }
    let n = diffs.len();
    let mean = diffs.iter().sum::<f64>() / n as f64;
    let var = if n > 1 {
        diffs.iter().map(|d| (d - mean).powi(2)).sum::<f64>() / (n - 1) as f64
    } else {
        0.0
    };
    let se = (var / n as f64).sqrt();
    let scale = 1000.0 / cfg.big_blind.as_u64() as f64;
    let (m, s) = (mean * scale, se * scale);
    let (lo, hi) = (m - 1.96 * s, m + 1.96 * s);
    let verdict = if lo > 0.0 {
        "前缀 reach **显著优于** uniform（配对 CI 下界 > 0）"
    } else if hi < 0.0 {
        "前缀 reach **显著劣于** uniform（配对 CI 上界 < 0）"
    } else {
        "前缀 reach 与 uniform 无显著差（配对 CI 跨 0；功效预算内判不动）"
    };
    println!("\n========== 配对差：前缀 reach − uniform（{n} 手双计入）==========");
    println!("  Δmbb/g = {m:+.2}  SE = {s:.2}  CI95 = [{lo:+.2}, {hi:+.2}]");
    println!("  → {verdict}");
    println!(
        "  counted: 前缀臂 {} / uniform 臂 {}",
        prefix.counted, uniform.counted
    );
}

// ===========================================================================
// reshape + CLI
// ===========================================================================

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
    let mut hands_per_seat = 200u64;
    let mut seed = 0xA11C_E5A1_u64;
    let mut iterations = 1000u64;
    let mut trigger = SearchTrigger::AllPostflop;
    let mut deep_menu = true;
    let mut max_nodes = 1_000_000usize;
    let mut range_mix = 0.25f64;
    let mut stacks_bb: Option<[u64; N_SEATS]> = None;
    let next = |it: &mut std::iter::Skip<std::env::Args>, name: &str| -> Result<String, String> {
        it.next().ok_or_else(|| format!("{name} 需要一个值"))
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
            "--hands-per-seat" => {
                hands_per_seat = next(&mut it, &arg)?
                    .parse()
                    .map_err(|e| format!("bad hands: {e}"))?
            }
            "--seed" => {
                let raw = next(&mut it, &arg)?;
                seed = raw
                    .strip_prefix("0x")
                    .map(|h| u64::from_str_radix(h, 16))
                    .unwrap_or_else(|| raw.parse())
                    .map_err(|e| format!("bad seed: {e}"))?;
            }
            "--search-iterations" => {
                iterations = next(&mut it, &arg)?
                    .parse()
                    .map_err(|e| format!("bad iters: {e}"))?
            }
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
            "--stacks" => {
                let raw = next(&mut it, &arg)?;
                let v: Vec<u64> = raw
                    .split(',')
                    .map(|s| s.trim().parse::<u64>())
                    .collect::<Result<_, _>>()
                    .map_err(|e| format!("bad --stacks: {e}"))?;
                if v.len() != N_SEATS {
                    return Err(format!("--stacks 须 {N_SEATS} 个 BB 值，得 {}", v.len()));
                }
                let mut arr = [0u64; N_SEATS];
                arr.copy_from_slice(&v);
                stacks_bb = Some(arr);
            }
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
        hands_per_seat,
        seed,
        search,
        stacks_bb,
    })
}
