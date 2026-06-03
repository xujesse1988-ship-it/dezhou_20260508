# 项目当前状态（v3）

## 一句话:heads-up 阶段（已收尾）

heads-up NLHE 已收尾——1B dense blueprint(vultr `artifacts/run_dense_lockfree/nlhe_es_mccfr_final_001000000000.ckpt`,
9.3 GB)对 Slumbot 近 break-even(AIVAT 10000 手 raw −85.25 / AIVAT −108.31 mbb/g,CI 跨 0 = 符合预期、未显著),
LCFR / batched-parallel / dense 后端 + v4 bucket / AIVAT 评测链全部端到端验证;**完整
heads-up 操作细节(吞吐表 / 优化历程 / 逐 run 对照 / Slumbot+AIVAT 子集诊断 / 桶表 b3sum) 见 git 历史的
`docs/status_v2.md`**(已删)+ `docs/aivat_eval.md` + `docs/temp/*`。Slumbot 对战数据采集作为收尾持续动作仍在跑。

## 当前阶段:6-max blueprint-only

主线 = **6-max(路线 A:blueprint-only + 100BB,2026-05-30 立项)**。阶段 S1–S5 量化门槛 + 决策记录 + 亲验
代码就绪度 → **`docs/six_max_nlhe_target.md`**(主线入口文档)。

**进度(2026-06-01)**:S1–S3 均已闭/近闭(规则余 100k PokerKit 重跑;S2 树规模 + A3×A4 接进生产;S3 实测 HU
单对手桶可复用)。**S4 训练已跑完一轮并独立复核**:N=3@200 1B dense 训练(基建 commit `5b793e9` + AWS run)
S4 gate PASS(vs random/call-station/overly-tight,CI95 下界全 > 0)——但**「1B 已收敛」被 run log 自身推翻**:
实 **56% 表覆盖仍线性爬**、preflop 支配对翻转 **14%** = 欠训练(`ConvergenceMonitor` 只采 169 个 preflop 根、
看不到全表)。用户提的 **N=4(1.445B infoset)/500 桶会 backfire**(都 ×infoset → 覆盖率更差);**200 桶质量实测
健康**(排除坏桶)。**真修法 = preflop reshape**(删非 SB 开池 limp + 加 2.25BB 开池档,commit `fdc66db`,
`--reshape {none|nolimp|preopen|preopen-small}`,byte-equal 守住 cross-check):nolimp 缩树 **4.2×**(55.2M infoset/1.91GiB)、
preopen 0.68×(157.9M/5.46GiB)、preopen-small 0.45×(103.0M/3.57GiB)。**nolimp 1B 已跑完(vultr,wall 7.5h)并验证诊断**:删 limp 的 4 个非盲位 preflop
支配翻转 **13.0%→0.85%(15× 干净)**、AA/KK/QQ 全 raise 1.00、limp 归零、覆盖 64.5%;**SB 故意留 limp 作对照 →
18.4%→18.5% 不动、仍 limp AA**,证因果(动了的塌/没动的不动)+ 指向 SB 需便宜开池档。nolimp 干净但偏紧(只 3.5BB 单档)。
**preopen(加 2.25BB 开池档)已在 AWS 跑到 ~2.1B 暂停**(LCFR 不可 resume;checkpoint 1B/2B 存 vultr
`artifacts/run_6max_s4_preopen/`):非盲位 1B 即干净(0.2-0.6%)**且范围放宽近 GTO**(BTN 37→44-45%、UTG 14→18%)
证实开池档修好 nolimp 偏紧;**SB 改善(limp 43→30%、AA raise 43→68%)但仍 limp AA ~32%、翻转 ~17%,1B→2B 近乎平、
证据不足不下结论**(欠训练 vs 抽象天花板未分)。⚠ **2026-06-02 GTO Wizard 真值修正:SB limp / AA-limp 是 GTO 非缺陷**
(SB 100bb cEV limp 49% / AA limp 47%)→ 不删 SB limp、不为 SB 跑 5B。

