//! E1：性能 SLO 阈值断言（workflow §E1 §输出 / validation §8）。
//!
//! 把 [`pluribus_stage1_validation.md`] §8 的四条性能门槛转成 release-only
//! 阈值断言，让 E2 性能优化拥有一个机器可验证的"完成"信号。所有断言**当前
//! 预期失败**——B2 的朴素枚举评估器约 10k–1M eval/s，远低于 10M eval/s 的
//! 单线程门槛；E2 替换为 lookup-table 评估器后断言全绿，即 §E2 §出口标准
//! "E1 所有 SLO 断言通过" 的物质载体。
//!
//! 运行方式：
//!
//! ```text
//! cargo test --release --test perf_slo -- --ignored
//! ```
//!
//! 全部 `#[ignore]` 因为：
//!
//! 1. **Release-only**：debug profile 下评估器会再慢 10–50×，断言数字无意义。
//! 2. **CI 默认套件不应破红**：E1 closure 的预期状态就是失败；放进默认 `cargo
//!    test` 会让 main 长期红着，掩盖真正的回归。CI quick path 走的是
//!    `cargo bench --bench baseline`（产出数据，不断言）。
//! 3. **机器依赖**：吞吐对 CPU/cache 敏感；同样的代码在不同 host 上数字差
//!    2–3×。`ignored` 让运维显式选定要跑性能验收的硬件。
//!
//! 角色边界：本文件属 `[测试]` agent。`[实现]` agent 不得修改。

use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Instant;

use poker::eval::NaiveHandEvaluator;
use poker::training::kuhn::{KuhnGame, KuhnInfoSet};
use poker::training::leduc::{LeducGame, LeducInfoSet};
use poker::training::nlhe::SimplifiedNlheGame;
use poker::training::{
    exploitability, EsMccfrTrainer, KuhnBestResponse, LeducBestResponse, Trainer, VanillaCfrTrainer,
};
use poker::{
    canonical_observation_id, Action, BucketConfig, BucketTable, Card, ChaCha20Rng, ChipAmount,
    EquityCalculator, GameState, HandEvaluator, HandHistory, InfoAbstraction, LegalActionSet,
    MonteCarloEquity, PreflopLossless169, RngSource, StreetTag, TableConfig,
};

