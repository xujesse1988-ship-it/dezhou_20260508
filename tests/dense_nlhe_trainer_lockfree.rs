//! Dense **lock-free atomic** 并行路径
//! ([`DenseNlheEsMccfrTrainer::step_parallel_lockfree`]) 的回归门槛
//! （`docs/temp/nlhe_dense_parallel_merge_alternatives_2026_05_28.md` §A.9）。
//!
//! Lockfree 路径下 CAS race 顺序不定，跨 run 不再 byte-equal；不能复用
//! [`tests/dense_nlhe_trainer.rs::dense_step_parallel_byte_equal_hashmap`] 的合同。
//! 本文件给本路径自己的回归 anchor：
//!
//! - **`lockfree_smoke_no_panic`**：trainer 不 panic、update_count 正确。
//! - **`lockfree_self_consistency`**：average_strategy 每行 `Σ_a avg(a) ≈ 1.0`（CAS
//!   race 不破坏归一化语义，sum 偏 < 1e-9）。
//! - **`lockfree_avg_strategy_close_to_hashmap`**：与 HashMap deterministic 路径
//!   同 seed 跑相同总 update 数后，traverser 已访问 infoset 上 average_strategy
//!   L∞ 鲁棒统计（median / p75 宽松门槛 + coverage 下界）。
//! - **`lockfree_with_lcfr_period_smoke`**：lockfree × LCFR period rescale
//!   组合 smoke——跨多个 period boundary 不 panic + average sum ≈ 1.0 +
//!   与 HashMap LCFR 同 seed 鲁棒统计对照（验 `rescale_all` 在 lockfree par_iter
//!   join 后正确触发；详 `DenseNlheEsMccfrTrainer::step_parallel_lockfree` doc）。
//!
//! 全部 `#[ignore]`：dense 两表 4.62 GiB 满分配 + lockfree 8 worker 并行写入需
//! ≥ 8 GiB 机器（vultr 7.7 GiB 边缘可跑短跑；HashMap 对照测试需 ≥ 10 GiB）。
//! 沿用 [`tests/dense_nlhe_trainer.rs`] 的 v4 artifact skip 兜底——v4 artifact
//! 不在机器上时 eprintln + return（不 assert）。

use std::path::PathBuf;
use std::sync::Arc;

use poker::training::nlhe::SimplifiedNlheGame;
use poker::training::nlhe_dense_trainer::DenseNlheEsMccfrTrainer;
use poker::training::{EsMccfrTrainer, Trainer};
use poker::{BucketTable, ChaCha20Rng, InfoSetId, RngSource};

/// 候选 v4 artifact 路径（相对 repo root）。沿用 `dense_nlhe_trainer.rs`。
const V3_ARTIFACT_CANDIDATES: &[&str] = &[
    "artifacts/bucket_table_default_500_500_500_seed_cafebabe_schemav4.bin",
    "artifacts/bucket_table_default_500_500_500_seed_cafebabe_v4.bin",
];

/// 驱动 trainer step 的 base seed（多 worker rng pool 每 tid 派生独立 seed）。
const RNG_SEED: u64 = 0x44_4E_4C_4B_46_5F_42_45; // "DNLKF_BE"
/// trainer master seed（仅派生 checkpoint rng_substream_seed 占位，step 不消费）。
const MASTER_SEED: u64 = 0x44_4E_4C_4B_46_5F_4D_53; // "DNLKF_MS"

/// 加载任一可用 v4 artifact；缺失 → eprintln + `None`（skip）。
fn load_bucket_table_or_skip() -> Option<Arc<BucketTable>> {
    for path in V3_ARTIFACT_CANDIDATES {
        let p = PathBuf::from(path);
        if !p.exists() {
            continue;
        }
        match BucketTable::open(&p) {
            Ok(t) => return Some(Arc::new(t)),
            Err(e) => eprintln!("skip-candidate: BucketTable::open({path}) 失败：{e:?}"),
        }
    }
    eprintln!(
        "skip: 无 v4 bucket table artifact（候选 {V3_ARTIFACT_CANDIDATES:?}）；\
         dev box / vultr / AWS host 有 artifact 时跑。"
    );
    None
}

