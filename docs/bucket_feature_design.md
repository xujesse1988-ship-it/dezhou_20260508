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

#### 169 classes vs 1326 combos

OCHS warmup（§2.3 / §2.4）在 169 个 preflop class 上做聚类（13 pair + 78 suited + 78 offsuit）。但 preflop 的 **1326 = C(52, 2)** 真实手牌空间在每个 class 内有多个 specific suit combos：

| class 类型 | 数量 | combos / class | 例子 |
|---|---|---|---|
| pocket pair | 13 | **6** | AA: AsAh / AsAd / AsAc / AhAd / AhAc / AdAc |
| suited | 78 | **4** | AKs: AsKs / AhKh / AdKd / AcKc |
| offsuit | 78 | **12** | AKo: AsKh / AsKd / AsKc / AhKs / AhKd / AhKc / AdKs / AdKh / AdKc / AcKs / AcKh / AcKd |
| 合计 | 169 | **avg 7.84** | 13×6 + 78×4 + 78×12 = 1326 |

**关键**：preflop EHS 在 class 内 suit-invariant（deck 旋转对称），warmup 用 169 class 不丢信息；**postflop 不是** —— 具体 combo 与 board 的 suit 交互（flush draw、blocker）让同 class 不同 combo 在 fixed board 上 equity 差异显著。

#### 定义（combo 级展开）

OCHS_N 在 runtime feature 阶段 **必须按 combo 展开**：

```
OCHS_N[k] = (1 / |valid_combos(cluster_k, hero, board)|) × Σ {
                  equity_vs_hand(hero, opp_combo, board)
                  : class ∈ cluster_k
                  : opp_combo ∈ combos_for_class(class)
                  : opp_combo 与 hero / board 无 card 重叠
                }
```

冲突剔除策略：board + hero 占 5-7 张 specific cards，每张 card 会让若干 class 内 combos 失效（如 hero 持 As 时 AA 仅剩 3 个有效 combos = AhAd / AhAc / AdAc）。**剔除粒度从 class 改为 combo**：

- 旧（rep 路径）：rep 与 hero/board 冲突 → 整 class 跳过 → cluster k 信号塌缩
- 新（combo 路径）：仅冲突 combo 跳过 → cluster k 仍有大量有效 combos 贡献

#### 实现 pseudo-code

```rust
/// 返回 class_id ∈ 0..169 对应的全部 specific combos（6 / 4 / 12 个）。
fn combos_for_class(class_id: u8) -> Vec<[Card; 2]> {
    if class_id <= 12 {
        // pocket pair: 6 combos = C(4, 2) suits
        let r = Rank::from_u8(class_id).unwrap();
        let suits = [Suit::Spades, Suit::Hearts, Suit::Diamonds, Suit::Clubs];
        let mut out = Vec::with_capacity(6);
        for i in 0..4 {
            for j in (i+1)..4 {
                out.push([Card::new(r, suits[i]), Card::new(r, suits[j])]);
            }
        }
        out
    } else if class_id <= 90 {
        // suited: 4 combos = 4 suits
        let (high, low) = decode_high_low(class_id - 13);
        let suits = [Suit::Spades, Suit::Hearts, Suit::Diamonds, Suit::Clubs];
        suits.iter().map(|&s|
            [Card::new(Rank::from_u8(high).unwrap(), s),
             Card::new(Rank::from_u8(low).unwrap(), s)]
        ).collect()
    } else {
        // offsuit: 12 combos = 4 × 3 distinct suit pairs
        let (high, low) = decode_high_low(class_id - 91);
        let suits = [Suit::Spades, Suit::Hearts, Suit::Diamonds, Suit::Clubs];
        let mut out = Vec::with_capacity(12);
        for &s1 in &suits {
            for &s2 in &suits {
                if s1 != s2 {
                    out.push([Card::new(Rank::from_u8(high).unwrap(), s1),
                              Card::new(Rank::from_u8(low).unwrap(), s2)]);
                }
            }
        }
        out
    }
}

fn ochs_n(hero: [Card; 2], board: &[Card], evaluator: &dyn HandEvaluator, n: u8) -> Vec<f64> {
    let table = ochs_table(n, evaluator);
    let mut out = Vec::with_capacity(n as usize);
    for cluster_id in 0..(n as usize) {
        let mut sum = 0.0;
        let mut count = 0u32;
        for &class_id in &table.classes_per_cluster[cluster_id] {
            for opp_combo in combos_for_class(class_id) {
                if pair_overlaps(&hero, &opp_combo) || any_overlaps_board(&opp_combo, board) {
                    continue;
                }
                sum += equity_vs_hand(hero, opp_combo, board, evaluator);
                count += 1;
            }
        }
        out.push(if count > 0 { sum / count as f64 } else { 0.5 });
    }
    out
}

// equity_vs_hand 不变（按 board.len() 走 1-shot showdown / 46 / 990 enumerate）。
```

