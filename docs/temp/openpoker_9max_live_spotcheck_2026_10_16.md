# OpenPoker 9-max 实盘抽查（2026-10-16）

## 1. 部署与参数

- 机器：AWS 54.89.242.250（us-east-1，8 vCPU / 15.6 GiB，无 swap，Ubuntu 26.04），用户 `ubuntu`，目录 `~/openpoker/`。
- advisor binary：本机 HEAD `c54c2f1` 编译（`target/release/openpoker_advisor`，只依赖 libc / libgcc_s）。
- blueprint：`artifacts/run_9max_all1pot_cap4_b500/nlhe_9max_all1pot_cap4_b500_8b_preflop_flop.crop`
  （8B update，只含翻前 + 翻牌；sha256 `559779aa…f898` 远端校验通过）。
- bucket 表：`bucket_table_default_500_500_500_seed_cafebabe_schemav4.bin`（sha256 `650bb51f…2927`）。
- 启动脚本 `~/openpoker/run.sh`（读 `.env` 里的 `API_KEY`），tmux 会话 `openpoker`，日志 `~/openpoker/play.log`：

```bash
python3 -u tools/openpoker_play.py --seats 9 \
  --cropped-checkpoint <上面的 crop> --bucket-table <上面的 bucket 表> \
  --reshape all-1pot --postflop-cap 4 \
  --search --search-trigger all-postflop \
  --search-flop-prefer-blueprint on --search-deep-menu --search-prewarm \
  --search-max-nodes 100000 \
  --api-key "$API_KEY"
```

- 03:15 UTC 首次启动（`max_nodes` 默认 8000），03:28 UTC 改 `--search-max-nodes 100000` 重启。
- advisor 常驻 RSS 约 9.5 GB，整机剩约 5.6 GB 可用，跑了一个多小时没有增长。没有 swap，大子树建树时有被 OOM 杀掉的风险。
- 抽查脚本：`~/openpoker/spot_check.py <actions.jsonl> <hh.jsonl> [since_unix_ts]`，逐条打印我方决策（来源 / 概率 / 实发动作），并标出可疑项。

## 2. 问题 1（严重）：搜索解的平均策略缺失时，按均匀分布出牌

### 现象

转牌、河牌实时搜索返回的 `probs` 是**精确均匀**的（4 个动作各 0.25，或 5 个动作各 0.2），advisor 照这个分布抽样出牌。

本次实盘（截至 2026-10-16 约 04:00 UTC）：**22 次搜索决策里有 6 次是均匀分布（27%）**。

| hand | 底牌 | 公共牌 | 均匀分布的动作 | 实际抽中 |
|---|---|---|---|---|
| f2c05796 | AcJc | 5d Js Ts 9c | check / bet0.5pot / bet1pot / allin | **allin**（93BB 推约 18BB 底池，对手 Q8 成顺跟注，-87BB） |
| 5d5d1df3 | 4sKs | 9h 6d 8s 6h | check / bet0.5pot / bet1pot / allin | check |
| 58c1e2a7 | ThJh | Ah Qc Td | fold / call / raise1pot / allin | **allin** |
| b7754980 | JdAd | Jc 4h 9d | fold / call / raise0.5pot / raise1pot / allin | call |
| b7754980 | JdAd | Jc 4h 9d 9h | 同上 | **fold**（顶对顶踢脚弃牌） |
| a89e1be1 | Js4s | Kc 7h 8h | fold / call / raise0.5pot / raise1pot / allin | raise0.5pot |

对照：6 月那次 9 人桌（`openpoker9_actions_searchon1000_prewarm_par4_b500.jsonl`，完整 blueprint、
翻后锚定搜索为主）1242 次搜索决策里只有 6 次均匀（0.5%），其中 22 在 4sJdTs 推全下、99 在 7h7s4c 推全下、
A8 在 7hAdKc 面对下注弃牌。**本次 crop blueprint（转牌 / 河牌全靠搜索）下比例高了约 50 倍**，原因未查。

### 根因（代码层）

1. `read_current_strategy`（`src/training/subgame.rs:2762`）只在 `average_strategy` 返回**空 Vec** 时报错放弃
   （`subgame.rs:2777`，注释：「当前桶未被访问 → Err」）。
2. `EsMccfrTrainer::average_strategy`（`src/training/trainer.rs:617`）只要该 infoset 在 **regret 表**里有记录就不返回空，
   而是转给 `StrategyAccumulator::average_strategy`。
3. `StrategyAccumulator::average_strategy`（`src/training/regret.rs:318`）在 strategy_sum 里**没有记录或总和为 0** 时
   返回均匀分布 `1/n`。
