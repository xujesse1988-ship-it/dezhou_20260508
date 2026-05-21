# Bucket 特征设计（per-street）

## 1. 每条街的信息结构

| 街 | 已知 | 未知 | 主要决策信号 |
|---|---|---|---|
| flop | 3 board + 2 hole | 2 future cards（C(47, 2) = 1081 outcomes） | 当前强度 + draw 潜力 + 对手范围 |
| turn | 4 board + 2 hole | 1 future card（46 outcomes） | 当前强度 + 对手范围 |
| river | 5 board + 2 hole | 无 | showdown 强度 vs 对手范围分布 |

核心差异：

- flop / turn：主要信号是 **"未来 equity 分布"**，需要捕获 made hand vs draw 的 CDF 形状。
- river：无未来牌，主要信号是 **"对手范围分布"**，需要细粒度的多维对手 class 相对强度。

→ 每条街用不同特征。

## 2. 特征设计

```
flop  = equity_hist_8 (over 1081) + OCHS_8   = 16 dim
turn  = equity_hist_8 (over 46)   + OCHS_8   = 16 dim
river = OCHS_16                               = 16 dim
```

三街维度统一 16，centroid u8 量化 schema 复用；**语义每街不同**。

### 2.1 equity_hist_8（flop / turn 专用）

#### 定义

对每条街，枚举所有未来牌 outcome：

- flop：C(47, 2) = 1081 个 (turn_card, river_card) 无序对，从剩余 47 张牌中取
- turn：46 个 river_card 单卡，从剩余 46 张牌中取

对每个 future outcome，计算 hero 在 resulting 5-card board 上的 deterministic equity vs uniform random opp_hole（enumerate opp_hole over C(45, 2) = 990 个）：

```
equity(hero, full_board)
  = (1/990) × Σ over opp ∈ unused_45_C2 of {
      +1 if rank(hero, full_board) > rank(opp, full_board)
      +0.5 if rank(hero, full_board) == rank(opp, full_board)
      0 otherwise
    }
```

将 1081（或 46）个 equity 值按 [0, 1] 等宽 8 bin 离散化（边界 0/8, 1/8, ..., 7/8, 1），输出归一化频率向量 ∈ ℝ⁸（各分量和 = 1）。

边界处理：equity = k/8 的样本归入 bin k（k = 0..7），equity = 1.0 归入 bin 7（最后一个 bin 区间为 [7/8, 1]）。

#### 实现 pseudo-code

```rust
fn equity_hist_8(hole: [Card; 2], board: &[Card; 3 or 4], evaluator: &dyn HandEvaluator) -> [f64; 8] {
    let used = build_used_set(&hole, &[], board);  // 52 → 47 或 46 unused
    let unused: Vec<Card> = (0..52).filter(|c| !used[c]).map(Card::from_u8).collect();
    let mut bins = [0u32; 8];
    let mut total = 0u32;

    // future outcome 枚举
    match board.len() {
        3 => {
            for i in 0..unused.len() {
                for j in (i+1)..unused.len() {
                    let full_board = [board[0], board[1], board[2], unused[i], unused[j]];
                    let eq = equity_exact_river(hole, &full_board, evaluator);
                    bins[bin_index(eq)] += 1;
                    total += 1;
                }
            }
        }
        4 => {
            for &c in &unused {
                let full_board = [board[0], board[1], board[2], board[3], c];
                let eq = equity_exact_river(hole, &full_board, evaluator);
                bins[bin_index(eq)] += 1;
                total += 1;
            }
        }
        _ => unreachable!(),
    }
    bins.map(|n| n as f64 / total as f64)
}

fn equity_exact_river(hole: [Card; 2], full_board: &[Card; 5], evaluator: &dyn HandEvaluator) -> f64 {
    let used = build_used_set(&hole, &[], full_board);
    let unused: Vec<Card> = (0..52).filter(|c| !used[c]).map(Card::from_u8).collect();  // 45 张
    let me_rank = evaluator.eval7(&[hole[0], hole[1], full_board[0..5]].concat());
    let mut wins_x2 = 0u64;
    for i in 0..unused.len() {
        for j in (i+1)..unused.len() {
            let opp_rank = evaluator.eval7(&[unused[i], unused[j], full_board[0..5]].concat());
            wins_x2 += compare_x2(me_rank, opp_rank);  // 2 / 1 / 0
        }
    }
    wins_x2 as f64 / (2.0 * 990.0)
}

fn bin_index(eq: f64) -> usize {
    let idx = (eq * 8.0).floor() as usize;
    idx.min(7)  // eq == 1.0 → bin 7
}
```

#### 性能注解

- `me_rank` 在内层 990 opp enumeration 内不变 → 预计算 1 次（§E hot path 模式，equity.rs:546-548）
- 1081 outer × 990 inner = 1.07M 对 opp 评估 / flop sample
- evaluator.eval7 在 hero 侧已预计算，每对评估 = 1 次 opp eval7

#### 含义

完整 equity CDF 形状。隐含 EHS（一阶矩 = Σ p_k · (k+0.5)/8）、EHS²（二阶矩）、高阶矩、多峰性。能区分：

