# 6-max NLHE blueprint 求解器目标（立项）

## 文档目标 + 与既有文档关系

2026-05-30：heads-up NLHE 主线告一段落（1B dense blueprint 对 Slumbot 近 break-even、符合预期，
见 `status_v2.md`），主线转向 **6-max（6 人）No-Limit Texas Hold'em**。本文档是新主线的入口文档，
作用对应 heads-up 阶段的 `heads_up_nlhe_solver_target.md`。

- `heads_up_nlhe_solver_target.md`：heads-up 阶段（H1–H5），**已收尾**，仅剩 Slumbot 对战数据采集。
- `temp/pluribus_path.md`：6-max 长线 8 阶段全景参考（含实时搜索 / 服务化）。本文档**不是**重写它，
  而是把**当前承诺的范围**（路线 A：blueprint-only，跳过实时搜索）切出来、对齐真实代码状态、给可验收门槛。
- 本文档 = 当前主线验收目标；`pluribus_path.md` 的阶段 6（实时搜索）及之后**暂为非目标**。

## 决策锁定（2026-05-30，用户拍板）

- **D-6M-001 路线 A：blueprint-only 先打通**。先把 6-max 离线自对弈 blueprint 端到端跑通（参数化 game →
  多人抽象 → 复用现成 N-generic trainer → 实测对战评测），**不做实时 depth-limited search**。搜索（路线 B /
  `pluribus_path.md` 阶段 6）等 blueprint-only 闭环稳定后再立项。理由：复用约 70% 现有基建；blueprint 是一切
  地基；先把"多人抽象怎么做"这个最大未知数验证掉。
- **D-6M-002 码深 100BB**。6-max 标准码深 = Pluribus 同档，树比 HU 的 200BB 小、更贴近真实 6-max 场。
  （HU 的 200BB profile 不迁移。）
- **D-6M-003 评测范式切换**：6-max 是多人一般和博弈，LBR / exploitability 不再是真实质量度量（见下节），
  质量以**实测对战胜率**为主，LBR 仅作诊断。

## 6-max 的根本不同（决定整套范式）

heads-up 是二人零和 → CFR 可证收敛 Nash → exploitability/LBR 是真度量、不可剥削 == 最优。
**6-max 是多人一般和**，这一点推翻三件事：

1. **CFR 自对弈不再保证收敛 Nash**（多人 Nash 计算 PPAD-hard；且打 Nash 不保证赢——均衡选择 + 隐性合谋）。
   → 接受"无理论保证、靠实测"的 Pluribus 路线。收敛性靠**监控**（average-regret 应 sublinear、entropy、
   动作概率震荡），不靠定理。
2. **LBR/exploitability 失去理论意义**。仍可当"策略烂不烂"的诊断（能被 LBR 暴打就是差），但"LBR 低 = 好"
   不成立。heads-up 最依赖的质量闸门到这里换成**实测对战**。
3. **没有"训到 floor 就停"**。质量相对于对手场，无同种 floor。
4. **没有强 6-max 公开参考 bot**（不像 Slumbot 之于 HUNL）。这是真实评测缺口，S5 要专门解决。

## 现有代码就绪度（亲验 file:line，2026-05-30）

**✅ 已 N-generic，直接复用：**
- 规则引擎：座位/盲注/行动顺序/**多人 side pot**/多人 showdown/per-seat payoff vector。
  `src/rules/state.rs:846–938`（逐 contribution level 算 side pot，返回 per-seat 收益向量）；
  `src/rules/config.rs` 已有 `default_6max_100bb()`。
- CFR trainer（ES-MCCFR / LCFR，alternating traverser）：`recurse_es` 取终局值 `G::payoff(&state, traverser)`
  （`trainer.rs:653`），**不取负、不假设 `1 - player`**，traverser 按 `% n_players` 轮换（`trainer.rs:493`）。
  这正是 Pluribus 用的多人 external-sampling MCCFR 形式。
- InfoSetId 打包：position 已留 4 bit（支持 0..15 座），**不用改 bit 分配**（`src/abstraction/map/mod.rs`）。
- dense 表后端 / checkpoint 流式写：格式不绑人数（按 betting tree 结构走），复用。

**⚠️ 中等改动：**
- `SimplifiedNlheGame` 硬编码 `n_seats=2`（`src/training/nlhe.rs:310`）→ 参数化到 6 座（或新 game 类型），
  重建 betting tree（节点数随之暴涨，需 S2 量化）。
