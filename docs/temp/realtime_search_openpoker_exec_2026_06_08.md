# 执行文档：6-max NLHE 实战剥削 bot（OpenPoker）

> 2026-06-08 / 分支 `6max`。**目标 = 最大化剥削 OpenPoker bot 池;对手建模 = 主角。** 强弱只认真实对局
> （设计文档 §11.5d 已证自对弈探针测不了绝对强度）。机制/基建复用 `docs/temp/realtime_search_design_2026_06_03.md`
> （下称**设计文档**）。本文取代早先的「均衡-flow 执行版」——架构已从「blueprint + 可选均衡搜索」转向「**剥削为中心**」。

## 0. 目标 · 核心洞察 · 决策记录

**目标**：在真实 bot 池**赢最多**（剥削），不是"不被打死"（均衡）。OpenPoker = 开发者 bot + 排行榜,大概率有大量
**静态、可剥削**的弱 bot → 剥削的天花板远高于均衡。

**核心洞察（这是整个架构的支点）**：**剥削 = 对固定对手模型求 best-response = 单边 max-EV**。
- 均衡 CFR：所有人策略都是未知数、迭代到不动点;多人**无 Nash 保证**、还贵。
- best-response：对手策略**从模型给定（固定）**,只剩我们一个未知数 → well-defined、唯一、一遍树期望即可算,
  **没有多人均衡收敛问题**,更便宜、更 anytime。
- 难点全压一处：**对手模型准不准** → 正好是我们要当主角的东西。这条路天花板高、工程上也更聚焦干净,且消掉了
  「多人 CFR 无保证」那条疑虑。

**决策记录（2026-06-08 用户拍板）**：
1. 目标 = 剥削池;对手建模 = 主角。
2. 冷启动默认 = **blueprint 兜底**,均衡搜索引擎后置（甚至可能不需要）。
3. 剥削激进度 = **置信度加权**（数据足才偏离 baseline,防错模型）。
4. best-response 主路径 = **直接树期望（expectimax 对固定模型）**,不上 CFR-BR。

## 1. 架构（剥削为中心，分层）

```
每决策：
  1. 识别桌上对手（按 name）→ 载入其模型（新对手用 population 先验）
  2. 在真实码深/人数/board 上建子树
  3. 把对手节点钉成模型给的动作概率（固定）
  4. 在时间预算内算我方 best-response（expectimax）
  5. 安全：按模型置信度，在 best-response 与 blueprint baseline 间加权
  6. 返回动作
持续：
  记全桌动作 + 摊牌 → 更新逐对手模型
```

- **数据管道（地基，§2）** → **对手模型（主角，§3）** → **best-response 引擎（§4）** → **冷启动兜底 + 置信度加权（安全）**。
- 真实环境三硬约束仍是底层：**任意下注尺寸**（off-tree PHM 映射,设计文档 §12）、**筹码不固定**（子树按真实码深建 →
  这是 search > blueprint 的核心）、**每步时限可配**（`time_budget` 5/10/20s,anytime）。

## 2. Phase 0 — 数据管道（先行，本文落地第一步）

**做什么**：把 OpenPoker 客户端日志从"只记我方动作"升级成"**全桌手牌历史（HH）**"。

- 落 `tools/openpoker_play.py`（driver 本就收到全部事件）。**隔离 advisor 路径**：不改 `HandState` / advisor 请求 →
  advisor 行为 byte-identical;新增独立 `--hh-log`（默认 `openpoker_handhistory.jsonl`）。
- **每手一条记录**：`hand_id` / `button` / `my_seat` / `blinds` / **players（seat→name + 起始 stack）** /
  我方 `hole` / **全动作序（各座 + street + 动作 + size + 行动后 stack + pot）** / `board`（按街）/
  **摊牌（`shown_cards` / `winners` / `final_stacks` / `pot`）**。
- 数据来源（消息自带,见设计文档 client §1）：对手 **name** 取自 `your_turn.players`;每条 `player_action` 带
  `seat/action/amount/street/stack/pot`;`hand_result` 带 `shown_cards/winners/final_stacks/pot`。