- "made hand 守不住"（双峰：左 bin 集中 0-2 = 被 outdraw，右 bin 集中 5-7 = 守住）
- "draw 没翻盘"（左偏：bin 0-3 占大头）
- "稳定中等强度"（单峰中心 bin 3-4）
- "坚果"（右尾极端：bin 7 占 > 90%）

### 2.2 OCHS_N（三街都用）

#### 定义

将 169 preflop class 按 EHS 聚成 N 个对手 cluster（OCHS warmup table，详见 §2.3）。给定 (hero_hole, board)，对每个 cluster k 计算：

```
OCHS_N[k] = (1 / |cluster_k|') × Σ over class ∈ cluster_k of {
                  equity_vs_hand(hero, class_rep, board)
                  if class_rep 不与 hero / board 冲突
                }
```

其中 `|cluster_k|'` 是 cluster_k 中**未冲突**的 class 数（与 hero/board 有 card 重叠的 rep 跳过；冲突 rep 全跳过时落回 0.5）。

输出 ∈ ℝᴺ，每维 ∈ [0, 1]。

#### 实现 pseudo-code

```rust
fn ochs_n(hole: [Card; 2], board: &[Card], evaluator: &dyn HandEvaluator, n: u8) -> Vec<f64> {
    let table = ochs_table(n, evaluator);  // OnceLock 缓存 (§2.3)
    let mut out = Vec::with_capacity(n as usize);
    for cluster_id in 0..(n as usize) {
        let classes = &table.classes_per_cluster[cluster_id];
        let mut sum = 0.0;
        let mut count = 0u32;
        for &class_id in classes {
            let opp = table.representative_hole[class_id as usize];
            if pair_overlaps(&hole, &opp) || any_overlaps_board(&opp, board) {
                continue;
            }
            sum += equity_vs_hand(hole, opp, board, evaluator);  // 见下
            count += 1;
        }
        out.push(if count > 0 { sum / count as f64 } else { 0.5 });
    }
    out
}

fn equity_vs_hand(hole: [Card; 2], opp: [Card; 2], board: &[Card], eval: &dyn HandEvaluator) -> f64 {
    match board.len() {
        5 => {  // river: 1-shot showdown
            let me = eval.eval7(&[hole[0], hole[1], board[0..5]].concat());
            let op = eval.eval7(&[opp[0], opp[1], board[0..5]].concat());
            if me > op { 1.0 } else if me == op { 0.5 } else { 0.0 }
        }
        4 => {  // turn: enumerate 46 river outcomes
            let used = build_used_set(&hole, &opp, board);
            let mut wins_x2 = 0u64;
            let mut count = 0u64;
            for c in (0..52).filter(|c| !used[*c as usize]) {
                let full_board = [board[0], board[1], board[2], board[3], Card::from_u8(c)];
                wins_x2 += compare_x2(
                    eval.eval7(&[hole[0], hole[1], full_board[0..5]].concat()),
                    eval.eval7(&[opp[0], opp[1], full_board[0..5]].concat()),
                );
                count += 1;
            }
            wins_x2 as f64 / (2.0 * count as f64)
        }
        3 => {  // flop: enumerate C(45,2) = 990 (turn, river) outcomes
            // 同 turn 但双层循环
        }
        _ => unreachable!(),
    }
}
```

#### 性能注解

- N=8 / N=16 OCHS warmup table 在 OnceLock 静态缓存（每进程一次性 cost ≈ 10s）
- equity_vs_hand 内层 enumerate 都是 deterministic，不消耗 RNG
- 平均 cluster 大小：N=8 → ~21 class/cluster；N=16 → ~10.6 class/cluster
- 冲突 skip 数：board+hero 占 5-7 张，169 reps 中约 ≤ 7 rep 冲突，影响 ≤ 33% per cluster

#### 含义

对每个对手 "假想范围" 给出 hero 的相对强度。river N 提高到 16 因为 river 没有 hist 维度，需要更多 OCHS 粒度填充信息空间，且 river deterministic 计算便宜（每 dim ≤ 22 次 eval7）。

### 2.3 OCHS warmup table（共享基础设施）

OCHS_N 依赖的对手 cluster 划分由独立 warmup 算法生成，写入 `OchsTable.classes_per_cluster`。已存在于 `equity.rs::build_ochs_table`。

#### 算法

