# 下一代策略引擎设计：多场景 · 加权 range · 多策略

> 一句话：把当前**单一弱场剥削引擎**升级成「**场型选路 + 策略族 + per-opponent 加权 range**」的三层结构，让同一套引擎能打弱场、强场、混合场，并把「对手 range」从一个标量升级成可表达听牌与极化的分布。
>
> 定位：本文是**前瞻设计草案**，不是已实现状态。落地必须逐阶段走「调 config / 加规则 → 跑 `heuristic9_sim` 回归门 → 不退化才上线」的既有纪律（`docs/heuristic9_design.md` §4/§6）。
>
> 配套必读：`docs/heuristic9_strategy.md`（现策略代码级走读）、`docs/heuristic9_design.md`（架构 + 迭代闭环）、`docs/invariants.md`（硬约束）、`docs/status_v3.md`（真实状态）。
>
> 工作语言：中文散文 + 英文 code identifier；所有结论带 `file:line`。

---

## 0. 缘起与目标

### 0.1 现状的两个根本局限

**局限一：只有一套打法，且只针对弱场。** 引擎的设计目标白纸黑字写着「专门对付弱对手场」而非「不可被剥削」（`heuristic9_strategy.md:11`）。它把弱场假设焊进了代码，最典型的是 `aggressor_size_tighten`（`policy.rs:73`）——「大注 = 价值、少诈唬 → 收紧对手 range」（`policy.rs:70` 注释自认这是刻意剥削假设、非 GTO）。碰上会平衡的强对手，这条假设会被**反向剥削**（对手用 polar 大注把 hero 打弃）。

**局限二：对手 range 是一个 board-blind 的标量。** `build_opp_thresholds`（`policy.rs:53`）对每个对手只输出一个 `thr`（最低起手强度分位），`equity()` 再硬过滤 `strength[canonical_index] >= thr`（`equity.rs:176`）。三个硬伤：

1. **board-blind**：`strength[idx]` 是 `preflop_equity_vs_random`（`equity.rs:326`），与当前公共牌无关。`76s` 在 `9♠8♦2♣` 上是两头顺听，但它进不进 range 只取决于翻前强度是否 ≥ `thr` —— 翻前 `76s ≈0.40 < thr≈0.57`，**永远被滤掉**，听牌从不被当听牌建模。
2. **恒线性**：range 永远从坚果向下取（`threshold_for_top`，`equity.rs:132-139`），无法表达「坚果 + 诈唬、抽掉中间」的**极化**形态；翻后动作只能把标量拧紧/放松，从不重塑构成。
3. **表达力不足**：一个标量 + 一个 profile 位移无法区分「同样 open，但 A 是 capped-linear、B 是 polar」的两个对手。

这两条其实是**同一个设计取舍的两面**：正因为假定对手是弱场、不做平衡，才敢用「标量分位 + 大注一律收紧」这种粗近似。要支持强场，就必须同时升级 range 模型。

### 0.2 三条目标

| # | 目标 | 对应章节 |
|---|---|---|
| ① | **加权 range**：每个对手一个 169 类权重向量，翻前设先验 + 翻后按下注/board 贝叶斯重加权（听牌、polar 自然进 range） | §2 |
| ② | **多策略**：弱/强/混合场支持，`Strategy` 抽象 + 场型选路，按场景选打法 | §3 |
| ③ | **系统优化**：除①②外的全部优化机会，按 P0/P1/P2 排 | §4 |

### 0.3 贯穿全文的四条红线

- **默认逐字节等于现基线**：任何默认路径改动，在「默认 config + 关剥削」下必须 mbb/g byte-equal、重建漂移 0.000%、fallback 0（延续 v0.2–v0.4 门控纪律，`heuristic9_design.md:229`）。
- **座位无关**：所有新逻辑 `num_seats` 驱动、逐活跃座循环（照 `policy.rs:78`），2–9 人零改（`invariants.md` §6）。唯一例外是 blueprint 查表（座数特定），必须门控回落。
- **永不犯大错**：`safe_fallback`（`policy.rs:201`）/ `legalize`（`policy.rs:158`）作为所有策略共享的地板。
- **sim 回归门是唯一裁决者**：教科书正确 ≠ 本场 +EV（`Limper`、对子面限深两个改动都被门驳回，`heuristic9_design.md:184/238`）。

---

## 1. 全局架构：三块怎么拼在一起

三项改动不是并列的，它们有清晰的依赖与协同：

```
                        ┌─────────────────────────────────────────────┐
  Request → GameView →  │  safe_fallback（无手牌兜底）                   │
                        │        ↓                                      │
                        │  select_strategy(view, opp, cfg)   ← §3 场型选路│
                        │        ↓ 分派                                  │
                        │  ┌── Exploitative（现打法，弱场）              │
                        │  ├── BalancedHeuristic（强场，需 polar range）─┼──┐
                        │  └── Mixed = 逐对手门控（非第三套策略）        │  │
                        │        ↓ 每个策略内部建对手 range              │  │
                        │  build_opp_ranges → Vec<RangeVec>  ← §2 加权 range│
                        │        ↓                                      │  │
                        │  equity_weighted(&[RangeVec])      ← §2 加权采样│
                        │        ↓                                      │  │
                        │  legalize（合法性地板）                        │  │
                        └─────────────────────────────────────────────┘  │
                                                                          │
   §2 的 polar range 正是 §3 BalancedHeuristic 的地基 ────────────────────┘
```

**关键协同（三个 agent 独立收敛到的同一处）**：

- **§2（加权 range）是 §3（Balanced 策略）的地基**。B 的 `BalancedHeuristic` 要打强场，核心就是「对手大注 range 是 polar（坚果 + 诈唬）」——这正是 §2 的 `RangeVec` 贝叶斯似然自然产出的东西。没有 §2，Balanced 只能在 linear-top range 上硬凑。
- **行动线记忆（§4 P0-1）是①②的共同前置**。§2 §9 想「按每对手逐动作序列更新 range」、§3 想「判 villain 会不会下注以决定 check-raise」、§3 想「识别强 reg」，全都需要 `GameView` 保留整手下注线——而现在它只留当前街 aggressor（`view.rs:520`）。
- **per-street 对手统计（§4 P0-3）是 §3 场型检测的命根**。§3 最大盲点是「用现有 VPIP/PFR/AF 判不出强 reg」，而 P0-3 要补的 fold-to-cbet / 3bet% / 摊牌亮诈率正是判「强」所需的信号。
- **`aggressor_size_tighten` 的可剥削性（§4 P2-3）会被 §2 吸收**：新 range 模型里「大注 → 更 polar」由似然参数涌现，不再是全局常数收紧。

