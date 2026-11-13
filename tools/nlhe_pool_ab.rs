//! 针对牌池训练的效果：同一棵树上「普通 blueprint」（`--base`）和「候选」（`--cand`，例如带
//! `--pool-model` 训的）两份 dense checkpoint 的对照。回答两个问题：对牌池多赚多少，对不按牌池打的
//! 对手亏多少。
//!
//! hero 轮流坐每个座位，同一副牌和同一串随机数下分别用两份 checkpoint 打一遍，报告「候选 − 普通」的
//! 每百手收益差（BB/100，成对差的 95% 区间）。其余 7 个座位依次是：
//!
//! 1. **纯牌池**：翻前每个决策都按牌池的翻前策略（[`PoolPreflopStrategy`]，置信度当成 1），翻后按普通
//!    blueprint。牌池的翻后打法没有建模，所以这一行只说明翻前。
//! 2. **混合**：翻前每个决策以该节点的置信度概率按牌池打，否则按普通 blueprint（接近带牌池训练时面对的
//!    对手，区别是训练时「否则」那部分是候选自己）。
//! 3. **普通 blueprint**：完全不按牌池打的对手——候选为了针对牌池付出的代价。
//! 4. **候选自己**。
//!
//! 评估用的牌池策略由 `--pool-model` 和 `--pool-noise` / `--pool-conf-k` / `--pool-conf-max` 现建，可以
//! 和训练候选时用的不同：换 `--pool-noise` 就是检验「哪些牌做哪个动作」这条假设错了时结果还在不在。
//!
//! 噪声对照：`--cand` 给另一份普通 blueprint（换随机种子），四行的差就是训练噪声。
//!
//! ```bash
//! cargo run --release --bin nlhe_pool_ab -- \
//!   --bucket-table artifacts/bucket_table_default_500_500_500_seed_cafebabe_schemav4.bin \
//!   --seats 8 --reshape ante-flop2-3bet-o75-limp --postflop-cap 2 --stack-bb 200 --ante 100 \
//!   --rake-bps 400 --rake-cap 1000 --pool-model artifacts/pool_model_20261111/pool_model.json \
//!   --base <普通 ckpt> --cand <候选 ckpt> --hands-per-seat 200000 --threads 8
//! ```

use std::process::ExitCode;
use std::sync::Arc;

use poker::training::nlhe::{SimplifiedNlheGame, SimplifiedNlheState};
use poker::training::nlhe_betting_tree::{six_max_reshape, SIX_MAX_RESHAPE_NAMES};
use poker::training::nlhe_dense_trainer::DenseNlheEsMccfrTrainer;
use poker::training::pool_model::PoolModel;
use poker::training::pool_strategy::{PoolPreflopStrategy, PoolStrategyParams};
use poker::training::{Game, NodeKind, PlayerId};
use poker::{AbstractAction, BucketTable, ChaCha20Rng, ChipAmount, RngSource, Street, TableConfig};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("[nlhe_pool_ab] failed: {e}");
            ExitCode::from(2)
        }
    }
}

struct Args {
    bucket_table: String,
    seats: u8,
    reshape: String,
    postflop_cap: u8,
    stack_bb: u32,
    ante: u64,
    rake_bps: u32,
    rake_cap: u64,
    base: String,
    cand: String,
    cand_label: String,
    pool_model: String,
    pool: PoolStrategyParams,
    hands_per_seat: u64,
    seed: u64,
    threads: usize,
}