```rust
// 常量（equity.rs hardcoded）
const OCHS_TRAINING_SEED: u64 = 0x0CC8_5EED_C2D2_22A0;
const OCHS_PRECOMPUTE_ITER: u32 = 10_000;

fn build_ochs_table(n_clusters: u32, evaluator: &dyn HandEvaluator) -> OchsTable {
    // Step 1: 169 preflop class 每类的 representative hole
    let reps: [[Card; 2]; 169] = std::array::from_fn(|i| representative_hole_for_class(i));
    //   class 0..12 = pocket pair (rank = class_id, Spades + Hearts)
    //   class 13..90 = suited (双 Spades)
    //   class 91..168 = offsuit (Spades + Hearts)

    // Step 2: per-class EHS via MC（独立 sub-stream per class）
    let mut ehs = [0.0_f64; 169];
    for class_id in 0..169 {
        let sub_seed = derive_substream_seed(OCHS_TRAINING_SEED, OCHS_FEATURE_INNER, class_id);
        let mut rng = ChaCha20Rng::from_seed(sub_seed);
        let mut wins_x2 = 0u64;
        for _ in 0..OCHS_PRECOMPUTE_ITER {
            let (opp_hole, full_board) = sample_opp_and_board(&reps[class_id], 5, &mut rng);
            wins_x2 += compare_x2(
                evaluator.eval7(&[reps[class_id], full_board].concat()),
                evaluator.eval7(&[opp_hole, full_board].concat()),
            );
        }
        ehs[class_id] = wins_x2 as f64 / (2.0 * OCHS_PRECOMPUTE_ITER as f64);
    }

    // Step 3: 1D k-means K=n_clusters on the 169 EHS scalar
    let features: Vec<Vec<f64>> = ehs.iter().map(|&x| vec![x]).collect();
    let kmeans_res = kmeans_fit(
        &features, KMeansConfig::default_d232(n_clusters),
        OCHS_TRAINING_SEED, OCHS_WARMUP, OCHS_WARMUP,
    );

    // Step 4: D-236b reorder（按 EHS 中位数升序，cluster 0 = weakest）
    let (_, reordered) = reorder_by_ehs_median(kmeans_res.centroids, kmeans_res.assignments, &ehs);

    // Step 5: build inverted index
    let mut classes_per_cluster: Vec<Vec<u8>> = vec![Vec::new(); n_clusters as usize];
    for (class_id, &cid) in reordered.iter().enumerate() {
        classes_per_cluster[cid as usize].push(class_id as u8);
    }
    OchsTable { representative_hole: reps, classes_per_cluster }
}
```

#### 169 class id 编码（equity.rs::representative_hole_for_class）

| class_id 范围 | 类型 | 数量 | 解码 |
|---|---|---|---|
| 0..=12 | pocket pair | 13 | rank = class_id；rep = (rank♠, rank♥) |
| 13..=90 | suited | 78 | (high, low) 反解 idx = class_id - 13，rep = (high♠, low♠) |
| 91..=168 | offsuit | 78 | (high, low) 反解 idx = class_id - 91，rep = (high♠, low♥) |

#### Dump 工具

新增 `tools/ochs_warmup_dump.rs`：

```bash
cargo run --release --bin ochs_warmup_dump -- --n-clusters 8  > artifacts/ochs_warmup_8.json
cargo run --release --bin ochs_warmup_dump -- --n-clusters 16 > artifacts/ochs_warmup_16.json
```

vultr / 本地跨架构 byte-equal 已验证（OCHS_TRAINING_SEED hardcoded + NaiveHandEvaluator deterministic）。Wall ~260 ms each。

artifact JSON 字段：`n_clusters / ochs_training_seed / ochs_precompute_iter / evaluator / representative_hole / class_labels (人读 "AA"/"AKs"/"72o") / ehs_per_class / classes_per_cluster / cluster_labels / cluster_centroid_ehs / cluster_summary`。

#### N=8 clustering（真值；OCHS_PRECOMPUTE_ITER=10000）

EHS 全 169 class 范围：[0.3276, 0.8558]。

| cluster | size | EHS 区间 | centroid | 成员（人读 label） |
|---|---|---|---|---|
| C7 (top) | 5 | [0.7452, 0.8558] | 0.7992 | TT, JJ, QQ, KK, AA |
| C6 | 12 | [0.6315, 0.7228] | 0.6584 | 66, 77, 88, 99, KQs, ATs, AJs, AQs, AKs, AJo, AQo, AKo |
| C5 | 22 | [0.5812, 0.6282] | 0.6033 | 55, QTs, QJs, K8s-KJs, A3s-A9s, ATo, KTo, KJo, KQo, QJo（共 22 类） |
| C4 | 30 | [0.5339, 0.5792] | 0.5552 | 33, 44, T9s, J8s, J9s, JTs, Q6s-Q9s, K3s-K7s, A2s, JTo, Q9o, KTo... |
| C3 | 29 | [0.4843, 0.5306] | 0.5069 | 22, 97s, 98s, T6s-T8s, J3s-J7s, Q2s-Q5s, T9o, J8o-JTo, Q7o-Q9o... |
| C2 | 26 | [0.4392, 0.4824] | 0.4616 | 75s, 76s, 85s-87s, 94s-96s, T2s-T5s, 87o-98o, T6o-T8o, J5o-J7o... |
| C1 | 32 | [0.3825, 0.4338] | 0.4077 | 43s, 52s-54s, 62s-65s, 72s-74s, 82s-84s, 92s, 93s, 65o-86o, 75o-95o... |
| C0 (bottom) | 13 | [0.3276, 0.3795] | 0.3565 | 32s, 42s, 32o, 42o, 43o, 52o, 53o, 62o, 63o, 72o, 73o, 82o, 83o |