依赖矩阵见 §4 末与 §5 路线图。

---

## 2. 对手 Range 重构：从标量 `thr` 到 169 类权重向量 `RangeVec`

> 替换 `build_opp_thresholds`（`policy.rs:53`）的单标量模型，改为**每个对手一个 169 类归一化权重向量**，喂给 `equity()`（`equity.rs:145`）加权采样。`EquityEngine` 强度表本身不改（`EquityEngine::new`，`equity.rs:88`）。

> **落地状态（2026-07-02，为 opponent_range_ml M3 服务先落一半）**：§2.2 翻前模板先验 +
> §2.3 逐动作贝叶斯算子已移植进 **`src/heuristic/range.rs`**（`RangeModel::{prior,op,
> class_axes,causal_logpriors}`），并对 Python 参考（`range_baselines.B2RangeVec`）**golden
> 对拍字节级通过**（`tests/heuristic_range_parity.rs`，800 快照 max|Δ|=1.1e-16）。配套
> `EquityEngine::from_strength` 载入 `strength169.json`（Rust 现算 MC 与 Python MT 必不同，
> 故载表对齐）。**续（同日）**：§2.1 `RangeVec` + §2.5 `equity_weighted`（加权枚举→CDF
> →`rng.unit()` 二分，degenerate 对 `equity` max|Δ|=0.011，`tests/heuristic_equity_weighted.rs`）、
> §2.7 `RangeConfig` + `decide` 分流（`equity_dispatch`）、**在线 ML range**（`GameView::
> to_range_sample`→tokenizer→`MlRange::predict`→`RangeVec`→`equity_weighted` 接进决策，
> `use_ml_range` 默认 false，smoke 通）均已落地；P0-1 行动线（`view.rs`）备 §2.9 喂料。
> **剩**：远端 sim A/B 验 EV 才翻默认；画像 fidelity（`OpponentTable`→prof）；启发式版
> `build_opp_ranges`（`use_range_vec`，用 `RangeModel` 先验/算子而非 ML）尚未接 `decide`
> （ML 路径先行）。**注：owner 已解除本引擎「默认 byte-equal」纪律（§2.6），改以 sim/live
> EV 为准。**

### 2.1 数据结构

```rust
/// 单个对手在当前决策点的 range：169 类的归一化质量分布。
/// 下标与 canonical_index（equity.rs:57）同轴：pair 0..12 / suited 13..90 / offsuit 91..168。
#[derive(Clone)]
pub struct RangeVec {
    /// mass[idx] = 该 169 类整体占对手 range 的概率质量。∑mass == 1。
    /// combo 权重约定沿用 pair=6 / suited=4 / offsuit=12（equity.rs:100-106）。
    pub mass: [f64; 169],
}
```

`mass[idx]` = 对手拿着 idx 这一类中任意一个 combo 的总概率；采样时再平摊到该类去 dead 后存活的具体 combo 上（§2.5）。

**迁移锚点**——把旧标量精确复刻成硬 membership 向量：

```rust
impl RangeVec {
    pub fn from_threshold(thr: f64, eq: &EquityEngine) -> RangeVec {
        let mut m = [0.0; 169];
        for idx in 0..169 {
            if eq.strength[idx] >= thr { m[idx] = combo_weight(idx) as f64; }
        }
        normalize(&mut m); RangeVec { mass: m }
    }
}
```

**成本**：169×8B = 1352 B/对手，≤8 对手 ~11 KB（对比当前 8 B，可忽略）。

### 2.2 翻前先验（替换 frac 桶 0.08/0.17/0.34/0.55/0.80）

保留 `build_opp_thresholds` 的**角色分派顺序与「加注者优先」**（`policy.rs:83-98`），把「角色 → 单标量 frac」换成「角色 → range 模板」。模板 = 中心强度 `s0` + 软度 `k` + 顶部截断 `cap_top` + air 地板：

```rust
fn prior_mass(tpl: &PreflopTemplate, eq: &EquityEngine) -> [f64; 169] {
    let mut m = [0.0; 169];
    for idx in 0..169 {
        let s = eq.strength[idx];                              // equity.rs:80，与旧同尺
        let incl = logistic((s - tpl.s0) / tpl.k);            // k→0 退化成硬阈值 s>=s0
        let cap  = 1.0 - logistic((s - tpl.cap_top) / tpl.k); // 抽掉「太强会 3bet」的顶部
        m[idx] = combo_weight(idx) as f64 * (incl * cap + tpl.air_floor);
    }
    normalize(&mut m); m
}
```

- `s0 = eq.threshold_for_top(frac)`（`equity.rs:125`），`frac` 沿用 `policy.rs:86-98` 桶值 → **`k→0, cap_top→1, air_floor→0` 时逐类等于 `from_threshold(threshold_for_top(frac))`**，翻前先验默认可退化到旧线性 range（迁移单调性锚点）。
- 角色差异化（这是标量模型永远做不到的构成差异）：
  - **open-raiser / 3bettor**：`cap_top=1.0`（线性、不截顶，弱场默认合并 range）。
  - **flat/cold-caller**（`committed_bb` 桶）：`cap_top≈0.75` → **capped-linear**，抽掉「本会 3bet 的 AA/KK 顶部」，`air_floor` 略高。
  - **limp / check option**：`s0` 低、`k` 大、`air_floor` 高 → 近似「任意两张但仍有顶端坚果」。
- **画像位移**：现有 `range_threshold_shift`（`opponent.rs:136`，±`adjust_cap`）改为**平移 `s0`**（nit 正、station 负），语义与旧一致，避免与 §2.7 的 profile 挂钩双算。

### 2.3 翻后贝叶斯更新（本节核心）

**算子**：给定先验 `m`、board `B`、本街观测动作 `a`，后验 `m'[idx] ∝ m[idx] · L(a | idx, B)`，再 normalize。

把每个 169 类在 B 上映射到两根正交轴：

- **value 轴** `v(idx,B) = made_class(rep, B).rank() / 5`（`features.rs:98,29`，Air=0…Monster=5）。
- **draw 轴** `d(idx,B)`：由 `draws()`（`features.rs:193`）的 `flush_draw / oesd / gutshot / combo_draw`（`:44-48`）合成，如 `d = 0.9·combo + 0.6·flush + 0.5·oesd + 0.25·gutshot`（夹 [0,1]）。

