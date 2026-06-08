# 执行文档：postflop 实时搜索（用户指定配置）→ OpenPoker 外部对手验证

> 日期 2026-06-08 / 分支 `6max`（代码落地另开分支）。**这是执行 runbook，不是设计探索。**
> 设计与历史判决见 `docs/temp/realtime_search_design_2026_06_03.md`（下称**设计文档**，§1–§12）。本文只管「按用户
> 2026-06-08 拍板的配置落地 + 在 OpenPoker 验证」。
>
> 用户 2026-06-08 拍板（钉死）：
> 1. **就按用户给的流程原样执行**——不裁剪、不替换成设计文档 §11.5d 之后我建议的「只测 river-finer-menu / 限 ≤3-way」。
> 2. **验证只走 OpenPoker 外部对手**；**不做任何自对弈探针强度重跑**（设计文档 §11.5d 已证 search-vs-self 探针结构上
>    答不了「加搜索绝对是否更强」——强 blueprint = 强 field，惩罚任何偏移）。
> 3. **去掉 8000 节点 cap**，改由 10s wall budget 兜底；保留一个**远高的纯防爆 OOM 后备**（非 8000 强度 cap），超限回 blueprint。
> 4. OpenPoker：api_key 已获取（运行时 `--api-key` 传入，**不入库**）；账号 jesse_xu 授权在线 **30 分钟**。

---

## 0. 用户指定流程（2026-06-08，原文转录）

> 本文「配置照搬用户流程」的**流程原文**即此节；§2「目标配置」逐项对照本节，§3 是其代码落地。
> （用户原文写「CFP」，= CFR 笔误，下文统一 CFR。）

**主流程（每次决策）：**

- **step 1 选择**：preflop 都用 blueprint，postflop 都启用实时策略。
- **step 2 Range 估计**。
- **step 3 建子树 + CFR 求解**：**根据人数建子树**——剩 3 人就建 3 人、bet size = {0.5pot, 1pot} 的子树；剩 4 人就建
  4 人、bet size = {1pot} 的子树。即 **≤3 人 → 建对应人数 bet size = {0.5pot, 1pot} 子树；≥4 人 → 建对应人数 bet size
  = {1pot} 子树**；然后迭代 CFR 算策略。
- **step 4 取策略，返回对应行动**。

**每街重复 + 街专属菜单：**

- 每街重新执行 **step 2（Range 估计）+ step 3（建子树 + CFR 求解）**；**同街复用子树**。
- **turn 街**：无论人数多少都建 bet size = {0.5pot, 1pot} 子树。
- **river 街**：无论人数多少都建 bet size = {0.33pot, 0.66pot, 1pot} 子树。

**全局：**

- 迭代次数**按时间控制，不超过 10 秒**。
- **不用 depth-limit**（2026-06-08 补充）= step 3 的 CFR **解到终局**。

> 注（归并，非原文）：step 3「按人数」菜单（≤3 `{0.5,1}` / ≥4 `{1}`）是**首次 postflop 建树（flop）**的规则；turn/river
> 用街专属菜单、「无论人数多少」明确覆盖按人数规则 → **flop 是唯一按人数变菜单的街**（§2 表同此）。

---

## 1. 边界与原则

1. **配置照搬用户流程**（§0 流程原文，§2 逐项对照）。已知近似/风险**诚实标注**（§4），但**不据此改配置**——强弱判据交给 OpenPoker。
2. **强度判据只在 OpenPoker**。**禁**：`tools/six_max_search_probe` 跑 mbb/g 判强弱（= 重跑 §11.5d Arm A，答案已知
   −652/−426 且探针 confound）。
3. **正确性 smoke 仍做、且必做**（与「禁自对弈强度测试」不冲突——这些是 plumbing / no-panic / byte-equal 守门，能让
   「会崩的接线 / 错算法」fail，符合 `feedback_less_ceremony`）：见 §6。
4. **去 8000 cap**：撤掉 `max_subtree_nodes=8000` 这个**强度兜底**，live 改由 10s wall budget + 高 OOM 后备兜底（§3-③、§4-2）。

---

## 2. 目标配置（钉死 = 代码开关；step 编号见 §0）