`equity_vs_hand` 实现（按 `board.len()` 分支）：

```rust
fn equity_vs_hand(hero: [Card; 2], opp: [Card; 2], board: &[Card], eval: &dyn HandEvaluator) -> f64 {
    match board.len() {
        5 => {  // river: 1-shot showdown，2 次 eval7
            let me = eval.eval7(&[hero[0], hero[1], board[0..5]].concat());
            let op = eval.eval7(&[opp[0], opp[1], board[0..5]].concat());
            if me > op { 1.0 } else if me == op { 0.5 } else { 0.0 }
        }
        4 => {  // turn: enumerate 46 river outcomes
            let used = build_used_set(&hero, &opp, board);
            let mut wins_x2 = 0u64; let mut count = 0u64;
            for c in (0..52).filter(|c| !used[*c as usize]) {
                let full_board = [board[0], board[1], board[2], board[3], Card::from_u8(c)];
                wins_x2 += compare_x2(
                    eval.eval7(&[hero[0], hero[1], full_board[..]].concat()),
                    eval.eval7(&[opp[0], opp[1], full_board[..]].concat()),
                );
                count += 1;
            }
            wins_x2 as f64 / (2.0 * count as f64)
        }
        3 => {  // flop: enumerate C(45, 2) = 990 (turn, river) outcomes
            // 同 turn 但双层循环
        }
        _ => unreachable!(),
    }
}
```

#### 为什么需要 combo 展开 —— suit 交互举例

数值方向约定：OCHS_N[k] = **hero** 在 opp 范围下的 equity（formal definition §2.2 + `equity_vs_hand` 实现 `me > op → 1.0`）。下表所有 equity 数值都是 **hero 视角**，不是 opp 视角。

**例 1**：board = `Ts 9s 8s`（monotone flop），hero = `Jd Jc`，opp class = `AKs`（4 combos）：

| opp_combo | hero equity vs opp_combo | 解读 |
|---|---|---|
| AsKs | ~0.22 | opp 有 nut flush draw + 2 overcards，hero 是 dog |
| AhKh | ~0.55 | opp 只剩 2 overcards |
| AdKd | ~0.55 | 同上 |
| AcKc | ~0.55 | 同上 |
| **mean** | **0.47** | 真实 "vs AKs class hero equity" |

旧 rep 路径用 AsKs，OCHS_N[k] = 0.22；真实 mean = 0.47 → **低估 hero 0.25**，等价于 "严重高估 AKs 这类对手的威胁"。

**例 2**：board = `Ts 9s 8s`（同上），hero = `As Ah`：

| opp_combo of AKs class | 状态 | hero equity |
|---|---|---|
| AsKs | conflict（As 已被 hero 占）→ 跳过 | — |
| AhKh | conflict（Ah 被 hero 占）→ 跳过 | — |
| AdKd | hero overpair AA + nut flush draw（As blocker），opp Ax 顶 blocked | ~1.0 |
| AcKc | 同上 | ~1.0 |

旧 rep 路径：rep = AsKs，与 hero 冲突 → **整 AKs class 在该 cluster 内被跳过** → cluster 信号塌缩到非 AKs 类成员，方差爆炸。
新 combo 路径：剩 2 个有效 combos hero equity 平均 ≈ 1.0，正确反映 "vs AKs 我打爆"。

