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

**第一步 = S1:钉死 6-max 规则正确性(正确性优先,先于任何训练)**——引擎虽已 N-generic 但从没在 6-max 下像
HU 那样验过:多人 side pot(3+ 同时 all-in)、多人 showdown 顺序、dead button、6-max 盲注/行动顺序,1M 手零
非法 + PokerKit 跨验证。

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

## 关键代码入口

- CFR/LCFR-MCCFR:`src/training/trainer.rs`(`EsMccfrTrainer` / `recurse_es` / `recurse_es_parallel` /
  `maybe_lcfr_rescale`;`with_lcfr_period(P)`)。Brown & Sandholm 2018 Discounted MCCFR(arxiv 1809.04040)。
- NLHE state + 树:`src/training/nlhe.rs` + `nlhe_betting_tree.rs`;按街动作抽象 `src/abstraction/action.rs`
  (`StreetActionAbstraction`,单一来源 `nlhe.rs::nlhe_action_abstraction()`)。
- 抽象:preflop 169 lossless `src/abstraction/preflop.rs`;equity/OCHS `src/abstraction/equity.rs`;
  InfoSetId 打包 `src/abstraction/map/mod.rs`。
- 评测:LBR `src/training/lbr.rs`;baseline `src/training/nlhe_eval.rs`;AIVAT `src/training/aivat*.rs`。
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
