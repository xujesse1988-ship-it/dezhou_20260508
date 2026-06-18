# 会话总结：A3hh 深码多人 turn 搜索"收敛"调查（2026-06-15）

> ⚠️ 本文是**过程记录 + 自我质疑**，不是定论。用户要求："不要轻易下结论，我猜中间肯定有什么弄错了。"
> 下面刻意把「做了什么 / 看到什么」（事实层）与「我据此下的判断」（解释层）分开，并单列 §5「可能错 / 没证实」。
> 结论倾向：**事实层数据基本可信，但我中途下的几个判断过度了，§5 的 E1–E3 是最该收回或回查的。**
> → **2026-06-15 续（下午）**：用埋点实测把 E1–E5 钉死/修正，见 **§8**。一句话：E2 已解、E5 修正、E3 证伪、E1 校准为「高算力趋势=call 但未稳收」；并纠正 §7 第 3 条「该节点没被采样到」的说法（其实**被访问了**，饿死的是 average 不是 regret）。

## 1. 起因

用户在另一台机器跑 openpoker live，参数：
- `--checkpoint artifacts/run_6max_s4_preopen_n3_10b/nlhe_es_mccfr_final_010000000000.ckpt`
- `--bucket-table …200_200_200…`（blueprint 桶=200）
- `--reshape preopen`
- `--search --search-trigger all-postflop --search-time-budget-ms 8000 --search-lcfr --search-max-nodes 4000000`
- `--search-solve-threads 12 --search-prewarm`
- `--search-bucket-table …default_500_500_500…`（search 桶=500）
- `--search-deep-menu`

某一手：
- hero seat5（BB）持 **Ah 3h**，board **4s 7c 5h Jh**（turn）
- 3-way，深码极不对称：BTN seat3≈9637、MP seat1≈735、hero≈1851；盲注 10/20
- 动作：preflop MP raise50 / BTN call / hero call；flop 三家 check；turn hero bet160 → MP call → BTN raise320 → hero 决策
- **live 输出**：allin 63.4% / raise½ 29.1% / call 0.03% / fold 7.4%，`source=search`，`info_set=55642503910522991`，`solve_updates=1489344`

用户问：**这个决策收敛了吗？** 后续追问：3 人能不能收敛？不同 seed 会不会收敛出不同决策？为什么 6M 反而没解出来？

牌力客观事实：Ah3h 在 4s7c5h Jh = 坚果同花听牌（9 张红心→Ah nut flush）+ 轮子卡顺（3 张非红心 2→A2345）≈ **12 clean outs**。

## 2. 实验设置（vultr 4-core，HEAD 40c4ddf）

把 live 那条 Request JSON 存成 `spot_a3.json`（已核对 = live 第三条 req，info_set 复现一致），喂 `openpoker_advisor` stdin bin，扫 iteration × seed × 桶表。

**与 live 的差异（= 潜在偏差源，务必记住）**：
| 维度 | 我的复跑 | live |
|---|---|---|
| budget 轴 | 固定迭代 `--search-iterations` | 时间 `--search-time-budget-ms 8000`（→iterations=MAX） |
| 线程 | 4（配 vultr 核数） | 12 |
| seed | 自选 1..5 | 未知 base_seed（**没复现**，故复现不出 63/29/0/7 本身） |
| prewarm | 无（stdin bin 无此 flag） | `--search-prewarm` |
| 其余 flag | 对齐 | — |

→ 我研究的是**这个节点的收敛性质**，严格说**不是 live 那一次决策的精确重放**。

## 3. 原始数据