#### 性能注解

- 平均 combos / class = 7.84；剔除与 hero/board 冲突后实际生效 ~5-7 combos / class（依 board / hero suit composition）
- per OCHS_N call 内 `equity_vs_hand` 调用数从 "~21 (N=8) / ~11 (N=16) class" 提升到 "~165 / ~83 combo"（约 6-8×）
- N=8 / N=16 OCHS warmup table 仍走 169-class（preflop suit-invariant，warmup 不需 combo 级），OnceLock 缓存不变
- `combos_for_class` 输出确定性，可在 process startup 预计算成 `[Vec<[Card; 2]>; 169]` static table 避免每次 OCHS call 重算（属于实现优化）

#### 含义

对每个对手 "假想范围" 给出 hero 的相对强度。**combo 展开确保 suit-interactive board（monotone / two-tone / paired，占 ~60% postflop 状态）下 OCHS 分量正确**。river N 提高到 16 因为 river 没有 hist 维度，需要更多 OCHS 粒度填充信息空间，且 river deterministic 计算便宜（每 combo 仅 2 次 eval7）。

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

**关于距离度量的简化**：8-bin histogram 概念上应该用 EMD（Wasserstein-1）距离反映 bin 间 ordinal 关系。但 EMD k-means 的 centroid update 在 fixed-bin 1D histogram 上**没有闭式解**（per-bin 算术均值 ≠ 1D Wasserstein barycenter，后者要 quantile averaging，是个常见误区）。当前实现走 **L2 全路径作为 baseline**，是 heuristic 选择，不是数学等价。后续 stage 若发现 169 个点上 EMD vs L2 cluster 划分显著不同，再实现完整 EMD assignment + 真 Wasserstein barycenter centroid update（`cluster.rs::emd_1d_unit_interval` 已实现 distance 函数，barycenter 需新增 + 新 DistanceMetric 分支）。

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

实现阶段把 `equity.rs::ochs_table()` 的 1D-EHS warmup 路径**直接替换**为 postflop-hist：

1. `OchsTable.classes_per_cluster` 由 `dump_ochs_warmup_postflop_hist(n_clusters, n_rivers, evaluator)` 生成；OnceLock 缓存 key 升级为 `(n_clusters, n_rivers)`
2. `EquityCalculator::ochs` 内层循环改 combo 级（§2.2），`representative_hole_for_class` 改名为 `combos_for_class` 返回 `Vec<[Card; 2]>`
3. n_rivers 默认 1000（warmup wall ~1.5s on vultr 4-core）

`equity.rs::build_ochs_table` + `representative_hole_for_class` 旧 1D-EHS rep-level 路径可整体删除（不再被任何 caller 引用），dump 工具 `--mode ehs` 保留作 doc §2.3 数据复现 + algorithm validation。

## 3. 距离度量

| 维度组 | 距离 | 理由 |
|---|---|---|
| equity_hist_8 | 1D EMD on [0,1] | bin 之间有 ordinal 关系，L2 把 "bin 0 vs bin 1" 和 "bin 0 vs bin 7" 视为同等差异，EMD 反映概率质量在 equity 轴上的真实位移。`cluster.rs::emd_1d_unit_interval` 已实现。 |
| OCHS_N | L2 | 各 cluster 维度独立无序（D-236b 按 EHS 中位数重编号后有顺序，但 cluster 间不存在 "相邻" 概念）。 |
| 混合 feature | `EMD(hist) + α · L2(OCHS)` | α 决定两组维度的相对权重；先取 α=1 跑 baseline，按 bucket-内 spread 调参。 |

river 全维 L2。

**实现注意**：混合距离不是 `cluster.rs::kmeans_fit_production` 的小改 —— 现有实现是 L2 全路径（assignment + centroid update 都假设 Euclidean）。EMD assignment 要新 distance 函数；EMD centroid update 在 fixed-bin 1D histogram 上没有闭式解（per-bin 算术均值不是 Wasserstein barycenter）。落地需在 kmeans 内引入 `DistanceMetric { L2, MixedEmdL2 { hist_dims, alpha } }` 新分支，包含独立的 assignment 与 centroid update 实现。Stage 实施初期先走 L2 全路径出 baseline bucket table；EMD 分支作为后续 distance-metric ablation 加入，不阻塞 Stage 1。

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