**下注似然**（本街下注者，尺寸 `β = high_bet/pot`，同 `policy.rs:72` 的 `bet_frac`）：

```
κ(β) = κ0 + κ1·β                                  // 极化指数随注大小增长
L_bet(idx) = a_v · v^κ(β)  +  a_d · d^κ(β)  +  ε_air
```

- **极化自然涌现**：`L_bet` 在 `v→1`（坚果）与 `d→1`（强听）两端都高，中段 `v≈0.5, d≈0` 的边缘成手被压低 → 后验 = **NUTS + DRAWS，中间抽走 = polar**。
- **注越大 → 越极化（不只是越紧）**：`β↑ ⇒ κ↑ ⇒ v^κ、d^κ 更陡`，中段成手指数级消失、两端保留。弱场默认用小 `a_d`（大注=价值、少诈）即可复现现有 `aggressor_size_tighten` 的剥削偏置——但现在是**构成重塑**而非标量拧紧。

其余动作似然（把「大注收紧」推广成对每种动作的构成塑形）：

- **过牌**（在池、无人下注）：`L_check = c0 + c_med·tent(v) + c_giveup·[v<0.15 ∧ d<0.2]`，`tent(v)=1-2|v-0.5|` 中段峰 → 控池中对/放弃空气留在 check range。
- **跟注**（在池、`committed_round==high_bet`、非本街 aggressor）：`L_call = b_v·bell(v; μ_v, σ) + b_d·d − b_nut·[v>0.9] − b_air·[v<0.1 ∧ d<0.15]` → **condensed range**（有牌但非坚果不加注、非空气才跟），标量模型无法表达的第三形态。
- **加注**：`L_raise = r_v·v^κr + r_d·d^κr + ε_air`，`κr` 大 → 强 polar。

**序列应用**：理想是对该对手本手每个动作按街序依次施算子。当前从 `GameView` 只能可靠导出**每对手本街聚合角色**；v0.4 起 `HistAction.street`（`view.rs:20-26`）已带权威街标，为「逐动作重放每对手序列」预留素材（见 §2.9 与 §4 P0-1）。

### 2.4 Board-aware 听牌纳入（76s on 9♠8♦2♣ 走通）

1. `canonical_index(7♠,6♠)` → suited 类，`representative_hand`（`equity.rs:292`）给代表手。
2. `made_class(rep, board)` = Air（`features.rs:112`）→ `v ≈ 0`。
3. `draws(rep, board)`：`straight_completing_ranks`（`features.rs:170`）测得补 5 / 补 T 均成顺 → `oesd=true`（`features.rs:226`）→ `d` 高。
4. **旧模型**：`76s≈0.40 < thr≈0.57`，`equity.rs:176` 滤除，「成 OESD」引擎从未看见。
5. **新模型**：先验 `mass` 很小但非零（`air_floor`）；对手下注 → `L_bet(76s)=a_v·0 + a_d·d^κ + ε` 被上调 → **作为半诈存活** → hero 的中对/顶对对下注 range 的 equity 正确算进听牌。

**类→combo 的 board 交互**（blocker / 花色）：一个 169 类展开成 ≤12 combo，交互不同——
- **suit 依赖同花听**：`draws()` 顺子逻辑 rank-only（`features.rs:222`），FD 是 suit 依赖（`features.rs:215-219`）；故 `d` 的顺子分量用代表手，同花分量按 two-tone 面存活比例均值化（4 combo 中 1 个有 FD → ×1/4）。
- **blocker/dead**：类展开只枚举 `avail`（`equity.rs:166`，已去 hero+board）；如 99 在 9-high 面只余 `C(3,2)=3` 个存活 combo 且已是 set（`made_class` 自动升级）。§2.5 按存活 combo 平摊 + 可选 `combo_refine` 精确处理。

### 2.5 喂给 `equity()`

签名 `opp_thresholds: &[f64]`（`equity.rs:149`）→ `opp_ranges: &[RangeVec]`。改两处：

**枚举**（替换 `equity.rs:167-190`）：不再硬过滤，改为按类质量平摊到存活 combo + 建 CDF：

```rust
for r in opp_ranges {
    let mut cdf = Vec::new(); let mut acc = 0.0;
    for (ca, cb) in enumerate_avail_pairs(&avail) {          // avail: equity.rs:166
        let idx = canonical_index(ca, cb);
        if r.mass[idx] <= 0.0 { continue; }
        let mut w = r.mass[idx] / n_alive[idx] as f64;       // 类质量平摊到存活 combo
        if cfg.range.combo_refine { w *= per_combo_draw_like(ca, cb, board); } // 花色精确，默认关
        acc += w.max(W_EPS); cdf.push((ca, cb, acc));
    }
    if acc <= 0.0 { cdf = all_pairs_uniform(&avail); }       // 空池兜底，复刻 equity.rs:181-188
    allowed.push(cdf);
}
```

**采样**（替换 `equity.rs:202-227`）：均匀 `rng.below(len)`（`equity.rs:205`）→ CDF 二分：

```rust
let x = rng.unit() * total_weight(cdf);                     // rng.unit() equity.rs:40，守 invariants §2
let (a, b) = binary_search_cdf(cdf, x);
// 冲突重抽 40 次 / draw_unused 兜底原样保留（equity.rs:204-219）
```

补公共牌、评估、平分（`equity.rs:232-261`）不动。空池兜底、重抽、兜底随机全部保留 → 鲁棒性不退。

### 2.6 迁移路径

1. **主开关** `RangeConfig.use_range_vec: bool`（serde default `false`，`config.rs:13` 的 `#[serde(default)]` 保证缺字段走默认）。
2. **不动旧路径**：保留 `build_opp_thresholds` + 旧 `equity(&[f64])`；新增 `build_opp_ranges` + `equity_weighted`。`decide` 两处调用点（翻前 `policy.rs:333`、翻后 `policy.rs:389`）按开关分流。开关 OFF → **逐字节等于基线**（不进新采样器，RNG 消费序列不变）。
3. **degenerate 自检**：`from_threshold` + 关软边/极化（`k→0, a_d=0, κ1=0`）应让新路径**数值逼近**旧路径（非 byte-equal，因 CDF 二分 vs `rng.below` 消费 RNG 不同；用统计等价 + 强度单调性单测，参照 `heuristic9_design.md:113`）。
4. **回归门 + 专用 baseline**：仿 `Limper`/`PairedTrap` 先例，加 **`PolarBettor` baseline**（下注只出坚果+强听、中段过牌）让「新模型抓 polar range」的收益可测；`use_range_vec=true` 须多 seed 不退才翻默认。