fn usage() -> &'static str {
    "usage: nlhe_pool_ab --bucket-table <path> --base <ckpt> --cand <ckpt> --pool-model <json>\n\
     \t[--cand-label <text>] [--seats <N> (8)] [--reshape <name> (ante-flop2-3bet-o75-limp)] [--postflop-cap <N> (2)]\n\
     \t[--stack-bb <N> (200)] [--ante <chips> (100)] [--rake-bps <n> (0)] [--rake-cap <chips> (0)]\n\
     \t[--pool-noise <x> (0.75)] [--pool-conf-k <n> (50)] [--pool-conf-max <m> (0.9)]\n\
     \t[--hands-per-seat <N> (100000)] [--seed <u64> (1)] [--threads <N> (8)]"
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        bucket_table: String::new(),
        seats: 8,
        reshape: "ante-flop2-3bet-o75-limp".into(),
        postflop_cap: 2,
        stack_bb: 200,
        ante: 100,
        rake_bps: 0,
        rake_cap: 0,
        base: String::new(),
        cand: String::new(),
        cand_label: "候选".into(),
        pool_model: String::new(),
        pool: PoolStrategyParams::default(),
        hands_per_seat: 100_000,
        seed: 1,
        threads: 8,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut val = || {
            it.next()
                .ok_or_else(|| format!("{flag} requires a value\n{}", usage()))
        };
        fn num<T: std::str::FromStr>(s: String) -> Result<T, String> {
            s.parse::<T>().map_err(|_| format!("invalid number {s}"))
        }
        match flag.as_str() {
            "--bucket-table" => a.bucket_table = val()?,
            "--seats" => a.seats = num(val()?)?,
            "--reshape" => a.reshape = val()?,
            "--postflop-cap" => a.postflop_cap = num(val()?)?,
            "--stack-bb" => a.stack_bb = num(val()?)?,
            "--ante" => a.ante = num(val()?)?,
            "--rake-bps" => a.rake_bps = num(val()?)?,
            "--rake-cap" => a.rake_cap = num(val()?)?,
            "--base" => a.base = val()?,
            "--cand" => a.cand = val()?,
            "--cand-label" => a.cand_label = val()?,
            "--pool-model" => a.pool_model = val()?,
            "--pool-noise" => a.pool.noise = num(val()?)?,
            "--pool-conf-k" => a.pool.conf_k = num(val()?)?,
            "--pool-conf-max" => a.pool.conf_max = num(val()?)?,
            "--hands-per-seat" => a.hands_per_seat = num(val()?)?,
            "--seed" => a.seed = num(val()?)?,
            "--threads" => a.threads = num(val()?)?,
            other => return Err(format!("unknown flag {other}\n{}", usage())),
        }
    }
    if a.bucket_table.is_empty()
        || a.base.is_empty()
        || a.cand.is_empty()
        || a.pool_model.is_empty()
    {
        return Err(format!(
            "--bucket-table, --base, --cand and --pool-model are required\n{}",
            usage()
        ));
    }
    if a.hands_per_seat == 0 || a.threads == 0 {
        return Err("--hands-per-seat and --threads must be > 0".into());
    }
    Ok(a)
}

fn build_game(a: &Args, table: &Arc<BucketTable>) -> Result<SimplifiedNlheGame, String> {
    let (abs, rules) = six_max_reshape(&a.reshape, a.postflop_cap).ok_or_else(|| {
        format!(
            "unknown --reshape {} (expected {SIX_MAX_RESHAPE_NAMES})",
            a.reshape
        )
    })?;
    let cfg = TableConfig::n_max_at_bb(a.seats, a.stack_bb)
        .with_ante(ChipAmount::new(a.ante))
        .with_rake(a.rake_bps, ChipAmount::new(a.rake_cap));
    SimplifiedNlheGame::new_with_abstraction(Arc::clone(table), cfg, abs, rules)
        .map_err(|e| format!("build game: {e:?}"))
}

/// SplitMix64 混合，派生每手独立的种子（固定 `--seed` 可复现，与线程数无关）。
fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

fn hand_seed(seed: u64, cell: u64, seat: u64, hand: u64) -> u64 {
    mix(mix(mix(seed ^ cell.wrapping_mul(0xA5A5)) ^ seat) ^ hand)
}

fn uniform(rng: &mut dyn RngSource) -> f64 {
    (rng.next_u64() >> 11) as f64 / (1u64 << 53) as f64
}