**为什么先做它**：(a) 剥削的地基;(b) **从下次挂场起就开始攒对手数据——哪怕还在跑 blueprint**,模型在引擎做好前就长;
(c) 立即回答一个前提：**对手 name 是否稳定可持续追踪**（稳 → 逐对手建模成立;不稳 → 退 population）。

## 3. Phase 1 — 对手模型（主角）

- **离线 aggregator（新 tool）**：从 HH 估 per-(对手, 位置, 街, 动作, size 档, 在场人数) 的**动作频率** + 摊牌**range**。
- **粗 HUD 量先行**（数据少即可用、鲁棒）：VPIP / PFR / 3bet% / fold-to-cbet / cbet% / aggression-freq /
  fold-to-river-bet / fold-to-3bet / 摊牌强度分布 …
- **population 先验 + 逐对手 shrinkage**（冷启动有兜底,数据多了向逐对手收敛）。
- 模型格式 + advisor 载入接口。

## 4. Phase 2 — best-response 引擎（expectimax）

- **复用 subgame 基建**：`PublicBettingTree::build_subtree`（真实码深/人数,设计文档 §5a）、off-tree、`time_budget`、
  按 (街,人数) 选菜单（用户菜单 flop ≤3`{0.5,1}`/≥4`{1}`、turn`{0.5,1}`、river`{0.33,0.66,1}`,可配）。
- **改 solve**：对手节点钉成**模型给的动作概率**（不是均衡未知数）,我方节点 **max-EV**,hidden 用估的 range → 树期望
  （非 CFR 迭代）。anytime：预算内出当前最优。
- **置信度加权安全**：模型置信低 → 向 blueprint baseline 收敛;高 → 放开 best-response。
- ≥4-way 建树须放开 `width_redirect`（设计文档 / `nlhe_betting_tree.rs:379-385`,否则 panic）;去 8000 cap + 高 OOM 后备
  200k + 时间兜底（用户已定）。

## 5. 验证（OpenPoker，剥削视角）

- **总 mbb/100 + 排行榜位次**（目标是赢,不是不输）;**按对手分**（看是否打爆特定 bot）;按**见牌人数 / 我方开局有效栈**
  分桶;vs **blueprint-only baseline**（已在跑,实测 ~58% 决策 fallback = 真实场 blueprint 过半打不了 = 问题实证）。
- **对手模型质量诊断**：预测对手动作的命中率;估 range vs 实际亮牌偏差（分桶）。
- **诚实标注**：码深漂移（实测同桌 14–800BB）+ bot 池漂 + 单号分时段 → 是"真实场实测",按桶读。

## 6. 正确性 smoke / 执行步骤 / Go-NoGo

- **smoke（vultr,非强度）**：HH 日志 selftest 不破 advisor 路径（byte-identical）;真挂场 HH 记录字段齐、摊牌/名字捕到;
  Phase 2 后再加 best-response 的 no-panic / 归一 / ≤budget。
- **步骤**：Phase 0（HH 日志）→ 挂场攒数据 + 验名字稳定 → Phase 1（aggregator + 粗模型）→ Phase 2（best-response 引擎）
  → 置信度加权上线。代码改动 push → vultr fetch/reset（`feedback_vultr_sync_via_git`）。
- **Go**：Phase 0 = HH 记录完整且名字可追踪;Phase 1 = 模型预测命中率 > population 基线;Phase 2 = best-response 在
  OpenPoker 总 mbb/100 显著 > blueprint-only。

## 7. 已知疑虑（剥削视角，诚实）

1. **模型错 → 误调**：best-response 对错模型可亏大 → **置信度加权**（数据足才偏离）+ blueprint 兜底缓解;模型质量诊断盯。
2. **冷启动**：新对手数据少 → 先 population 先验 + blueprint,逐手收敛。
3. **对手会否适应**：若 bot 自适应则被反剥削;但开发者 bot 多半静态（可验:同对手前后期表现是否漂）。
4. **名字不稳**：若 OpenPoker 名字不可持续追踪 → 退 population 建模（Phase 0 立即验证）。
5. **码深 / preflop**：blueprint 兜底在 off-stack（尤其 preflop 短/深栈）弱;真实剥削里对手在各码深的倾向不同 →
   模型应带码深维度,blueprint 兜底是临时。

**状态：Phase 0 落地中**（HH 日志）。OpenPoker key 已验、blueprint-only baseline 已跑。
