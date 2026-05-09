//! E1：criterion benchmark 完整配置（workflow §E1 §输出）。
//!
//! 覆盖 `pluribus_stage1_validation.md` §8 SLO 的四类数据点：
//!
//! 1. **eval7 单线程**（SLO ≥ 10M eval/s）—— `eval7_naive/single_call`。
//! 2. **eval7 批量**（同 SLO，缓存友好对比） —— `eval7_naive/batch_1024_unique_hands`。
//! 3. **全流程模拟**（SLO ≥ 100k hand/s 单线程）—— `simulate/random_hand_6max_100bb`。
//! 4. **HandHistory 序列化 / 反序列化**（SLO ≥ 1M action/s 各方向）——
//!    `history/encode` 与 `history/decode`。
//!
//! 每个 bench 通过 [`Throughput::Elements`] 把单位钉到「ops per second」，
//! 让 criterion 的 `thrpt` 列直接可与 SLO 数字对照（eval/s / hand/s / action/s）。
//!
//! **本文件不做断言**。SLO 阈值断言放 `tests/perf_slo.rs`，由
//! `cargo test --release --test perf_slo -- --ignored` 运行；E1 closure 时所有
//! 阈值断言期望失败（朴素评估器 ≈ 10k–1M eval/s），E2 性能优化后补齐。
//!
//! CI 集成：
//!
//! - **快路径**（`.github/workflows/ci.yml::bench-quick`）：
//!   `cargo bench --bench baseline -- --warm-up-time 1 --measurement-time 1
//!   --sample-size 10 --noplot`，目标 30s 内出全套数据。
//! - **全量**（`.github/workflows/nightly.yml::bench-full`）：默认 criterion
//!   参数（warm-up 3s + measurement 5s × sample 100），每晚跑一次。
//!
//! 角色边界：本文件属 `[测试]` agent。`[实现]` agent 不得修改。

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use poker::eval::NaiveHandEvaluator;
use poker::{
    Action, Card, ChaCha20Rng, GameState, HandEvaluator, HandHistory, LegalActionSet, RngSource,
    TableConfig,
};

// ============================================================================
// 共享：随机 7-card 与单手随机模拟
// ============================================================================

/// 生成 `n` 组互不相同的随机 7-card hand（每手内部 7 张不重复，手与手之间允许重叠）。
fn make_random_hands(n: usize, seed: u64) -> Vec<[Card; 7]> {
    let mut rng = ChaCha20Rng::from_seed(seed);
    (0..n).map(|_| random_seven_cards(&mut rng)).collect()
}

fn random_seven_cards(rng: &mut dyn RngSource) -> [Card; 7] {
    let mut deck: [u8; 52] = std::array::from_fn(|i| i as u8);
    for i in 0..7 {
        let j = i + (rng.next_u64() % ((52 - i) as u64)) as usize;
        deck.swap(i, j);
    }
    [
        Card::from_u8(deck[0]).expect("0..52"),
        Card::from_u8(deck[1]).expect("0..52"),
        Card::from_u8(deck[2]).expect("0..52"),
        Card::from_u8(deck[3]).expect("0..52"),
        Card::from_u8(deck[4]).expect("0..52"),
        Card::from_u8(deck[5]).expect("0..52"),
        Card::from_u8(deck[6]).expect("0..52"),
    ]
}

/// 单手随机模拟。返回终局 [`HandHistory`]（克隆，便于 caller 后续做 to_proto）。
///
/// 与 `tests/fuzz_smoke.rs::run_one_hand` 的 happy-path 等价：seed 决定发牌
/// （via [`GameState::new`]），动作 rng 用 `seed ^ 0xDEAD_BEEF` 派生。256 步硬
/// 上限防御 fuzz 死循环（实测 6-max 单手通常 < 30 个 RecordedAction）。
fn simulate_one_hand(cfg: &TableConfig, seed: u64) -> HandHistory {
    let mut state = GameState::new(cfg, seed);
    let mut action_rng = ChaCha20Rng::from_seed(seed.wrapping_add(0xDEAD_BEEF));
    for _ in 0..256 {
        if state.is_terminal() {
            break;
        }
        let la = state.legal_actions();
        let action = sample_action(&la, &mut action_rng).expect("legal action available");
        state.apply(action).expect("apply");
    }
    state.hand_history().clone()
}