| 用户 step | 配置 | 现有开关 / 新代码 |
|---|---|---|
| step1 preflop blueprint | 加载 10B preopen ckpt（`run_6max_s4_preopen_n3_10b/...010000000000`，设计文档 §11.5c）| 现有 `--reshape preopen` |
| step1 postflop 全实时 | `trigger = AllPostflop` | 现有 `SearchTrigger::AllPostflop`（默认 `FlopFirstUnraised`，**须改 live 配置**）|
| step2 每街 range 估计 | `use_blueprint_range = true`（per-seat marginal，设计文档 §5.b）| 现有默认 true |
| step3 同街复用子树 | `resolve_root = RoundStart`（从下注轮起点建树、同街多决策共享同一 solve）| 现有默认 RoundStart |
| step3 按 (街,人数) 选 bet 菜单 | flop: live≤3 `{0.5,1}` / live≥4 `{1}`；turn: `{0.5,1}`；river: `{0.33,0.66,1}` | **新**（§3-①）|
| step3 解到终局（不 depth-limit）| `depth_limit = false`、`biased_leaf = false` | 现有默认 false/false（= 6a / §11.5d Arm A）|
| step3 迭代按时间 ≤10s | `time_budget = Some(10s)` | **新**（§3-③；现在是固定 `iterations`）|
| 去 8000 cap | `max_subtree_nodes` 关（live 设极大）+ 高 OOM 后备 + 时间兜底 | **改**（§3-③）|
| step4 取策略返回 | 取 actor 真实手 root infoset 分布、按 tag 对齐 legal | 现有 `subgame_search` 返回值 |
| 验证 | OpenPoker live（外部对手）| 现有 client（§5），advisor 切 search-on（§3-⑥）|

---

## 3. 代码触点（逐个：file:line / 改什么 / 为什么）

文件根 `/home/shaopeng/dezhou_20260508`。

**① 按 (街, live 人数) 选 bet 菜单（新，核心改动）**
`subgame_search`（`src/training/subgame.rs`）现把 `game.abstraction().clone()` 透传给 `SubgameNlheGame::new`
（`subgame.rs:130-143`）。改为：root 街 + root 处 live 人数已知时，**现构造 `StreetActionAbstraction::per_street`**
（`src/abstraction/action.rs:641`）：
- `[preflop(占位), flop_menu(live), {0.5,1}, {0.33,0.66,1}]`，`flop_menu(live) = live<=3 ? {0.5,1} : {1}`。
- 解到终局 → 一次 flop solve 内部也建 turn/river 节点，走数组里 turn/river 槽（count-independent），自洽。
- `BetRatio`：`{0.5,1}` = `HALF/FULL`、`{1}` = `FULL`（`action.rs:58-60`）；`{0.33,0.66,1}` 的 0.33/0.66 用
  `BetRatio::from_f64` 量化。live 取 root（round-start）处 `live_count`（`src/training/nlhe_betting_tree.rs:533`）。

**② 放开 width_redirect 以建 ≥4-way 子树（新，关键正确性，否则 panic）**
`build_subtree` 走 `filter_actions`，有不变量 `width_redirect==OFF || live_count<=width_redirect`
（`nlhe_betting_tree.rs:379-385`）。生产 blueprint `width_redirect=3`（A4，postflop 收口 ≤3-way）。**直接建 4-way
postflop 子树 → `live_count=4 > 3` → debug 断言炸 / release 未定义**。改：subgame 建树时 `rules.width_redirect`
设 `WIDTH_REDIRECT_OFF`（`nlhe_betting_tree.rs:123`）——redirect 是 **preflop 进场**规则，subgame root 在 postflop、
preflop 已定，关掉不改 postflop 过滤（`filter_actions:580-584` 的 block_passive 仅 redirect!=OFF 时算），只是让 ≥4-way 树合法建出。

**③ 10s wall budget 取代 8000 cap + 高 OOM 后备（新 + 改）**
- 加 `SubgameSearchConfig.time_budget: Option<Duration>`（`subgame.rs:650-689`）。
- 求解循环（`subgame.rs:1056` cap 检查之后的 `for _ in 0..cfg.iterations`）：`Some(d)` 时改 while，每 N（如 256）iters 查
  `Instant::now()`，超 `d` 停；`None` 走旧 `for 0..iterations`（**保留确定性路径供测试 byte-equal**，§4-4）。设**最小迭代地板**
  （如 ≥1000，机器抖动也别只跑几次）。
- 去 cap：live 配置 `max_subtree_nodes = usize::MAX`，`subgame.rs:1056` 的 `n_nodes > cap` 越界回落对 live 失效。
- **高 OOM 后备（用户已定）**：保留一个**远高于 8000 的纯防爆上限**（建议默认 **200_000**，非强度 cap），建树后 `num_nodes`
  超它 → 回 blueprint（守 `正确性大于一切`：6-way 解到终局别 OOM-crash live）。建议复用 `max_subtree_nodes` 字段语义但 live 值
  设 200k（而非 8000）—— 8000 是强度 cap（去掉），200k 是 OOM 后备（保留），二者数值区分清楚即可。
- **Instant 从 subgame_search 入口起计**，覆盖建树+求解；10s 内 0 次完整迭代 / 建树超 OOM 后备 → 回 blueprint。