### 2.7 Config 面

`config.rs:14` 的 `HeuristicConfig` 加 `range: RangeConfig`（`#[serde(default)]`）：

```rust
pub struct RangeConfig {
    pub use_range_vec: bool,        // 主开关，默认 false
    pub prior_softness_k: f64,      // logistic 宽度，→0 复刻硬阈值
    pub flat_cap_top: f64,          // 跟注 range 顶部截断（capped-linear）
    pub air_floor_open: f64,
    pub bet_value_weight: f64,      // a_v
    pub bet_draw_weight: f64,       // a_d（弱场默认小 = 大注读价值）
    pub bet_air_floor: f64,         // ε_air
    pub polarity_kappa0: f64,       // κ0
    pub polarity_kappa1: f64,       // κ1：注大小→极化斜率
    pub check_medium_weight: f64,   // c_med
    pub call_value_center: f64,     // μ_v
    pub call_draw_weight: f64,      // b_d
    pub raise_polarity: f64,        // κr
    pub combo_refine: bool,         // per-combo 精确 draw 似然（默认 false）
}
```

profile 挂钩复用 `classify`（`opponent.rs:113`）：station → 抬 `air_floor`/降 `κ1`；maniac → 抬 `bet_draw_weight`/`bet_air_floor`。这把 `ExploitAdjust`（`opponent.rs:151`）从「平移标量」升级成「重塑构成」。**默认参数须调到「开关 ON 时复现现有弱场倾向」**（小 `a_d` + 大 `κ1` ≈ 现 `aggressor_size_tighten`），保证迁移单调。

### 2.8 复杂度 / 性能

| 项 | 现状 | 新模型 |
|---|---|---|
| 内存/对手 | 8 B | 1352 B（≤8 对手 ~11 KB） |
| range 构建 | O(169) 一次 `threshold_for_top` | 先验 O(169)；每街更新 O(169) 个 `made_class`/`draws`（`NaiveHandEvaluator`，廉价）；街数 k≤5 → <1000 evals/对手/决策 |
| 采样枚举 | O(C) 硬过滤（C≈1000–1200） | 同一枚举加权 + 建 CDF，同阶 O(C) |
| 单次抽牌 | O(1) `rng.below` | O(log C)≈10 二分；×samples(1200,`config.rs:36`)×n_opp，trivial |

关键省法：board-relative `v[169]`/`d[169]` **每决策点算一次**烘进 `mass`，采样枚举里每 combo 只做 O(1) 乘法。`EquityConfig.samples` 预算无需改。全程 f64（启发式层允许，`heuristic9_design.md:116`），显式 RNG（`invariants` §2）。

### 2.9 风险与开放问题

1. **喂料粒度（最大结构性 TODO）**：`build_opp_ranges` 当前只能导出每对手本街聚合角色，拿不到逐动作序列。§2.3 算子对序列成立，但要真喂需在 `GameView` 建每对手动作日志（依赖 §4 P0-1）。
2. **模型误设 = 头号 EV 风险**：似然参数是先验猜测，过度极化会让 hero 面对大注过弃/过抓诈。必须 sim 校准（§2.6），否则重蹈「教科书正确、本场 -EV」。
3. **画像双算**：`range_threshold_shift` 改平移 `s0`（§2.2），须与 §2.7 的 profile 挂钩不重复施加同一份 `adjust_cap`。
4. **类内非均匀**：`mass/n_alive` 假设类内等权，suit 依赖听牌破坏均匀；`combo_refine` 部分修正但增本。
5. **弱场保真**：现「大注=价值」剥削偏置正在贡献 live 盈利；默认参数须证明**不劣于**旧标量在弱场表现。
6. **多人加权重抽偏差**：对手间去重靠 40 次重抽（`equity.rs:204`），加权采样下重抽改变边际分布，需评估偏差。

---

## 3. 多策略架构：弱 / 强 / 混合场

> 把 `decide`（`policy.rs:131`）从「一套写死的流水线」升级成「共享地板 + `Strategy` trait 分派 + 场型选路」。选路所需上下文（`view`/`eq`/`opp`/`cfg`）已全在 `decide` 入参里 → **`mod.rs` 调用签名不用改**（`mod.rs:44`）。

### 3.1 Strategy 抽象 + 共享地板

```rust
// 新增 src/heuristic/strategy/mod.rs
pub trait Strategy {
    fn decide(&self, ctx: &DecisionCtx) -> Decision;   // 出未合法化的 Decision
    fn id(&self) -> &'static str;                       // 进 Decision.source，遥测用
}
pub struct DecisionCtx<'a> {
    pub view: &'a GameView, pub eq: &'a EquityEngine,
    pub opp: &'a OpponentTable, pub cfg: &'a HeuristicConfig,
    pub hole: [Card; 2], pub rng: &'a mut Rng,          // situation_seed 派生（policy.rs:31）
}
```

**关键取舍**：`build_opp_thresholds` 返回的 linear-top 阈值和 `ExploitAdjust` **本身就是剥削假设的一部分**（size-tighten `policy.rs:73`、画像偏移 `policy.rs:105`）。因此 range 建模**下沉进各策略内部**，trait 只共享中立上下文。`Exploitative` 复用现 `build_opp_thresholds`；`BalancedHeuristic` 用 §2 的 polar-aware `RangeVec`。

**当前逻辑 → `Exploitative` 策略：几乎零改**。函数体 = 现 `decide` 里 `safe_fallback` 之后、`legalize` 之前那段（`policy.rs:144-152`）；`preflop_decide`/`postflop_decide`（`policy.rs:219/379`）签名原样保留成私有实现。

**「永不犯大错」地板压在所有策略之下，不进 trait，由编排器统一施加**：

```rust
pub fn decide(view, eq, cfg, opp) -> Decision {
    let hole = match view.hole { Some(h) => h, None => return safe_fallback(view, "no_hole") };
    let mut rng = Rng::new(situation_seed(view, cfg));
    let strat = select_strategy(view, opp, cfg);          // §3.4 新增
    let d = strat.decide(&DecisionCtx { view, eq, opp, cfg, hole, rng: &mut rng });
    legalize(d, view)                                     // 共享地板不变（policy.rs:158）
}
```

`safe_fallback`（`policy.rs:201`）在选路**之前**、`legalize`（`policy.rs:158`）在分派**之后**、尺寸都走 `raise_to_clamped`（`policy.rs:606`，守 `invariants.md:47` §5）。

### 3.2 策略变体