观察：
- 1D k-means 在 EHS 高端密度低 → top cluster C7 把 TT..AA 全合并（5 类），premium pair gap 小于均匀切分
- C0 有 13 个，是 trash 集中区；32s / 42s 因 EHS 落在 0.36-0.38 与最差 offsuit 同 cluster
- C6 把 "中等口袋对" 66-99 + AK/AQ/AJ + KQs 全揉成 12 类，OCHS_8 内 "强中等" 这一类相当宽

#### N=16 clustering（真值；OCHS_PRECOMPUTE_ITER=10000）

| cluster | size | EHS 区间 | centroid | 代表成员 |
|---|---|---|---|---|
| C15 (top) | 3 | [0.7987, 0.8558] | 0.8253 | QQ, KK, AA |
| C14 | 3 | [0.7228, 0.7746] | 0.7475 | 99, TT, JJ |
| C13 | **1** | [0.6965, 0.6965] | 0.6965 | 88（k-means 把 88 单独分一类） |
| C12 | 11 | [0.6282, 0.6692] | 0.6464 | 66, 77, KQs, A9s-AKs, AJo, AQo, AKo |
| C11 | 16 | [0.5953, 0.6204] | 0.6077 | 55, QTs, QJs, K9s-KJs, A5s-A8s, KTo-KQo, ... |
| C10 | 17 | [0.5615, 0.5878] | 0.5733 | 44, JTs, Q8s, Q9s, K6s-K8s, A2s-A4s, QTo-QJo, ... |
| C9 | 16 | [0.5377, 0.5580] | 0.5475 | 33, T9s, J8s, J9s, Q6s, Q7s, K4s, K5s, JTo, Q8o, Q9o, K6o, ... |
| C8 | 10 | [0.5182, 0.5354] | 0.5266 | T8s, J7s, Q5s, K2s, K3s, T9o, J9o, Q7o, K4o, K5o |
| C7 | 18 | [0.4913, 0.5130] | 0.5027 | 22, 97s, 98s, T7s, J4s-J6s, Q2s-Q4s, T8o, J7o, ... |
| C6 | 13 | [0.4693, 0.4862] | 0.4785 | 86s, 87s, 96s, T5s, T6s, J2s, J3s, 98o, T7o, ... |
| C5 | 10 | [0.4522, 0.4625] | 0.4577 | 76s, 95s, T2s-T4s, 87o, 97o, T6o, J3o, J4o |
| C4 | 7 | [0.4338, 0.4455] | 0.4418 | 65s, 75s, 85s, 94s, 96o, T5o, J2o |
| C3 | 12 | [0.4169, 0.4308] | 0.4239 | 64s, 74s, 84s, 92s, 93s, 76o, 85o, 86o, 95o, T2o-T4o |
| C2 | 9 | [0.3993, 0.4122] | 0.4046 | 53s, 54s, 73s, 82s, 83s, 65o, 75o, 93o, 94o |
| C1 | 14 | [0.3693, 0.3935] | 0.3839 | 32s, 43s, 52s, 62s, 63s, 72s, 54o, 64o, 73o, 74o, 82o, 83o, ... |
| C0 (bottom) | 9 | [0.3276, 0.3648] | 0.3494 | 42s, 32o, 42o, 43o, 52o, 53o, 62o, 63o, 72o |

观察：
- C13 单独把 **88** 切出来 — 88 的 EHS = 0.6965，与 99（0.7228）有 ~0.026 间隔，与 77（推测 ~0.66）也有可比 gap，1D k-means 选择把它隔离
- top 3 cluster (C13/C14/C15) 总共只有 7 个 hand class — N=16 把所有 premium 资源放在精细切分中高强度
- 中低区间 (C0-C7) 单 cluster size 7-18，分辨率明显高于 N=8

#### artifact 进 git

`artifacts/ochs_warmup_8.json` / `artifacts/ochs_warmup_16.json` 进 git（每个 ~7-8 KB），写入 BLAKE3 sum 文件供 CI 复现。该 artifact 是 §2.2 OCHS_N feature 计算的**输入**，bucket table 训练时不重复 dump。

BLAKE3：

```
ochs_warmup_8.json:  16ef30c3fa5831cef4d3398a2bfbcd3cd0cece184feb525769c3a03d3cc55a27
ochs_warmup_16.json: 04908b397ebe67ed1f00a6eb9fecdb651ed7ae691e09f6f2fe2be023e7b4964a
```

#### 1D-EHS warmup 的已知问题

1D EHS k-means 只看一个 scalar，把 **postflop 行为完全不同但 EHS 接近** 的 hand 揉到一个 cluster：

- **N=8 C6** = `66, 77, 88, 99, KQs, ATs, AJs, AQs, AKs, AJo, AQo, AKo`（12 类）— set-mining pocket pairs（双峰 equity）与 top-pair big-card（中等 equity）混在一起；OCHS_8 对这类 cluster 求 mean equity 时丢失了 "对手是 set-miner 还是 top-pair player" 的区分
- **N=16 C12** 仍然包含 66, 77 + AK/AQ — N 加大没根治问题

postflop equity 分布形状（histogram）才是真信号。§2.4 给出替代方案。

### 2.4 OCHS warmup table —— postflop-histogram 路径（推荐替代 §2.3）

#### 动机

§2.3 1D-EHS warmup 把 set-mining hand 和 top-pair hand 揉一起，源于只看 mean equity。**equity 分布形状才能区分两者**：

