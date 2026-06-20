# 裁剪 turn blueprint：把 river 的 root range 统一改走「turn 已解子树后验」

日期 2026-06-19。承接 `unanchored_range_design_2026_06_10.md`（档二′-跨街复用）。

## 0. 一句话目标

在 OpenPoker search-on 生产参数下（`--search-trigger all-postflop` + 解到终局 +
`--search-prewarm`），让 **river 决策的根 range 无论锚定/脱影子路径都来自「turn 已解子树
沿 turn 实际动作线条件化的后验」**，从而 turn blueprint 在 river 上的唯一消费者被替换掉
→ blueprint checkpoint 可裁到只剩 **preflop + flop**（turn / river 街都不再被读）。

适用参数（本文所有结论基于此组开关，`tools/openpoker_advisor.rs` 默认值已核实）：

```
--search --search-trigger all-postflop --search-time-budget-ms 12000 --search-lcfr
--search-max-nodes 40000000 --search-solve-threads 24 --search-prewarm
--search-bucket-table <1000/1000/1000>
# 隐含默认：unanchored_prefix_reach=ON(:116) / unanchored_cross_street=ON(:128)
#           flop_prefer_blueprint=OFF(:136) / depth_limit=false（解到终局，decide :961 传 None leaf_values）
```

## 1. 已核实事实（裁剪的前提，全部带 file:line）

### 1.1 blueprint 在这套参数下被读的全部位置

| 街 | 当「决策」读? | 当「搜索 range 先验」读? |
|---|---|---|
| preflop | ✅ `blueprint_distribution`（all-postflop 不搜 preflop，`decide` :1002） | ✅ 所有 postflop 子树前缀 |
| flop | ❌（被搜索） | ✅ turn/river 决策前缀 |
| **turn** | ❌（被搜索） | ✅ **仅 river 决策的 range 先验** |
| **river** | ❌（被搜索） | ❌（river 之后无街，没有谁把 river 当前缀） |

- postflop 决策分布全来自 CFR 解；`decide` 里 `info_set_for_cards`（`openpoker_advisor.rs:899`）
  只取 `.raw()` 写日志，**不查策略**。`blueprint_distribution`（:1002）只在 `want_search==false`
  即 **preflop** 触发。→ postflop blueprint σ 的**唯一**真实读者是 range 估计。
- range 估计 `estimate_range`（`subgame.rs:1086`）沿**当前街之前**的每个决策节点查
  `info_set_for_cards(node_id, hole, board_prefix)`（:1113-1116），节点 `street_tag` 进 info set。
  - 锚定 inner：`range_decisions = prior`（`street < current`，:2049-2068）。
  - 脱锚 inner：`prefix_ranges` 同样 filter `street < cur`（:2905）。
- **river 的前缀含 turn 决策 → 读 turn 的 info set。turn 是 blueprint 在 river 之后的唯一
  消费者；river 自身的 info set 永不被读。**（上一轮已逐路径钉死。）

### 1.2 turn 解能活到 river（缓存 prev-slot 机制，已核实）

`SubgameSolveCache`（`subgame.rs:1609`）= 主 slot + `prev` slot：

- `store()`（:1663-1668）：存新解时旧主 slot 若**同手换街**（board 长度变）→ 提升到 `prev`。
- turn→river 顺序（all-postflop + prewarm）：turn solve 入主 slot → river prewarm/首决策 solve
  时 turn 同手换街（板 4→5）→ turn 提升到 `prev`。
- 取 range 时机：river **prewarm** 时（river 未 store）turn 在 `current()`；river **决策**时
  （river 已是主 slot）turn 在 `prev()`。`cross_ranges` 两 slot 都试
  （脱锚 inner :2887 `try_slot(c.current()).or_else(|| try_slot(c.prev()))`）。
- **prev-slot 只保留紧前一街**（单街回溯）——而我们要的恰好就是「river 复用 turn / turn 复用
  flop」，紧前一街足够。

### 1.3 cross_street_posterior_range 读的是「已解子树」，不是 blueprint

`cross_street_posterior_range`（`subgame.rs:1450`）沿 turn 子树实际动作线读
`prev.trainer.average_strategy`（:1519-1520），即 turn **解出来的 σ**，全程不碰 blueprint。
身份校验：`hand_seed` 相等 + board 严格前缀（prev+1==next，:1468）+ 导航 `prev_within` 走得通
（:1474）。当前**额外**有一道 kind 闸：`prev.kind != KEY_KIND_UNANCHORED → None`（:1460）。