**S5 进度(2026-06-03):①② 端到端打通**。off-tree advisor 引擎落地(`src/training/blueprint_advisor.rs`:一张权威
GameState + 每 blueprint 一份抽象影子,off-tree 翻译推进;slumbot_advisor 重构为薄壳 byte-equal)。① 跨抽象 h2h
(`tools/six_max_blueprint_h2h`)实测 **nolimp×preopen 相对强度相当(双向 CI 跨 0)**;② OpenPoker 客户端
(`tools/openpoker_advisor.rs` + `tools/openpoker_play.py`)**live 连通性 smoke 已过**(账号 jesse_xu,4 手全 blueprint
驱动 0 报错)。**两个边界**:off-tree 只忠实尺寸差异(结构性 limp-gap 显式 Desync/兜底)+ lone-hero 粒度税 confound
(不能干净排内在强度);**实测码深漂移严重**(真实桌 14BB–800BB)。剩绝对强度量化(挂场数百手 + 排行榜,需用户授权时长)。
详见 `six_max_nlhe_target.md` S4 续⑥⑦ + S5 续、`temp/openpoker_client_design_2026_06_02.md` §9。

**S6 进度(2026-06-03):实时搜索推进,MVP subgame-solver 核心已落地 + vultr 验证**(分支 `6max-rts-mvp`
commit `cf9efdb`,**未并入 6max**)。设计成文 `temp/realtime_search_design_2026_06_03.md`(文献调研+代码复用映射+
对抗验证):方法定型 = Pluribus/Modicum tabular depth-limited search(非 DeepStack/ReBeL)。三件落地:①
`PublicBettingTree::build_subtree`(以中途 public state 为根建 betting 子树,`nlhe_betting_tree.rs`);②**加性**
`GameState::resample_hidden`(克隆中途态+保留公共牌/下注态+重发隐藏牌,终局走权威 `payouts()` → **S1 不受影响**,
`rules/state.rs`);③ `SubgameNlheGame impl Game`(delegate SimplifiedNlheGame、仅重写 root()=resample+子树 →
复用 `EsMccfrTrainer` 零改,`training/subgame.rs`)。**vultr 验证**:6 新测试全过 + 全 lib 67/67 无回归(byte-equal
守门未破)。剩 6a 收尾 = subgame_search 接 `blueprint_advisor.rs:421`(flop-only 触发)+ 不退化 h2h 探针
(search-on vs blueprint-only,需真 ckpt);6b(continual re-solving + biased leaf)/6c(off-tree PHM)后续。详见
`six_max_nlhe_target.md` S6。

**6-max 范式切换**:多人一般和 → CFR 不保证收敛 Nash、**LBR/exploitability 失去理论意义**(只当诊断,质量以
实测对战为准)、无"训到 floor 就停"、无强 6-max 公开参考对手(不像 Slumbot 之于 HUNL)。详见 target 文档。

## 算法正确性(验证完成的基础,6-max 直接复用)

| 项目 | 状态 | 依据 |
|---|---|---|
| Kuhn / Leduc Vanilla CFR | ✅ 收敛 closed-form `-1/18` / exploit `<0.1` | `tests/cfr_kuhn.rs` `cfr_leduc.rs` |
| Leduc ES-MCCFR / LCFR-MCCFR | ✅ `ev_p0` 收敛 -0.087;ES 路径 BLAKE3 byte-equal anchor | `tools/leduc_es_mccfr_report` |
| 简化 NLHE ES-MCCFR / LCFR | ✅ LCFR 优化路径 100M LBR 1,233 → 500M 1,126(100M 即饱和) | `run_lcfr_*` on vultr |
| dense 后端 + v4 bucket | ✅ byte-equal HashMap(5 对照)+ 100M LBR 1,143 ≈ baseline(同质量);吞吐 ~2.2× 且长 run 不塌、RAM 平 5.2 GiB、ckpt 不暴涨 | `tests/dense_nlhe_trainer.rs` |
| AIVAT 评测器 | ✅ 无偏全证;真日志降方差 1.21×(精确项 deals+runout) | `tests/aivat_nlhe_*.rs` `docs/aivat_eval.md` |
| **CFR trainer / 规则引擎 6-max N-generic** | ✅ 亲验:`recurse_es` 取 `payoff(traverser)` 不取负、traverser `% n_players`;规则多人 side pot 返回 per-seat payoff 向量 | `trainer.rs:493,653` `state.rs:846–938` |

## 6-max 复用 / 需重做(亲验 file:line,2026-05-30)

**✅ 已 N-generic 直接复用**:规则引擎(side pot / showdown / per-seat payoff,`state.rs:846–938`,
`config.rs::default_6max_100bb()`)、CFR trainer(ES-MCCFR/LCFR alternating traverser,`trainer.rs:493,653`)、
InfoSetId position 已留 4 bit(支持 0..15 座,`abstraction/map/mod.rs`)、dense 后端 / checkpoint(不绑人数)。