// ============================================================================
// 共享：随机 7-card 与单手随机模拟（与 benches/baseline.rs 同义）
// ============================================================================

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
        let to = sample_chip(min, max, rng);
        candidates[n] = Action::Bet { to };
        n += 1;
    }
    if let Some((min, max)) = la.raise_range {
        let to = sample_chip(min, max, rng);
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

fn sample_chip(min: ChipAmount, max: ChipAmount, rng: &mut dyn RngSource) -> ChipAmount {
    let lo = min.as_u64();
    let hi = max.as_u64();
    if lo >= hi {
        return min;
    }
    ChipAmount::new(lo + rng.next_u64() % (hi - lo + 1))
}

/// 在 `n_iters` 次 eval7 调用上测量吞吐（eval/s）。`hands` 为输入池。
fn measure_eval7_throughput(hands: &[[Card; 7]], n_iters: usize) -> f64 {
    let evaluator = NaiveHandEvaluator;
    let len = hands.len();
    let start = Instant::now();
    let mut acc = 0u32;
    for i in 0..n_iters {
        acc ^= evaluator.eval7(&hands[i % len]).0;
    }
    let elapsed = start.elapsed();
    // 防 DCE：把 acc 用副作用写进 stderr（稳定且不会被优化掉）。
    if acc == u32::MAX {
        eprintln!("(unreachable) acc=u32::MAX");
    }
    n_iters as f64 / elapsed.as_secs_f64()
}

// ============================================================================
// SLO #1：单线程 eval7 ≥ 10M eval/s（validation §4 / §8）
// ============================================================================

#[test]
#[ignore = "perf SLO; opt-in via `cargo test --release --test perf_slo -- --ignored`"]
fn slo_eval7_single_thread_at_least_10m_per_second() {
    let hands = make_random_hands(1024, 0xE1_5101);
    // 朴素 eval7 实测约 ~50–500 ns/call（debug 慢 10×）；release 下 200k iters
    // 约 10–100 ms，足够样本量但不会让 CI 等待。
    let throughput = measure_eval7_throughput(&hands, 200_000);
    eprintln!(
        "[slo-eval7-single] 实测 {:.0} eval/s（SLO 门槛 ≥ 10,000,000）",
        throughput
    );
    assert!(
        throughput >= 10_000_000.0,
        "eval7 单线程 {:.0} eval/s < SLO 10M eval/s（E1 closure 期望失败；E2 高性能评估器接入后必须通过）",
        throughput
    );
}

// ============================================================================
// SLO #2：多线程接近线性扩展，至少到 8 核（validation §4 / §8）
// ============================================================================

#[test]
#[ignore = "perf SLO"]
fn slo_eval7_multithread_linear_scaling_to_8_cores() {
    let cores_target = std::thread::available_parallelism()
        .map(|n| n.get().min(8))
        .unwrap_or(1);
    if cores_target < 2 {
        eprintln!(
            "[slo-eval7-multi] 当前 host 只有 {cores_target} 核，无法验证多线程扩展，跳过断言"
        );
        return;
    }

    let hands = Arc::new(make_random_hands(1024, 0xE1_5102));
    let per_thread_iters = 200_000usize;

    // 单线程基线：用相同 per_thread_iters，对比公平。
    let single = measure_eval7_throughput(&hands, per_thread_iters);

    // cores_target 线程并发；每个线程独立 iter。聚合吞吐 = 总 iter / 总耗时。
    let start = Instant::now();
    let handles: Vec<_> = (0..cores_target)
        .map(|tid| {
            let hands = Arc::clone(&hands);
            thread::spawn(move || {
                let evaluator = NaiveHandEvaluator;
                let len = hands.len();
                let mut acc = 0u32;
                for i in 0..per_thread_iters {
                    acc ^= evaluator.eval7(&hands[(i + tid * 7919) % len]).0;
                }
                acc
            })
        })
        .collect();
    let mut total_acc = 0u32;
    for h in handles {
        total_acc ^= h.join().expect("thread join");
    }
    let elapsed = start.elapsed();
    if total_acc == u32::MAX {
        eprintln!("(unreachable) total_acc=u32::MAX");
    }
    let multi = (cores_target * per_thread_iters) as f64 / elapsed.as_secs_f64();
    let scaling = multi / single;
    let efficiency = scaling / cores_target as f64;
    eprintln!(
        "[slo-eval7-multi] cores={cores_target} single={single:.0} multi={multi:.0} \
         scaling={scaling:.2}x efficiency={efficiency:.2}（门槛 ≥ 0.70）"
    );
    assert!(
        efficiency >= 0.70,
        "eval7 多线程效率 {efficiency:.2} < 0.70（{cores_target} 核近似线性扩展未达成）"
    );
}

// ============================================================================
// SLO #3：单线程全流程模拟 ≥ 100k hand/s（validation §8）
// ============================================================================

#[test]
#[ignore = "perf SLO"]
fn slo_simulate_full_hand_at_least_100k_per_second() {
    let cfg = TableConfig::default_6max_100bb();
    // 5,000 手在 release 下大约 50–500 ms（B2 实测：~3,000 hand/s 数量级），
    // 给样本量留余地的同时不爆 CI 时长。
    let n_hands = 5_000u64;
    let start = Instant::now();
    let mut total_actions = 0usize;
    for seed in 0..n_hands {
        let h = simulate_one_hand(&cfg, seed);
        total_actions = total_actions.wrapping_add(h.actions.len());
    }
    let elapsed = start.elapsed();
    let hand_per_sec = n_hands as f64 / elapsed.as_secs_f64();
    eprintln!(
        "[slo-simulate] 实测 {hand_per_sec:.0} hand/s（{n_hands} 手 / {:.3}s，\
         平均 {:.1} action/hand）；SLO 门槛 ≥ 100,000",
        elapsed.as_secs_f64(),
        total_actions as f64 / n_hands as f64,
    );
    assert!(
        hand_per_sec >= 100_000.0,
        "全流程模拟 {hand_per_sec:.0} hand/s < SLO 100k hand/s（E1 期望失败；E2 必须通过）"
    );
}

// ============================================================================
// SLO #4：HandHistory 序列化 / 反序列化 ≥ 1M action/s 各方向（validation §8）
// ============================================================================

#[test]
#[ignore = "perf SLO"]
fn slo_history_encode_at_least_1m_action_per_second() {
    let cfg = TableConfig::default_6max_100bb();
    let histories: Vec<HandHistory> = (0..1024u64).map(|s| simulate_one_hand(&cfg, s)).collect();
    let total_actions: u64 = histories.iter().map(|h| h.actions.len() as u64).sum();
    assert!(
        total_actions >= 1_000,
        "样本动作数 {total_actions} 太少，吞吐测量噪声过大"
    );
    // 把 1024 手循环 N 圈，让总耗时 ~100ms 量级，样本量稳。
    let n_loops = 32usize;
    let start = Instant::now();
    let mut total_bytes: usize = 0;
    for _ in 0..n_loops {
        for h in &histories {
            total_bytes = total_bytes.wrapping_add(h.to_proto().len());
        }
    }
    let elapsed = start.elapsed();
    if total_bytes == usize::MAX {
        eprintln!("(unreachable) total_bytes=usize::MAX");
    }
    let actions_per_sec = (n_loops as u64 * total_actions) as f64 / elapsed.as_secs_f64();
    eprintln!(
        "[slo-history-encode] 实测 {actions_per_sec:.0} action/s（{} loops × {total_actions} actions \
         in {:.3}s）；SLO 门槛 ≥ 1,000,000",
        n_loops,
        elapsed.as_secs_f64(),
    );
    assert!(
        actions_per_sec >= 1_000_000.0,
        "HandHistory encode {actions_per_sec:.0} action/s < SLO 1M action/s"
    );
}

#[test]
#[ignore = "perf SLO"]
fn slo_history_decode_at_least_1m_action_per_second() {
    let cfg = TableConfig::default_6max_100bb();
    let histories: Vec<HandHistory> = (0..1024u64).map(|s| simulate_one_hand(&cfg, s)).collect();
    let total_actions: u64 = histories.iter().map(|h| h.actions.len() as u64).sum();
    let bytes_arr: Vec<Vec<u8>> = histories.iter().map(|h| h.to_proto()).collect();
    let n_loops = 32usize;
    let start = Instant::now();
    let mut acc: u64 = 0;
    for _ in 0..n_loops {
        for buf in &bytes_arr {
            let h = HandHistory::from_proto(buf).expect("decode ok");
            acc = acc.wrapping_add(h.actions.len() as u64);
        }
    }
    let elapsed = start.elapsed();
    if acc == u64::MAX {
        eprintln!("(unreachable) acc=u64::MAX");
    }
    let actions_per_sec = (n_loops as u64 * total_actions) as f64 / elapsed.as_secs_f64();
    eprintln!(
        "[slo-history-decode] 实测 {actions_per_sec:.0} action/s（{} loops × {total_actions} actions \
         in {:.3}s）；SLO 门槛 ≥ 1,000,000",
        n_loops,
        elapsed.as_secs_f64(),
    );
    assert!(
        actions_per_sec >= 1_000_000.0,
        "HandHistory decode {actions_per_sec:.0} action/s < SLO 1M action/s"
    );
}

// ============================================================================
// 阶段 2 §E1 §输出：stage2_* SLO 阈值断言（`pluribus_stage2_validation.md` §8）
// ============================================================================
//
// 三条阶段 2 性能门槛断言（D-280 / D-281 / D-282），与 stage-1 SLO 同形态：
// release-only opt-in via `cargo test --release --test perf_slo -- --ignored`，
// `#[ignore]` 让 CI 默认套件不破红。E1 closure 期望全部 / 部分失败（B2 / C2 朴素
// 实现），E2 [实现] 优化后必须全绿。`workflow §E1 §输出` 字面 "断言为待达成
// 状态"。
//
// 角色边界：本节属 `[测试]` agent。`[实现]` agent 不得修改。

/// 阶段 2 §E1 §输出 SLO #1：抽象映射 `≥ 100,000 mapping/s` 单线程（D-280）。
///
/// 测量 `(GameState, hole) → InfoSetId` 全路径单线程吞吐——preflop 路径走
/// `PreflopLossless169::map`（D-217 closed-form）。E2 优化方向（workflow §E2 line
/// 451）：preflop 169 mapping 改 `[u8; 1326]` 直接表替代任何条件分支。
#[test]
#[ignore = "stage2 perf SLO"]
fn stage2_abstraction_mapping_throughput_at_least_100k_per_second() {
    let cfg = TableConfig::default_6max_100bb();
    let state = GameState::new(&cfg, 0);
    let abs = PreflopLossless169::new();
    // 200 组互不相同的 hole 输入；hand_class_169 路径对 hole 敏感，单点输入会
    // 让分支预测过拟合。1326 起手牌总数远大于 200，足够覆盖 169 等价类的常见
    // 分布而不退化为单类。
    let mut rng = ChaCha20Rng::from_seed(0xE1AB_5101);
    let holes: Vec<[Card; 2]> = (0..200)
        .map(|_| {
            let (_, hole) = sample_postflop_input(&mut rng, 3);
            hole
        })
        .collect();

    let n_iters = 500_000usize;
    let start = Instant::now();
    let mut acc: u64 = 0;
    for i in 0..n_iters {
        let hole = holes[i % holes.len()];
        let id = abs.map(&state, hole);
        acc = acc.wrapping_add(id.raw());
    }
    let elapsed = start.elapsed();
    if acc == u64::MAX {
        eprintln!("(unreachable) acc=u64::MAX");
    }
    let throughput = n_iters as f64 / elapsed.as_secs_f64();
    eprintln!(
        "[stage2-abstraction-mapping] 实测 {throughput:.0} mapping/s（SLO 门槛 ≥ 100,000；\
         {n_iters} mappings / {:.3}s）",
        elapsed.as_secs_f64(),
    );
    assert!(
        throughput >= 100_000.0,
        "抽象映射 {throughput:.0} mapping/s < SLO 100k mapping/s（E1 closure 期望失败；\
         E2 性能优化（preflop [u8; 1326] 直接表）后必须通过）"
    );
}

/// 阶段 2 §E1 §输出 SLO #2：bucket lookup `P95 ≤ 10 μs`（D-281）。
///
/// 测量 `(street, board, hole) → bucket_id` 单次查表延迟分布——`canonical_observation_id`
/// （sort + first-appearance suit remap + FNV-1a，dominant 成本）+
/// `BucketTable::lookup`（mmap-equivalent `bytes[off + id*4..]` 读取）。
///
/// **fixture 取舍**：100/100/100 + cluster_iter=200 与 `tests/bucket_quality.rs`
/// `cached_trained_table` 同型（~70 s release 训练 setup），不依赖 95 KB 的
/// `artifacts/bucket_table_default_500_500_500_seed_cafebabe.bin`（gitignore，
/// 见 C2 §C-rev1 §1 carve-out）。bucket 数量不影响 lookup body cache 行为。
///
/// E2 优化方向（workflow §E2 line 449）：bucket lookup hot path 内存布局优化
/// （cache-friendly canonical id 编码）。
#[test]
#[ignore = "stage2 perf SLO"]
fn stage2_bucket_lookup_p95_latency_at_most_10us() {
    let evaluator: Arc<dyn HandEvaluator> = Arc::new(NaiveHandEvaluator);
    let table = BucketTable::train_in_memory(
        BucketConfig {
            flop: 100,
            turn: 100,
            river: 100,
        },
        0xC2_FA22_BD75_710E,
        evaluator,
        200,
    );

    // 每条街 5_000 sample × 3 街 = 15_000 latencies；P95 索引位 14_249，分布尾部
    // 估计噪声 < 5%。Instant::now() 在 Linux x86_64 走 clock_gettime(CLOCK_MONOTONIC)，
    // ~20 ns 系统调用开销 ≪ 10 μs SLO 门槛，可直接计入测量。
    let n_per_street = 5_000usize;
    let mut latencies_ns: Vec<u64> = Vec::with_capacity(n_per_street * 3);
    let mut rng = ChaCha20Rng::from_seed(0xE1BC_2002);
    for (street, board_len) in [
        (StreetTag::Flop, 3usize),
        (StreetTag::Turn, 4usize),
        (StreetTag::River, 5usize),
    ] {
        for _ in 0..n_per_street {
            let (board, hole) = sample_postflop_input(&mut rng, board_len);
            let board_slice: &[Card] = &board[..board_len];
            let t0 = Instant::now();
            let obs_id = canonical_observation_id(street, board_slice, hole);
            let bucket = table.lookup(street, obs_id);
            let dt = t0.elapsed();
            // 防 DCE：把 bucket 传入 black_box，避免编译器把 lookup 整段消除。
            std::hint::black_box(bucket);
            latencies_ns.push(dt.as_nanos() as u64);
        }
    }
    latencies_ns.sort_unstable();
    let p50 = latencies_ns[latencies_ns.len() / 2];
    let p95_idx = (latencies_ns.len() as f64 * 0.95) as usize;
    let p95 = latencies_ns[p95_idx];
    let p99_idx = (latencies_ns.len() as f64 * 0.99) as usize;
    let p99 = latencies_ns[p99_idx];
    eprintln!(
        "[stage2-bucket-lookup] {} samples（{n_per_street}/街 × 3 街）：P50 = {p50} ns / \
         P95 = {p95} ns / P99 = {p99} ns（SLO 门槛 P95 ≤ 10,000 ns = 10 μs）",
        latencies_ns.len(),
    );
    assert!(
        p95 <= 10_000,
        "bucket lookup P95 {p95} ns > SLO 10,000 ns = 10 μs（E1 closure 期望失败；\
         E2 优化 hot path 内存布局后必须通过）"
    );
}

/// 阶段 2 §E1 §输出 SLO #3：equity Monte Carlo `≥ 1,000 hand/s` @ 10k iter（D-282）。
///
/// 测量 `MonteCarloEquity::equity(hole, board, rng)` 默认 10,000 iter 单线程吞吐。
/// **仅用于离线 clustering 训练**——D-225 锁运行时映射热路径不允许触发 Monte
/// Carlo（必须命中 lookup table）。
///
/// 阶段 1 §4 / §6.5 间接约束：`10,000 iter / hand × 1,000 hand/s = 10M eval/s`
/// 正好打满阶段 1 SLO；阶段 1 实测 20.76M eval/s 提供约 2× 缓冲。E1 期望失败
/// 仍是 B2 朴素实现 deck 拷贝 / RNG 抽样开销在 10k iter 路径上尚未优化。
///
/// E2 优化方向（workflow §E2 line 450）：equity Monte Carlo 多线程 + SIMD 优化（如必要）。
#[test]
#[ignore = "stage2 perf SLO"]
fn stage2_equity_monte_carlo_throughput_at_least_1k_hand_per_second() {
    let evaluator: Arc<dyn HandEvaluator> = Arc::new(NaiveHandEvaluator);
    let calc = MonteCarloEquity::new(evaluator).with_iter(10_000);

    // 100 手 × 10k iter @ 1k hand/s SLO ⇒ ~0.1 s 理论；B2 朴素估计 ~10×（朴素
    // eval ~20M eval/s release × 2 evals / iter = 10M iter/s 上限 / 10k iter ≈
    // 1k hand/s，刚好 SLO 边界，外加 RNG / deck 开销可能掉到 200–500 hand/s）。
    let n_hands = 100usize;
    let mut sample_rng = ChaCha20Rng::from_seed(0xE1E0_3003);
    let inputs: Vec<([Card; 5], [Card; 2])> = (0..n_hands)
        .map(|_| sample_postflop_input(&mut sample_rng, 3))
        .collect();

    let mut equity_rng = ChaCha20Rng::from_seed(0xE1E0_3003_u64.wrapping_add(0xDEAD_BEEF));
    let start = Instant::now();
    let mut acc = 0.0f64;
    for (board, hole) in &inputs {
        let board_slice: &[Card] = &board[..3];
        let eq = calc
            .equity(*hole, board_slice, &mut equity_rng)
            .expect("equity ok on valid (hole, board) pair");
        acc += eq;
    }
    let elapsed = start.elapsed();
    if !acc.is_finite() {
        eprintln!("(unreachable) acc=NaN");
    }
    let throughput = n_hands as f64 / elapsed.as_secs_f64();
    eprintln!(
        "[stage2-equity-mc] 实测 {throughput:.1} hand/s @ 10k iter（{n_hands} hand / {:.3}s，\
         平均 equity = {:.4}）；SLO 门槛 ≥ 1,000 hand/s",
        elapsed.as_secs_f64(),
        acc / n_hands as f64,
    );
    assert!(
        throughput >= 1_000.0,
        "equity MC {throughput:.1} hand/s < SLO 1,000 hand/s（E1 closure 期望失败；\
         E2 多线程 + SIMD 优化后必须通过）"
    );
}

// ============================================================================
// 共享 fixture（与 `benches/baseline.rs::sample_postflop_input` 同形态）
// ============================================================================

/// 从 RngSource 抽取 `board_len + 2` 张不重复的 Card 拆成 (board\[0..5\], hole\[2\])。
/// `board` 数组仅前 `board_len` 项有效，与 `canonical_observation_id` 接受的
/// `board: &[Card]` 切片语义一致——`bench` 与 SLO 测试共用同一抽样算法保证
/// 输入分布一致性。
fn sample_postflop_input(rng: &mut dyn RngSource, board_len: usize) -> ([Card; 5], [Card; 2]) {
    debug_assert!((3..=5).contains(&board_len));
    let mut deck: [u8; 52] = std::array::from_fn(|i| i as u8);
    let total = board_len + 2;
    for i in 0..total {
        let j = i + (rng.next_u64() % ((52 - i) as u64)) as usize;
        deck.swap(i, j);
    }
    let mut board = [Card::from_u8(0).expect("0 valid"); 5];
    for (i, slot) in board.iter_mut().enumerate().take(board_len) {
        *slot = Card::from_u8(deck[i]).expect("0..52");
    }
    let hole = [
        Card::from_u8(deck[board_len]).expect("0..52"),
        Card::from_u8(deck[board_len + 1]).expect("0..52"),
    ];
    (board, hole)
}

// ============================================================================
// 阶段 3 §E1 §输出：stage3_* SLO 阈值断言（`pluribus_stage3_validation.md` §8 +
// `pluribus_stage3_decisions.md` §7 D-360..D-369 + D-348）
// ============================================================================
//
// 六条 stage 3 性能门槛断言（D-360 训练时长 × 2 + D-361 训练吞吐 × 2 + D-348
// exploitability 计算 × 2），与 stage 1 / stage 2 SLO 同形态：release-only
// opt-in via `cargo test --release --test perf_slo -- --ignored`，`#[ignore]`
// 让 CI 默认套件不破红。E1 closure 期望部分失败（B2 / C2 朴素实现，特别是
// D-361 多线程 SLO 在 C2 serial-fallback `step_parallel` 路径下必然失败），
// E2 \[实现\] 优化 + D-321-rev1 真并发后必须全绿。workflow §E1 line 278 字面
// "perf 测试不暴露新 API"。
//
// 角色边界：本节属 stage 3 \[测试\] agent。\[实现\] agent 不得修改。

/// v3 production artifact path（D-314-rev1 lock）。与 `tests/cfr_simplified_nlhe.rs`
/// + `benches/stage3.rs` 同 const，跨测试 / bench 共享 ground truth。
const STAGE3_V3_ARTIFACT_PATH: &str =
    "artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin";

/// v3 artifact body BLAKE3 ground truth（CLAUDE.md "当前 artifact 基线"）。
/// 用于 stage 3 NLHE SLO 测试 helper 兜底 sanity check：artifact body hash
/// 不匹配 v3 → eprintln + skip（与 cfr_simplified_nlhe.rs 同型）。
const STAGE3_V3_BODY_BLAKE3_HEX: &str =
    "67ee555439f2c918698650c05f40a7a5e9e812280ceb87fc3c6590add98650cd";

/// D-360 SLO #1 + #2 字面 iteration count：Kuhn / Leduc 10K iter Vanilla CFR。
const STAGE3_CFR_ITERS: u64 = 10_000;

/// D-348 BR SLO 用的 trained-trainer iter 数：BR 计算延迟与 trainer 训练程度
/// 无关（BR 算法复杂度由 InfoSet 数量和树规模决定，不读 regret 量级），
/// 1K iter 即可让 trainer.average_strategy 在所有 reachable InfoSet 上 populated；
/// 训练成本 ~10 ms release，远不影响 BR 测量。
const STAGE3_BR_TRAIN_ITERS: u64 = 1_000;

/// D-361 NLHE 单线程吞吐 SLO 测量 update 数：≥ 10K update/s 下 20K updates ≈
/// 2 s baseline，样本量稳定且不爆 ignored 套件时长。
const STAGE3_NLHE_SINGLE_THREAD_UPDATES: u64 = 20_000;

/// D-361 NLHE 4-core 吞吐 SLO 测量 update 数：≥ 50K update/s 下 50K updates ≈
/// 1 s baseline；C2 serial-fallback `step_parallel` 路径下实测 ≈ 单线程吞吐，
/// 期望 SLO 失败（E1 closure 形态）。
const STAGE3_NLHE_FOUR_CORE_UPDATES: u64 = 50_000;

/// 加载 v3 artifact 并构造 `SimplifiedNlheGame`；artifact 缺失 / schema 不匹配 /
/// `SimplifiedNlheGame::new` 失败时 eprintln + 返回 `None`（pass-with-skip）。
/// 与 `tests/cfr_simplified_nlhe.rs::load_v3_artifact_or_skip` 同型路径，避免
/// 跨 test crate 共享 helper 引入 pub API（perf_slo 不暴露新 API per workflow
/// §E1 line 278）。
fn stage3_load_v3_artifact_or_skip() -> Option<SimplifiedNlheGame> {
    let path = PathBuf::from(STAGE3_V3_ARTIFACT_PATH);
    if !path.exists() {
        eprintln!(
            "[stage3-nlhe-slo] skip: v3 artifact `{STAGE3_V3_ARTIFACT_PATH}` 不存在（CI / \
             GitHub-hosted runner 典型场景；本地 dev box / vultr / AWS host 有 artifact 时跑）。"
        );
        return None;
    }
    let table = match BucketTable::open(&path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[stage3-nlhe-slo] skip: BucketTable::open 失败：{e:?}");
            return None;
        }
    };
    let body_hex = stage3_blake3_hex(&table.content_hash());
    if body_hex != STAGE3_V3_BODY_BLAKE3_HEX {
        eprintln!(
            "[stage3-nlhe-slo] skip: artifact body BLAKE3 `{body_hex}` 不匹配 v3 ground truth \
             `{STAGE3_V3_BODY_BLAKE3_HEX}`（D-314-rev1 lock 要求 v3 artifact；stale v1/v2 路径 skip）。"
        );
        return None;
    }
    match SimplifiedNlheGame::new(Arc::new(table)) {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!("[stage3-nlhe-slo] skip: SimplifiedNlheGame::new 失败：{e:?}");
            None
        }
    }
}