## 6. 训练流水线

训练分两阶段：**Stage 1 算特征 → 落盘二进制文件；Stage 2 读盘 → k-means → bucket table**。

### 6.1 设计动机

既有实现（`bucket_table.rs::build_bucket_table_bytes`）单进程一次性完成 "算特征 + 跑 k-means + 写 artifact"。flop / turn / river 三街顺序串行，全 N enumerate 算特征是主导成本（§6.3 估算 flop alone ~5.3×10¹² eval7）。

问题：

- K-means 调参（K 值、距离度量、初值 seed）每次都要重算 ~12 小时特征，无法快速迭代
- features `Vec<Vec<f64>>` 内存峰值 river ~12 GB，需要大内存主机
- 单进程失败 → 全部重来；中途机器故障无法续跑

Two-stage 分离：

- **Stage 1**：feature 计算昂贵但**一次性**。三街独立 N × 16 dim → 三个 `.bin` 文件。
- **Stage 2**：feature mmap 读取，k-means + reorder + 量化 → bucket table。K-means 内存峰值 ~K × dim + thread-local accumulator，**与 N 解耦**。

ROI：

- 同一份 feature file 可跑多组 K（500 / 1000 / 200）/ 距离（L2 / EMD / mixed-α）/ reorder 策略 → bucket quality 验证 §5 spread 指标可在 ~30 min 内迭代一次（vs 12h）
- Stage 2 可在普通主机跑（不需要 32 vCPU + 64 GB）
- Stage 1 中断可按 chunk 续跑（§6.5）

### 6.2 Stage 1：特征文件生成

CLI：`tools/bucket_features_dump.rs`

```bash
cargo run --release --bin bucket_features_dump -- \
    --street flop  --output artifacts/features_flop.bin
cargo run --release --bin bucket_features_dump -- \
    --street turn  --output artifacts/features_turn.bin
cargo run --release --bin bucket_features_dump -- \
    --street river --output artifacts/features_river.bin
```

三街独立可并行起三个进程。

**算法**：

```rust
for canonical_id in 0..N(street) {
    let (board, hole) = canonical_enum::nth_canonical_form(street, canonical_id);
    let feature: [f32; 16] = match street {
        Flop | Turn => {
            let hist = equity_hist_8(hole, &board, evaluator);  // §2.1
            let ochs = ochs_n(hole, &board, evaluator, 8);       // §2.2 combo 级
            concat(hist, ochs)  // 8 + 8 = 16
        }
        River => {
            ochs_n(hole, &board, evaluator, 16)                  // §2.2 combo 级
        }
    };
    write_at_offset(file, canonical_id, feature);  // row-major
}
```

rayon par_iter over canonical_id，按 chunk 收集后顺序写盘（保证 byte-equal 不依赖线程数，与 cluster.rs::kmeans_fit_production 同型）。

**文件格式**（per-street `features_<street>.bin`）：

```text
=== header (80 bytes, 16-byte aligned) ===
offset 0x00: magic: [u8; 8] = b"PLBKFEAT"
offset 0x08: schema_version: u32 LE = 1
offset 0x0c: street: u32 LE       (0 = flop, 1 = turn, 2 = river)
offset 0x10: n_canonical: u32 LE  (1_286_792 / 13_960_050 / 123_156_254)
offset 0x14: n_dims: u32 LE = 16
offset 0x18: dtype: u32 LE = 0    (0 = f32 LE; 1 = f64 LE; 其他保留)
offset 0x1c: feature_layout: u32 LE
                                  (0 = hist_8 || OCHS_8;
                                   1 = OCHS_16;
                                   其他保留)
offset 0x20: ochs_warmup_blake3: [u8; 32]
                                  (BLAKE3 of artifacts/ochs_warmup_postflop_<N>_n<R>.json)
offset 0x40: ochs_n_rivers: u32 LE        (warmup 时的 n_rivers 参数)
offset 0x44: ochs_n_clusters: u32 LE      (warmup 时的 n_clusters 参数；
                                           flop/turn = 8，river = 16)
offset 0x48: pad: [u8; 8] = 0             (使 header 对齐到 0x50 = 80 字节)

=== body (n_canonical × n_dims × 4 bytes, row-major f32 LE) ===
sample i 的 16 维特征从 offset 80 + i × 64 起，连续 64 bytes。
sample 顺序与 canonical_enum::nth_canonical_form(street, i) 一致。

=== trailer (32 bytes) ===
offset (file_len - 32): blake3: [u8; 32] = BLAKE3(file[..file_len - 32])
```