**(a) Exploitative — 现打法（弱场，已实现，零行为变化）。** 默认选路永远选它 → byte-equal。

**(b) BalancedHeuristic — 强场（近期推荐）。** 务实界定野心：完整 CFR/GTO 求解器是另一个项目（就是本仓库 blueprint 主线）。给一个谱系：

| 档 | 改动 | 座位无关 | 迭代成本 | 不可剥削度 |
|---|---|---|---|---|
| L0 `BalancedHeuristic`（**推荐**）| 复用现 policy 骨架，换 4 处假设 | ✅ | 低 | 弱（近似）|
| L1 富 polar range + blocker 诈唬选择 | L0 + §2 `RangeVec` | ✅ | 中 | 中 |
| L2 `BlueprintStrategy` 查表 | 包 `blueprint_advisor.rs:445` | ❌ 训死人数 | 高 | 真保证，仅训练座数 |

**L0 = 同一 policy 骨架、四处换假设**：
1. **拆 size-based 收紧**：`aggressor_size_tighten`（`policy.rs:73`）置零或反转（对大注略放宽，polar 读）。
2. **polar range 而非 linear-top**：走 §2 的 `RangeVec`（面对大注建「顶端 ∪ 底端诈唬」双峰）。这是 L0 唯一非平凡新代码，**由 §2 提供**。
3. **GTO-ish 频率、关剥削**：`ExploitAdjust` 钳为 `Default`，价值:诈唬走固定近平衡配比（河牌约 2:1）。
4. **接上两个死档旋钮**：`MixingConfig.temperature`（`config.rs:184`，边界 softmax）和 `checkraise_freq`（`config.rs:149`）在 Balanced 下接线（随机仍从 `ctx.rng` 取，守 `invariants.md:18`）。

L0 复用约 90% 现 policy 代码，保住座位无关与快迭代（启发式相对 blueprint 的核心优势）。

**是否复用 CFR blueprint 做 L2**：仓库已有独立 blueprint 腿（`blueprint_advisor.rs`、`tools/train_cfr`；HU 1B dense 对 Slumbot 近 break-even，`six_max_nlhe_target.md:5`）。可包 `BlueprintStrategy`：`num_seats` 命中已训档则查表、否则回落 L0。**但这是要拍板的大决策**：blueprint 按固定人数训、与座位无关性正交冲突、需维护两套产物。**建议近期不做，留成未来 trait 实现、`num_seats` 门控。**

**(c) Mixed — 混合场（做成「细粒度选路」而非第三套策略）。** 混合场 = 同桌既有弱鱼又有强 reg，多人底池同时面对两类。选一整套「桌级策略」是错的。复用引擎里**已经是 per-opponent 的两个接缝**：

- `build_opp_thresholds` 本就逐座循环（`policy.rs:78`）、逐名施加画像位移（`policy.rs:105`）→ 把 `aggressor_size_tighten`（`policy.rs:73/101`）由「对本街下注者一律收紧」改成「**按该下注者分类 + 置信度门控**」：确证弱才收紧，未知/tag/确证强不收紧甚至反向。
- `aggregate_exploit`（`opponent.rs:176`）**已经**只对确证弱类型发力（`opponent.rs:191-217`）→ 混合场直接沿用。

所以「Mixed」不是第三个 `Strategy`，而是把弱场假设全部**逐对手门控**后的自然产物。这把「per-hand vs per-opponent」张力干净化解：**自身频率结构 = 桌级选（带滞回），对手 range 建模 = 逐座选。**

### 3.3 场型检测

**逐对手层（已有微调）**：`classify`（`opponent.rs:113`）已把够 `min_hands`（18，`config.rs:227`）的对手分 5 类。加语义标签 `is_exploitably_weak = {station, maniac, nit, lag}`，`Tag` 归「solid/可能强」。

**桌级层（新增）** `OpponentTable::classify_field(active_names, cfg) -> FieldType`，逐活跃座聚合（`num_seats` 驱动）：`known_weak 多数 → Weak`；`known_solid 多数（或桌级 prior=Strong）→ Strong`；`两类都显著 → Mixed`；`已知太少 → Unknown`。

**冷启动（unknown → 选哪套）：选 Exploitative。** 理由：(i) 全项目前提是弱场；(ii) byte-equal 硬要求；(iii) 剥削式对 tag 也近正不亏（v0.1：tag 9max +1728、HU +249，`heuristic9_design.md:141`）。但真强场会被 size-tighten 反剥削 → 提供桌级 `field_prior: Weak|Strong` 让运营在已知硬池翻默认。

**根本盲点（必须诚实标注）**：`OppStats`（`opponent.rs:13`）只有 VPIP/PFR/AF，**强 reg 和 solid tag 在这三个量上无法区分**（tag 就是「gap<0.18、无极端」的兜底桶，`opponent.rs:126-131`）。要真检出「会平衡」需新信号：大注摊牌亮诈率、cbet%、fold-to-cbet、check-raise 能力（= §4 P0-3）。近期务实代理：不奢望自动判强，靠桌级 prior + 逐座 size-tighten 门控兜住反剥削风险。

### 3.4 选择与切换

`select_strategy(view, opp, cfg)` 置于 `decide` 建完 view、`safe_fallback` 之后、dispatch 之前，读 `cfg.strategy` + `classify_field`。两级粒度：

- **桌级（整策略切换）**：只决定「我方自身频率结构」= Exploitative vs BalancedHeuristic，带滞回，per-table。
- **座级（range 建模）**：size-tighten 门控 + 画像位移永远逐座（`policy.rs:78-105`），per-opponent，不受桌级切换影响。多人池里对 station 收紧、对强 reg 不收紧在同一次 `equity()` 里共存——因为 `build_opp_thresholds` 本就每座各建一条 range（`strategy.md:149`）。

**滞回（防抖）**：用存储态而非 dead-band 重算。`OpponentTable` 已持久化（`opponent.rs:72`），加 `field_state: FieldType`，只在 `observe_hand`（`&mut self`，`opponent.rs:77`）里按带边距规则更新（strong-score 越 0.6 才进、跌破 0.4 才退），`decide` 只读。复用现有「observe 累积 / decide 只读」读写分离，滞回态跨 session 持久。

### 3.5 Config 面（长出多策略且默认 byte-equal）

`HeuristicConfig`（`config.rs:12`）加两块，全 `#[serde(default)]`：