fn pick(probs: impl Iterator<Item = f64>, n: usize, rng: &mut dyn RngSource) -> usize {
    let u = uniform(rng);
    let (mut acc, mut last) = (0.0, 0);
    for (i, p) in probs.enumerate() {
        if p > 0.0 {
            acc += p;
            last = i;
            if u < acc {
                return i;
            }
        }
    }
    last.min(n - 1)
}

/// 一个座位怎么打。
#[derive(Clone, Copy)]
enum Policy<'a> {
    /// 按这份 checkpoint 的平均策略。
    Blueprint(&'a DenseNlheEsMccfrTrainer),
    /// 翻前以置信度概率（`pure` 时恒为 1）按牌池打，否则 / 翻后按 `rest`。
    Pool {
        pool: &'a PoolPreflopStrategy,
        pure: bool,
        rest: &'a DenseNlheEsMccfrTrainer,
    },
}

/// 返回动作下标。blueprint 没访问过的信息集（空向量）均匀抽。
fn choose(
    policy: Policy,
    state: &SimplifiedNlheState,
    actor: PlayerId,
    n_actions: usize,
    rng: &mut dyn RngSource,
) -> usize {
    let info = SimplifiedNlheGame::info_set(state, actor);
    let from_bp = |bp: &DenseNlheEsMccfrTrainer, rng: &mut dyn RngSource| -> usize {
        let probs = bp.average_strategy(info);
        if probs.len() != n_actions {
            return ((uniform(rng) * n_actions as f64) as usize).min(n_actions - 1);
        }
        pick(probs.into_iter(), n_actions, rng)
    };
    match policy {
        Policy::Blueprint(bp) => from_bp(bp, rng),
        Policy::Pool { pool, pure, rest } => {
            match pool.lookup(state.current_node_id, info.bucket_id()) {
                Some((conf, row)) if pure || uniform(rng) < f64::from(conf) => {
                    let total: f64 = row.iter().map(|p| f64::from(*p)).sum();
                    pick(row.iter().map(|p| f64::from(*p) / total), n_actions, rng)
                }
                _ => from_bp(rest, rng),
            }
        }
    }
}

/// hero 的翻前习惯（判断候选相对普通 blueprint 改了什么）。
#[derive(Clone, Copy, Default)]
struct HeroStats {
    hands: u64,
    vpip: u64,
    pfr: u64,
    /// 第一次行动时面对「有人平跟、没人加注」的次数，以及其中加注 / 跟着平跟的次数。
    vs_limp: u64,
    vs_limp_raise: u64,
    vs_limp_call: u64,
}

impl HeroStats {
    fn add(&mut self, o: &HeroStats) {
        self.hands += o.hands;
        self.vpip += o.vpip;
        self.pfr += o.pfr;
        self.vs_limp += o.vs_limp;
        self.vs_limp_raise += o.vs_limp_raise;
        self.vs_limp_call += o.vs_limp_call;
    }
}

fn play_hand<'a>(
    game: &SimplifiedNlheGame,
    policy_of: &dyn Fn(usize) -> Policy<'a>,
    seed: u64,
    hero: usize,
    stats: &mut HeroStats,
) -> SimplifiedNlheState {
    let mut rng = ChaCha20Rng::from_seed(seed);
    let mut state = game.root(&mut rng);
    let (mut raises, mut limps) = (0u32, 0u32);
    let (mut vpip, mut pfr, mut first) = (false, false, true);
    stats.hands += 1;
    while let NodeKind::Player(actor) = SimplifiedNlheGame::current(&state) {
        let actions = SimplifiedNlheGame::legal_actions(&state);
        let idx = choose(
            policy_of(actor as usize),
            &state,
            actor,
            actions.len(),
            &mut rng,
        );
        let action = actions[idx];
        let seat = actor as usize;
        let preflop = state.game_state.street() == Street::Preflop;
        let (max_bet, mine) = {
            let ps = state.game_state.players();
            (
                ps.iter()
                    .map(|p| p.committed_this_round.as_u64())
                    .max()
                    .unwrap_or(0),
                ps[seat].committed_total.as_u64(),
            )
        };
        let to_call = max_bet
            - state.game_state.players()[seat]
                .committed_this_round
                .as_u64();
        state = SimplifiedNlheGame::next(state, action, &mut rng);
        if !preflop {
            continue;
        }
        let put_in = state.game_state.players()[seat].committed_total.as_u64() - mine;
        let is_fold = matches!(action, AbstractAction::Fold);
        let is_raise = !is_fold && put_in > to_call;
        let is_call = !is_fold && !is_raise && to_call > 0;
        if seat == hero {
            vpip |= is_raise || is_call;
            pfr |= is_raise;
            if first && raises == 0 && limps > 0 && to_call > 0 {
                stats.vs_limp += 1;
                stats.vs_limp_raise += u64::from(is_raise);
                stats.vs_limp_call += u64::from(is_call);
            }
            first = false;
        }
        if is_raise {
            raises += 1;
        } else if is_call && raises == 0 {
            limps += 1;
        }
    }
    stats.vpip += u64::from(vpip);
    stats.pfr += u64::from(pfr);
    state
}

