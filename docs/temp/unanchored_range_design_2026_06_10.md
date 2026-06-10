# 脱锚搜索的 range 先验：从 uniform 升级的设计探索（2026-06-10）

> 状态：**探索结论记录，未实现**。背景见 `realtime_search_openpoker_exec_2026_06_08.md` §3.2 缺口②续
> （脱锚搜索落地时把 range 诚实退化为 uniform，「脱锚 range 细化」列为后置项）。本文把 2026-06-10
> 讨论出的可行路线 / 坑 / 实现要点钉下来，避免捡起来时重推导。

## 0. 现状与问题

脱锚搜索（`subgame_search_unanchored`，`src/training/subgame.rs:1721`）覆盖三类失同步场景：
off-stack all-in 线、真实 4+way、limp 池。这些正是主目标分布（深码/多人）最常见的形态，但当前
range 先验一律 uniform（`SubgameNlheGame::new` 的 root uniform resample）——理由是 blueprint reach
要沿全局树路径累乘，而该路径在失同步线上结构性不存在（100BB 树缺节点）。

**核心代码观察（本探索的支点）**：`estimate_range`（`subgame.rs:882`）的输入只是一组
`(node_id, tag, seat)` 三元组，逐个独立查 σ 再累乘——**它不需要一条连通的树路径**。锚定路径用
`decisions_on_path` 回溯只是「找到」这些三元组的方式。所以问题归结为：失同步之后，还能为哪些
历史决策配出**可辩护的** `(node_id, σ)`。

## 1. 三档方案（按可辩护程度）

### 档一：同步前缀 reach —— 干净，先做这个

失同步发生在某一个具体动作上；**之前的每一步影子都走通了，有精确的 blueprint 节点，不是近似**。
改法：lockstep 闭包（`tools/openpoker_advisor.rs:271`）失同步时不再丢弃整条路径，带回已同步前缀的
决策三元组列表 → 喂 `estimate_range`；失同步点之后的决策按无信息处理（因子 1）。

统计性质：这是**更粗的条件化，不是错误的条件化**——「给定已同步前缀的 range」是合法先验，
只是没用上后面的信息，不注入错信息（对比档二）。

按场景的覆盖增益：

| 场景 | 失同步点 | 前缀能恢复什么 |
|---|---|---|
| off-stack all-in 线（如 `offstack_allin_req` 测试场景：UTG 短码 shove → SB raise-over 时断） | raise-over 动作 | shove + 各家 fold 之前的全部 preflop 决策 = 大部分 range 信息 |
| 真实 4+way | 第 4 个进池者的 call（width_redirect 收口处） | 前 3 家的决策 |
| limp 池 | 第一个动作（open-limp 无节点） | 前缀为空 → 与现状 uniform 等价，无增益（见档三） |

**前缀内的坑（必须处理）：AllIn-tag 决策要跳过或设地板。** 前缀里 tag 为 `AllIn` 的决策，σ 语义
仍是「100BB 全栈 shove」。真数：1B nolimp blueprint 的 RFI 表里 AA 在 node 0 的 `σ[AllIn] = 0.001`
——blueprint 在 100BB 几乎从不开池 shove，而真实 30BB shove 的 range 宽得多。把这个 σ 乘进去会把
shover 的 range 错误收成「100BB shove range」（几乎只剩超强牌的微小混合），**比 uniform 更糟**。
普通 ratio 档 / 被动动作的几何失真是温和的（尺寸已按比例投影），AllIn 是 100BB 假设（`stack_bucket=0`，
`nlhe.rs:123`，exec 文档 §0.3）撒谎最狠的地方。v1 = 直接跳过 AllIn-tag 决策（因子 1）。

### 档二：失同步点之后的代理节点映射 —— 不建议默认开

给断点之后的决策找特征相近的 blueprint 节点当代理（按街 / 位置 / 本街 raise 数 / pot-odds 桶匹配）
查 σ。技术上可做，但本质是**拿可能错的信息换没有信息**：代理节点的局面结构（limp 池 vs raised 池、
SPR）与真实局面不同，σ 答的是另一个问题，且 §0.3 的批评双重生效（节点错 + 码深错）。uniform 至少
是诚实的零信息，错先验会把搜索往坑里带。若试：独立 flag + off-stack 场景 h2h A/B（uniform vs 代理）
拿到证据再说。

### 档三：limp 池 = 结构性死路，只能等对手数据

limper 的 range 在 blueprint 里**不存在**——nolimp 树剪掉了所有 open-limp 边，dense 表里没有任何
一行回答「什么牌会 limp」，任何映射都是无中生有。诚实答案 = §4.2 数据管道（HH 日志）+ 剥削加分项
（步 D）的 population 先验（limper 偏被动 / range 封顶），数据源是实测不是 blueprint。

## 2. 实现要点（管道几乎现成）

- `SubgameNlheGame::new_with_ranges`（`subgame.rs:168`）已接受任意 per-seat range 向量，锚定/脱锚
  共用同一求解核；脱锚现在只是传 `None` 走 uniform。给 `subgame_search_unanchored_cached` 加
  `Option<ranges>` 参数即可。
- **solve 缓存 key 已逐位哈希 ranges**（`solve_cache_key`，`subgame.rs:1120`；None 哈希 `[0]` 标记）
  ——前缀 reach 接进去自动进 key，不会读错均衡（缓存正确性不需要额外动作）。
- 前缀决策是请求的纯函数 → seeded 可复现 / replay / AIVAT 一致性不破。
- RoundStart 的街切分照旧适用：只累乘当前街**之前**的决策（当前街 betting 在子博弈内由 CFR 解，
  `subgame.rs:1285`）。
- advisor 侧：lockstep 闭包返回 `Err(reason)` 时附带已同步前缀的 `Vec<(NodeId, tag, seat)>`，
  经 `decide_search_unanchored` 透传。

## 3. 守护与验收

- 默认关：不带前缀 reach 时脱锚路径与现行为 byte-equal（既有测试不动）。
- 新测试钉死：①off-stack 场景下前缀 reach 产出的 range 非 uniform 且 AllIn-tag 决策被跳过；
  ②limp 池场景前缀为空 → 与 uniform 路径 byte-equal；③ranges 进 key（开/关前缀 reach 必 cache miss）。
- 强度验收走 h2h A/B（uniform vs 前缀 reach，off-stack 触发场景集），不凭直觉上生产。

## 4. 结论

**值得做的是档一（前缀 reach + 跳过 AllIn tag）**，覆盖 off-stack all-in 线和真 4+way 两类主目标
场景；档二风险大于收益、留作有 A/B 证据后的选项；limp 池的 range 升级不走 blueprint、等对手数据
（步 D）。
