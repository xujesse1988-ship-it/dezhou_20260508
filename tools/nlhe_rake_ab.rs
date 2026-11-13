//! 抽水对 blueprint 的影响：同一棵树上「不抽水训的」和「带抽水训的」两份 dense checkpoint 的对照。
//!
//! 两份 checkpoint 必须用同一组游戏参数训练，只差 `--rake-bps` / `--rake-cap`（抽水不改树形状，两边
//! 信息集编号相同，所以可以在同一局里按座位混用）。输出两部分：
//!
//! 1. **各自打法**：每份 blueprint 全桌自对弈（在带抽水的游戏里），按位置统计入池率、加注率、没人入池时
//!    的弃 / 平跟 / 各档加注比例、面对一次加注时的弃 / 跟 / 再加注比例，以及看翻牌比例、平均每手总投入、
//!    每座每百手交的抽水。
//! 2. **交叉对打**：游戏 ∈ {带抽水, 不抽水} × 其余座位用的 blueprint ∈ {不抽水训, 带抽水训}，hero 轮流
//!    坐每个座位，同一副牌和同一串随机数下分别用两份 blueprint 打一遍，报告「带抽水训 − 不抽水训」的
//!    每百手收益差（BB/100，成对差的 95% 区间）。
//!
//! 用法：
//! ```bash
//! cargo run --release --bin nlhe_rake_ab -- \
//!   --bucket-table artifacts/bucket_table_default_500_500_500_seed_cafebabe_schemav4.bin \
//!   --seats 8 --reshape ante-flop2-3bet-open2 --postflop-cap 2 --stack-bb 200 --ante 100 \
//!   --rake-bps 400 --rake-cap 1000 \
//!   --plain <不抽水 ckpt> --raked <带抽水 ckpt> --hands-per-seat 200000 --threads 8
//! ```
//! `--control`：噪声对照，`--raked` 给另一份**不抽水**训的 checkpoint（换随机种子），两份之间的差就是
//! 训练噪声，用来判断上面的差是不是抽水造成的。

use std::process::ExitCode;
use std::sync::Arc;

use poker::training::nlhe::{SimplifiedNlheGame, SimplifiedNlheState};
use poker::training::nlhe_betting_tree::{six_max_reshape, SIX_MAX_RESHAPE_NAMES};
use poker::training::nlhe_dense_trainer::DenseNlheEsMccfrTrainer;
use poker::training::{Game, NodeKind, PlayerId};
use poker::{AbstractAction, BucketTable, ChaCha20Rng, ChipAmount, RngSource, Street, TableConfig};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("[nlhe_rake_ab] failed: {e}");
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
    plain: String,
    raked: String,
    hands_per_seat: u64,
    seed: u64,
    threads: usize,
    /// 噪声对照：`--raked` 给的其实也是不抽水训的（另一个种子），按不抽水的游戏加载。
    control: bool,
}