### 3a. 500 桶 search（`a3_convergence_sweep.tsv`）
| iters | seed | fold | call | r½ | r1 | allin | chosen |
|---|---|---|---|---|---|---|---|
| 1.5M | 1 | .126 | .353 | .009 | .009 | **.504** | allin |
| 1.5M | 2 | .109 | .346 | .109 | **.328** | .109 | raise½ |
| 1.5M | 3 | .026 | .369 | .026 | .026 | **.554** | allin |
| 1.5M | 4 | .2 | .2 | .2 | .2 | .2 | fold（**uniform**） |
| 1.5M | 5 | .2 | .2 | .2 | .2 | .2 | raise½（**uniform**） |
| 6M | 1 | .2 | .2 | .2 | .2 | .2 | raise1（**uniform**） |
| 6M | 2 | .062 | **.455** | .006 | .121 | .356 | raise½ |
| 6M | 3 | **.670** | .314 | .016 | .000 | .000 | fold |
| 24M | 1 | .003 | **.860** | .003 | .003 | .131 | call |
| 24M | 2 | .041 | .044 | .007 | .118 | **.791** | allin |
| 24M | 3 | .152 | **.847** | .000 | .000 | .000 | call |

### 3b. 200 桶 search（`a3_convergence_sweep_b200.tsv`）
| iters | seed | fold | call | r½ | r1 | allin | chosen |
|---|---|---|---|---|---|---|---|
| 1.5M | 1 | **.551** | .152 | .099 | .099 | .099 | call |
| 1.5M | 2 | .020 | .313 | .034 | .082 | **.551** | allin |
| 1.5M | 3 | .059 | .242 | .011 | .042 | **.646** | allin |
| 6M | 1 | **.476** | .181 | .261 | .026 | .057 | call |
| 6M | 2 | .2 | .2 | .2 | .2 | .2 | raise½（**uniform**） |
| 6M | 3 | .133 | **.546** | .046 | .059 | .216 | call |
| 24M | 1 | **.474** | .207 | .244 | .024 | .051 | call |
| 24M | 2 | .023 | **.959** | .017 | .000 | .000 | call |
| 24M | 3 | .001 | **.998** | .000 | .000 | .001 | call |

### 3c. 决定论探针（seed2，6M，200 桶，复现性；`probe_det.tsv`）
| label | threads | info_set | 输出 |
|---|---|---|---|
| rep1 | 4 | 55642503910522991 | .2×5 |
| rep2 | 4 | 55642503910522991 | .2×5 |
| rep3 | 4 | 55642503910522991 | .2×5 |
| single | 1 | 55642503910522991 | .2×5 |

## 4. 我中途下的判断 + 自评可信度（🟢稳 / 🟡待验证 / 🔴可能下早了）

- 🟢 **`solve_updates` 重复（turn 两决策都 1489344）= 同街缓存复用，非 bug。** 现象 + 缓存设计支持。
- 🟢 **8s/500 桶这套在这类节点没收敛。** 同算力换 seed 输出天差地别（500@1.5M：allin / raise½ / allin / uniform / uniform）。数据很硬。
- 🟢 **`uniform .2×5` = 该节点这次 solve 没被采样到（返回零信息默认）。** 现象层硬。
- 🟢 **探针：uniform 可复现、单线程也 uniform、info_set 正确。** → 非线程竞争、非 node_id 映射错、确定性。硬。
- 🟡 **换 200 桶没"根治"。** 200@24M 仍 seed 间不一致 + 6M 还吐过 uniform；但"根治"标准我没量化。
- 🟡 **节点结构性低 reach → 饿死。** 合理假设，但**没实测 reach**，是从 uniform 反推。
- 🔴 **"重心=call，live 的 jam 是欠收敛次优 / 非 GTO"。** 最该收回。见 §5 E1。
- 🔴 **非嵌套机制 = 批调度（subgame.rs:844）。** 解释链没闭合，被单线程结果反证一部分。见 §5 E3。
- 🔴 **"该走 giveup 却 emit uniform"。** 没真正 trace 到出 uniform 的消费代码。见 §5 E2。

## 5. ⚠️ 哪里可能错了 / 没证实（本文重点）