- **set-mining (66-99)** 在大多数 board 上是中等强度，但 ~12% board hit set 后变 monster → hist 中峰 + 高 bin 长尾
- **top-pair (AK/AQ)** 取决于 board 是否出 A/K，hit 时是 strong top pair，没 hit 时是 high-card hand → hist **明显双峰**（低 bin + 高 bin）
- **elite pairs (KK/AA)** 几乎所有 board 都是 overpair → hist 右偏单峰

L2 距离在 8-bin histogram 上能直接捕获这些形状差异。

#### 算法

```rust
// 常量
const OCHS_TRAINING_SEED: u64 = 0x0CC8_5EED_C2D2_22A0;  // 复用既有 seed
const POSTFLOP_WARMUP_BOARD_SAMPLE: u32 = 0x0008_0000;  // 新 op_id (cluster.rs)

pub fn dump_ochs_warmup_postflop_hist(
    n_clusters: u32,
    n_rivers: u32,
    evaluator: Arc<dyn HandEvaluator>,
) -> OchsPostflopWarmupDump {
    let reps = std::array::from_fn(|i| representative_hole_for_class(i));

    // Step 1: per-class equity histogram。rayon 并行 over 169 classes。
    let per_class: Vec<([f64; 8], f64)> = (0..169).into_par_iter().map(|class_id| {
        let rep = reps[class_id];
        let sub_seed = derive_substream_seed(
            OCHS_TRAINING_SEED, POSTFLOP_WARMUP_BOARD_SAMPLE, class_id as u32,
        );
        let mut rng = ChaCha20Rng::from_seed(sub_seed);

        let mut bin_counts = [0u32; 8];
        let mut equity_sum = 0.0;
        for _ in 0..n_rivers {
            let full_board = sample_5_card_board(&rep, &mut rng);  // 不与 rep 重叠
            // Exact equity = enumerate C(45, 2) = 990 opp_hole on this 5-card board
            let me_rank = evaluator.eval7(&[rep, full_board].concat());
            let mut wins_x2 = 0;
            for (i, j) in unused_45.iter_pairs() {  // 990 pairs
                let opp_rank = evaluator.eval7(&[i, j, full_board].concat());
                wins_x2 += compare_x2(me_rank, opp_rank);  // 2/1/0
            }
            let equity = wins_x2 as f64 / (2.0 * 990.0);
            let bin = (equity * 8.0).floor().min(7) as usize;
            bin_counts[bin] += 1;
            equity_sum += equity;
        }
        let hist = bin_counts.map(|c| c as f64 / n_rivers as f64);
        (hist, equity_sum / n_rivers as f64)
    }).collect();

    // Step 2: K-means (L2) on 169 × 8 histograms
    let features = per_class.iter().map(|(h, _)| h.to_vec()).collect();
    let kmeans_res = kmeans_fit(
        &features, KMeansConfig::default_d232(n_clusters),
        OCHS_TRAINING_SEED, OCHS_WARMUP, OCHS_WARMUP,
    );

    // Step 3: reorder by per-cluster median of EHS_mean (centroid 跟随)
    let ehs_means: [f64; 169] = per_class.iter().map(|(_, e)| *e).collect();
    let (centroids, assignments) =
        reorder_by_ehs_median(kmeans_res.centroids, kmeans_res.assignments, &ehs_means);

    // Step 4: inverted index
    ...
}
```

**关于距离度量的简化**：8-bin histogram 应该用 EMD（Wasserstein-1）距离反映 bin 间 ordinal 关系，但 EMD k-means 的 centroid update 在 1D fixed-bin 下与 L2 等价（per-bin 算术均值就是 1D Wasserstein barycenter）；assignment step 用 EMD vs L2 在 169 个点的小数据集上经验差异有限。当前实现走 L2 全路径，未来 stage 升级若发现 EMD 边际显著可换距离函数（cluster.rs::emd_1d_unit_interval 已实现）。

#### Dump 工具用法

```bash
cargo run --release --bin ochs_warmup_dump -- --mode postflop-hist \
    --n-clusters 8 --n-rivers 1000 > artifacts/ochs_warmup_postflop_8_n1000.json
cargo run --release --bin ochs_warmup_dump -- --mode postflop-hist \
    --n-clusters 16 --n-rivers 1000 > artifacts/ochs_warmup_postflop_16_n1000.json
```

Wall（vultr 4-core）：
- n_rivers=1000：**~1.5s**
- n_rivers=10000：**~14.6s**（noise floor 检查，与 n=1000 比 cluster 划分基本一致 → n=1000 已足够稳定）

vultr / 本地跨架构 byte-equal 验证通过（OCHS_TRAINING_SEED + POSTFLOP_WARMUP_BOARD_SAMPLE sub-stream 派生协议 deterministic）。

#### N=8 postflop-hist clustering（真值；n_rivers=10000）