/// 把 `[0, total)` 切成 `threads` 段并行跑，按段顺序返回结果。
fn parallel<T: Send>(threads: usize, total: u64, f: impl Fn(u64, u64) -> T + Sync) -> Vec<T> {
    let chunk = total.div_ceil(threads as u64).max(1);
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads as u64)
            .map(|t| {
                let (lo, hi) = ((t * chunk).min(total), ((t + 1) * chunk).min(total));
                let f = &f;
                scope.spawn(move || f(lo, hi))
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("worker panicked"))
            .collect()
    })
}

#[derive(Clone, Copy, Default)]
struct Paired {
    n: u64,
    sum_cand: f64,
    sum_base: f64,
    sum_d: f64,
    sq_d: f64,
    cand: HeroStats,
    base: HeroStats,
}

fn pct(k: u64, n: u64) -> f64 {
    if n == 0 {
        0.0
    } else {
        100.0 * k as f64 / n as f64
    }
}

#[allow(clippy::too_many_arguments)]
fn cross(
    label: &str,
    game: &SimplifiedNlheGame,
    field: Policy,
    base: &DenseNlheEsMccfrTrainer,
    cand: &DenseNlheEsMccfrTrainer,
    a: &Args,
    cell: u64,
) -> Paired {
    let n = game.n_players();
    let bb = game.table_config().big_blind.as_u64() as f64;
    let total = a.hands_per_seat * n as u64;
    let acc = parallel(a.threads, total, |lo, hi| {
        let mut p = Paired::default();
        for i in lo..hi {
            let (seat, hand) = ((i % n as u64) as usize, i / n as u64);
            let seed = hand_seed(a.seed, cell, seat as u64, hand);
            let run = |hero: &DenseNlheEsMccfrTrainer, st: &mut HeroStats| {
                let end = play_hand(
                    game,
                    &|s| {
                        if s == seat {
                            Policy::Blueprint(hero)
                        } else {
                            field
                        }
                    },
                    seed,
                    seat,
                    st,
                );
                SimplifiedNlheGame::payoff(&end, seat as PlayerId)
            };
            let x = run(cand, &mut p.cand);
            let y = run(base, &mut p.base);
            p.n += 1;
            p.sum_cand += x;
            p.sum_base += y;
            p.sum_d += x - y;
            p.sq_d += (x - y) * (x - y);
        }
        p
    })
    .into_iter()
    .fold(Paired::default(), |mut t, p| {
        t.n += p.n;
        t.sum_cand += p.sum_cand;
        t.sum_base += p.sum_base;
        t.sum_d += p.sum_d;
        t.sq_d += p.sq_d;
        t.cand.add(&p.cand);
        t.base.add(&p.base);
        t
    });
    let nn = acc.n as f64;
    let mean = acc.sum_d / nn;
    let se = ((acc.sq_d / nn - mean * mean) / nn).sqrt();
    println!(
        "| {label} | {:+.1} | {:+.1} | {:+.1} ±{:.1} |",
        100.0 * acc.sum_cand / nn / bb,
        100.0 * acc.sum_base / nn / bb,
        100.0 * mean / bb,
        196.0 * se / bb,
    );
    acc
}