- 动作抽象 `StreetActionAbstraction`：6-max preflop 动作空间远比 HU 丰富（open/3bet/4bet/squeeze/cold-call ×
  多位置），需扩到**按位置**的 size 集。
- Game trait 零和约束（D-332 `payoff(0)+payoff(1)=0`）→ 推广为"全玩家和 = 0"（筹码守恒，本就成立）。

**❌ 重活（也正是 Pluribus 论文的难点）：**
- 抽象层 equity / OCHS **假设 1 个对手**（`src/abstraction/equity.rs:39,79`）→ 多人 equity 特征要重做、
  桶要重新聚类。**这是全项目最大未知数，S3 先验证。**
- 评测：LBR 硬编码 `probe_idx % 2`（`src/training/lbr.rs:5`）失去意义；AIVAT 单对手模型（`aivat*.rs`）要推广
  到多对手；baseline 全是 HU 模式 → 要新 baseline + 解决"没有强 6-max 参考对手"。

**一句话**：引擎和求解器内核当初真按 6-max 建好了，可直接吃；真正工作量集中在**抽象（多人 equity）**和
**评测（无 LBR、无强参考对手）**两端。

## 阶段与量化门槛（路线 A）

### S1：6-max 规则正确性钉死（正确性优先，先于任何训练）

**实测纠正前文「从没在 6-max 下验过」的假设（2026-05-30 探索 + vultr 实跑）**：规则层是**按 6-max 先建、
先验**的——项目最初目标就是 6-max（heads-up 是 2026-05-17 才降级的，见 `heads_up_nlhe_solver_target.md`）。
所有错误高发的规则都已实现并带决策记录、且有测试：

- 最小加注 / 不完整加注不重开下注（all-in for less 合法但不给已行动者重开）= `D-033/D-035`
  （`state.rs:500,508`，`last_full_raise_size` + `raise_option_open`）。
- 多人 side pot（3+ 同时 all-in / all-in-for-less / 未跟注返还）= `compute_payouts` / `contribution_levels` /
  `single_contributor_tranches`（`state.rs:826–940`）。
- 奇数筹码归属（button 左手第一个赢家）= `D-039-rev1`（`state.rs:909–923,984–995`）。
- 摊牌顺序（末轮最后加注者先亮，否则 button 左手第一个）= `D-037-rev1`（`state.rs:997–1011`，每街重置）。
- N 座盲注 / 行动顺序（preflop UTG=button+3 先动、BB 最后有 option；postflop SB 先动）= `state.rs:1013–1048`，
  heads-up 走 button=SB 特例（`D-022b-rev1`）。
- **dead button 不适用**：自对弈 solver 全程固定 N 座、无 sit-in/sit-out（`D-032`，`config.rs`），按钮机械左移，
  无空座 → 此前门槛里的 "dead button" 是误列，删。

测试现状（2026-05-30 vultr HEAD `fae5fdf` 实跑全绿）:`scenarios`(10) / `scenarios_extended`(27) /
`side_pots`(8,含 2/3/4-way + 奇数筹码 + 未跟注返还) / `heads_up_rules`(3) 全过;`cross_validation` 默认
`default_6max_100bb()`,**100 手对 PokerKit 0.4.14 `matches:100 diverged:0 skipped:0`**(独立参考实现逐手比对
payouts + showdown_order)。`.venv-pokerkit` 已在 vultr 装好。

**S1 唯一未闭项 = 重跑 100k 手 PokerKit 跨验证**（`cross_validation_pokerkit_100k_random_hands`，`#[ignore]`,
`scripts/run-cross-validation-100k.sh`）。理由不是走形式:`src/rules/` 在 D1 那次 100k 之后又被两个 **perf**
commit 动过 showdown/payout 热路径(`c8fff0a` showdown rank 预算 + pot_winners 缓存、`4e7b3a2` D-378 fast
path),按"正确性大于一切"这类改动后最该重跑最强 gate。成本:vultr 4 核 N=4 ≈ 2.75h(python 子进程 0.4s/手)。

### S2：6-max 树规模量化 + game 参数化

- 扩 `tools/nlhe_betting_tree_sizing` 到 6-max / 100BB，实测**节点数 / infoset 数 / dense 两表 RAM**
  （HU 240,096 节点 / 119.7M infoset 是 2 人数；6-max 会大数量级，需先量再定预算）。