### 1.4 当前脱锚 vs 锚定的差距（要补的就是这块）

- **脱锚 river**（`decide_search_unanchored` :1504）：已经走 cross-street（:1569
  `rt.unanchored_cross_street.then_some(prev_within)`），`cross_ranges.or(prefix_ranges)`（:2946）。
  但 cross 仅在 turn 子树是 `KEY_KIND_UNANCHORED` 时命中（kind 闸），且 `prefix_ranges` **仍被
  计算**（读 turn blueprint），只是命中时被丢弃。
- **锚定 river**（`subgame_search_cached` :1937 → inner :1982）：**完全没有 cross-street**，
  range 永远走 `estimate_range`（:2075-2111，读 turn blueprint）。advisor 锚定分支已从
  `build_real_auth` 拿到 `_prev_within` 但**丢弃**（:920 注释「锚定…不取 prev_within」）。

→ 要让 turn blueprint 在**所有** river 决策上都不被读，必须：①锚定路径接 cross-street；
②kind 闸放开（让 turn 不管 anchored 还是 unanchored 都能被 river 复用）。

## 2. 方案

### 2.1 改动 A — 放开 kind 闸（`subgame.rs:1460`）

```rust
// 旧：if prev.kind != KEY_KIND_UNANCHORED || prev.hand_seed != hand_seed { return None; }
// 新：只校验同手；anchored / unanchored 解都是合法子树解，沿动作线条件化得到的后验都是
//     合法 range 先验（anchored 是 100BB 近似，与它替换掉的 estimate_range(blueprint) 同等
//     近似级别——不更差）。身份由 hand_seed + board 严格前缀 + 导航三道守住。
if prev.hand_seed != hand_seed { return None; }
```

允许**跨 kind 复用**（turn anchored → river unanchored，或反之）：live 一手内可能 turn 决策时
lockstep Ok（anchored 解）、river 决策时已 off-stack（unanchored）。不允许跨 kind 会让这类
river 退回读 turn blueprint，裁剪不彻底。

### 2.2 改动 B — 锚定 inner 接 cross-street（`subgame.rs:1982 subgame_search_cached_inner`）

1. 加参数 `cross_street: Option<&[(Action, bool)]>`。
2. 在 `ranges_opt`（:2075-2111，estimate_range）算完、`cache` 重绑（:2130）之后、**key（:2131）与
   `move` 闭包（:2149）之前**，插入跨街计算（照搬脱锚 inner :2871-2890）：

   ```rust
   let cross_ranges: Option<Vec<Vec<f64>>> = match (cross_street, cache.as_deref()) {
       (Some(prev_within), Some(c)) => {
           let holes = all_hole_combos();
           let try_slot = |prev: Option<&SolvedSubgame>| prev.and_then(|p|
               cross_street_posterior_range(p, prev_within, root_state, &holes,
                   cfg.range_uniform_mix, auth_actor, hand_seed));
           try_slot(c.current()).or_else(|| try_slot(c.prev()))
       }
       _ => None,
   };
   let ranges_opt = cross_ranges.or(ranges_opt); // 跨街优先，覆盖 blueprint estimate
   ```
   - 借用顺序：`cache.as_deref()` 取**只读** peek 产出 owned `Vec`，借用随即释放，后续
     `cache` 的可变 lookup/store（:2212）不受影响——与脱锚 inner 同款，借用检查可过。
   - depth_limit 路径在 :2130 已把 `cache` 置 None → `cross_ranges` 自然 None，无需额外 guard。
3. 把新参数透传：`subgame_search_cached`（:1937）加同名参数，`subgame_search`（:1890）传 `None`。

注：`ranges_opt` 进 `solve_cache_key`（:2138 `ranges_opt.as_deref()`）→ 开/关跨街、命中/未命中
自动产生不同 key，不会串均衡。这点与脱锚 inner 完全同构。

### 2.3 改动 C — 锚定 prewarm 接 cross-street（`subgame.rs:3062 subgame_search_prewarm`）

prewarm 必须与决策时算出**同一份 ranges 才命中 key**（否则白预热）。给
`subgame_search_prewarm` 加 `cross_street` 参数透传给 inner。两路同读 `cache.current()`（river
prewarm 时 turn 仍在主 slot）+ 同 `prev_within` → 同 ranges → 同 key。

### 2.4 改动 D — advisor 穿线（`tools/openpoker_advisor.rs`）

- `decide` 锚定分支（:920）：不再丢 `_prev_within`，构造
  `cross_street = rt.unanchored_cross_street.then_some(prev_within.as_slice())` 传入
  `subgame_search_cached`。