### E1 — "该 call、live jam 错了"是过度结论（最该收回）
- 我把 **500 桶和 200 桶读数混在一起**数"4/6 call"——但两者是**不同抽象**，策略不可直接合并/投票。
- 每个（桶, 算力）格只有 3 seed，样本极小。
- 自相矛盾：这些读数**本身就被我判为没收敛**，却用它们投出"该 call"。
- **没有 EV / exploitability 输出**，完全无法支撑"call > jam"。12-outs 坚果听牌 jam 半诈唬本就是合法、可能最优的线。
- ✅ **正确表述**：具体频率（63%）不可信；但"jam 这个动作错了"**我证明不了**，不该暗示 live 打错。

### E2 — uniform 的代码路径没钉死（引用可能指错实现）
- 我引 `nlhe_dense_trainer.rs:343-350`（dense trainer，未 touched 返回**空 Vec**）。
- 但 grep 显示有**多个 `average_strategy` 实现**（trainer.rs:126/617，regret.rs:318，dense:343）。搜索 solve 实际走哪个**未确认**；
  HashMap 版对未见 infoset 返回的是 **uniform `[1/n]`** 而非空。所以"`.2×5` 到底从哪段出来、是空→上层兜 uniform 还是直接 uniform"**我没 trace 实**。结论（没采样到）大概率对，但**引用的 file:line 可能是错的实现**。
- 更刺眼的矛盾：早先调研说"空→`search_giveup`→check-when-free"，可这里 `source=search` 出 uniform、**没 giveup**。这个矛盾我**没解释**。
  → **这是最该回去查清的代码点**：搜索 solve 用哪个 average_strategy？未访问到底 emit uniform 还是 giveup？为何没 giveup？

### E3 — "1.5M 解了、6M 没解 → budget 改轨迹"的因果没闭合
- 唯一"解了"的数据点是 **4 线程-1.5M**（allin .55 等）。我**没有单线程-1.5M**。
- 完全可能：**单线程在任何 budget 都 miss 这个节点**，而"1.5M 解了"只是 4 线程那一次的脆弱偶然。果真如此，
  则"1.5M vs 6M"根本不是干净的 budget 对比，我讲的"批调度（844 行）导致非嵌套"就**站不住或不完整**。
- 要钉死需补：单线程-1.5M、以及同 (seed,budget,threads) 复跑是否 byte-equal。**没做。**

### E4 — 复现保真度（见 §2）
- 没复现 live base_seed、用了不同 budget 类型/线程、少了 --search-prewarm。结论方向可能一致，但严格说不是同一计算。

### E5 — "低 reach 饿死"一个根因解释了太多现象，有过度统一风险
- 低算力 uniform、6M uniform、跨 seed 分裂、跨桶分裂——全被我归到"节点稀 + 采样不保证"一个故事。
- 可能其中某个其实另有原因：deep-menu 在这个动作序裁了档？off-tree 映射？子树构造对深码不对称 side-pot 的处理？**没逐一排除。**

## 6. 要变成定论，需要补的实验

1. **EV 标尺**：给这个节点的 call vs jam 各做 MC rollout / best-response 估 EV——否则"哪个对"无解（直接拆 E1）。
2. **trace uniform 消费路径**：搜索 solve 用哪个 `average_strategy`？未访问 emit uniform 还是 giveup？为何没 giveup（拆 E2）。
3. **单线程 budget 阶梯**（1.5M/6M/24M，threads=1）：单线程是否任何 budget 都 miss（拆 E3）。
4. **直接量 reach**：solve 内数这个 infoset 访问次数 / 总迭代，验证"低 reach"（拆 E5）。
5. **复跑保真**：同 (state,seed,iters,threads) 跑两次是否 byte-equal，确认确定性边界。

## 7. 目前能比较稳地说的（仅这些）

- 8s / 500 桶在这类深码多人 turn 节点上，单次 live 读数是噪声分布里的一次抽样，**不可当点估计**（同算力换 seed 天差地别，数据硬）。
- 这个具体节点，至 24M（16× live）在 500 与 200 桶下都没收敛到跨 seed 一致的策略。
- `uniform .2×5` 是确定性的"该节点没被采样到"，**不是线程竞争、不是 node_id 映射错**（探针证）。
- **但**："该 call / live 打错了 / 根因就是批调度 / 引用的 uniform 代码行"——这些都**没坐实**，按 §5 待查。