**⚠️ 中等改动**:`SimplifiedNlheGame` 硬编码 `n_seats=2`(`nlhe.rs:310`)→ 参数化 + 重建 betting tree(HU
240,096 节点 / 119.7M infoset 是 2 人数,6-max 暴涨,S2 量);动作抽象扩到**按位置**;Game trait 零和约束
(`game.rs:120`)推广为"全玩家和=0"。

**❌ 重活(Pluribus 论文难点)**:① 抽象层 equity/OCHS 假设 **1 对手**(`equity.rs:39,79`)→ 多人 equity +
重聚类桶(**最大未知数,S3 先验证**);② 评测重构——LBR `probe_idx%2`(`lbr.rs:5`)多人失义、AIVAT 单对手要
推广多对手、要新 baseline + 解决无强参考对手。

> **现状(2026-06-03,上为 05-30 初评快照)**:⚠️ 中等改动**已做**(P4 `08b3edc` 参数化 `new_with_abstraction`,
> n_seats 由 config 驱动)。❌① **已消解**(S3 实测 HU 单对手桶可复用进 A3×A4 ≤3-way,不需重做多人 equity)。
> ❌② **评测已重构**:S5 off-tree advisor 引擎(`blueprint_advisor.rs`)+ ① 跨抽象 h2h + ② OpenPoker 客户端
> (强参考对手缺口已接、live smoke 已过)。剩 = AIVAT 多对手(S5③,按需) + 绝对强度量化挂场。

## 关键代码入口

- CFR/LCFR-MCCFR:`src/training/trainer.rs`(`EsMccfrTrainer` / `recurse_es` / `recurse_es_parallel` /
  `maybe_lcfr_rescale`;`with_lcfr_period(P)`)。Brown & Sandholm 2018 Discounted MCCFR(arxiv 1809.04040)。
- NLHE state + 树:`src/training/nlhe.rs` + `nlhe_betting_tree.rs`;按街动作抽象 `src/abstraction/action.rs`
  (`StreetActionAbstraction`,单一来源 `nlhe.rs::nlhe_action_abstraction()`)。
- 抽象:preflop 169 lossless `src/abstraction/preflop.rs`;equity/OCHS `src/abstraction/equity.rs`;
  InfoSetId 打包 `src/abstraction/map/mod.rs`。
- 评测:LBR `src/training/lbr.rs`;baseline `src/training/nlhe_eval.rs`;AIVAT `src/training/aivat*.rs`。
- **S5 跨抽象 advisor 引擎**:`src/training/blueprint_advisor.rs`(一张权威 GameState + 每 blueprint 一份抽象影子,
  off-tree 翻译推进:`outgoing_action` / `advance_shadow_by_applied` / `play_cross_abstraction_hand` /
  `evaluate_cross_abstraction_h2h`)。薄壳:① h2h `tools/six_max_blueprint_h2h`;② OpenPoker
  `tools/openpoker_advisor.rs` + `tools/openpoker_play.py`(WS driver);HU `tools/slumbot_advisor.rs`(复用同核)。
- 规则:`src/rules/`(config / state / side pot / showdown)。

代码结构:`src/{rules,hand_eval,abstraction,training}/` + `tests/`(cargo test)+ `tools/`(诊断/训练 binary)。

## 持久 artifact + 主机

- **vultr `~/dezhou_20260508/artifacts/`**:1B dense ckpt(`run_dense_lockfree/`,9.3 GB,Slumbot 续跑 +
  6-max baseline 候选)、HU bucket 表(1000 / 500,**HU equity,6-max 需重做多人桶**;b3sum/EVR 见 git 历史)。
- 主机表:

| host | 角色 | 状态 |
|---|---|---|
| vultr 64.176.35.138 (4 vCPU / 7.7 GiB) | 持久存储 + 短测试 | 长期持有;**跑不动 NLHE 训练**(3M update 进 swap) |
| AWS(按需起/停,IP 每次变) | 训练 | HU 用 c6a.8xlarge(32 vCPU);6-max 大概率不够,待 S2 sizing 定更大机 |

## 构建 / 测试

命令见 `CLAUDE.md`。**测试一律在 vultr 远端跑**(本机仅 build / fmt / clippy,本机跑结果不可信);
性能/正确性 SLO + BLAKE3 anchor 在 `cargo test --release -- --ignored`;可选 PokerKit 跨验证(6-max S1 要用)。

## 文档维护规则

- 工作笔记 / 临时数据 → `docs/temp/*.md`。
- 本文档 = 当前代码真实状态入口;主线验收目标在 `docs/six_max_nlhe_target.md`。