fn sample_action(la: &LegalActionSet, rng: &mut dyn RngSource) -> Option<Action> {
    let mut candidates: [Action; 6] = [Action::Fold; 6];
    let mut n = 0usize;
    if la.fold {
        candidates[n] = Action::Fold;
        n += 1;
    }
    if la.check {
        candidates[n] = Action::Check;
        n += 1;
    }
    if la.call.is_some() {
        candidates[n] = Action::Call;
        n += 1;
    }
    if let Some((min, max)) = la.bet_range {
        let lo = min.as_u64();
        let hi = max.as_u64();
        let to = if lo >= hi {
            min
        } else {
            poker::ChipAmount::new(lo + rng.next_u64() % (hi - lo + 1))
        };
        candidates[n] = Action::Bet { to };
        n += 1;
    }
    if let Some((min, max)) = la.raise_range {
        let lo = min.as_u64();
        let hi = max.as_u64();
        let to = if lo >= hi {
            min
        } else {
            poker::ChipAmount::new(lo + rng.next_u64() % (hi - lo + 1))
        };
        candidates[n] = Action::Raise { to };
        n += 1;
    }
    if la.all_in_amount.is_some() {
        candidates[n] = Action::AllIn;
        n += 1;
    }
    if n == 0 {
        return None;
    }
    let idx = (rng.next_u64() as usize) % n;
    Some(candidates[idx])
}

// ============================================================================
// bench：评估器（SLO §8 ≥ 10M eval/s 单线程）
// ============================================================================

fn bench_eval7(c: &mut Criterion) {
    let evaluator = NaiveHandEvaluator;
    // 1024 组 hand，足够大让 cache miss 与命中混合，但小到能放进 L2。
    // 同样的种子保证每次 bench 同分布，可对比。
    let hands = make_random_hands(1024, 0xE1_0001);
    let len = hands.len();

    let mut group = c.benchmark_group("eval7_naive");
    group.throughput(Throughput::Elements(1));
    group.bench_function("single_call", |b| {
        let mut idx = 0usize;
        b.iter(|| {
            let h = &hands[idx % len];
            idx = idx.wrapping_add(1);
            black_box(evaluator.eval7(black_box(h)));
        });
    });

    // 一次跑 1024 个 hand 的 batch 视角：thrpt 直接 ≈ eval/s。
    group.throughput(Throughput::Elements(len as u64));
    group.bench_function("batch_1024_unique_hands", |b| {
        b.iter(|| {
            let mut acc = 0u32;
            for h in &hands {
                acc ^= evaluator.eval7(black_box(h)).0;
            }
            black_box(acc);
        });
    });
    group.finish();
}

// ============================================================================
// bench：全流程随机手模拟（SLO §8 ≥ 100k hand/s 单线程）
// ============================================================================

fn bench_simulate(c: &mut Criterion) {
    let cfg = TableConfig::default_6max_100bb();
    let mut group = c.benchmark_group("simulate");
    group.throughput(Throughput::Elements(1));
    group.bench_function("random_hand_6max_100bb", |b| {
        let mut seed_counter = 0u64;
        b.iter(|| {
            let seed = seed_counter;
            seed_counter = seed_counter.wrapping_add(1);
            black_box(simulate_one_hand(black_box(&cfg), black_box(seed)));
        });
    });
    group.finish();
}

// ============================================================================
// bench：HandHistory 序列化 / 反序列化（SLO §8 ≥ 1M action/s 各方向）
// ============================================================================

fn bench_history(c: &mut Criterion) {
    let cfg = TableConfig::default_6max_100bb();
    // 预生成 256 手 hand history；累计 actions 用作 throughput 的分子。
    let histories: Vec<HandHistory> = (0..256u64).map(|s| simulate_one_hand(&cfg, s)).collect();
    let total_actions: u64 = histories.iter().map(|h| h.actions.len() as u64).sum();
    let bytes_arr: Vec<Vec<u8>> = histories.iter().map(|h| h.to_proto()).collect();

    let mut group = c.benchmark_group("history");
    group.throughput(Throughput::Elements(total_actions));

    group.bench_function("encode", |b| {
        b.iter(|| {
            let mut total = 0usize;
            for h in &histories {
                total = total.wrapping_add(h.to_proto().len());
            }
            black_box(total);
        });
    });

    group.bench_function("decode", |b| {
        b.iter(|| {
            let mut acc = 0u32;
            for buf in &bytes_arr {
                let h = HandHistory::from_proto(black_box(buf)).expect("decode");
                acc = acc.wrapping_add(h.actions.len() as u32);
            }
            black_box(acc);
        });
    });
    group.finish();
}