## 8. 2026-06-15 续（下午）：实测把 E1–E5 钉死 / 修正

> 同一 spot（`spot_a3.json`）做了四件事：① turn 子树节点数实测；② seed2/6M/200桶 avg-vs-current dump；③ 200桶 3×3 grid（iters×seed）dump avg/current/raw-regret/strategy_sum；④ 读 trainer/regret 源码钉死机制。埋点（`solve_subgame` 报节点数 + `read_current_strategy` dump 策略状态）用完即回滚，未进主线。

**8.0 turn 子树节点数 = 7,361（确定性）。** `deep_menu_for` 在这选**宽 {0.5,1} 菜单**（3-way、第二大 Active 栈≈1801 / 轮起点 pot≈160 = SPR≈11 ≪ 40×pot 阈值）。只占 4M cap 的 0.18% → **「没收敛」不是树太大 / 撞 cap / 建树超时**，这条原因排除。与 seed/iters/线程/桶表无关（建树 RNG = 固定常数 `TREE_BUILD_RNG_SEED`）。

**8.1 E2 已解 —— uniform .2×5 + source=search 不 giveup 的真因。** `read_current_strategy` 读 `average_strategy(&info)`，**仅返回空 Vec 才 Err→giveup**；`EsMccfrTrainer::average_strategy` 只在该 infoset **同时缺席 regret 与 strategy_sum 表**时返回空。但此 infoset **在表里（被访问过）**、strategy_sum 五档全等 → average 归一成**非空均匀 `[.2×5]`** → `is_empty` 不触发 → source=search 配均匀。**非 bug**，是「present 但 reach 饿死」的 average。早先「空→search_giveup→check-when-free」是 **dense trainer**（返回空 Vec）语义；搜索 solve 走 HashMap 版 `EsMccfrTrainer`，未访问/全均匀 → 非空均匀。

**8.2 E5 修正 —— 饿死的是 average，不是 regret。** 此 infoset **被访问了**（raw regret ±上万、符号分明）→ regret/current-strategy **没饿死**；饿死的是 **average 累加器（strategy_sum）**，因其权重 = **π_trav = hero 自己到该节点的 reach**（=σ_hero(root 下注)），很小。代码：`strategy_sum += π_trav·σ`（trainer.rs:248） vs `regret += π_opp·(cfv−v)`（trainer.rs:245）—— **两个量用不同 reach 权重**，average 饿死在 hero 自身 reach、regret 吃对手 reach 不饿死。这就是「均匀 average + 巨大 regret」并存的根。

**8.3 E1/E3 更新 —— 200桶 9 格 sweep（`a3_strat_sweep.txt`）。** 列序 F/C/r½/r1/AI：

| iters | seed | avg（emit） | current（regret-match） | strategy_sum total |
|---|---|---|---|---|
| 1.5M | 1 | .55/.15/.10/.10/.10 | .16/.55/.08/0/.21 | 0.022 |
| 1.5M | 2 | .02/.31/.03/.08/**.55** | 0/1/0/0/0 | 0.40 |
| 1.5M | 3 | .06/.24/.01/.04/**.65** | 0/.06/.35/.26/.34 | 0.23 |
| 6M | 1 | **.48**/.18/.26/.03/.06 | **1/0/0/0/0** | 0.083 |
| 6M | 2 | **.20/.20/.20/.20/.20** | 0/.80/0/0/.20 | **0.0067** |
| 6M | 3 | .13/**.55**/.05/.06/.22 | .07/.62/0/0/.31 | 0.071 |
| 24M | 1 | .47/.21/.24/.02/.05 | .43/.57/0/0/0 | 0.091 |
| 24M | 2 | .02/**.96**/.02/0/0 | **0/1/0/0/0** | 3.02 |
| 24M | 3 | 0/**.998**/0/0/0 | **0/1/0/0/0** | 18.8 |