| cluster | size | EHS_mean 区间 | centroid_ehs | hist [bin0..7] 形状 | 代表成员 |
|---|---|---|---|---|---|
| C7 (top) | 6 | [0.721, 0.854] | 0.781 | `.00 .00 .00 .02 .09 .26 .36 .27` 右倾单峰 | 99, TT, JJ, QQ, KK, AA |
| C6 | 5 | [0.570, 0.695] | 0.628 | `.01 .01 .06 .16 .30 .21 .07 .17` 中心峰 + 右尾 | **44, 55, 66, 77, 88** (set-mining) |
| C5 | 50 | [0.498, 0.671] | 0.579 | `.01 .07 .20 .19 .10 .09 .19 .16` 弱双峰 | 22, 33, QJs, K2s-KQs, A2s-AKs, KQo, AJo, AQo, AKo, ... |
| C4 | 17 | [0.512, 0.591] | 0.543 | `.04 .17 .19 .06 .06 .18 .14 .17` | T8s, T9s, J8s, J9s, JTs, Q7s-QTs, T9o, JTo, ... |
| C3 | 35 | [0.417, 0.538] | 0.478 | `.07 .24 .15 .09 .12 .13 .08 .13` | T2s-T7s, J2s-J6s, Q2s-Q5s, K2s-K3s, T8o, ... |
| C2 | 16 | [0.385, 0.514] | 0.449 | `.19 .21 .06 .06 .13 .14 .06 .14` | 87s, 92s-98s, 87o, 98o, ... |
| C1 | 14 | [0.372, 0.464] | 0.418 | `.33 .10 .04 .08 .17 .10 .05 .14` | 65s, 75s, 76s, 82s-86s, 76o, 82o-86o, ... |
| C0 (bottom) | 26 | [0.326, 0.418] | 0.384 | `.40 .04 .04 .13 .16 .06 .03 .13` 左倾单峰 + 中尾 | 32s-72s, 32o-72o（trash） |

观察：
- **C6 = 44, 55, 66, 77, 88** —— set-mining pocket pairs 完全单独成簇，hist 中心峰 + 右尾形状区别于 top-pair big-card
- C5 把 22/33 + AK/AQ/AJ + suited Aces 揉到 50 类是 N=8 容量不够细分的妥协；N=16 能进一步分开（见下表）
- C7 把 99-AA 6 个 premium pairs 合一起，比 1D-EHS C7 (TT-AA 5 类) 多了 99 —— hist 形状角度 99 与 TT/JJ 同类（overpair candidate）

#### N=16 postflop-hist clustering（真值；n_rivers=10000）

| cluster | size | EHS_mean 区间 | centroid_ehs | hist [bin0..7] 形状 | 代表成员 |
|---|---|---|---|---|---|
| C15 (top) | 2 | [0.826, 0.854] | 0.829 | `.00 .00 .00 .01 .04 .11 .47 .36` | KK, AA |
| C14 | 4 | [0.721, 0.800] | 0.758 | `.00 .00 .01 .03 .11 .34 .30 .22` | 99, TT, JJ, QQ |
| C13 | 3 | [0.631, 0.695] | 0.660 | `.00 .01 .04 .10 .32 .26 .10 .18` 中心峰 + set 尾 | **66, 77, 88** (set-mining) |
| C12 | 10 | [0.615, 0.671] | 0.635 | `.00 .04 .17 .17 .09 .07 .23 .23` **显著双峰** | **KJs, KQs, ATs, AJs, AQs, AKs, KQo, AJo, AQo, AKo** (top-pair big-card) |
| C11 | 9 | [0.578, 0.628] | 0.596 | `.00 .03 .18 .24 .09 .12 .20 .14` | A6s, A7s, A8s, A9s, A6o-A9o, ATo |
| C10 | 2 | [0.570, 0.602] | 0.580 | `.02 .02 .09 .26 .27 .13 .04 .17` | **44, 55** (low set-miners) |
| C9 | 8 | [0.543, 0.596] | 0.570 | `.00 .05 .18 .26 .12 .06 .18 .14` | **A2s-A5s, A2o-A5o** (wheel suited / offsuit Aces) |
| C8 | 23 | [0.503, 0.620] | 0.557 | `.01 .11 .22 .14 .08 .11 .18 .15` | QTs, QJs, K2s-KQs, KTo-KJo, Q9o-QJo, ... |
| C7 | 20 | [0.494, 0.577] | 0.531 | `.04 .19 .19 .05 .07 .18 .12 .16` | T8s, T9s, J7s-JTs, Q6s-Q9s, T8o-T9o, JTo, ... |
| C6 | 1 | [0.533, 0.533] | 0.523 | `.02 .03 .19 .31 .20 .05 .03 .16` | **33** |
| C5 | 1 | [0.498, 0.498] | 0.494 | `.03 .05 .30 .25 .15 .04 .02 .16` | **22** |
| C4 | 30 | [0.417, 0.529] | 0.472 | `.07 .24 .15 .09 .12 .12 .08 .13` | T2s-T7s, J2s-J6s, Q2s-Q5s, K2s-K3s, ... |
| C3 | 16 | [0.385, 0.514] | 0.449 | `.19 .21 .06 .06 .13 .14 .06 .14` | 87s, 92s-98s, 87o, 92o-98o, ... |
| C2 | 10 | [0.372, 0.464] | 0.411 | `.32 .12 .04 .09 .16 .10 .05 .13` | 82s-86s, 82o-86o |
| C1 | 15 | [0.380, 0.459] | 0.412 | `.39 .04 .03 .09 .18 .09 .04 .15` | 53s-76s, 54o-76o |
| C0 (bottom) | 15 | [0.326, 0.383] | 0.370 | `.41 .04 .04 .15 .15 .05 .03 .12` | 32s, 42s, 43s, 52s, 62s, 72s, 32o-72o |