**④ trigger / resolve / range（现有，设 live 默认）**
`Contestant.search`（`src/training/blueprint_advisor.rs`）填 `SubgameSearchConfig{ trigger: AllPostflop,
resolve_root: RoundStart, use_blueprint_range: true, depth_limit: false, biased_leaf: false, time_budget: Some(10s),
max_subtree_nodes: 200_000, .. }`。

**⑤ 不接 leaf value 表（现有，确认）**
解到终局 → 不需 `src/training/subgame_leaf_value.rs` / `LeafValueTables`，`leaf_values=None`、`depth_limit=false`。
绕开 §11.5b leaf-miss / §11.5d biased 净害。**别建叶子表**（省内存省时）。

**⑥ OpenPoker advisor 接 `subgame_search`（新接线）**
`tools/openpoker_advisor.rs::decide`（`:174`）现：重放历史建 `real: GameState`（`:218`）+ 抽象影子 `abs`
（`:236 advance_shadow_by_applied`）→ 查 blueprint `dist`（`:277` 附近）→ `sample_discrete`（`:279`）→ `outgoing_action`（`:282`）。
改：`dist` 之前插 search 分支（**与 `blueprint_advisor.rs:421` 自对弈插桩同构**）：

```text
if should_search(&real, trigger) {                           // postflop 触发（AllPostflop）
    subgame_search(&real, actor, &abs, &legal_abs, &cfg, ...) // 用 real 当 auth、abs 取 node_id/legal
        .unwrap_or_else(|_| blueprint_dist(...))              // 任一失败回 blueprint（同现有 fallback 哲学）
} else { blueprint_dist(...) }                               // preflop 等非触发点纯 blueprint
```

stateless advisor 每决策重放整手 → 可现算 `subgame_search` 要的：round-start 快照（重放到本街起点）、`node_id`（影子当前
节点）、`decision_ordinal`（历史长度）、`strategy_fn`（advisor 已加载 trainer，`:504`）。失败仍落 `source=fallback:<reason>`。
CLI 加 `--search`（off 时 byte-equal 现行为）。

---

## 4. ≥4-way 的已知近似与正确性边界（诚实标注，用户已接受执行）

1. **blueprint 抽象 postflop 只有 ≤3-way**（A4 width_redirect=3）。real 4-way+ flop 对外部对手**会发生**（OpenPoker 实测同桌
   14–800BB、4+ 进 flop 常见）。这一支：
   - **range 估计**：4-way postflop infoset 在 dense 表里塌进与 ≤3-way 同 `(bucket,pos,betting_state,street)` 的 infoset
     （InfoSetId 不编码 live 人数，`src/abstraction/info.rs`）→ **不崩、有损**（拿 ≤3-way 训练出的 σ）。
   - **子树本身**：靠 §3-② 放开 width_redirect 才建得出；菜单 `{1}`（用户指定）控宽度。
   - **结论**：4-way+ 是「blueprint 抽象外硬解」（设计文档 §5.e 原警告区），用户已知并选择执行；OpenPoker 指标按 flop 见牌人数
     分桶单看这一支。
2. **去 cap → 大树欠训练 / 构建耗时**：6-way flop→river 解到终局即便 `{1}`/`{0.5,1}` 也可能很大。10s 内迭代摊薄 → 深层近 uniform
   （§10.5 同病，这里无 depth-limit 兜底）。`time_budget` 至少保证 **10s 内返回**（不 hang OpenPoker）；**高 OOM 后备（200k）防建树
   爆内存**（§3-③，用户已定）。欠训练表现进 OpenPoker 指标。
3. **flop/turn 菜单 = blueprint 菜单 `{0.5,1}`**（`nlhe_betting_tree.rs:138`）→ 那两街是「同抽象上、marginal-range 近似下局部
   重解 blueprint」；river `{0.33,0.66,1}` 是唯一比 blueprint 更细处。这是 §11.5d 判负的同一 regime——**但本次对外部对手测**，正是
   探针答不了的那个问题，故执行有意义。
4. **可复现**：`time_budget` 路径迭代数随机器变 → **不 byte-equal**（live 用、可接受）。**测试一律走 `time_budget=None` + 固定
   `iterations`**，守既有 byte-equal 契约（§6）。
5. **off-tree**：river 用了 blueprint 没有的 `{0.33,0.66}` → 选出后经 `map_off_tree`（6c PHM，设计文档 §12）翻成真实 to、夹
   `your_turn` 的 `[min_raise,max_raise]`（见 OpenPoker client 设计 §5）。incoming 对手任意 size 仍走 `advance_shadow_by_applied`。

---

## 5. OpenPoker 验证口径（复用现有 client）

client 详情见 `docs/temp/openpoker_client_design_2026_06_02.md`（下称 **client 文档**）。

- **复用**：`tools/openpoker_advisor.rs`（§3-⑥ 切 search-on）+ `tools/openpoker_play.py`（WS driver，无策略，不改）。
  账号 jesse_xu 已注册、live smoke 通过（client 文档 §9）。driver 真实联机：`--api-key <key> --checkpoint <preopen ckpt>
  --bucket-table <200桶> --reshape preopen --num-hands <N>`（api_key 运行时传，不入库）。