- **低算力（1.5M/6M）：avg 和 current 都是噪声。** current 跨 seed 也乱（6M-seed1=纯 fold、seed2=call/allin、1.5M-seed3=raise 重）→ **收回会话中途「current 偏 call」（只看了 seed2 一格）。**
- **高算力（24M=16×live）：** seed2/seed3 → **纯 call**（avg .96/.998、current `0/1/0/0/0`、strategy_sum call=3.0/18.8），regret **只有 call 非负**，fold/raise/allin 在 −150k…−280k（弃 12 outs = −278k regret）。**但 seed1 在 24M 仍饿死**（ss total 0.091）、未收敛（avg fold/raise 混）。
- **E1 校准结论：高算力趋势 = CALL**（2/3 seed 决断收敛 + regret 把 fold/raise/allin 判成大负）→ 给早先被收回的「重心=call」**真实支持（靠收敛证据，不是 §5 E1 那个非法的 200+500 桶投票）**。**但**：即便 16×live 也未稳收（seed1 异议），且 8s live 到不了 24M → **live 那次 allin 63% 是低-reach 噪声抽样，不是收敛策略**。
- **E3 证伪**（「1.5M 解了、6M 没、budget 改轨迹」）：没有哪个低算力格「解了」；1.5M-seed2 的 allin .55 也是噪声抽样。收敛只在 24M 才冒头、还只 2/3 seed。

**8.4 reach 是主变量 + strategy_sum 非单调。** strategy_sum total 跨 9 格 **0.0067→18.8（≈2800×）**；< ~0.4 = 噪声，大到（24M seed2/3）才锁 call。**strategy_sum 不是单调计数器（LCFR 下）**：每 period ×`n/(n+1)`（trainer.rs:401），固定迭代下**恰 50 次**（period_size=iters/50；单/多线程 update_count 都正好=iters）→ period-k 喂养只存活 ≈k/51 → 饿死节点**衰减**。1.5M（0.40）与 6M（0.0067）是**两条独立轨迹**（同 seed，但 period_size 30k vs 120k → 第一次 rescale 后分叉）；50 次 rescale 的 **profile 完全相同**，period_size 只改绝对时点 = 让轨迹分叉、不改加权曲线 → 0.40 vs 0.0067 是**独立轨迹方差**，不是 rescale 不同。**修正：strategy_sum 量级 = 近期 π_trav 加权 reach，会往下掉，不是访问次数。**

**8.5 子树 infoset 总数 ≈ 266k @ 24M（纠正 7361×200=1,472,200 的估计）。** 实测 table_infosets：182k（1.5M）→ 228k（6M）→ 266k（24M），仍在涨，比 1.47M 上界小 ~5.5×（并非每节点都是决策点、每点也到不齐 200 桶）。

**8.6 仍未做（直接证据缺口）。** ① root 上 hero 的 bet 频率 + BTN 的 raise 频率没直接量（π_trav vs π_opp 哪个主导饿死，是机制推 + 跨 seed 200× 方差间接证）；② seed1 为何 24M 仍饿死（采样个例）未单独追；③ 500桶（§3a）同款 dump 未做；④ **EV/best-response 标尺（§6①）仍没有 → 「call 对不对」最终仍无独立 EV 裁判，只有「解真游戏收敛到 call」这一结构性证据**。

## 9. 2026-06-15 续③：ES vs vanilla(chance-sampled) 同子树实测（钉死饿死机制 + 否掉"换 vanilla"简单修法）

> 起因：质疑"建子树后固定 50 次 LCFR / external sampling 迭代"是否合理。埋点 `exp/a3-trainer-compare`（`read_current_strategy` 内同一 live 子树跑三 solver，单线程墙钟 checkpoint[8/30/60]s × seed{1,2,3}，B200 search 桶；6max 主线未碰）。query node=6896，动作 [Fold,Call,R0.5,R1,AllIn]。产物 `a3_trainer_compare.tsv`。