```rust
pub struct HeuristicConfig {
    pub equity, preflop, postflop, mixing, exploit,  // 现有 = Exploitative profile
    pub strategy: StrategySelectionConfig,           // 新增：选路
    pub balanced: BalancedProfileConfig,             // 新增：Balanced 覆盖参数
}
pub struct StrategySelectionConfig {
    pub mode: SelectionMode,      // Default = Fixed(Exploitative) ← byte-equal 命根
    pub field_prior: FieldType,   // Default = Weak
    pub field_min_hands: u32,     // 桌级切换门（> min_hands=18）
    pub hysteresis_enter: f64,    // 0.60
    pub hysteresis_exit: f64,     // 0.40
}
```

**byte-equal 机制**：`Default = Fixed(Exploitative)` → `select_strategy` 永远返回 Exploitative → 走与今日同一条路径。顶层 `equity/preflop/postflop/mixing/exploit` 原样不动（老 JSON config 照常解析）。`BalancedProfileConfig` 只在 Balanced 被选中才消费。推荐「顶层 = Exploitative profile + 独立 `balanced` 子树」（byte-equal 安全性平凡可证），而非把 profile 做成 map。强场跑 = `--config strong.json`（设 `mode="balanced"` 或 `field_prior="strong"`），沿用现有 partial-JSON 扫旋钮通路。

### 3.6 集成点与不变量

接线全在 `policy.rs`，`mod.rs:44` 不改。不变量核对（`docs/invariants.md`）：§2 无全局 RNG（混合从 `ctx.rng` 取）✅；§3 无 unsafe ✅；§1 浮点隔离（启发式层 f64，不泄进 rules/evaluator）✅；§5 Raise{to} 绝对值（走 `raise_to_clamped`）✅；§6 座位无关（`select_strategy`/`classify_field` 逐活跃座，唯 L2 blueprint 门控例外）✅；§7 改算法须外部对照 → Balanced 的「平衡频率」宣称用仓库 LBR/best-response 工具（`tools/six_max_search_probe`、`tools/leduc_lbr_report`）对照。

### 3.7 测试与灰度

回归门三层：
1. **默认 byte-equal 硬门**（一票否决）：默认 config + exploit off → mbb/g 逐字节一致、漂移 0.000%、fallback 0。
2. **Balanced EV 门——需新 baseline**：sim 现有 baseline（`heuristic9_sim.rs:37-51`）全是弱/简单型，**没有一个会反剥削 size-tighten**，故现在量不出「Balanced 是否赢强对手」。须加 `SizeExploiter`（polar 大注含诈，专门惩罚 size-tighten 假设），有 `Limper`/`PairedTrap` 先例（为让漏洞可门控测量专门加 baseline，`heuristic9_design.md:184/247`）。
3. **两侧门 + 选路正确性**：Balanced 须在强 baseline 翻正/持平、对弱 baseline 不崩；A/B 同 seed 比 mbb/g±CI95；`--field mixed` 拼真混合桌（几 Station + 几 SizeExploiter）测逐座门控；Rust 单测断言 `classify_field`、冷启动、滞回不抖。

**灰度**：先 ship「trait 抽象 + Exploitative 包装 + Fixed 选路」（纯重构 byte-equal）；再 ship BalancedHeuristic + SizeExploiter baseline（默认不选中）；最后接 `classify_field` 自动选路（prior=Weak 保守）。

> ⚠ 机器约束：只跑改动相关 targeted 测试，别裸 `cargo test`（1.6GB 机器 OOM）。

### 3.8 风险与大决策

1. **检测盲点（最大风险）**：强 reg 用现统计看起来就是 Tag，误判强场为弱 → 被 size-tighten 反剥削是最贵的错。缓解：保守 prior + 逐座门控 + 不确定默认弱。根治需给 `OppStats` 加平衡性信号（§4 P0-3）。
2. **反向误判成本低**：误判弱为强 → Balanced 对 station 少赚，成本 < 风险 1，可接受。
3. **Balanced 不是真不可剥削**：别过度承诺；真保证只有 solver/blueprint 给（仅训练座数内）。
4. **滞回持久化副作用**：早期一次坏读会跨 session 黏住，需衰减/定期重评。

---

## 4. 优化 Backlog（除 §2 §3 之外）

> 凡与 §2（加权 range）、§3（多策略）相关处只标依赖。优先级：**P0** = 高 EV / 基本独立 / 是其他项地基；**P1** = 明确 EV 可单独落；**P2** = 收益薄或需先落 §2/§3。

### P0

**P0-1 · 翻后「行动线」记忆缺失。** 现状：`view.rs` 重放时**算了**逐街 aggressor（`aggressor_by_street`，`view.rs:277,319-321`），但 `GameView` 只导出 `preflop_aggressor`（`view.rs:496-500`）和当前街 `street_aggressor`（`view.rs:520`），翻牌/转牌 aggressor 被丢弃。局限：无法识别 delayed c-bet / check-back（range 封顶）/ double-barrel / check-raise 线；`i_have_initiative()`（`view.rs:620-624`）会给**过期主动权**（hero 翻牌 c-bet、转牌示弱，河牌仍自判有主动权 → 错走 `policy.rs:437` c-bet 诈唬）；`build_opp_thresholds` 角色 `frac` 把「翻牌下大注、转牌过牌」与「两街都过牌」归同档。改进：`GameView` 增 per-street 动作摘要 + 派生量（`villain_capped_flop`、`is_delayed_cbet`、`facing_second_barrel`）。**EV M-L，工作量 M**（数据已在重放里）。**独立**，且是 §2.9 喂料、P0-3、P1-1 的地基。低风险（纯增信息，默认可 byte-equal）。**部分落地（2026-07-02）**：`GameView` 已增 `seat_actions`（逐座逐街行动线，权威街标）+ `street_aggressors[0..3]`（暴露此前丢弃的翻牌/转牌/河牌 aggressor）+ 只读访问器，**纯增 / decide 零引用 → byte-equal**（`heuristic9` 10/10 守，`tests/heuristic_p0_1_action_line.rs`）。**未做**：`i_have_initiative()` 过期主动权修复（行为变更、须 sim 门）+ 派生量（`villain_capped_flop` 等，待消费端定义精确语义）。数据基座已就位，供 range-ml 在线 `GameView→Sample` 适配器消费。