fn usage() -> &'static str {
    "usage: nlhe_rake_ab --bucket-table <path> --plain <ckpt> --raked <ckpt> --rake-bps <n> [--rake-cap <chips>]\n\
     \t[--seats <N> (default 8)] [--reshape <name> (default ante-flop2-3bet-open2)] [--postflop-cap <N> (default 2)]\n\
     \t[--stack-bb <N> (default 200)] [--ante <chips> (default 100)]\n\
     \t[--hands-per-seat <N> (default 100000)] [--seed <u64> (default 1)] [--threads <N> (default 8)]\n\
     \t[--control]  (noise floor: --raked is a second checkpoint trained WITHOUT rake, e.g. another seed)"
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        bucket_table: String::new(),
        seats: 8,
        reshape: "ante-flop2-3bet-open2".to_string(),
        postflop_cap: 2,
        stack_bb: 200,
        ante: 100,
        rake_bps: 0,
        rake_cap: 0,
        plain: String::new(),
        raked: String::new(),
        hands_per_seat: 100_000,
        seed: 1,
        threads: 8,
        control: false,
    };
    let mut it = std::env::args().skip(1);
    let val = |it: &mut dyn Iterator<Item = String>, name: &str| {
        it.next().ok_or_else(|| format!("{name} needs a value"))
    };
    let num = |s: String, name: &str| {
        s.parse::<u64>()
            .map_err(|_| format!("{name}: not a number: {s}"))
    };
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--bucket-table" => a.bucket_table = val(&mut it, "--bucket-table")?,
            "--seats" => a.seats = num(val(&mut it, "--seats")?, "--seats")? as u8,
            "--reshape" => a.reshape = val(&mut it, "--reshape")?,
            "--postflop-cap" => {
                a.postflop_cap = num(val(&mut it, "--postflop-cap")?, "--postflop-cap")? as u8
            }
            "--stack-bb" => a.stack_bb = num(val(&mut it, "--stack-bb")?, "--stack-bb")? as u32,
            "--ante" => a.ante = num(val(&mut it, "--ante")?, "--ante")?,
            "--rake-bps" => a.rake_bps = num(val(&mut it, "--rake-bps")?, "--rake-bps")? as u32,
            "--rake-cap" => a.rake_cap = num(val(&mut it, "--rake-cap")?, "--rake-cap")?,
            "--plain" => a.plain = val(&mut it, "--plain")?,
            "--raked" => a.raked = val(&mut it, "--raked")?,
            "--hands-per-seat" => {
                a.hands_per_seat = num(val(&mut it, "--hands-per-seat")?, "--hands-per-seat")?
            }
            "--seed" => a.seed = num(val(&mut it, "--seed")?, "--seed")?,
            "--threads" => a.threads = num(val(&mut it, "--threads")?, "--threads")? as usize,
            "--control" => a.control = true,
            "-h" | "--help" => return Err(usage().to_string()),
            other => return Err(format!("unknown argument {other}\n{}", usage())),
        }
    }
    if a.bucket_table.is_empty() || a.plain.is_empty() || a.raked.is_empty() {
        return Err(usage().to_string());
    }
    if a.rake_bps == 0 {
        return Err(
            "--rake-bps must be > 0 (the rake the --raked checkpoint was trained with)".into(),
        );
    }
    if a.hands_per_seat == 0 || a.threads == 0 {
        return Err("--hands-per-seat and --threads must be > 0".into());
    }
    Ok(a)
}