fn stage3_blake3_hex(bytes: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

// ----------------------------------------------------------------------------
// SLO #1：Kuhn 10K iter Vanilla CFR `< 1 s` release（D-360）
// ----------------------------------------------------------------------------

/// D-360 字面上界 `< 1 s` release for Kuhn 10K iter Vanilla CFR。
///
/// Vanilla CFR 在 Kuhn 上 1 iter = 1 完整博弈树 DFS × 2 traverser；Kuhn 12
/// InfoSet × 2 action ≈ 240K node visits per 10K iter；release 下应 `< 1 s`
/// 在任意现代 host 上达成。E1 closure 在 B2 朴素实现下大概率通过（Kuhn 树规模
/// 小到 SLO 余量充足），E2 优化路径主要给 Leduc / NLHE。本测试是 D-360 lower
/// bound trip-wire。
#[test]
#[ignore = "stage3 perf SLO; opt-in via `cargo test --release --test perf_slo -- --ignored`"]
fn stage3_kuhn_10k_iter_under_1s_release() {
    let master_seed: u64 = 0xE153_014B_5548_4EFF;
    let mut trainer = VanillaCfrTrainer::new(KuhnGame, master_seed);
    let mut rng = ChaCha20Rng::from_seed(master_seed);

    let start = Instant::now();
    for _ in 0..STAGE3_CFR_ITERS {
        trainer
            .step(&mut rng)
            .expect("Kuhn Vanilla CFR step 期望成功（D-330 容差仅 warn 不 panic）");
    }
    let elapsed = start.elapsed();
    let iters_per_sec = STAGE3_CFR_ITERS as f64 / elapsed.as_secs_f64();
    eprintln!(
        "[stage3-kuhn-10k] 实测 {STAGE3_CFR_ITERS} iter / {:.3} s = {iters_per_sec:.0} iter/s\
         （SLO 门槛 ≤ 1.000 s）",
        elapsed.as_secs_f64(),
    );
    assert!(
        elapsed.as_secs_f64() < 1.0,
        "Kuhn 10K iter Vanilla CFR 耗时 {:.3} s ≥ D-360 字面阈值 1.0 s",
        elapsed.as_secs_f64(),
    );
}

// ----------------------------------------------------------------------------
// SLO #2：Leduc 10K iter Vanilla CFR `< 60 s` release（D-360）
// ----------------------------------------------------------------------------

/// D-360 字面上界 `< 60 s` release for Leduc 10K iter Vanilla CFR。
///
/// Leduc ~288 InfoSet × 树规模 × 10K iter ≈ 数百万 node visits；release 下应
/// `< 60 s`。E1 closure 在 B2 朴素 `HashMap<InfoSet, Vec<f64>>` 路径下可能边界
/// 紧（具体由 host CPU + cache 决定），E2 优化方向（D-303 + D-306 `SmallVec`
/// hot path、D-336 CDF lookup table）翻面。
#[test]
#[ignore = "stage3 perf SLO; opt-in via `cargo test --release --test perf_slo -- --ignored`"]
fn stage3_leduc_10k_iter_under_60s_release() {
    let master_seed: u64 = 0xE153_024C_4544_55FF;
    let mut trainer = VanillaCfrTrainer::new(LeducGame, master_seed);
    let mut rng = ChaCha20Rng::from_seed(master_seed);

    let start = Instant::now();
    for _ in 0..STAGE3_CFR_ITERS {
        trainer
            .step(&mut rng)
            .expect("Leduc Vanilla CFR step 期望成功（D-330 容差仅 warn 不 panic）");
    }
    let elapsed = start.elapsed();
    let iters_per_sec = STAGE3_CFR_ITERS as f64 / elapsed.as_secs_f64();
    eprintln!(
        "[stage3-leduc-10k] 实测 {STAGE3_CFR_ITERS} iter / {:.3} s = {iters_per_sec:.0} iter/s\
         （SLO 门槛 ≤ 60.000 s）",
        elapsed.as_secs_f64(),
    );
    assert!(
        elapsed.as_secs_f64() < 60.0,
        "Leduc 10K iter Vanilla CFR 耗时 {:.3} s ≥ D-360 字面阈值 60.0 s",
        elapsed.as_secs_f64(),
    );
}

// ----------------------------------------------------------------------------
// SLO #3：简化 NLHE 单线程 ES-MCCFR `≥ 10,000 update/s` release（D-361 单线程）
// ----------------------------------------------------------------------------

/// D-361 字面下界 `≥ 10,000 update/s` release 单线程 for 简化 NLHE ES-MCCFR。
///
/// 100M update / 10K update/s = 10,000 s ≈ 2.78 h 单 host 可行（D-342 验收门槛
/// 100M update）。E1 closure 在 C2 朴素实现下 throughput 由 `RegretTable` HashMap
/// hit rate + cfv 累积 alloc 模式决定，E2 优化方向（`SmallVec` / lookup-table /
/// 借用替 clone）翻面。
///
/// artifact 缺失（CI 典型场景）走 eprintln + pass-with-skip（与 cfr_simplified_nlhe.rs
/// 同型），本地 dev box / vultr / AWS host 有 artifact 时跑真实 throughput。
#[test]
#[ignore = "stage3 perf SLO; opt-in via `cargo test --release --test perf_slo -- --ignored`"]
fn stage3_simplified_nlhe_single_thread_throughput_ge_10k_update_per_s() {
    let Some(game) = stage3_load_v3_artifact_or_skip() else {
        return;
    };
    let master_seed: u64 = 0xE153_034E_4C48_45FF;
    let mut trainer = EsMccfrTrainer::new(game, master_seed);
    let mut rng = ChaCha20Rng::from_seed(master_seed);

    // warm-up 100 update：首批 update 触发 RegretTable lazy alloc，让 throughput
    // 测量段落只反映 steady-state cost（与 stage 2 P95 SLO `sample_postflop_input`
    // 同型 warm-up 思路）。
    for _ in 0..100 {
        trainer.step(&mut rng).expect("NLHE warm-up step");
    }

    let start = Instant::now();
    for _ in 0..STAGE3_NLHE_SINGLE_THREAD_UPDATES {
        trainer
            .step(&mut rng)
            .expect("NLHE ES-MCCFR step 期望成功（D-330 容差仅 warn 不 panic）");
    }
    let elapsed = start.elapsed();
    let throughput = STAGE3_NLHE_SINGLE_THREAD_UPDATES as f64 / elapsed.as_secs_f64();
    eprintln!(
        "[stage3-nlhe-single] 实测 {STAGE3_NLHE_SINGLE_THREAD_UPDATES} update / {:.3} s = \
         {throughput:.0} update/s（SLO 门槛 ≥ 10,000）",
        elapsed.as_secs_f64(),
    );
    assert!(
        throughput >= 10_000.0,
        "NLHE ES-MCCFR 单线程 {throughput:.0} update/s < D-361 字面阈值 10,000 update/s\
         （E1 closure 期望失败时由 E2 优化路径翻面）",
    );
}

// ----------------------------------------------------------------------------
// SLO #4：简化 NLHE 4-core ES-MCCFR `≥ 50,000 update/s` release（D-361 多线程）
// ----------------------------------------------------------------------------

/// D-361 字面下界 `≥ 50,000 update/s` release 4-core for 简化 NLHE ES-MCCFR
/// （效率 ≥ 0.5）。
///
/// **C2 closed 时形态**：`EsMccfrTrainer::step_parallel` = serial-equivalent
/// fallback（D-321-rev1 lock 段落 `pluribus_stage3_decisions.md` §10.2，2026-05-13）；
/// 单次 `step_parallel(&mut [Box<dyn RngSource>; 4], 4)` = 4 顺序 `step` 调用，
/// 实测吞吐 ≈ 单线程，SLO 期望失败。**E2 \[实现\] 落地真并发后**（D-321-rev1
/// thread-local accumulator + batch merge）必须达成 ≥ 50K update/s。
///
/// host 限制：`thread::available_parallelism() < 4` 时 eprintln 提示 + return
/// （pass-with-skip）；与 stage 1 多线程 SLO 同型 host-load skip 路径。
#[test]
#[ignore = "stage3 perf SLO; opt-in via `cargo test --release --test perf_slo -- --ignored`"]
fn stage3_simplified_nlhe_4core_throughput_ge_50k_update_per_s() {
    let cores_target = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    if cores_target < 4 {
        eprintln!(
            "[stage3-nlhe-4core] skip: host 仅 {cores_target} core，< 4 core 无法验证 \
             D-361 4-core SLO（host 强制 4-core 时跑）。"
        );
        return;
    }
    let Some(game) = stage3_load_v3_artifact_or_skip() else {
        return;
    };
    let master_seed: u64 = 0xE153_044E_3443_52FF;
    let mut trainer = EsMccfrTrainer::new(game, master_seed);
    // 4 个独立 RngSource 派生自 master_seed + 不同 nonce，避免 4 线程 RNG byte-equal
    // 导致 sampled trajectory 完全重合（D-308 sub-stream 独立性同型思路）。
    let mut rng_pool: Vec<Box<dyn RngSource>> = (0..4u64)
        .map(|tid| {
            let seeded = master_seed.wrapping_add(0xDEAD_BEEF_u64.wrapping_mul(tid + 1));
            Box::new(ChaCha20Rng::from_seed(seeded)) as Box<dyn RngSource>
        })
        .collect();

    // warm-up 4 update（per pool size）：触发 RegretTable lazy alloc。
    trainer
        .step_parallel(&mut rng_pool, 4)
        .expect("NLHE warm-up step_parallel");

    // 总 update 数 = pool size × n_calls = 4 × n_calls = STAGE3_NLHE_FOUR_CORE_UPDATES。
    let n_calls = STAGE3_NLHE_FOUR_CORE_UPDATES / 4;
    let start = Instant::now();
    for _ in 0..n_calls {
        trainer
            .step_parallel(&mut rng_pool, 4)
            .expect("NLHE ES-MCCFR step_parallel 期望成功（C2 serial-fallback 路径）");
    }
    let elapsed = start.elapsed();
    let total_updates = n_calls * 4;
    let throughput = total_updates as f64 / elapsed.as_secs_f64();
    eprintln!(
        "[stage3-nlhe-4core] 实测 {total_updates} update / {:.3} s = {throughput:.0} \
         update/s（SLO 门槛 ≥ 50,000；C2 serial-fallback 期望失败，E2 \\[实现\\] 真并发\
         落地 D-321-rev1 thread-local accumulator + batch merge 后必须通过）",
        elapsed.as_secs_f64(),
    );
    assert!(
        throughput >= 50_000.0,
        "NLHE ES-MCCFR 4-core {throughput:.0} update/s < D-361 字面阈值 50,000 update/s\
         （E1 closure 期望失败；E2 真并发实现后必须通过）",
    );
}

// ----------------------------------------------------------------------------
// SLO #5：Kuhn exploitability 单次计算 `< 100 ms` release（D-348）
// ----------------------------------------------------------------------------

/// D-348 字面上界 `< 100 ms` release for Kuhn 单次 exploitability 计算。
///
/// `exploitability::<KuhnGame, KuhnBestResponse>` 内部走 `BR::compute` 2 次
/// （player 0 + player 1）+ full-tree backward induction over 12 InfoSet × 2
/// action；release 下 < 100 ms 余量充足。让 F3 \[报告\] 4 checkpoint Kuhn
/// exploitability 实测能在 `4 × 100 ms = 400 ms` 内完成。
///
/// 注：trainer.average_strategy 在 BR closure 中是 hot path（每 BR 节点查
/// HashMap 一次）；E1 closure 在 B2 朴素实现下 `< 100 ms` 一般通过，E2 优化
/// 主要给 D-361 NLHE 吞吐。
#[test]
#[ignore = "stage3 perf SLO; opt-in via `cargo test --release --test perf_slo -- --ignored`"]
fn stage3_kuhn_best_response_under_100ms_release() {
    let master_seed: u64 = 0xE153_054B_4252_5FFF;
    let mut trainer = VanillaCfrTrainer::new(KuhnGame, master_seed);
    let mut rng = ChaCha20Rng::from_seed(master_seed);
    // 预训练（BR 性能与 trainer 训练程度无关；populate average_strategy 让 BR
    // closure 拿到真实 strategy 而非空 HashMap）。
    for _ in 0..STAGE3_BR_TRAIN_ITERS {
        trainer.step(&mut rng).expect("Kuhn pretrain step");
    }
    let avg_closure = |info: &KuhnInfoSet, _n: usize| trainer.average_strategy(info);

    let start = Instant::now();
    let expl = exploitability::<KuhnGame, KuhnBestResponse>(&KuhnGame, &avg_closure);
    let elapsed = start.elapsed();
    let ms = elapsed.as_secs_f64() * 1000.0;
    eprintln!(
        "[stage3-kuhn-br] 实测 exploitability = {expl:.6} chips/game / 单次耗时 {ms:.2} ms\
         （SLO 门槛 ≤ 100 ms）",
    );
    assert!(
        expl >= 0.0,
        "exploitability {expl} 必须非负（D-340 定义 `(BR_0 + BR_1) / 2`）",
    );
    assert!(
        elapsed.as_secs_f64() < 0.100,
        "Kuhn exploitability 单次计算耗时 {ms:.2} ms ≥ D-348 字面阈值 100 ms",
    );
}

// ----------------------------------------------------------------------------
// SLO #6：Leduc exploitability 单次计算 `< 1 s` release（D-348）
// ----------------------------------------------------------------------------

/// D-348 字面上界 `< 1 s` release for Leduc 单次 exploitability 计算。
///
/// `exploitability::<LeducGame, LeducBestResponse>` 内部走 `BR::compute` 2 次，
/// 加 backward induction polynomial in InfoSet count（~288 InfoSet × 树规模）；
/// release 下应 `< 1 s`。让 F3 \[报告\] 4 checkpoint Leduc exploitability 实测
/// 能在 `4 × 1 s = 4 s` 内完成。
#[test]
#[ignore = "stage3 perf SLO; opt-in via `cargo test --release --test perf_slo -- --ignored`"]
fn stage3_leduc_best_response_under_1s_release() {
    let master_seed: u64 = 0xE153_064C_4252_5FFF;
    let mut trainer = VanillaCfrTrainer::new(LeducGame, master_seed);
    let mut rng = ChaCha20Rng::from_seed(master_seed);
    for _ in 0..STAGE3_BR_TRAIN_ITERS {
        trainer.step(&mut rng).expect("Leduc pretrain step");
    }
    let avg_closure = |info: &LeducInfoSet, _n: usize| trainer.average_strategy(info);

    let start = Instant::now();
    let expl = exploitability::<LeducGame, LeducBestResponse>(&LeducGame, &avg_closure);
    let elapsed = start.elapsed();
    let ms = elapsed.as_secs_f64() * 1000.0;
    eprintln!(
        "[stage3-leduc-br] 实测 exploitability = {expl:.6} chips/game / 单次耗时 {ms:.2} ms\
         （SLO 门槛 ≤ 1000 ms）",
    );
    assert!(
        expl >= 0.0,
        "exploitability {expl} 必须非负（D-341 同 D-340 定义）",
    );
    assert!(
        elapsed.as_secs_f64() < 1.0,
        "Leduc exploitability 单次计算耗时 {ms:.2} ms ≥ D-348 字面阈值 1000 ms",
    );
}