4. 本实现的 ES-MCCFR 里，strategy_sum 只在 hero 当遍历者时累加，regret 空槽在任何遍历者路过时都会建
   （见下一小节）。hero 的 infoset 只被对手遍历路过 → regret 有（全 0）、strategy_sum 无 → 第 3 步返回均匀
   → 第 1 步的检查放过了它。

这违反了 `subgame.rs:957` 的约定：「**绝不**把缺失的 σ 当『未访问 → 均匀』吞掉」。完全没访问到的情况
（regret 和 strategy_sum 都没有）会被正确拦下，本次实盘出现过 1 次：
`search_giveup:unanchored:subgame 当前决策 infoset 未被 CFR 访问`。

### 为什么浅节点也会缺策略（离线重放确认）

本实现的 strategy_sum **只在 hero 自己当遍历者时**累加（`trainer.rs:732-733`，
按 `pi_trav × σ`），而 regret 空槽在**任何遍历者**路过时都会建（`trainer.rs:722` `get_or_init`）。所以
「regret 有、strategy_sum 无」= 该 infoset 只在对手遍历时被路过，hero 遍历时一次都没到。缺的不是树的深度，
是**牌这一维的采样**：

1. 子博弈每次迭代从 range 里**重新抽 hero 的底牌**（`subgame.rs:619-622` `sample_holes_from_ranges`），
   hero 的真实手不固定。要解出真实手所在桶的策略，hero 当遍历者时得抽中这个桶。
2. 生产配置 `live_traversers=false`：遍历者在 **9 个座位**里轮转，弃牌座位轮到时零学习
   （`subgame.rs:915-924` 注释）。单挑底池里 1000 次迭代只有约 2/9 有效，hero 当遍历者约 **111 次**。
3. hero 的 range 摊在几百个转牌桶上，111 次里一次都没抽中真实桶的概率很高 → 均匀。

另有一类是**对手走了解里几乎不走的线**（Js4s 那手：hero 翻牌下注 → 对手加注 → hero）：对手 σ(加注) 极小，
hero 遍历时对手几乎不抽到加注，hero 就到不了这个节点。这一类多跑迭代帮助很小（推断，未直接打印对手 σ 核实）。

离线重放（远端 `~/openpoker/replay.py`，把 6 个均匀请求原样喂给 advisor，seed 1）：

| 请求 | 生产 1000 | `--search-live-traversers` 1000 | 只加迭代 9000 | live + 20k（seed 1 / 7） | live + 100k |
|---|---|---|---|---|---|
| AcJc 5dJsTs9c | 均匀 | allin 0.59 | allin 0.62 | check 0.82 / 0.27 | **check 0.96** |
| 4sKs 9h6d8s6h | 单独重放不均匀 | check 0.85 | bet0.5 0.70 | check 0.46 / 0.67 | check 0.82 |
| ThJh AhQcTd | 均匀 | call 0.66 | call 0.81 | call 0.95 / 1.00 | call 0.99 |
| JdAd Jc4h9d | 均匀 | allin 0.47 | allin 0.30 | allin 0.46 / 0.52 | raise1pot 0.36 |
| JdAd Jc4h9d9h | 均匀 | call 0.66 | call 0.60 | call 0.98 / 0.53 | call 0.83 |
| Js4s Kc7h8h（对手加注线） | 均匀 | **均匀** | **均匀** | **均匀 / 均匀** | fold 1.00 |

结论：

- 1000 次迭代离收敛很远：均匀只是最极端的表现，非均匀的输出也很不稳（20k 下两个 seed 都能差很多）。
- `--search-live-traversers` 是已有开关，单挑底池等于有效迭代 ×4.5，生产没开。
- live + 100k 单线程每次 solve 2.0–6.5 s（6 个请求实测），远低于 120 s 超时。

### 修法

1. **兜底检查（已修）**：`read_current_strategy`（`src/training/subgame.rs`）在「完全未访问」之外，把三种
   **均匀占位**状态也判为未解出 → `Err` → 走现有 giveup 兜底：
   - strategy_sum 无记录：只被对手遍历路过（regret 全 0）；
   - strategy_sum 总和为 0：hero 遍历到过，但自身 reach 恒 0（例：advisor 测试里 QQ 河牌面对下注，regret 已学到
     `[fold +1375, call +5575, raise −22646, allin −46025]`，strategy_sum `[0,0,0,0]`）；
   - 平均策略恰好均匀：只被 hero 遍历过一次（首次遍历 regret 全 0，按均匀出牌累加进 strategy_sum）——ThJh、
     JdAd Jc4h9d9h 两例即此类（strategy_sum 分别为 4×0.166、5×0.2）。只按「无记录或总和为 0」修，这两例照样均匀。

   试过「占位时改用 regret 换算的当前策略」，已否决：一次访问后的当前策略 = 一个随机对手手牌的最佳应对，
   重放里 ThJh 给 raise1pot 1.00，而 live + 100k 收敛结果是 call 0.99。
   单测 `read_current_strategy_rejects_undefined_average` 锁住全部状态（关掉检查即失败）。三个旧测试此前在 300 次
   迭代下靠均匀占位「通过」，迭代已上调：`cropped_blueprint_turn_river_search_only` → 1000、
   `anchored_cross_street_changes_solve_key` → 1000、`cross_street_changes_solve_key` → 3000（它们测缓存 key /
   crop 一致性，与策略质量无关）。修后生产配置（1000 次）重放 30 个实盘搜索请求：均匀输出 0。