/// 建 `n` 个独立 `ChaCha20Rng`（per-tid nonce），同 `base` 下两次调用产同 pool。
fn build_rng_pool(base: u64, n: usize) -> Vec<Box<dyn RngSource>> {
    (0..n as u64)
        .map(|tid| {
            let seeded = base.wrapping_add(0xDEAD_BEEF_u64.wrapping_mul(tid + 1));
            Box::new(ChaCha20Rng::from_seed(seeded)) as Box<dyn RngSource>
        })
        .collect()
}

/// `step_parallel_lockfree` 短跑 smoke：不 panic、update_count 等于
/// `n_threads × batch_per_worker × n_calls`，trainer 不 abort。
#[test]
#[ignore = "dense 两表满分配 4.62 GiB 虚拟空间；release --ignored 单独跑"]
fn lockfree_smoke_no_panic() {
    let Some(bucket_table) = load_bucket_table_or_skip() else {
        return;
    };
    let game = SimplifiedNlheGame::new(Arc::clone(&bucket_table)).expect("dense game");
    let mut trainer = DenseNlheEsMccfrTrainer::new(game, MASTER_SEED);

    let n_threads = 4;
    let batch_per_worker = 16;
    let n_calls = 10; // 4 × 16 × 10 = 640 update（smoke）
    let mut pool = build_rng_pool(RNG_SEED, n_threads);
    for _ in 0..n_calls {
        trainer
            .step_parallel_lockfree(&mut pool, n_threads, batch_per_worker)
            .expect("step_parallel_lockfree must not error");
    }
    assert_eq!(
        trainer.update_count(),
        (n_threads * batch_per_worker * n_calls) as u64,
        "update_count 应等于 n_threads × batch_per_worker × n_calls"
    );
    eprintln!(
        "[lockfree smoke] {} update OK（{} thread × B={} × {} call）",
        trainer.update_count(),
        n_threads,
        batch_per_worker,
        n_calls
    );
}

/// average_strategy 每行 `Σ_a avg(a) ≈ 1.0`：lockfree 路径 CAS race 顺序不定，
/// 但归一化语义（`avg(a) = S(a) / Σ_b S(b)`）保持——任何 traverser 访问过的行
/// 都应满足 sum 偏 ≤ 1e-9。未访问行退化均匀分布也满足。
///
/// 用 HashMap trainer 同 seed lockstep 仅作**采集 traverser 已访问 infoset key 集**
/// （dense 表是扁平数组，无 key 枚举入口）；不比较两路径数值。
#[test]
#[ignore = "dense + HashMap key 采集，峰值 ~7 GiB；release --ignored 单独跑"]
fn lockfree_self_consistency() {
    let Some(bucket_table) = load_bucket_table_or_skip() else {
        return;
    };
    let game_dense = SimplifiedNlheGame::new(Arc::clone(&bucket_table)).expect("dense game");
    let game_hm = SimplifiedNlheGame::new(Arc::clone(&bucket_table)).expect("hashmap game");

    let mut dense = DenseNlheEsMccfrTrainer::new(game_dense, MASTER_SEED);
    let mut hm: EsMccfrTrainer<SimplifiedNlheGame> = EsMccfrTrainer::new(game_hm, MASTER_SEED);

    let n_threads = 4;
    let batch_per_worker = 16;
    let n_calls = 16; // 4 × 16 × 16 = 1024 update
    let mut pool_dense = build_rng_pool(RNG_SEED, n_threads);
    let mut pool_hm = build_rng_pool(RNG_SEED, n_threads);
    for _ in 0..n_calls {
        dense
            .step_parallel_lockfree(&mut pool_dense, n_threads, batch_per_worker)
            .expect("dense step_parallel_lockfree");
        hm.step_parallel(&mut pool_hm, n_threads, batch_per_worker)
            .expect("hashmap step_parallel");
    }

    let visited: Vec<InfoSetId> = hm.strategy_sum().inner().keys().copied().collect();
    assert!(
        visited.len() > 500,
        "仅访问 {} 个 infoset，样本太少",
        visited.len()
    );
    let mut checked = 0usize;
    for &info in &visited {
        let avg = dense.average_strategy(info);
        if avg.is_empty() {
            // dense touched bit 未置位（仅作为非-traverser 路过；见
            // `DenseNlheEsMccfrTrainer::current_strategy` doc）。跳过——HashMap
            // 路径返回 uniform，dense 返回空，逻辑等价。
            continue;
        }
        let sum: f64 = avg.iter().sum();
        assert!(
            (sum - 1.0).abs() < 1.0e-9,
            "lockfree average_strategy sum 偏 @ info {:#x}: sum={sum:.17} avg={avg:?}",
            info.raw()
        );
        for (i, &p) in avg.iter().enumerate() {
            assert!(
                (0.0..=1.0 + 1.0e-12).contains(&p),
                "lockfree average_strategy 越界 @ info {:#x} action {i}: p={p}",
                info.raw()
            );
        }
        checked += 1;
    }
    assert!(
        checked > 0,
        "至少应该校验过 1 个已 touch infoset（visited={}）",
        visited.len()
    );
    eprintln!(
        "[lockfree self-consistency] {checked}/{} visited infoset sum ≈ 1.0 ✓",
        visited.len()
    );
}