fn run() -> Result<(), String> {
    let a = parse_args()?;
    let table = Arc::new(
        BucketTable::open(std::path::Path::new(&a.bucket_table))
            .map_err(|e| format!("BucketTable::open({}) failed: {e:?}", a.bucket_table))?,
    );
    let game = build_game(&a, &table)?;
    let load = |path: &str, what: &str| {
        DenseNlheEsMccfrTrainer::load_checkpoint(
            std::path::Path::new(path),
            build_game(&a, &table)?,
        )
        .map_err(|e| format!("load {what} {path}: {e:?}"))
    };
    let base = load(&a.base, "--base")?;
    let cand = load(&a.cand, "--cand")?;
    let model = PoolModel::load(std::path::Path::new(&a.pool_model))?;
    let pool = PoolPreflopStrategy::build(&game, &model, a.pool)?;
    let nodes = pool.nodes();
    let mean_conf = nodes.iter().map(|n| n.confidence).sum::<f64>() / nodes.len() as f64;

    println!(
        "# 针对牌池的对照：{} 座 {}BB ante {} {} cap {}；抽水 {} bps 封顶 {} chips",
        a.seats, a.stack_bb, a.ante, a.reshape, a.postflop_cap, a.rake_bps, a.rake_cap
    );
    println!(
        "普通：{}（{} 次更新）\n{}：{}（{} 次更新）",
        a.base,
        base.update_count(),
        a.cand_label,
        a.cand,
        cand.update_count()
    );
    println!(
        "评估用的牌池：{}，模糊 {}，置信度 = min({}, n / (n + {}))，{} 个翻前节点平均 {:.3}；每格 {} 手 × {} 座，seed {}",
        a.pool_model,
        a.pool.noise,
        a.pool.conf_max,
        a.pool.conf_k,
        nodes.len(),
        mean_conf,
        a.hands_per_seat,
        a.seats,
        a.seed
    );
    println!(
        "\n## hero 换成「{}」比「普通」多赚多少（BB/100）",
        a.cand_label
    );
    println!(
        "| 其余 7 座 | hero={} | hero=普通 | 差（95% 区间） |",
        a.cand_label
    );
    println!("|---|---|---|---|");
    let fields: [(&str, Policy); 4] = [
        (
            "纯牌池（翻前全按牌池，翻后普通）",
            Policy::Pool {
                pool: &pool,
                pure: true,
                rest: &base,
            },
        ),
        (
            "混合（翻前按置信度掷币，否则普通）",
            Policy::Pool {
                pool: &pool,
                pure: false,
                rest: &base,
            },
        ),
        ("普通 blueprint", Policy::Blueprint(&base)),
        ("候选自己", Policy::Blueprint(&cand)),
    ];
    let mut rows = Vec::new();
    for (i, (label, field)) in fields.into_iter().enumerate() {
        rows.push((
            label,
            cross(label, &game, field, &base, &cand, &a, 10 + i as u64),
        ));
    }
    println!("\n## hero 的翻前习惯");
    println!("| 其余 7 座 | hero | 入池 | 加注 | 面对平跟：加注 / 跟着平跟（次数） |");
    println!("|---|---|---|---|---|");
    for (label, r) in &rows {
        for (who, s) in [(a.cand_label.as_str(), &r.cand), ("普通", &r.base)] {
            println!(
                "| {label} | {who} | {:.1}% | {:.1}% | {:.1}% / {:.1}%（{}） |",
                pct(s.vpip, s.hands),
                pct(s.pfr, s.hands),
                pct(s.vs_limp_raise, s.vs_limp),
                pct(s.vs_limp_call, s.vs_limp),
                s.vs_limp
            );
        }
    }
    Ok(())
}