文件大小（f32 dtype）：

| 街 | N canonical | body size | 总 size |
|---|---|---|---|
| flop | 1,286,792 | 82 MB | ~82 MB |
| turn | 13,960,050 | 894 MB | ~894 MB |
| river | 123,156,254 | 7.88 GB | ~7.88 GB |
| 合计 | 138,403,096 | 8.84 GB | **~8.84 GB** |

f32 精度（24-bit mantissa）远超下游 u8 centroid 量化精度（~0.004），不损失。

`ochs_warmup_blake3` 字段让 Stage 2 能校验 "用同一 OCHS warmup artifact 算的特征"，防止 warmup 漂移导致特征语义不一致。

### 6.3 Stage 2：K-means + bucket table 生成

CLI：`tools/bucket_kmeans_fit.rs`

```bash
cargo run --release --bin bucket_kmeans_fit -- \
    --feature-flop  artifacts/features_flop.bin  \
    --feature-turn  artifacts/features_turn.bin  \
    --feature-river artifacts/features_river.bin \
    --bucket-flop  500 \
    --bucket-turn  500 \
    --bucket-river 500 \
    --training-seed 0xcafebabe \
    --output artifacts/bucket_table.bin
```

**算法**（per street）：

```rust
let mmap = open_feature_file(path);  // 校验 magic + blake3 + ochs_warmup_blake3
let features: &[[f32; 16]] = mmap.body_as_rows();

// 1. K-means (cluster.rs::kmeans_fit_production，改入参为 &[[f32; 16]])
let kmeans_res = kmeans_fit_production(features, KMeansConfig::default(K),
                                        training_seed, init_op, split_op);

// 2. 距离度量：Stage 1 实施先走 L2 baseline（全街）。
//    mixed EMD+L2 (flop/turn) 需要新增 DistanceMetric 分支 + EMD centroid
//    update（fixed-bin 1D histogram 上无闭式解，不是 per-bin 算术均值），
//    列为后续 ablation 实验，不阻塞 Stage 1 出 bucket table。

// 3. reorder by EHS median（centroid hist 中心质量 / OCHS mean）
let (centroids, assignments) = reorder_by_ehs_median(...);

// 4. centroid u8 量化
let (q_centroids, min, max) = quantize_centroids_u8(&centroids);

// 5. lookup_table[canonical_id] = assignments[canonical_id]
let lookup_table: Vec<u32> = assignments.into_iter().map(|x| x as u32).collect();
```

三街独立处理，最终合并写一个 bucket_table.bin（schema 见 §7）。

bucket_table.bin header 增加引用 feature file 的 BLAKE3 chain：

```
offset 0x58: feature_flop_blake3:  [u8; 32]
offset 0x78: feature_turn_blake3:  [u8; 32]
offset 0x98: feature_river_blake3: [u8; 32]
```

让 bucket_table → features → ochs_warmup 形成可验证 hash chain。

### 6.4 训练规模估算

**Stage 1（特征计算）**，每 sample eval7 调用数（hero rank 复用前 upper bound，OCHS 按 §2.2 combo 级展开，avg ~6 effective combos/class 扣除冲突后）：