- **超时安全**：OpenPoker action 超时 **120s**（client 文档 §1）≫ 我们 10s budget → **无超时风险**。
- **指标**（driver/advisor 落日志聚合，`openpoker_actions.jsonl`）：
  - **mbb/100 + CI**，**按我方开局有效栈分桶**（码深漂移 client 文档 §4：只信近 100BB 桶 [80,125]BB；买入锁 2000、漂出区间
    `leave_table` 重 join）。
  - **per-position**（盯 SB/BB——§10.4/§10.5/§11.5d 退化一致集中盲位）。
  - **按 flop 见牌人数分桶**（单看 ≥4-way 抽象外支，§4-1）。
  - **运维**：search 触发率 / fallback 率 / `time_budget` 耗尽率 / 建树 abort（OOM 后备）率 / desync 计数。
- **A/B（search-on vs blueprint-only）**：免费号只 1 bot（client 文档 §8）→ **只能分时段轮换**（场漂、噪声大）。先各自长跑取**绝对
  mbb/100**（近 100BB 桶）对比；并行受控比较需 Pro，后置。**单账号 30 分钟窗口**先做哪个见 §7。
- **可选**：多对手 AIVAT 降方差（`project_aivat_slumbot_eval`：单边 P_a={chance,我方} 即无偏），bot 池方差大时再上，后置。
- **诚实标注**（报告必写）：码深漂移 + bot 池漂 + 单号分时段 → OpenPoker 数是「近 100BB 场近似强度」，非干净 100BB 基准。

---

## 6. 正确性 smoke（vultr；非强度测试）

本机仅 `cargo build --all-targets` / `cargo fmt --all --check` / `cargo clippy --all-targets -- -D warnings`。
行为正确性一律 vultr（`feedback_tests_on_vultr`）：

- 4-way postflop 子树**建得出 no-panic**（§3-②：width_redirect OFF 后 live=4/5 建树不炸）。
- `time_budget=Some(d)` 路径**≤d 内返回**、分布归一、动作全在 legal；`time_budget=None` 路径与改前 **byte-equal**（守 §4-4 契约）。
- 去 cap 后大树**不越界回落**（live 配置）、**超 200k OOM 后备回 blueprint**、小树仍正常。
- 真 10B preopen ckpt 一手 live-replay：`desync=0 / illegal=0`，search 真触发、fallback 合理。
- **不跑** `six_max_search_probe` 判强弱（§1-2）。

---

## 7. 执行步骤（有序）+ 30 分钟窗口用法

**现实顺序**：search-on **不能 live 测**，直到 §3 代码写完 + vultr smoke 过。30 分钟 OpenPoker 窗口是 live-test 预算。

1. **（窗口内、即可做）blueprint-only baseline + key 验证**：用现有 client（已 committed，vultr 可直接跑）blueprint-only 挂场，
   既**验 api_key/账号 live 可用**、又**banked baseline**（A/B 必需、单账号本就要时分）。监控前几手确认连上、0 崩，再让它累积。
2. **代码**（§3 ①–⑥，本机 build/fmt/clippy）。
3. **vultr 正确性 smoke**（§6）。代码改动 push → vultr fetch/reset（`feedback_vultr_sync_via_git`，禁 rsync）。
4. **（后续窗口）search-on live**：smoke 过后，用下一个在线窗口跑 search-on 挂场，落 §5 指标。
5. **主机**：advisor 单决策 ≤10s solve；去 cap 的 4–6way 解到终局可能吃多核/内存 → 视实测子树规模定 vultr 4 核够不够，或按
   `feedback_high_perf_host_on_demand` 申请更大机（先报预算 + provider，别假设旧 host 还在）。

---

## 8. Go / No-Go

- **操作门（必过，否则停下查）**：无超时 / 无崩 / desync=0；fallback、`time_budget` 耗尽、建树 abort（OOM 后备）率在可接受范围；
  4-way 支不 OOM。任一不过 = plumbing bug，先修（`feedback_correctness_no_carveout`：偏离 ≥10× 立即追根因）。
- **强度信号（OpenPoker，近 100BB 桶）**：search-on mbb/100 vs blueprint-only；**CI 分离才下结论**。单号分时段 + bot 池漂噪声 →
  绝对水平判断须够手数（数千手量级起）。
- **诚实结论形态**：「近 100BB 近似强度下，postflop 全实时（用户配置）相对 blueprint-only 是 +X / −X / 不可分辨 mbb/100，≥4-way 支
  单独为 Y」——不夸张成干净 GTO 实测。

**状态：待执行**（§3 代码未写；OpenPoker baseline 可即跑）。