2. **加有效迭代**（配置，主修复）：`--search-live-traversers` 已改为 advisor 默认开（`DEFAULT_SEARCH_LIVE_TRAVERSERS`，
   `--search-live-traversers off` 显式关；库层 `SubgameSearchConfig::default()` 仍为 false，离线 A/B 工具与旧测试基线不变），迭代 10 万量级 + `--search-time-budget-ms`
   当上限 + `--search-solve-threads` 并行。兜底检查只能把乱出牌换成保守动作，迭代够才有好策略。
3. **对手走冷门线**（Js4s 类）：靠 1 兜底；根治需要在对手实际动作后从当前节点重解，另议。
4. 未查：`cropped_blueprint_turn_river_search_only` 开 `live_traversers`（1000 / 3000 次）反而解不出 QQ 那个点，
   原因未知。

### 10 万次迭代耗时实测（54.89.242.250，8 vCPU）

远端 `~/openpoker/bench.py`：本次实盘全部 30 个搜索请求（17 个 2 人底池 / 10 个 3 人 / 3 个 4 人）逐个重放，
`--search-iterations 100000`，每请求 driver 侧计时（含建树、跨街补解；同街第二次决策命中缓存约 0 ms）。

| 配置 | 中位数 | p90 | 最大 |
|---|---|---|---|
| 不开 live，单线程 | 965 ms | 2443 ms | 2545 ms |
| 开 live，单线程 | 2457 ms | 5897 ms | 6517 ms |
| 开 live，4 线程 | 1139 ms | 2316 ms | 2533 ms |
| 开 live，8 线程 | 778 ms | 1767 ms | 1988 ms |

- 同样 10 万次，不开 live 反而快：7/9 的迭代轮到弃牌座位，几乎不花时间，也不学东西。开 live 的 10 万次全是有效迭代，
  单挑底池相当于不开时的 45 万次。
- 3 人底池比 2 人慢约 1.5–2 倍。4 人底池有一手子树 116721 节点超 cap，直接放弃（约 220 ms）。
- 8 线程比 4 线程只快约 30%（8 vCPU 多半是 4 物理核超线程）。
- 开 live 单线程 10 万次时，ThJd 9s8c5s（3 人）有一次仍报「当前决策 infoset 未被访问」，其它配置下正常 —— 冷门线即使
  10 万次也可能走不到，兜底检查必须有。

## 3. 问题 2（小漏洞）：面对平跟入池，`limp_heuristic` 弃得太紧

blueprint 是 no-limp 训练的，对手平跟后走 `limp_heuristic`（`tools/openpoker_advisor.rs:1786`）。
本次大量 `limp_heuristic:fold`（130 次决策里 24 次），例如按钮位 K9o / Q8o / 96s 面对一家平跟弃牌、
JTs 面对平跟 + 加注弃牌。按钮位 K9o 面对一个平跟正常应该隔离加注或跟着平跟。已知短板，单次损失小，暂不修。

## 4. 子树节点上限

- 默认 `max_nodes=8000` 时，单挑池子在翻牌、转牌就超限（66376 / 49489 / 21744 节点），直接放弃搜索走兜底。
- 改为 100000 后：重启以来 36 次 solve / prewarm 最长 116 ms（多数个位数毫秒），只出现过 1 次超限（116721 节点）。
- OpenPoker 每步超时 120 秒，按现有表现时间不是瓶颈；内存是更紧的约束（见第 1 节）。

## 5. 待办

1. ~~修问题 1：兜底检查 + 单测~~（已修，见第 2 节修法 1）。
2. 用 `--search-live-traversers --search-solve-threads 4 --search-iterations 100000 --search-time-budget-ms 3000`
   重新上线，统计兜底触发频率。
3. 考虑给机器加 swap，或换 32 GB 机器。
4. 第 2 节修法 4：live_traversers 下 crop 测试解不出 QQ 节点的原因。