- `prewarm` 锚定分支（:578）：同样构造 `cross_street` 传入 `subgame_search_prewarm`。
- **flag 复用** `rt.unanchored_cross_street`：现在它同时管锚定 + 脱锚 cross-street。flag 名保留
  （避免改 CLI / 大面积 churn），但更新 doc（`:128` / `:440`）说明它现覆盖两条路径；
  `off` = 两路都退（A/B 对照臂 / 回退）。

## 3. 兜底与降级（必须明确，不能丢）

`cross_street_posterior_range` 返回 `None`（→ 退 `estimate_range`/`prefix_ranges`）的真实情况：

1. turn search **giveup**（没存 turn 子树）；
2. turn 真实下注 off-menu → `subtree_decisions_on_real_line` 导航失配；
3. board / hand_seed 自验不过（手内不该发生，跨手会发生）。

**关键利好**：即便把 turn 从 blueprint 裁掉，`estimate_range` 是**逐决策**降级的（坏 σ→`1/n`，
:1118-1122）——river 前缀仍含 preflop+flop 决策、那些照常读真 σ。所以兜底 range =
「preflop+flop reach × turn 当 uniform」，**不是全 uniform**，preflop/flop 信息保留。

→ 裁 turn 的最坏情况：river range 丢掉 turn 下注战信息，**不崩、不退全 uniform**。

## 4. 正确性 gate（按项目「正确性优先」规则，默认开 / 裁剪前必须过）

1. **search=None / cross=None byte-equal**：新参数默认 `None`（`subgame_search` 薄壳传 None），
   锚定路径在 cross 关时必须与旧行为逐位相同。补单测。
2. **锚定跨街「本街多决策一致性」回归**（prev-slot）：镜像
   `cross_street_within_street_consistency_prev_slot`（:6759），换成锚定路径 + 真桶
   （stub 下 cross==prefix 测不出 prev-slot，真桶才有效）。
3. **跨 kind 复用确定性 + 覆盖 estimate**：真桶下构造 turn(anchored 解)→river，断言
   river `cross_ranges=Some` 且覆盖了 `estimate_range`，且固定迭代下确定性可复现。
4. **决策级 A/B（保留 turn blueprint 跑）**：扩 `six_max_cross_street_ab`（或新工具）到锚定路径，
   量 river 决策 TV / argmax 翻转：新「turn 子树后验」vs 旧「estimate_range(turn blueprint)」。
   方向应与 §动机一致（后验比断点前粗前缀准）。这是**只测 range 来源差异**的受控配对差。
   - **✅ 已做（2026-06-19，commit `56c1a12`）**：`six_max_cross_street_ab --anchored`（on-tree
     100BB flop→turn→river 线，turn 子树解入缓存，两臂只差 river 的 cross；真 1B nolimp blueprint +
     **真 200 桶**——该 checkpoint 用 200/200/200 cafebabe 表训，非 usage 例里写的 500；桶固定两臂
     一致 = 干净「只测 range 来源」）。实测 96 手×{2000,5000}iter×2 seed：**cross 触发 ~98% / river
     决策 mean TV 0.38–0.42 / median 0.38–0.41 / argmax 翻转 60–67%**，**对迭代数稳**（2000→5000
     mean 0.38→0.40 不洗掉 = 结构性信号非欠收敛噪声）、**对 seed 稳**。方向 = 逐手再优化（OFF
     blueprint estimate 过 check 的成手 ON 转价值下注 / OFF 过 jam 的空气 ON 收手），与 §动机
     「turn 子树后验更贴本子博弈」一致。诚实边界同脱锚 A/B：构造谱非真实频率（翻转率 = 给定这些
     on-tree 线）、无 EV 锚（「不同」≠「更好」）、200 桶（生产搜索另用 1000 → 量级或更大）、~35%
     deal skip（桶未访问，两臂同 skip 不偏差）。**这是与脱锚 A/B / 翻档一 ON 同一证据等级**（强机制
     + 决策级方向）；它证「裁掉 turn blueprint、river 改走 turn 后验」**确实改 river 决策且方向合理**，
     但「裁是否无损」由命中率（§4.5）+ 兜底（§3）定，非本项。