- 参数化 `SimplifiedNlheGame`（或新 `SixMaxNlheGame`）到 6 座 + 100BB；用 N-generic tree builder 重建树。
- 定按位置动作抽象的初版 size 集（先粗：fold/check/call + 少量 pot ratio + allin，按位置精选）。
- 门槛：sizing 报告出炉（含 total_slots / 内存 / action_count 直方图，自洽校验 total_rows==infosets）；
  树构建确定性可复现；据此定**算力预算 + 训练机**（大概率 ≥ Pluribus 64 核级，对齐
  `feedback_high_perf_host_on_demand` 先报预算给用户起机）。

### S3：多人信息抽象（最大研究点，先验证再训练）

- 实现**多人 equity 特征**（hand vs N 个随机对手，N 随在座人数变；不是 HU 的 1 对手 pairwise）。
- 按 6 位置重做 postflop bucket 聚类；preflop 仍 lossless 169（位置由 InfoSetId 的 position bit 区分）。
- bucket 质量闸门（沿用 `pluribus_path.md` 阶段 2 + 现有 `bucket_quality`）：potential-aware 特征参与聚类、
  桶内 EHS² 标准差有上限（建议 < 0.05）、桶内 all-in equity 分布 EMD/KL 在阈内、同手→同桶端到端验证。
- 映射确定性：同状态重复映射 1,000,000 次 bucket id 完全一致。
- **决策分叉**（S3 内部需定）：多人 equity 用精确枚举 / MC 估计 / potential-aware 直方图——成本 vs 精度权衡，
  类比 HU 阶段 kmeans 的 wall/RSS 实测先量。

### S4：6-max blueprint 训练（复用 N-generic trainer + dense 后端）

- 用 ES-MCCFR / LCFR + dense 后端，6 traverser 轮流更新（trainer 已支持，无需改算法）。
- **多人收敛监控**（多人 CFR 可能震荡/regret 线性增长，必须能告警定位）：实时输出 average-regret 增长曲线
  （应 sublinear）、策略 entropy、动作概率震荡幅度。
- 单次训练连续 24h 无崩溃 / 无内存泄漏；checkpoint 保存恢复后训练曲线连续、策略查询一致。
- first-usable blueprint 门槛：update 数按 S2 量出的 infoset 数定（HU "100M 饱和"经验**不照搬**——预期
  10¹⁰–10¹¹ 量级，对标 Pluribus）。
- 门槛：blueprint-only 在 `1,000,000` 手评测中稳定击败 random / call-station / tight-aggressive 三类基线
  （必要非充分）。

### S5：6-max 评测重构

- head-to-head 实测对战（多人 baseline）：输出 mbb/g、SE、置信区间、**按位置拆分收益**。
- **多对手 AIVAT** 降方差（6-max 方差远大于 HU，且无 LBR 捷径 → AIVAT 比 HU 更不可或缺；现有 `aivat*.rs`
  单对手要推广）。评测报告同时出原始与方差缩减后的 mbb/g。
- LBR 仅作诊断指标（注明多人下的理论局限），不当质量闸门。
- 解决"无强参考对手"：可拿 HU 训的 blueprint 摆 6 座当 baseline，或自对弈不同 6-max blueprint 互评。
- 固定 seed 评测可复现；每个候选策略出评测报告 + 策略版本哈希。

### S6（非目标，parked）：实时 depth-limited search

路线 B / `pluribus_path.md` 阶段 6。Pluribus 真正赢人的机制，但占总量 30–40%、极易写错。**blueprint-only
闭环（S1–S5）稳定后再单独立项**，不在当前承诺范围。

## 明确非目标

- 当前不做多人 continual re-solving / biased leaf strategies / 完整 Pluribus 复现。
- 当前不追求 Pluribus 级超人类 6-max 质量（blueprint-only 单独达不到，这是已知的）。
- 不以线上牌局自动化为目标。
- 不换神经网络路线（ReBeL / 深度 RL）——与当前纯 Rust tabular-CFR 基建脱节，如未来重估再另议。

## 算力 / 主机假设

- 6-max blueprint 训练预期 ≥ Pluribus 量级（论文 64 核 / 8 天 / ~10¹¹ updates）。具体机型/时长**待 S2 sizing
  定数后**按 `feedback_high_perf_host_on_demand` 报预算给用户起机；HU 的 c6a.8xlarge 大概率不够，需更大/更久。
- dense 表更刚需（6-max infoset 可能十亿级）；bucket kmeans 拟合成本参考 HU 实测（c6a.8xlarge 量级），
  6-max 多人特征 + 6 位置会更贵，S3 先量。