fn build_game(
    a: &Args,
    table: &Arc<BucketTable>,
    rake: bool,
) -> Result<SimplifiedNlheGame, String> {
    let (abs, rules) = six_max_reshape(&a.reshape, a.postflop_cap).ok_or_else(|| {
        format!(
            "unknown --reshape {} (expected {SIX_MAX_RESHAPE_NAMES})",
            a.reshape
        )
    })?;
    let mut cfg = TableConfig::n_max_at_bb(a.seats, a.stack_bb).with_ante(ChipAmount::new(a.ante));
    if rake {
        cfg = cfg.with_rake(a.rake_bps, ChipAmount::new(a.rake_cap));
    }
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

/// 按 blueprint 平均策略抽一个动作；该信息集没访问过（空向量）时均匀抽。
fn sample_action(
    bp: &DenseNlheEsMccfrTrainer,
    state: &SimplifiedNlheState,
    actor: PlayerId,
    actions: &[AbstractAction],
    rng: &mut dyn RngSource,
) -> (usize, bool) {
    let info = SimplifiedNlheGame::info_set(state, actor);
    let probs = bp.average_strategy(info);
    let u = (rng.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
    if probs.len() != actions.len() {
        return (
            ((u * actions.len() as f64) as usize).min(actions.len() - 1),
            true,
        );
    }
    let mut acc = 0.0;
    let mut last = 0;
    for (i, &p) in probs.iter().enumerate() {
        if p > 0.0 {
            acc += p;
            last = i;
            if u < acc {
                return (i, false);
            }
        }
    }
    (last, false)
}

/// 没人入池时第一次行动的分类：弃 / 平跟（含 SB 补齐）/ 各档加注（千分比标签）/ 全下。
const OPEN_KINDS: usize = 6;
const OPEN_LABELS: [&str; OPEN_KINDS] = ["弃", "平跟", "小档加注", "中档加注", "大档加注", "全下"];

#[derive(Clone, Default)]
struct PosStats {
    hands: u64,
    vpip: u64,
    pfr: u64,
    unopened: [u64; OPEN_KINDS],
    /// 没人入池时加注到的金额合计（chips）与次数，按档。
    open_to_sum: [u64; OPEN_KINDS],
    /// 面对恰好一次加注的第一次行动：弃 / 跟 / 再加注。
    vs_raise: [u64; 3],
    net_sum: f64,
    net_sq: f64,
}

#[derive(Clone, Default)]
struct TableStats {
    pos: Vec<PosStats>,
    hands: u64,
    flops: u64,
    flop_players: u64,
    pot_sum: u64,
    rake_sum: f64,
    uniform_fallback: u64,
    decisions: u64,
}

impl TableStats {
    fn new(n: usize) -> Self {
        TableStats {
            pos: vec![PosStats::default(); n],
            ..Default::default()
        }
    }

    fn merge(&mut self, o: &TableStats) {
        for (a, b) in self.pos.iter_mut().zip(&o.pos) {
            a.hands += b.hands;
            a.vpip += b.vpip;
            a.pfr += b.pfr;
            for k in 0..OPEN_KINDS {
                a.unopened[k] += b.unopened[k];
                a.open_to_sum[k] += b.open_to_sum[k];
            }
            for k in 0..3 {
                a.vs_raise[k] += b.vs_raise[k];
            }
            a.net_sum += b.net_sum;
            a.net_sq += b.net_sq;
        }
        self.hands += o.hands;
        self.flops += o.flops;
        self.flop_players += o.flop_players;
        self.pot_sum += o.pot_sum;
        self.rake_sum += o.rake_sum;
        self.uniform_fallback += o.uniform_fallback;
        self.decisions += o.decisions;
    }
}

/// 一手牌：`bp_of(seat)` 给出每个座位用的 blueprint。返回终局状态；`stats` 非空时记录打法统计。
fn play_hand<'a>(
    game: &SimplifiedNlheGame,
    bp_of: &dyn Fn(usize) -> &'a DenseNlheEsMccfrTrainer,
    seed: u64,
    mut stats: Option<&mut TableStats>,
) -> SimplifiedNlheState {
    let n = game.n_players();
    let button = game.table_config().button_seat.0 as usize;
    let mut rng = ChaCha20Rng::from_seed(seed);
    let mut state = game.root(&mut rng);
    // 翻前统计用：已有几次加注、几个平跟、每座是否行动过 / 入池 / 加注。
    let (mut raises, mut limps) = (0u32, 0u32);
    let mut acted = vec![false; n];
    let mut vpip = vec![false; n];
    let mut pfr = vec![false; n];
    let mut saw_flop = false;
    while let NodeKind::Player(actor) = SimplifiedNlheGame::current(&state) {
        let actions = SimplifiedNlheGame::legal_actions(&state);
        let (idx, uniform) =
            sample_action(bp_of(actor as usize), &state, actor, &actions, &mut rng);
        let action = actions[idx];
        let seat = actor as usize;
        let street = state.game_state.street();
        let (max_bet, before_round, before_total) = {
            let ps = state.game_state.players();
            (
                ps.iter()
                    .map(|p| p.committed_this_round.as_u64())
                    .max()
                    .unwrap_or(0),
                ps[seat].committed_this_round.as_u64(),
                ps[seat].committed_total.as_u64(),
            )
        };
        if street != Street::Preflop && !saw_flop {
            saw_flop = true;
            if let Some(s) = stats.as_deref_mut() {
                s.flops += 1;
                s.flop_players += state
                    .game_state
                    .players()
                    .iter()
                    .filter(|p| p.status != poker::PlayerStatus::Folded)
                    .count() as u64;
            }
        }
        state = SimplifiedNlheGame::next(state, action, &mut rng);
        if let Some(s) = stats.as_deref_mut() {
            s.decisions += 1;
            s.uniform_fallback += u64::from(uniform);
        }
        if street != Street::Preflop {
            continue;
        }
        let put_in = state.game_state.players()[seat].committed_total.as_u64() - before_total;
        let to_call = max_bet - before_round;
        let is_fold = matches!(action, AbstractAction::Fold);
        let is_raise = !is_fold && put_in > to_call;
        let is_call = !is_fold && !is_raise && to_call > 0;
        if let Some(s) = stats.as_deref_mut() {
            let ps = &mut s.pos[(seat + n - button) % n];
            if !acted[seat] {
                if raises == 0 && limps == 0 && (is_fold || is_call || is_raise) {
                    let kind = if is_fold {
                        0
                    } else if is_call {
                        1
                    } else {
                        match action {
                            AbstractAction::AllIn { .. } => 5,
                            AbstractAction::Raise { ratio_label, .. }
                            | AbstractAction::Bet { ratio_label, .. } => {
                                match ratio_label.as_milli() {
                                    0..=400 => 2,
                                    401..=900 => 3,
                                    _ => 4,
                                }
                            }
                            _ => 4,
                        }
                    };
                    ps.unopened[kind] += 1;
                    if is_raise {
                        ps.open_to_sum[kind] += before_round + put_in;
                    }
                } else if raises == 1 && to_call > 0 {
                    ps.vs_raise[if is_fold {
                        0
                    } else if is_call {
                        1
                    } else {
                        2
                    }] += 1;
                }
            }
        }
        acted[seat] = true;
        if is_raise {
            raises += 1;
            pfr[seat] = true;
            vpip[seat] = true;
        } else if is_call {
            vpip[seat] = true;
            if raises == 0 {
                limps += 1;
            }
        }
    }
    if let Some(s) = stats {
        s.hands += 1;
        s.pot_sum += state
            .game_state
            .players()
            .iter()
            .map(|p| p.committed_total.as_u64())
            .sum::<u64>();
        let mut sum = 0.0;
        for seat in 0..n {
            let net = SimplifiedNlheGame::payoff(&state, seat as PlayerId);
            sum += net;
            let ps = &mut s.pos[(seat + n - button) % n];
            ps.hands += 1;
            ps.vpip += u64::from(vpip[seat]);
            ps.pfr += u64::from(pfr[seat]);
            ps.net_sum += net;
            ps.net_sq += net * net;
        }
        s.rake_sum -= sum;
    }
    state
}

fn pos_label(offset: usize, n: usize) -> String {
    match offset {
        0 => "BTN".to_string(),
        1 => "SB".to_string(),
        2 => "BB".to_string(),
        k if k == n - 1 => "CO".to_string(),
        k if k == n - 2 => "HJ".to_string(),
        3 => "UTG".to_string(),
        k => format!("UTG+{}", k - 3),
    }
}

fn pct(k: u64, n: u64) -> f64 {
    if n == 0 {
        0.0
    } else {
        100.0 * k as f64 / n as f64
    }
}

fn self_play(
    label: &str,
    game: &SimplifiedNlheGame,
    bp: &DenseNlheEsMccfrTrainer,
    a: &Args,
    cell: u64,
) {
    let n = game.n_players();
    let total = a.hands_per_seat * n as u64;
    let stats = parallel(a.threads, total, |lo, hi| {
        let mut s = TableStats::new(n);
        for h in lo..hi {
            play_hand(game, &|_| bp, hand_seed(a.seed, cell, 0, h), Some(&mut s));
        }
        s
    })
    .into_iter()
    .fold(TableStats::new(n), |mut acc, s| {
        acc.merge(&s);
        acc
    });
    let bb = game.table_config().big_blind.as_u64() as f64;
    println!("\n## 自对弈：{label}（带抽水的游戏，{} 手）", stats.hands);
    println!(
        "看翻牌 {:.1}%，翻牌平均 {:.2} 人；平均每手总投入 {:.1} BB（含没被跟、退回的部分）；抽水 {:.1} BB/100/座；没访问过的信息集占决策 {:.3}%",
        pct(stats.flops, stats.hands),
        stats.flop_players as f64 / stats.flops.max(1) as f64,
        stats.pot_sum as f64 / stats.hands as f64 / bb,
        100.0 * stats.rake_sum / stats.hands as f64 / bb / n as f64,
        pct(stats.uniform_fallback, stats.decisions),
    );
    println!("| 位置 | 入池 | 加注 | 没人入池时：弃 / 平跟 / 小档 / 中档 / 大档 / 全下（加注到 BB） | 面对一次加注：弃 / 跟 / 再加注 | 收益 BB/100 |");
    println!("|---|---|---|---|---|---|");
    let order: Vec<usize> = (3..n).chain(0..3.min(n)).collect();
    for off in order {
        let p = &stats.pos[off];
        let un: u64 = p.unopened.iter().sum();
        let opens: Vec<String> = (0..OPEN_KINDS)
            .filter(|&k| p.unopened[k] > 0)
            .map(|k| {
                let size = if (2..=4).contains(&k) {
                    format!(
                        "（{:.2}）",
                        p.open_to_sum[k] as f64 / p.unopened[k] as f64 / bb
                    )
                } else {
                    String::new()
                };
                format!("{} {:.1}%{size}", OPEN_LABELS[k], pct(p.unopened[k], un))
            })
            .collect();
        let vr: u64 = p.vs_raise.iter().sum();
        let mean = p.net_sum / p.hands as f64;
        let se = ((p.net_sq / p.hands as f64 - mean * mean) / p.hands as f64).sqrt();
        println!(
            "| {} | {:.1}% | {:.1}% | {} | {:.1}% / {:.1}% / {:.1}% | {:+.1} ±{:.1} |",
            pos_label(off, n),
            pct(p.vpip, p.hands),
            pct(p.pfr, p.hands),
            opens.join(" / "),
            pct(p.vs_raise[0], vr),
            pct(p.vs_raise[1], vr),
            pct(p.vs_raise[2], vr),
            100.0 * mean / bb,
            196.0 * se / bb,
        );
    }
    let (vp, pf, hands) = stats.pos.iter().fold((0, 0, 0), |(v, p, h), s| {
        (v + s.vpip, p + s.pfr, h + s.hands)
    });
    println!(
        "全桌入池 {:.1}%、加注 {:.1}%",
        pct(vp, hands),
        pct(pf, hands)
    );
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

/// 成对差的累计：hero 用 `raked` 的收益 − hero 用 `plain` 的收益（同一副牌、同一串随机数）。
#[derive(Clone, Copy, Default)]
struct Paired {
    n: u64,
    sum_a: f64,
    sum_b: f64,
    sum_d: f64,
    sq_d: f64,
}

fn cross(
    (game_label, field_label): (&str, &str),
    game: &SimplifiedNlheGame,
    field: &DenseNlheEsMccfrTrainer,
    plain: &DenseNlheEsMccfrTrainer,
    raked: &DenseNlheEsMccfrTrainer,
    a: &Args,
    cell: u64,
) {
    let n = game.n_players();
    let bb = game.table_config().big_blind.as_u64() as f64;
    let total = a.hands_per_seat * n as u64;
    let acc = parallel(a.threads, total, |lo, hi| {
        let mut p = Paired::default();
        for i in lo..hi {
            let (seat, hand) = ((i % n as u64) as usize, i / n as u64);
            let seed = hand_seed(a.seed, cell, seat as u64, hand);
            let run = |hero: &DenseNlheEsMccfrTrainer| {
                let st = play_hand(game, &|s| if s == seat { hero } else { field }, seed, None);
                SimplifiedNlheGame::payoff(&st, seat as PlayerId)
            };
            let (x, y) = (run(raked), run(plain));
            p.n += 1;
            p.sum_a += x;
            p.sum_b += y;
            p.sum_d += x - y;
            p.sq_d += (x - y) * (x - y);
        }
        p
    })
    .into_iter()
    .fold(Paired::default(), |mut t, p| {
        t.n += p.n;
        t.sum_a += p.sum_a;
        t.sum_b += p.sum_b;
        t.sum_d += p.sum_d;
        t.sq_d += p.sq_d;
        t
    });
    let nn = acc.n as f64;
    let mean = acc.sum_d / nn;
    let se = ((acc.sq_d / nn - mean * mean) / nn).sqrt();
    println!(
        "| {game_label} | {field_label} | {:+.1} | {:+.1} | {:+.1} ±{:.1} |",
        100.0 * acc.sum_a / nn / bb,
        100.0 * acc.sum_b / nn / bb,
        100.0 * mean / bb,
        196.0 * se / bb,
    );
}

fn run() -> Result<(), String> {
    let a = parse_args()?;
    let table = Arc::new(
        BucketTable::open(std::path::Path::new(&a.bucket_table))
            .map_err(|e| format!("BucketTable::open({}) failed: {e:?}", a.bucket_table))?,
    );
    let game_raked = build_game(&a, &table, true)?;
    let game_plain = build_game(&a, &table, false)?;
    // 各自按训练时的游戏加载（抽水进 layout 指纹：拿错文件会被拒）。
    let plain = DenseNlheEsMccfrTrainer::load_checkpoint(
        std::path::Path::new(&a.plain),
        build_game(&a, &table, false)?,
    )
    .map_err(|e| format!("load --plain {}: {e:?}", a.plain))?;
    let raked = DenseNlheEsMccfrTrainer::load_checkpoint(
        std::path::Path::new(&a.raked),
        build_game(&a, &table, !a.control)?,
    )
    .map_err(|e| format!("load --raked {}: {e:?}", a.raked))?;
    println!(
        "# 抽水对照：{} 座 {}BB ante {} {} cap {}；抽水 {} bps 封顶 {} chips",
        a.seats, a.stack_bb, a.ante, a.reshape, a.postflop_cap, a.rake_bps, a.rake_cap
    );
    if a.control {
        println!("**噪声对照**：下面标成「带抽水训」的那份其实也是不抽水训的（另一个随机种子），两份之间的差就是训练噪声。");
    }
    println!(
        "不抽水训：{}（{} 次更新）\n带抽水训：{}（{} 次更新）\n每格 {} 手 × {} 座，seed {}",
        a.plain,
        plain.update_count(),
        a.raked,
        raked.update_count(),
        a.hands_per_seat,
        a.seats,
        a.seed
    );

    self_play("不抽水训的 blueprint", &game_raked, &plain, &a, 1);
    self_play("带抽水训的 blueprint", &game_raked, &raked, &a, 2);

    println!("\n## 交叉对打：hero 换成「带抽水训」比「不抽水训」多赚多少（BB/100）");
    println!("| 游戏 | 其余座位 | hero=带抽水训 | hero=不抽水训 | 差（95% 区间） |");
    println!("|---|---|---|---|---|");
    let cells: [(&str, &SimplifiedNlheGame, &str, &DenseNlheEsMccfrTrainer); 4] = [
        ("带抽水", &game_raked, "不抽水训", &plain),
        ("带抽水", &game_raked, "带抽水训", &raked),
        ("不抽水", &game_plain, "不抽水训", &plain),
        ("不抽水", &game_plain, "带抽水训", &raked),
    ];
    for (i, (gl, g, fl, f)) in cells.into_iter().enumerate() {
        cross((gl, fl), g, f, &plain, &raked, &a, 10 + i as u64);
    }
    Ok(())
}