5. **复用命中率实测**：插桩数 river 决策里 `cross_ranges=Some` 占比（锚定 + 脱锚分别）。
   12s / 24 线程 / 1000 桶下 turn 一般能解出（非 giveup），命中率应高；但 off-menu 导航失配 +
   giveup 会拉低。**只有命中率足够高，裁 turn 才接近无损**——这是裁剪的 go/no-go 闸。
   - **✅ 已做（2026-06-20，commit `bc07c19`）= 锚定主体 GO**：①`SubgameSolveCache` 加
     `cross_attempts`/`cross_hits` 计数（两 inner 的 cross 块算完 cross_ranges 后、在 solve 的
     `(Some(c),Some(key))` 臂里 `record_cross`；与 solve-key 的 hits/misses 正交；单测
     `cross_telemetry_counts_attempts_and_hits` 钉空缓存→(1,0)/flop cross=None 不计/turn 复用 flop
     →(1,1)）。②`six_max_cross_street_ab --hitrate` 自对弈模式：blueprint 驱动全座生成真分布 on-tree
     手，hero 轮转、每手常驻 cache 先解 turn → river 决策 cross=Some(turn_within)，delta cache 跨街
     计数。**关键口径**：`estimate_range`（读 turn σ）在 cross 前**无条件**跑——裁 turn 后它对缺失
     key 退 uniform（§5.1），cross **命中即用后验覆盖、恢复 turn 信息**；未命中则留 uniform = §3
     退化。故命中率 = 裁剪**无损度**。**实测**（真 200 桶 + 真 blueprint，自对弈）：**锚定 river
     cross 命中率 = 100%**（nolimp 1B：25/25、232/232；preopen 10B：389/389；跨 seed/blueprint
     一致），**0 turn-giveup / 0 off-menu**。→ 裁 turn 对**锚定（on-tree）river 主体无损**（cross 永远
     恢复 turn 后验，§3 退化从不触发）。**诚实边界**：(a) 自对弈全 on-tree → off-menu 失配 ≈0 by
     construction（且 `map_off_tree` 总能映、结构性失配本就罕见）；(b) 固定 iter（1500），turn-giveup
     已 0、生产 12s/24 线程只会更少 → 命中率本读数是**下界**；(c)「river hero search 本身 giveup」
     ~46%（低 iter river 桶欠采样，正交——cross 在 solve 前已记，不影响命中率，生产高 iter 大降）；
     (d) **脱锚（off-stack/4way）river 尾自对弈触发 ~0 → 未测**，须 live（advisor 已插桩
     cross_attempts/cross_hits，开 off 臂跑 live 回填）。脱锚是 river 决策的小尾、机制同（turn 解→cross
     fire），但数值未实测确认。

## 5. 裁剪步骤（gate 全过之后，独立一步）

1. 先确认 trim 工具对「缺失 turn/river info set」的查询返回**空/零向量**而非 panic——
   `estimate_range`（:1118）与 `blueprint_distribution`（:1063）都对坏 σ 退 uniform，所以零向量
   是安全的；但要核实 dense/hashmap 查缺失 key 的具体返回，避免越界/panic。
2. 裁到 preflop+flop 后跑一遍 §4.1 byte-equal（preflop/flop 决策不受影响）+ live smoke。
3. 体积/加载收益记进 `six_max_nlhe_target.md` S6。

## 6. 风险与未决

- **跨 kind range 的栈基准不一致**：anchored turn 子树按 100BB 解，给 unanchored（off-stack）
  river 当先验略失配。判定为可接受近似（与现状 blueprint estimate 同级，且远好于 uniform）。
  若 A/B 显示 river 决策异常，再考虑「只同 kind 复用 + 异 kind 退 estimate」的保守变体。
- **prev-slot 单街回溯**：只复用紧前一街。turn→river / flop→turn 各自成立；不支持「river 直接
  复用 flop」（也不需要）。若中途插入额外 solve 驱逐 turn——本参数顺序流（turn-solve→
  river-prewarm→river-decision）不会发生，但需在插桩里盯 `cache.misses` 异常。
- **真 live EV 确认仍 pending**：与 cross-street 既有结论一致——决策级 + 机制可证，EV 靠 live
  功效不足（AIVAT 也救不动）。强弱判据继续走结构性正确 + 决策级 A/B，不等 live EV。

## 7. 落地顺序建议

A（kind 闸）→ B（锚定 inner）→ C（锚定 prewarm）→ D（advisor 穿线）→ §4.1-4.3 单测（本机
build/fmt/clippy + vultr 跑测）→ §4.4 A/B + §4.5 命中率（vultr，保留 turn blueprint）→ 判定
→ §5 裁剪。改动均在 cross 关时 byte-equal，可安全分步提交。