// ============================================================================
// criterion 入口
// ============================================================================
//
// 默认 sample_size = 100、warm-up 3s、measurement 5s。`criterion_group!` 内
// 部对配置链尾部追加 `.configure_from_args()`，所以 CLI 参数（CI 用
// `--sample-size 10 --warm-up-time 1 --measurement-time 1 --noplot`）总是优先于
// 这里的 baseline 设置——quick CI 路径压到 30s 以内、nightly 全量走默认。

criterion_group!(
    baseline,
    bench_eval7,
    bench_simulate,
    bench_history,
    // 阶段 2 B1 占位 bench（D-259 命名前缀 `abstraction/*`，与阶段 1 5 条
    // bench 共存；E1 才接 SLO 断言）。
    bench_abstraction_info_mapping,
    bench_abstraction_equity_monte_carlo,
);
criterion_main!(baseline);

// ============================================================================
// 阶段 2 §B1 §E 类：抽象层 bench harness 骨架（D-259 命名前缀 `abstraction/*`）
// ============================================================================
//
// 覆盖 `pluribus_stage2_workflow.md` §B1 §输出 E 类清单：
//
// - 单次 InfoSet mapping（`(GameState, hole) → InfoSetId`，对应 SLO D-280
//   单线程 ≥ 100,000 mapping/s，但 B1 不做断言）
// - 单次 equity Monte Carlo（`equity(hole, board, rng)`，对应 SLO D-282
//   单线程 ≥ 1,000 hand/s 默认 10k iter，但 B1 不做断言）
//
// **B1 状态**：A1 阶段 `PreflopLossless169::map` / `MonteCarloEquity::equity`
// 全部 `unimplemented!()`，运行 `cargo bench --bench baseline` 触到这两个 bench
// 时立即 panic。本 harness 落地 bench 入口 / 输入构造 / `Throughput::Elements`
// 单位钉法，B2 / E1 / E2 [实现] 与 [测试] 在此基础上扩展（E1 才接 SLO 阈值
// 断言到 `tests/perf_slo.rs::stage2_*`，与 stage 1 同型）。
//
// 角色边界：本段属 `[测试]` agent 产物（继承 baseline.rs 顶部声明）。

fn bench_abstraction_info_mapping(c: &mut Criterion) {
    use poker::{InfoAbstraction, PreflopLossless169, Rank, Suit};

    let cfg = TableConfig::default_6max_100bb();
    let state = GameState::new(&cfg, 0);
    let abs = PreflopLossless169::new();
    let hole = [
        Card::new(Rank::Ace, Suit::Spades),
        Card::new(Rank::Ace, Suit::Hearts),
    ];

    let mut group = c.benchmark_group("abstraction/info_mapping");
    group.throughput(Throughput::Elements(1));
    // bench：单次 (GameState, hole) → InfoSetId。B1 阶段 panic（unimplemented）。
    group.bench_function("preflop_lossless_169", |b| {
        b.iter(|| black_box(abs.map(black_box(&state), black_box(hole))));
    });
    group.finish();
}

fn bench_abstraction_equity_monte_carlo(c: &mut Criterion) {
    use std::sync::Arc;

    use poker::{EquityCalculator, MonteCarloEquity, Rank, Suit};

    let evaluator: Arc<dyn HandEvaluator> = Arc::new(NaiveHandEvaluator);
    let calc = MonteCarloEquity::new(Arc::clone(&evaluator)).with_iter(1_000);

    let hole = [
        Card::new(Rank::Ace, Suit::Spades),
        Card::new(Rank::Ace, Suit::Hearts),
    ];
    // 固定 flop 板，避开 hole。
    let board: [Card; 3] = [
        Card::new(Rank::Five, Suit::Spades),
        Card::new(Rank::Eight, Suit::Hearts),
        Card::new(Rank::Ten, Suit::Diamonds),
    ];

    let mut group = c.benchmark_group("abstraction/equity_monte_carlo");
    group.throughput(Throughput::Elements(1));
    // bench：单次 equity (1k iter，CI 短测试模式)。B1 阶段 panic（unimplemented）。
    group.bench_function("flop_1k_iter", |b| {
        let mut seed_counter = 0u64;
        b.iter(|| {
            let mut rng = ChaCha20Rng::from_seed(seed_counter);
            seed_counter = seed_counter.wrapping_add(1);
            black_box(calc.equity(black_box(hole), black_box(&board), &mut rng))
        });
    });
    group.finish();
}