**P0-2 · 下注尺寸引擎粗糙。** 现状：`pick_bet_size`（`policy.rs:541-558`）= `lerp(size_small=0.33, size_big=0.80, wetness)` + 低 SPR 加 0.15 + 唯一 overbet 条件（`policy.rs:550`）；签名 `_view` 未用（`policy.rs:542`，尺寸与位置/街/donk 无关）；`size_mid=0.55`（`config.rs:143`）**死档**。局限：尺寸对 wetness 单调，无 polar 结构、无跨街 geometry、donk 与 cbet 同尺寸、overbet 只给 Monster。改进：升级为多档尺寸选择器（block 0.25 / 标准 0.5-0.66 / polar 0.8-1.0 / overbet 1.3+），输入 (street, IP/OOP, is_donk, polarity, SPR, 目标河牌 SPR, 画像)，接线 `size_mid`，价值线按几何递增到河牌全下。**EV M-L**（v0.3 `station_value_size_mult` 单方向就 +112 mbb/g），**工作量 M**。**独立**；overbet-as-bluff 软依赖 §2（blocker）。中风险，须逐档扫参。

**P0-3 · 对手模型只有翻前 VPIP/PFR + 全局 AF。** 现状：`OppStats`（`opponent.rs:12-23`）只累计 vpip/pfr + 跨所有街合并的 `af()`（`opponent.rs:33-35`）；`classify` 次级判据硬编码未接 config（`opponent.rs:110-112` 自认）；剥削按类别 `±adjust_cap`，同类内不连续。局限：缺最有价值的 exploit 统计——**fold-to-cbet、cbet%、fold-to-3bet、WTSD、turn/river aggression、check-raise 频率**。改进：`ObservedAction` 已带 `street`（`opponent.rs:51`），扩 per-street 计数 + 复合率；剥削从「离散 ±cap」改「连续函数」（cbet 频率 = f(fold-to-cbet)）；魔法数进 `ExploitConfig`；按 villain 位置分桶。**EV M-L**（弱场直接变现），**工作量 M-L**（扩持久化 schema，注意兼容 `h9_live_prof*.json`）。与 §3 正交互补——§3 是场级选路、本项是个体逐街读，**是 §3 判「强 reg」的命根**。

### P1

**P1-1 · 无独立 check-raise 专家。** `checkraise_freq=0.18`（`config.rs:149`）死档；OOP 强牌直接 lead（`policy.rs:419-427`），CR 只能被动经下个决策点的 `value_raise`（`policy.rs:481-485`），无计划/无频控/无诈唬 CR。改进：加 OOP CR 专家（坚果/强两对/强听按 `checkraise_freq` 混「check→计划 raise」，顶对/中对走 check-call）。**EV M**（vs 会 cbet 的对手），**工作量 M**。软依赖 P0-1、须加「会 cbet 的 aggressor」baseline（P1-5）。

**P1-2 · Equity MC 方差 + 无缓存。** `samples=1200`（`config.rs:47`）→ 胜率标准误 ≈1.4%，而多个阈值只差 0.02-0.06（`value_equity=0.58` vs `+0.06`）→ 噪声随机翻转边界，还被烘进 baseline 污染回归门信噪比；combos 每决策重枚举（`equity.rs:167-190`）无缓存。改进：(a) live 提 `samples`（纯 config 快赢）；(b) 方差缩减（antithetic / 公共随机数 / river-turn 精确枚举）；(c) 量化 `thr` 缓存 combo 列表。**EV S-M，工作量 S→M**。**注意 §2 会重写这段采样**，缓存实现须与 §2 协调；但「提 live samples」「river 精确枚举」可先落不冲突。

**P1-3 · 特征粒度粗。** `MadeClass`（`features.rs:12-25`）把顺～皇同花顺全归 Monster、两对+三条全归 Strong、顶对+超对全归 TopPair——**无 kicker/坚果度**（overbet 对裸顺与四条一视同仁）；`draws()` river 直接返回空（`features.rs:195-203`）；`gutshot`/`overcards` 算了却 policy 从不读（`features.rs:229-234`，`has_meaningful_draw` 只认 flush/oesd/combo）；`wetness` 静态、无 runout 动态。改进：made_class 增坚果度子级、接线 gutshot/overcards 进弱半诈、texture 增「本街完成什么听牌 / board 配对动态」。**EV M，工作量 M**。坚果度独立；**nut-blocker 感知依赖 §2**。

**P1-4 · 翻前树浅。** 位置仅 5 桶（`view.rs:639-657`，9-max 全 EP 挤 `Early`）；面对加注只有 value (re)raise（`policy.rs:313`），**无诈唬 3bet/4bet**；squeeze 未建模（尺寸 `face*mult`，`policy.rs:321`，不随 caller 数放大）；`fourbet` 后无 5bet 分支。改进：EP 细分（UTG/MP/HJ）、频率驱动轻 3bet/4bet（带 blocker 优先）、squeeze 尺寸随 caller 递增、补 5bet-shove 门。**EV M**（9-max 主场），**工作量 M**。诈唬 3bet 手选软依赖 §2；位置细分/squeeze 独立。

**P1-5 · 测试/回归门/交叉验证覆盖。** 现状：单测 10 个 + sim baseline 全是弱/简单型（`heuristic9_sim.rs:37-51`），**无** baseline 覆盖会 cbet 的 aggressor / trapping CR / 3bettor / 平衡对手 → 测不出 P1-1/P0-3/P1-4/§3 的收益；exploit 调整/bet-sizing/texture/classify 收敛**无单测**；equity 只有单调性、无对精确枚举的交叉验证。改进：加上述 baseline + 单测 + 「heuristic equity vs 精确枚举」交叉测 + 回归门方差审计。**EV 间接高**（安全 ship 前所有项的前提），**工作量 M**。**独立，且应先于多数 P0/P1 落地。**

### P2

**P2-1 · 边界 softmax 混合（`mixing.temperature=0.10` 死档，`config.rs:184`）。** 现只有频率门（`rng.unit()<freq`，`policy.rs:441/489/510`），无「分数相近动作按 softmax 混合」。**EV S**（弱场对手不针对），**工作量 S-M**。**主要服务 §3 强场抗剥削 → 随 §3 落。**

**P2-2 · Multiway/SPR/有效栈 nuance。** 多人收紧是线性系数（`policy.rs:257/303/407/413`）；`effective_stack_chips()` 取「我 vs 最大对手栈」（`view.rs:585-601`），多短栈池会高估可博弈深度 → `commit_spr` 偏差。改进：有效栈按相关对手、多人惩罚改标定曲线。**EV S-M，工作量 M**。收益薄（200BB cash 主线深栈少见短栈多人）。**ICM/锦标赛明确 out-of-scope**（现目标 200BB cash，`six_max_nlhe_target.md` 路线内无锦标赛）。