/// lockfree 路径与 HashMap deterministic step_parallel 在**相同总 update / 相同
/// rng pool seed** 下的 average_strategy 偏差。CAS race 让两路径同 seed 不再
/// byte-equal（σ 读取的 cell snapshot 随机），且两路径**算法不同**：
/// - HashMap：pre-dispatch snapshot σ；本批所有 worker 看同一张 σ 表
/// - Lockfree：σ 读当下表；同批内 worker 间互相影响 σ
///
/// 两路径短跑后 trajectory 分布会渐进发散（同 seed 但 σ 不同 → 不同 sampled
/// action）。**最低访问的 infoset 上 L∞ 必然能跑到 1.0**（单次 update 把概率推到
/// 不同 action）；这不是 lockfree 的 bug，是 MCCFR 单样本本身的噪声。
///
/// 因此用**鲁棒统计**而非 worst-case L∞：sort 所有 infoset 的 L∞，看中位数 +
/// p75。两路径在「访问过」的多数 infoset 上应给出近似策略；只有低 visit 尾巴噪声大。
///
/// **门槛**（1920 update 噪声基线）：
/// - `median L∞ < 0.10`、`p75 L∞ < 0.20`：在两路径**交集** infoset 上的 MCCFR
///   单样本噪声 + 两路径 σ 语义差下的实测余量。命中说明 lockfree 在交集 infoset
///   上与 HashMap 同档；不命中说明 cell-level race（Hogwild! CAS 顺序破坏数值）。
/// - `0.7 ≤ dense_touched / hm_keys ≤ 1.3`：dense lockfree 实际访问 unique
///   infoset 数与 HashMap 应同数量级。塌方（dense_touched ≪ hm_keys）说明
///   lockfree recurse 系统性漏 state；爆涨（≫）说明触摸了不该触摸的 row。
///   **不 assert 两路径 trajectory 集的 intersection 大小**——同 rng pool 但
///   σ 语义不同（HM pre-dispatch snapshot vs lockfree live），第 1 个 batch
///   就 σ 分叉 → action 分叉 → state 分叉 → NLHE 巨大状态空间下两集合
///   90%+ 不重合是预期的（实测 ≈ 92.5%）。
///
/// **baseline 实测**（vultr 4-core AMD EPYC-Rome / 1920 update / 同 RNG_SEED；
/// 2026-05-28 跑 4 次取范围）：
/// - `HM strategy_sum.keys = 107523`（4 run 稳定）
/// - `dense strategy_sum touched ∈ [101747, 107038]`（touched_ratio 0.946 - 0.995；
///   ≪ 1.0 的浮动是 Hogwild! CAS race 让每次 trajectory 集略不同的预期）
/// - `dense regret touched`：与 strategy_sum 同（traverser 节点同时 set 两 bit）
/// - `n (intersection) ∈ [7286, 7368]`（intersection_coverage 0.068 - 0.070）
/// - `median L∞ = 0e0`、`p75 L∞ = 0e0`（> 75% 交集 infoset 完全 byte-equal —— σ
///   分叉前同 rng 同空表 → 同累积；分叉后才有差异）
/// - `p95 L∞ ∈ [0.332, 0.340]`
/// - `worst L∞ = 1.0`（尾部 trajectory 完全分叉是预期，单样本 MCCFR 单次 update
///   就能把概率推到不同 action）
/// - wall ~5.5s / Maximum RSS ~5.3 GiB（vultr 7.7 GiB 充裕）
#[test]
#[ignore = "dense + HashMap 两套表 + 短跑收敛对照，峰值 ~7 GiB；release --ignored 单独跑"]
fn lockfree_avg_strategy_close_to_hashmap() {
    let Some(bucket_table) = load_bucket_table_or_skip() else {
        return;
    };
    let game_dense = SimplifiedNlheGame::new(Arc::clone(&bucket_table)).expect("dense game");
    let game_hm = SimplifiedNlheGame::new(Arc::clone(&bucket_table)).expect("hashmap game");

    let mut dense = DenseNlheEsMccfrTrainer::new(game_dense, MASTER_SEED);
    let mut hm: EsMccfrTrainer<SimplifiedNlheGame> = EsMccfrTrainer::new(game_hm, MASTER_SEED);

    let n_threads = 4;
    let batch_per_worker = 16;
    let n_calls = 30; // 4 × 16 × 30 = 1920 update
    let mut pool_dense = build_rng_pool(RNG_SEED, n_threads);
    let mut pool_hm = build_rng_pool(RNG_SEED, n_threads);
    for _ in 0..n_calls {
        dense
            .step_parallel_lockfree(&mut pool_dense, n_threads, batch_per_worker)
            .expect("dense step_parallel_lockfree");
        hm.step_parallel(&mut pool_hm, n_threads, batch_per_worker)
            .expect("hashmap step_parallel");
    }
    assert_eq!(dense.update_count(), hm.update_count());

    let visited: Vec<InfoSetId> = hm.strategy_sum().inner().keys().copied().collect();
    assert!(
        visited.len() > 500,
        "仅访问 {} 个 infoset，样本太少",
        visited.len()
    );
    // 诊断：dense 自己的 touched_count 与 HM strategy_sum keys 数对比。
    // 区分 (a) dense 真访问了 ≈ HM 数量但 average_strategy 检测异常 vs
    // (b) dense lockfree 实际 trajectory 集与 HM 严重发散。
    let dense_strat_touched = dense.strategy_sum().touched_count();
    let dense_regret_touched = dense.regret_table().touched_count();
    eprintln!(
        "[diag] HM strategy_sum.keys={} | dense strategy_sum touched={} regret touched={}",
        visited.len(),
        dense_strat_touched,
        dense_regret_touched,
    );

    let mut diffs: Vec<f64> = Vec::with_capacity(visited.len());
    for &info in &visited {
        let avg_dense = dense.average_strategy(info);
        let avg_hm = hm.average_strategy(&info);
        if avg_dense.is_empty() || avg_dense.len() != avg_hm.len() {
            continue;
        }
        let l_inf = avg_dense
            .iter()
            .zip(&avg_hm)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0_f64, f64::max);
        diffs.push(l_inf);
    }
    assert!(!diffs.is_empty(), "无可比较的 infoset");
    // 绝对触摸量比：dense lockfree 实际访问 unique infoset 数应与 HM 同数量级。
    // 抓「lockfree 漏 state」的真 bug；不抓 trajectory 集发散（trajectory 发散
    // 是 σ 语义差异 + 同 rng pool 的预期行为，见模块 doc）。
    let touched_ratio = dense_strat_touched as f64 / visited.len() as f64;
    assert!(
        (0.7..=1.3).contains(&touched_ratio),
        "lockfree dense touched / HM keys = {touched_ratio:.3} 不在 [0.7, 1.3]：\
         dense strategy_sum touched={dense_strat_touched}, HM strategy_sum.keys={}",
        visited.len(),
    );
    // intersection coverage 仅作信息性输出（trajectory 发散预期，~7% 实测）。
    let intersection_coverage = diffs.len() as f64 / visited.len() as f64;
    diffs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = diffs.len();
    let median = diffs[n / 2];
    let p75 = diffs[(n * 3) / 4];
    let p95 = diffs[(n * 95) / 100];
    let worst = *diffs.last().unwrap();

    // 系统性漂移检测：median 应远小于均匀分布距离（≤2-action 上 0.5）。
    assert!(
        median < 0.10,
        "lockfree vs HashMap median L∞={median:e} > 0.10（n={n}, p75={p75:e}, p95={p95:e}, worst={worst:e}）",
    );
    assert!(
        p75 < 0.20,
        "lockfree vs HashMap p75 L∞={p75:e} > 0.20（n={n}, median={median:e}, p95={p95:e}, worst={worst:e}）",
    );
    eprintln!(
        "[lockfree vs HashMap] n={n} median={median:e} p75={p75:e} p95={p95:e} worst={worst:e} \
         touched_ratio={touched_ratio:.3} intersection_coverage={intersection_coverage:.3} ✓"
    );
}