观察：
- **C12** = 10 个 top-pair big-card hands（KJs/KQs/ATs+/AJ+/AQ+/AKo+/KQo），histogram **明显双峰** `.00 .04 .17 .17 .09 .07 .23 .23` —— 完美捕获 "hit top pair or busted high card" 二元行为
- **C13** = 66, 77, 88 单独成簇（hist 中心峰 `.32 .26` + set 尾 `.10 .18`），与 C12 top-pair 形状完全不同
- **C14** = 99, TT, JJ, QQ —— overpair pairs
- **C15** = KK, AA —— elite
- **C9 = A2s-A5s + A2o-A5o** 单独成簇（wheel-straight + 弱 kicker），与 A6s+ (C11) 分开
- **C10 = 44, 55** 单独 — 比 66 更弱的 set-miners
- **C6 / C5 = 33 / 22** 各自单独 — 22/33 在 postflop 是 "set-mining 但成功率低" 的特殊形态
- 完全没有 1D-EHS N=16 C12 那种 "66/77 + AK/AQ 混淆"

#### 对比：1D-EHS vs postflop-hist（N=8/16）

| 问题 | 1D-EHS | postflop-hist |
|---|---|---|
| 66/77/88 与 AK/AQ 混合？ | **是**（N=8 C6 全混；N=16 C12 仍混） | **否**（N=8 C6 = 44-88；N=16 C13 = 66-88，C12 = AK/AQ/AJ + KQs/KJs） |
| top-pair big-card 单独成簇？ | 否 | **是**（N=16 C12 = 10 hands 完全 top-pair big-card） |
| wheel Aces 与 high Aces 分开？ | 否 | **是**（N=16 C9 vs C11） |
| premium pair 内部分层？ | 粗（N=8 C7 = TT-AA 5 类） | **细**（N=16 C13/C14/C15 = 88 / 99-QQ / KK-AA 三层） |
| Wall（n_rivers=1000） | 260 ms | 1.5 s（6×） |
| Wall（n_rivers=10000） | 260 ms | 14.6 s（56×） |
| K-means features 维度 | 1D scalar | 8D histogram |

**推荐**：用 **postflop-hist + n_rivers=1000** 作为 OCHS warmup 默认路径。Wall 1.5s 一次性 warmup 成本（vs 1D-EHS 0.26s）对运行时无影响（OCHS table 是 OnceLock 缓存）。1D-EHS dump 保留作历史对照与 schema bump 前的 backward-compat artifact。

#### postflop-hist artifact

| 文件 | size | BLAKE3 |
|---|---|---|
| `artifacts/ochs_warmup_postflop_8_n1000.json` | 20 KB | `4e0d1c67244b864c4ce1211a391667a9880d8c3391b12ac3d5d068bc8a472b96` |
| `artifacts/ochs_warmup_postflop_8_n10000.json` | 20 KB | `8d1cbee99b20ce5992de1ebfa7ab06688788062fd9873c1edc7eedc3764b30c0` |
| `artifacts/ochs_warmup_postflop_16_n1000.json` | 22 KB | `f966972354ab692189a014a2bf0311c522123c926cd7d6a6c8ee8944d67adfda` |
| `artifacts/ochs_warmup_postflop_16_n10000.json` | 22 KB | `36e8738a50bdde19f8adb7e1e1598568d635ffc0e5d4e3fb8319797b8e29f5f7` |

JSON 字段（postflop-hist 模式）：`mode / n_clusters / ochs_postflop_training_seed / n_rivers_sampled / evaluator / representative_hole / class_labels / ehs_mean_per_class / equity_hist_per_class (169×8) / classes_per_cluster / cluster_labels / cluster_centroid_hist (n×8) / cluster_centroid_ehs / cluster_summary`。

#### 落地路径（实现阶段）

当前 `equity.rs::ochs_table()` 仍走 1D-EHS warmup（OnceLock cache）。要让 OCHS feature 路径走 postflop-hist，实现阶段需要：

1. 在 `OchsTable` 加 mode 字段（或新增 `OchsTablePostflop`），让 cache 按 mode + n_clusters + n_rivers 分桶
2. `ochs_table_postflop(n_clusters, n_rivers, evaluator)` 调 `dump_ochs_warmup_postflop_hist` 取 classes_per_cluster
3. `EquityCalculator::ochs` 增加 mode 选项（feature_set_id 升级到 2 = "hist_8 + OCHS_postflop"）
4. BucketTable header 增加 OCHS mode + n_rivers 字段，artifact schema_version 升 → 3

不在本文档承诺，作为 §2 feature 设计落地的前置 PR。

## 3. 距离度量