| 街 | N canonical | hist 部分 | OCHS 部分 | 单 sample eval7 | 全 N eval7 |
|---|---|---|---|---|---|
| flop | 1,286,792 | 1081 future × C(45,2) × 2 ≈ 2.14 M | 8 × ~21 × ~6 × C(45,2) × 2 ≈ 2.0 M | ~4.14 M | ~5.3 × 10¹² |
| turn | 13,960,050 | 46 future × C(45,2) × 2 ≈ 91 k | 8 × ~21 × ~6 × 46 × 2 ≈ 93 k | ~184 k | ~2.6 × 10¹² |
| river | 123,156,254 | — | 16 × ~11 × ~6 × 1 × 2 ≈ 2.1 k | ~2.1 k | ~2.6 × 10¹¹ |
| 合计 | — | — | — | — | **~8.2 × 10¹²** |

**Stage 1 wall**（@ ~50 ns/eval7，32-vCPU 主机）：

- flop：5.3 × 10¹² / 32 / 50 ns ≈ 3300 s ≈ **~55 min**
- turn：2.6 × 10¹² / 32 / 50 ns ≈ 1625 s ≈ **~27 min**
- river：2.6 × 10¹¹ / 32 / 50 ns ≈ 163 s ≈ **~3 min**
- 串行总 wall：**~85 min**；三街并发可降到 **~55 min**（flop bound）

**Stage 2（K-means）**，每 iter 成本（K=500，dim=16）：

| 街 | N | per-iter assignment ops | per-iter centroid update ops | 总 ops / iter |
|---|---|---|---|---|
| flop | 1.28 M | N × K × d = 1.0 × 10¹⁰ | N × d = 2.0 × 10⁷ | ~10¹⁰ |
| turn | 14 M | 1.1 × 10¹¹ | 2.2 × 10⁸ | ~10¹¹ |
| river | 123 M | 9.8 × 10¹¹ | 2.0 × 10⁹ | ~10¹² |

@ 1 ns/op + 32-vCPU rayon，river 单 iter ~30 s；典型 30 iter 收敛 → river ~15 min。flop/turn 各 ~1-2 min。

**Stage 2 总 wall**：~20 min for all 3 streets。可在 vultr 4-core 主机跑（~80 min），调参不阻塞 Stage 1。

### 6.5 实现要点

- **byte-equal**：Stage 1 输出文件给定 (canonical_enum, OCHS warmup, evaluator) 跨架构 byte-equal（与 stage 1 cross_arch baseline 同型）。Stage 2 输出 bucket table 给定 (feature file BLAKE3, training_seed, K) 也跨架构 byte-equal。
- **hero rank 复用**：hist / OCHS 内 990 opp_hole enumerate 共享 hero eval7 结果（§E hot path 模式），实测 ~2× speedup
- **不做 "rainbow non-paired board combo 塌缩" 优化**：rainbow board 上每种花色出现 1 次，suited opp combos 各自有 backdoor flush draw；叠加 hero 手牌 suit blocker 后，同 class 内 combos 在 equity 上仍有差异，不严格等价。要做塌缩需严格 suit-orbit 分组（按 hero suit / board suit composition 等价类）。当前 stage 以 full combo enumerate 为正确性 baseline，优化留待 suit-orbit 实现
- **`combos_for_class` static precompute**：进程启动时算一次 `[Vec<[Card; 2]>; 169]` 表，每次 OCHS call O(1) 查表
- **Stage 1 续跑**：feature file 按 chunk_size = 200_000 canonical 分段写入临时文件 `features_<street>.bin.part<chunk_idx>`；全部完成后 concat + 写 header / trailer。中断时 enum 已存在的 part，从 next missing chunk 继续
- **Stage 2 mmap**：feature file 用 `memmap2` 只读 mmap，进程内存峰值 = K × dim × f64 (centroids) + chunk × dim × f64 (accumulator)（~MB 级，与 N 解耦）。注：项目 D-275 `unsafe_code = "forbid"`，mmap 需走 `std::fs::read` 整段加载 → river 7.88 GB 内存。如内存预算紧张，Stage 2 改为 chunk-based 顺序读（Vec<u8> + 流式解析），代价是无法 mmap 跨进程共享
- **dtype 选 f32**：精度 24-bit 远超 u8 centroid 量化精度 ~8-bit；文件大小相对 f64 减半（17.68 GB → 8.84 GB），相对 u8 大 ~4×（8.84 GB vs 2.21 GB）但避免 quantization 噪声进入 k-means convergence