**P2-3 · `aggressor_size_tighten` 可剥削性。** `0.12*bet_frac`（`policy.rs:73-77`）对本街下注者按注大小恒收紧（弱场假设，`policy.rs:69-70` 自认），碰 polar 诈唬对手让 hero 过弃（`strategy.md:117` 已承认）。改进：让符号/幅度由画像（尤其 P0-3 河牌诈唬倾向）驱动。**EV S，工作量 S**。**依赖 P0-3，且与 §2 重叠（§2 会重写这段）→ 归入 §2 实现更自然。**

### 依赖关系速览

| 项 | 依赖 §2 加权 range | 依赖 §3 多策略 | 依赖 P0-1 line 记忆 |
|---|---|---|---|
| P0-1 line 记忆 | — | — | 本身 |
| P0-2 尺寸 | overbet-as-bluff 软依赖 | — | — |
| P0-3 per-street 剥削 | — | 语义相邻需协调 | 部分派生量 |
| P1-1 check-raise | — | — | 软依赖 |
| P1-2 equity 方差 | 缓存须协调 | — | — |
| P1-3 特征粒度 | nut-blocker 依赖 | — | runout 依赖 |
| P1-4 翻前树 | 诈唬 3bet 手选依赖 | — | — |
| P1-5 测试基建 | — | — | — |
| P2-1 softmax | — | 随 §3 | — |
| P2-3 大注收紧 | 归入 §2 | — | 符号靠 P0-3 |

---

## 5. 统一路线图（分阶段）

综合 §2/§3/§4 各自的落地建议，按**依赖 + 风险 + 可测性**排成一条线。每阶段以「默认 byte-equal + sim 回归门不退」为出闸条件。

**Phase 0 — 地基与可测性（零 EV 风险，先做）**
- P1-5：补齐 sim baseline（`PolarBettor`、`SizeExploiter`、会 cbet 的 aggressor、trapping CR）+ 关键单测 + equity 精确枚举交叉验证。**没有它，后面所有改动都无法证伪。**
- §3.1 的 `Strategy` trait 重构 + `Exploitative` 包装 + `Fixed` 选路（纯重构，byte-equal 一票否决门）。

**Phase 1 — 行动线地基**
- P0-1：`GameView` 保留整手下注线 + 派生量。顺带修 `i_have_initiative()` 过期主动权 bug。解锁 §2.9 喂料、P1-1、P0-3。

**Phase 2 — 两大 EV 杠杆（弱场先变现，多为独立）**
- P0-2：多档尺寸引擎（接 `size_mid`、跨街 geometry、donk/cbet 区分）。
- P0-3：per-street 对手统计（fold-to-cbet 等）+ 连续剥削。**同时是 §3 判强 reg 的前置。**

**Phase 3 — 加权 range（range 层大改）**
- §2：`RangeVec` 全套，`use_range_vec` opt-in，degenerate 自检 + `PolarBettor` 门。P2-3 在此并入。翻默认前是纯 config 旋钮。

**Phase 4 — 强场策略（依赖 §2 的 polar range）**
- §3.2(b) `BalancedHeuristic` L0（拆 size-tighten、走 §2 polar range、关剥削、接 softmax/check-raise）+ `SizeExploiter` 门。默认仍不选中。
- P1-1 check-raise、P2-1 softmax 在此接线。

**Phase 5 — 自动选路 + 收尾**
- §3.3/§3.4 `classify_field` + 滞回自动选路（默认 prior=Weak 保守）。
- 剩余 P1（P1-3 nut-blocker 子项、P1-4 翻前树）、P2-2。
- 大决策评审：是否投入 L2 `BlueprintStrategy`。

---

## 6. 需要拍板的大决策

| # | 决策 | 选项 | 本文倾向 |
|---|---|---|---|
| ① | 强场野心 | (a) 座位无关 BalancedHeuristic（快、弱保证）(b) 复用 CFR blueprint 查表（真保证、训死人数、维护两套）| **近期 (a)**，(b) 留未来 `num_seats` 门控 |
| ② | 判「强 reg」信号 | 现 VPIP/PFR/AF 判不出 → 必须给 `OppStats` 加平衡性信号（P0-3）| **做 P0-3**；在此之前靠桌级 prior 兜 |
| ③ | `RangeVec` 默认是否开 | opt-in 到证明弱场不退再翻默认 | **先 opt-in**，`combo_refine` 默认关 |
| ④ | 锦标赛/ICM | 现 200BB cash 主线无 | **out-of-scope** |
| ⑤ | 落地节奏 | 一次性大改 vs 分阶段 byte-equal 门 | **分阶段**（§5），延续 v0.2–v0.4 纪律 |

---

## 附：关键代码锚点

| 主题 | 位置 |
|---|---|
| 决策入口 / 选路接线 | `src/heuristic/policy.rs:131`（`decide`）|
| range 组装 / size-tighten 门控点 | `policy.rs:53`（`build_opp_thresholds`）/ `:73`（`aggressor_size_tighten`）|
| 共享地板 | `policy.rs:158`（`legalize`）/ `:201`（`safe_fallback`）/ `:606`（`raise_to_clamped`）|
| 翻后 / 尺寸 | `policy.rs:379`（`postflop_decide`）/ `:541`（`pick_bet_size`，`_view` 未用）|
| equity 消费 / 采样 | `src/heuristic/equity.rs:145`（`equity`）/ `:167-190`（枚举）/ `:176`（硬过滤）/ `:181`（空池兜底）|
| 强度表 / 索引 | `equity.rs:88`（`EquityEngine::new`）/ `:57`（`canonical_index`）/ `:125`（`threshold_for_top`）|
| 特征 | `src/heuristic/features.rs:12`（`MadeClass`）/ `:193`（`draws`）/ `:239`（`texture`）/ `:229-234`（gutshot/overcards 未消费）|
| 对手模型 | `src/heuristic/opponent.rs:13`（`OppStats`）/ `:113`（`classify`）/ `:136`（`range_threshold_shift`）/ `:176`（`aggregate_exploit`）|
| 行动线数据 | `src/heuristic/view.rs:277,319-321`（逐街 aggressor 算了却丢）/ `:520`（只导出当前街）/ `:620-624`（`i_have_initiative` 过期）|
| config | `src/heuristic/config.rs:12`（`HeuristicConfig`）/ 死档 `:143` `size_mid` `:149` `checkraise_freq` `:184` `temperature` |
| sim baseline | `tools/heuristic9_sim.rs:37-51`（现有）/ `:839`（per-seat 拼混合桌）|
| blueprint 腿（L2 可选）| `src/training/blueprint_advisor.rs:445` |