**9.1 事实层。**
- **ES（es_lcfr / es_nolcfr）跨 seed 灾难性不一致**：seed1=冻结噪声（lcfr→Call .565 / nolcfr→AllIn .68，**同 seed 不同答案**）；**seed2 = ss_mass 全程 0、2.0M+ iters 整个没采到该节点 → uniform .2×5**（§8.1 的 "uniform emitted as search" 现场复现）；seed3 = lcfr 采到(ss~1.3、Call/AllIn 摇摆) / nolcfr 又 ss_mass=0 never visited。**是否采到该节点 ≈ 随 seed×lcfr 抛硬币。**
- es_lcfr 冻结 + ss_mass 随墙钟**衰减**（seed1 0.39→0.10→0.05）：节点早期攒点 mass 后近乎不再被访问，LCFR rescale 持续打折 → 方向冻死。
- **vanilla：ss_mass 单调增**（每 seed，seed3 0.55→1.33→2.80）→ 节点每步都被访问、average 真在动，**跨 seed 一致偏 Fold**（60s: .48/.68/.79）——**不饿死**。
- **但 vanilla 吞吐塌**：~140ms/step（全树 7361-DFS × n_players × showdown eval）→ 60s 仅 ~420 iters，ES 同窗 ~2.07M iters（**每 step 贵 ~4800×**）。420 iters 远未收敛、且 vanilla 无 LCFR → average 被冷启动 uniform 重压。

**9.2 解释层 / 修正 §8.5。**
- **修正"节点被访问了"**（§8.5 只看 seed2 单格得的）：跨 seed 实测**两种失败模式都常见**——seed1=visited-but-starved、seed2/seed3-nolcfr=**never visited（ss_mass 严格 0）**。后者更普遍。
- **饿死根因 = 子树根在回合上游 + external sampling 的对手动作采样门**：node 被采到的概率/iter ≈ (traverser=hero)×σ_MP(call)×σ_BTN(raise)，低到 2M iters 常**整段没采到**。比 §8.2"π_trav 下权重"更狠——不是下权，是**根本不访问**。
- **"换 vanilla / chance-sampled"不是实时修法**：它修了饿死（每步访问、ss 单调增），却换来吞吐塌（每 step 贵 ~4800×）→ ES 因不访问、vanilla 因迭代不够，**两者 60s 内都收敛不了**。
- **固定 50 LCFR 本身被旁证无关**：lcfr/nolcfr 都失败，lcfr 只额外加 mass 衰减。问题在 rooting+sampler，不在 period 数。
- **EV/Call-对不对仍未解**（§8.6④ 照旧）：vanilla 偏 Fold vs §8 ES-24M 偏 Call，两者都欠训练 → 本实验只裁决**访问/饿死机制**，不裁决动作值。

**9.3 指向的真修法 = 把子树根重锚到决策节点本身。** cur_node=root ⇒ π_trav=1、ES 每 iter 必访问（无对手门）⇒ 同时拿到 vanilla 的"保证访问" + ES 的便宜吞吐。代价 = 须外供决策点入口 range（range 先验依赖 + 已记 over-exploit 风险）。**下一实验 = 重锚 ES（决策点为根 + 固定入口 range）vs 上游根 ES，本 spot 同框对照。**

---
_产物在 vultr `~/dezhou_20260508/`：`spot_a3.json`、`a3_convergence_sweep.tsv`、`a3_convergence_sweep_b200.tsv`、`probe_det.tsv`、`a3_strat_sweep.txt`（§8 的 9 格 dump）、`a3_trainer_compare.tsv`（§9 的 ES/vanilla 对照）、对应 `*.sh`（含 `sweep_a3_strat.sh`、`run_a3_compare.sh`）。§8 / §9 埋点均已回滚（§9 的 `exp/a3-trainer-compare` 分支已删，本机/远端/origin 全清，6max 未碰）。_