## 7. 新 artifact schema

本设计是 bucket table 的新实现，**不保留与既往 artifact 的兼容性**。原 BucketTable artifact 格式（`bucket_table.rs` 当前实现）整体重写，无 reader 双路径 / feature_set_id 多版本 / mode 字段开关。

新 artifact 在 `bucket_table.rs` 头部承诺：

```
feature semantics:
  flop  = equity_hist_8 (over 1081 future outcomes) + OCHS_8  = 16 dim
  turn  = equity_hist_8 (over 46 future outcomes)   + OCHS_8  = 16 dim
  river = OCHS_16                                              = 16 dim

OCHS 实现：
  warmup     = postflop-histogram（§2.4，n_rivers=1000）
  feature    = 1326-combo level expansion（§2.2）

K-means 距离：
  hist 维度 → 1D EMD on [0, 1]
  OCHS 维度 → L2
  flop/turn 混合 → EMD(hist) + α · L2(OCHS)，α=1 baseline

centroid 量化：u8 per-dim min/max（与 bucket_table.rs 既有 centroid_metadata
段同型，n_dims=16）。

BLAKE3 trailer（与既有同型，路径无关）。
```

5 段结构（header / centroid_metadata / centroid_data / lookup_table / trailer）沿用 `bucket_table.rs` D-244 物理布局，但**字段语义不同 → 必须显式 bump `feature_set_id`**。

旧 artifact：`schema_version=1, feature_set_id=1` = EHS² + OCHS_8 = 9 dim。
新 artifact：`schema_version=1, feature_set_id=2` = hist_8 + OCHS_8 / OCHS_16 = 16 dim。

reader 加载时按 `feature_set_id` 分派：

- `feature_set_id=1`：旧 9 维 schema —— 新代码不再支持，reader 直接拒绝（错误 `UnsupportedFeatureSet`，提示用户重新训练）。
- `feature_set_id=2`：新 16 维 schema —— 走本文档定义的解析路径。
- 其他值：reader 拒绝加载。

`bucket_table.rs` 头注释会同步更新到新 feature 语义；旧 schema 注释整体替换。`BUCKET_TABLE_DEFAULT_FEATURE_SET_ID` 常量从 1 改为 2。

新增 BLAKE3 hash chain 字段（追加在原 header 末尾）：

```
offset 0x58: feature_flop_blake3:  [u8; 32]
offset 0x78: feature_turn_blake3:  [u8; 32]
offset 0x98: feature_river_blake3: [u8; 32]
```

让 bucket_table → features_<street>.bin → ochs_warmup_postflop_<N>_n<R>.json → ground truth 形成可验证链。

**校验责任划分**（runtime vs 离线工具）：

| 步骤 | 校验内容 | runtime `BucketTable::open` | 离线工具 `tools/bucket_validate_chain` |
|---|---|---|---|
| 1 | bucket_table 自身 BLAKE3 trailer 匹配（文件未损坏）+ magic + schema_version + feature_set_id | **必查** | 查 |
| 2 | bucket_table 内 feature_<street>_blake3 == features_<street>.bin 实际 BLAKE3 | 不查 | 查 |
| 3 | features_<street>.bin 内 ochs_warmup_blake3 == ochs_warmup artifact 实际 BLAKE3 | 不查 | 查 |
| 4 | ochs_warmup artifact 跨架构 byte-equal（§2.4） | 不查 | 查（dump 工具 byte-equal regression） |

理由：runtime 部署通常只携带 bucket_table.bin 一个文件（features_*.bin 共 ~8.84 GB、ochs_warmup_*.json 不会随部署分发），无法在线做步骤 2-4。runtime 只查文件自身完整性 + schema 兼容；hash chain 完整性由训练机 / CI 上的离线工具承担。

- runtime 步骤 1 任一失败 → `BucketTable::open` 返回错误，进程拒绝加载。
- 离线工具步骤 2-4 任一失败 → 工具返回非 0，CI 失败，artifact 不进 git。