| 维度组 | 距离 | 理由 |
|---|---|---|
| equity_hist_8 | 1D EMD on [0,1] | bin 之间有 ordinal 关系，L2 把 "bin 0 vs bin 1" 和 "bin 0 vs bin 7" 视为同等差异，EMD 反映概率质量在 equity 轴上的真实位移。`cluster.rs::emd_1d_unit_interval` 已实现。 |
| OCHS_N | L2 | 各 cluster 维度独立无序（D-236b 按 EHS 中位数重编号后有顺序，但 cluster 间不存在 "相邻" 概念）。 |
| 混合 feature | `EMD(hist) + α · L2(OCHS)` | α 决定两组维度的相对权重；先取 α=1 跑 baseline，按 bucket-内 spread 调参。 |

river 全维 L2。

## 4. 不包含的特征及原因

- **EHS² scalar（当前 9 维方案的核心维度）**：equity_hist_8 的二阶矩 ∑ p_k · ((k+0.5)/8)² 严格包含 EHS² 信息，冗余。
- **PPOT / NPOT（Billings Loki/Poki 经典）**：定义为 histogram 在 0.5 阈值的有损投影（PPOT = 落后状态下 mass(Y > 0.5)，NPOT 对称）；hist_8 给出完整 CDF，PPOT/NPOT 可从中算出，反之不可。river PPOT = NPOT = 0 恒成立无信号。
- **Board texture flags（paired / suited / monotone）**：不同 board 落在不同 canonical_observation_id，texture 自然得到不同 equity 分布与 OCHS 向量；离散 flag 与 L2 / EMD k-means 距离语义不匹配。
- **Hand category one-hot（top pair / two pair / set / flush ...）**：equity_hist 的形状已隐含 made-hand 类型——top set 的 hist 集中在 [0.9, 1.0]，bluff catcher 的 hist 集中在 [0.5, 0.7]。one-hot 引入 8-9 个 sparse 维度对 L2 k-means 噪声大。

## 5. 验证

CFR 收敛验收（H3 LBR）之前可做的便宜检查：

1. **per-bucket 内部 equity_hist EMD spread**：每 bucket 取所有成员的 hist 与 bucket centroid hist 的 EMD，统计中位数 / 90 分位。当前 `std_dev < 0.05`（基于 EHS scalar 的标准差）替换成此度量。
2. **per-bucket 内部 OCHS L2 spread**：同上，用 OCHS 向量与 centroid 的 L2 距离。
3. **相邻 bucket 间 EMD**：D-233 阈值 `T_emd = 0.02` 扩展到每个 feature 维度组（hist 维度 EMD、OCHS 维度 L2）。
4. **空 bucket 数**：D-236 不变量，每条街 = 0。

终极判据是 H3 LBR / exploitability — 仅 CFR 收敛后可测。

## 6. 训练规模

每 sample 的 evaluator 调用数（eval7 计数；hero rank 可复用前提下数字减半，本表按朴素 2× 算 upper bound）：

| 街 | N canonical | hist 部分（per sample） | OCHS 部分（per sample） | 单 sample eval7 | 全 N eval7 |
|---|---|---|---|---|---|
| flop | 1,286,792 | 1081 future × C(45,2) × 2 ≈ 2.14 M | 8 × ~21 × C(45,2) × 2 ≈ 333 k | ~2.47 M | ~3.2 × 10¹² |
| turn | 13,960,050 | 46 future × C(45,2) × 2 ≈ 91 k | 8 × ~21 × 46 × 2 ≈ 15 k | ~106 k | ~1.5 × 10¹² |
| river | 123,156,254 | — | 16 × ~11 × 1 × 2 ≈ 339 | ~339 | ~4.2 × 10¹⁰ |

**flop 反而是最贵的街**：hist_8 在每个 future (turn, river) 上要做一次完整 5-card-board equity 计算（C(45, 2) = 990 个 opp_hole enumerate），1081 × 990 × 2 ≈ 2.14 M eval7 / sample 主导成本。river 因 deterministic 1-shot showdown 最便宜。

实际实现需考虑（不在本文档承诺，留给实现阶段决策）：

- flop hist 是否需要 future outcome 抽样（如 200/1081）替代 full enumerate
- hero rank 在 inner 990 opp enumerate 内复用（§E hot path 已有 precompute table 模式）
- OCHS inner 枚举是否需要降到 sample / 多 cluster 共享 inner outcome
- features 是否落盘以解耦 "算特征" 和 "跑 k-means"
- f32 vs f64 内存代价

## 7. 与现状的不变量差异

本设计不与现有 v3 artifact byte-equal：

- feature 维度仍 16 但语义改变（flop/turn 的 dim 0 从 EHS² 变成 hist[0]，river 的 dim 0..7 从 EHS²+OCHS_8 变成 OCHS_16[0..7]）
- k-means 距离从全 L2 改为混合 EMD + L2
- BLAKE3 content hash 必然漂移，新 artifact 是 v4

`feature_set_id` 需要 bump 到 2（v3 是 1 = "EHS² + OCHS_8"）。schema_version 是否同步 bump 由 reader 兼容性决定（centroid 维度仍 9 → 16 → header `n_dims` 需改）。