/// **lockfree × LCFR period rescale** 组合 smoke。
///
/// `step_parallel_lockfree` 在 `par_iter` join 后调 `maybe_lcfr_rescale`
/// ([`DenseNlheEsMccfrTrainer::step_parallel_lockfree`] 末尾，line 300)，靠
/// `rescale_all(&mut self)` 与 worker `&self` 借用互斥结构性保证 race 消失。
/// 之前测试链没有任何 case 同时打开 LCFR + lockfree——本测试补这条覆盖路径。
///
/// **跑法**：lockfree + HashMap 各跑 1920 update / period=500 → 跨 3 个 boundary
/// （500 / 1000 / 1500 完成；2000 未到）。HashMap 路径走 `step_parallel`
/// （deterministic merge，本测试不验 byte-equal，仅作 σ 收敛性参考）。
///
/// **门槛**：
/// - dense `update_count == 1920`
/// - dense 已访问 infoset 上 `Σ avg ≈ 1.0`（rescale 不破归一化语义）
/// - `0.7 ≤ dense_touched / hm_keys ≤ 1.3`（rescale 不该让 lockfree 系统性漏
///   state；同 vanilla 用绝对触摸量比，而非 intersection——trajectory 集发散
///   是 σ 语义差异 + 同 rng pool 的预期行为）
/// - 交集 infoset 上 `median L∞ < 0.15`、`p75 < 0.30`（LCFR rescale 引入的
///   额外 σ 漂移让门槛比 vanilla 0.10/0.20 略松）
///
/// **baseline 实测**（vultr 4-core AMD EPYC-Rome / 1920 update / period=500 /
/// 同 RNG_SEED；2026-05-28 跑 3 次取范围）：
/// - `HM strategy_sum.keys = 110187`（LCFR 不影响 traverser-visited 集大小，
///   与 vanilla 107523 同数量级）
/// - `dense strategy_sum touched ∈ [107239, 109313]`（touched_ratio 0.973 - 0.992）
/// - `dense regret touched`：与 strategy_sum 同
/// - `n (intersection) ∈ [10609, 10809]`（intersection_coverage 0.096 - 0.098；
///   比 vanilla 6.8% 略高，可能因 LCFR 衰减让两路径"新 trajectory"更易重合）
/// - `median L∞ = 0e0`、`p75 L∞ = 0e0`（与 vanilla 同样 > 75% byte-equal）
/// - `p95 L∞ ∈ [0.307, 0.323]`
/// - `worst L∞ = 1.0`（与 vanilla 同）
/// - `checked_sum == n`（所有交集 infoset 都满足 Σ avg ≈ 1.0 ± 1e-9，rescale
///   不破归一化语义 ✓）
///
/// 命中失败时优先核查：rescale 是否在某个 worker 上下文里被错误触发（应当只
/// 在 main thread `&mut self` 路径）、`global_scale` 是否在 par_iter 中被错
/// 误读取（worker 内每次 `accumulate_by_slot` 读当下 scale，但本批 rescale
/// 不该 fire）。
#[test]
#[ignore = "dense + HashMap 两套表 + LCFR 短跑，峰值 ~7 GiB；release --ignored 单独跑"]
fn lockfree_with_lcfr_period_smoke() {
    let Some(bucket_table) = load_bucket_table_or_skip() else {
        return;
    };
    let game_dense = SimplifiedNlheGame::new(Arc::clone(&bucket_table)).expect("dense game");
    let game_hm = SimplifiedNlheGame::new(Arc::clone(&bucket_table)).expect("hashmap game");

    let lcfr_period = 500u64;
    let mut dense =
        DenseNlheEsMccfrTrainer::new(game_dense, MASTER_SEED).with_lcfr_period(lcfr_period);
    let mut hm: EsMccfrTrainer<SimplifiedNlheGame> =
        EsMccfrTrainer::new(game_hm, MASTER_SEED).with_lcfr_period(lcfr_period);

    let n_threads = 4;
    let batch_per_worker = 16;
    let n_calls = 30; // 4 × 16 × 30 = 1920 update，跨 3 个 period boundary
    let mut pool_dense = build_rng_pool(RNG_SEED, n_threads);
    let mut pool_hm = build_rng_pool(RNG_SEED, n_threads);
    for _ in 0..n_calls {
        dense
            .step_parallel_lockfree(&mut pool_dense, n_threads, batch_per_worker)
            .expect("dense step_parallel_lockfree (lcfr)");
        hm.step_parallel(&mut pool_hm, n_threads, batch_per_worker)
            .expect("hashmap step_parallel (lcfr)");
    }
    let expected_updates = (n_threads * batch_per_worker * n_calls) as u64;
    assert_eq!(dense.update_count(), expected_updates);
    assert_eq!(hm.update_count(), expected_updates);
    assert!(
        expected_updates >= 3 * lcfr_period,
        "测试设置必须跨至少 3 个 period boundary 才有意义；updates={expected_updates}, period={lcfr_period}"
    );

    let visited: Vec<InfoSetId> = hm.strategy_sum().inner().keys().copied().collect();
    assert!(
        visited.len() > 500,
        "LCFR 短跑仅访问 {} 个 infoset，样本太少",
        visited.len()
    );

    // 1) 归一化语义：rescale 不该破 Σ avg ≈ 1.0。
    let mut checked_sum = 0usize;
    for &info in &visited {
        let avg = dense.average_strategy(info);
        if avg.is_empty() {
            continue;
        }
        let sum: f64 = avg.iter().sum();
        assert!(
            (sum - 1.0).abs() < 1.0e-9,
            "lockfree+LCFR average_strategy sum 偏 @ info {:#x}: sum={sum:.17} avg={avg:?}",
            info.raw()
        );
        for (i, &p) in avg.iter().enumerate() {
            assert!(
                (0.0..=1.0 + 1.0e-12).contains(&p),
                "lockfree+LCFR average_strategy 越界 @ info {:#x} action {i}: p={p}",
                info.raw()
            );
        }
        checked_sum += 1;
    }
    assert!(
        checked_sum > 0,
        "至少应该校验过 1 个已 touch infoset（visited={}）",
        visited.len()
    );

    // 2) 与 HashMap+LCFR 鲁棒对照（确认 rescale 在两路径上效果一致）。
    let mut diffs: Vec<f64> = Vec::with_capacity(visited.len());
    for &info in &visited {
        let avg_dense = dense.average_strategy(info);
        let avg_hm = hm.average_strategy(&info);
        if avg_dense.is_empty() || avg_dense.len() != avg_hm.len() {
            continue;
        }
        let l_inf = avg_dense
            .iter()
            .zip(&avg_hm)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0_f64, f64::max);
        diffs.push(l_inf);
    }
    assert!(!diffs.is_empty(), "无可比较的 infoset");
    // 同 vanilla close_to_hashmap：用绝对触摸量比，不用 intersection 比。
    let dense_strat_touched = dense.strategy_sum().touched_count();
    let dense_regret_touched = dense.regret_table().touched_count();
    let touched_ratio = dense_strat_touched as f64 / visited.len() as f64;
    eprintln!(
        "[diag lcfr] HM strategy_sum.keys={} | dense strategy_sum touched={} regret touched={}",
        visited.len(),
        dense_strat_touched,
        dense_regret_touched,
    );
    assert!(
        (0.7..=1.3).contains(&touched_ratio),
        "lockfree+LCFR dense touched / HM keys = {touched_ratio:.3} 不在 [0.7, 1.3]：\
         dense strategy_sum touched={dense_strat_touched}, HM strategy_sum.keys={}",
        visited.len(),
    );
    let intersection_coverage = diffs.len() as f64 / visited.len() as f64;
    diffs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = diffs.len();
    let median = diffs[n / 2];
    let p75 = diffs[(n * 3) / 4];
    let p95 = diffs[(n * 95) / 100];
    let worst = *diffs.last().unwrap();

    assert!(
        median < 0.15,
        "lockfree+LCFR vs HashMap+LCFR median L∞={median:e} > 0.15（n={n}, p75={p75:e}, p95={p95:e}, worst={worst:e}）",
    );
    assert!(
        p75 < 0.30,
        "lockfree+LCFR vs HashMap+LCFR p75 L∞={p75:e} > 0.30（n={n}, median={median:e}, p95={p95:e}, worst={worst:e}）",
    );
    eprintln!(
        "[lockfree+LCFR vs HashMap+LCFR] n={n} median={median:e} p75={p75:e} p95={p95:e} worst={worst:e} \
         touched_ratio={touched_ratio:.3} intersection_coverage={intersection_coverage:.3} checked_sum={checked_sum} ✓"
    );
}
