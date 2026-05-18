# H3 500M Checkpoint Investigation

日期：2026-05-18

对象：

- `artifacts/h3_500m_threads12/nlhe_es_mccfr_final_000500000000.ckpt`
- `artifacts/h3_500m_threads12/nlhe_es_mccfr_auto_*.ckpt`
- bucket table: `artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin`

## 结论

当前 500M checkpoint 不能作为“简化 heads-up NLHE 策略正在收敛”的可信证据。最初的 LBR proxy 曲线显示 0 到 100M 有明显改善，但 100M 到 500M 没有继续下降；进一步抽查 preflop average/current strategy 后，发现策略出现明显不合理的极端动作，例如 BB 对 SB limp 后用 `AKo`、`AKs` 高比例走 100BB all-in。

进一步检查确认，这不是检查工具本身的问题，而是简化 NLHE 的 `InfoSetId` 编码存在状态合并错误：同一个 `InfoSetId` 可以对应不同下注额和不同真实动作金额的决策点，导致 CFR regret 和 average strategy 把不同语义的动作混在同一表项里累计。

因此：

- 现有 `nlhe_es_mccfr_final_000500000000.ckpt` 策略质量不可信。
- 现有 checkpoint 的收敛趋势判断不可信。
- 在修复 InfoSet 编码之前，继续扩大训练量不能解决根因。

## 关键证据

### LBR Proxy 曲线

使用 `tools/nlhe_h3_report.rs` 对 0/100M/200M/300M/400M/500M checkpoint 评估，10k probes / 16 rollouts 下：

| checkpoint | updates | mean BR chips | SE |
|---|---:|---:|---:|
| uniform | 0 | 2711.58 | 35.09 |
| 100M | 100000008 | 1006.45 | 22.09 |
| 200M | 200000004 | 1014.61 | 21.99 |
| 300M | 300000000 | 1020.09 | 21.94 |
| 400M | 400000008 | 1026.88 | 22.08 |
| 500M | 500000000 | 1031.75 | 22.22 |

解释：

- `uniform -> 100M` 明显改善。
- `100M -> 500M` 没有继续改善，数值反而小幅上升。
- 该指标只是 H3 local best-response proxy，不是 formal exploitability，但足以提示趋势异常。

### Preflop 策略异常

从 `nlhe_es_mccfr_final_000500000000.ckpt` 加载 average strategy 后，抽样 preflop 策略：

BB after SB limp：

| hand | average strategy |
|---|---|
| `AKo` | Check 0.0%, Raise 2bb 0.8%, Raise 3bb 12.0%, AllIn 87.2% |
| `AKs` | Check 0.0%, Raise 2bb 0.3%, Raise 3bb 81.6%, AllIn 18.0% |
| `88` | Check 0.0%, Raise 2bb 0.9%, Raise 3bb 83.6%, AllIn 15.5% |

进一步读 checkpoint 内 regret 后，发现这不是 average strategy 早期噪声残留，current strategy 也已经学歪：

| spot | hand | current strategy |
|---|---|---|
| BB after SB limp | `AKo` | AllIn 100% |
| BB after SB limp | `AKs` | AllIn 100% |

### 最小 InfoSet 碰撞反例

在同一手牌、同一 SB hole、同一 preflop betting state 下：

```text
SB limp 后，BB raise 0.5 pot:
info = 0x23003d002
actions = Fold / Call 200 / Raise 400 / Raise 600 / AllIn 10000

SB limp 后，BB raise 1.0 pot:
info = 0x23003d002
actions = Fold / Call 300 / Raise 600 / Raise 900 / AllIn 10000

same_info_id = true
```

这是致命问题：同一个 `InfoSetId` 下，action index 数量相同，但动作语义和真实金额不同。

例如：

- index 1 在一个状态是 `Call 200`，另一个状态是 `Call 300`。
- index 2 在一个状态是 `Raise 400`，另一个状态是 `Raise 600`。
- index 3 在一个状态是 `Raise 600`，另一个状态是 `Raise 900`。

CFR 表只按 `InfoSetId + action_index` 累计 regret/strategy sum，因此这些状态被错误合并后，regret 更新会互相污染。

## 疑似根因

问题集中在 `src/training/nlhe.rs::SimplifiedNlheGame::info_set`。

当前简化 NLHE 的 `InfoSetId` 主要编码：

- preflop hand class / postflop bucket id
- position bucket
- stack bucket
- betting state
- street tag
- action availability mask

其中 action availability mask 只保证“同一个 `InfoSetId` 的 action 数量一致”，但不能保证“同一个 `InfoSetId` 的 action 语义一致”。面对不同下注额时，合法动作集合可以有相同 mask，但具体 `Call/Raise` 金额不同。

这违反了 CFR 表的基本要求：同一个 information set 下，每个 action index 必须代表同一种抽象动作语义。

## 影响范围

已知影响：

- `EsMccfrTrainer<SimplifiedNlheGame>` 训练出的 NLHE checkpoint。
- `average_strategy` / `current_strategy` 查询结果。
- 基于这些 checkpoint 的 LBR proxy、baseline evaluation、策略 hash 和人工策略抽查。

不直接否定：

- Kuhn / Leduc 的 CFR 与 exploitability 测试。
- checkpoint 二进制读写校验。
- H3 report 工具本身的确定性与加载逻辑。

## 修复方向

修复目标不是只让 action count 一致，而是让同一个 `InfoSetId` 下 action index 的语义一致。

可选方向：

1. 在 `InfoSetId` 中加入 betting history / pot-size / to-call / last-raise-size 的抽象编码。
2. 对 preflop 单独使用更精细的 betting sequence 编码，例如 limp、open size、3bet size、4bet size 等。
3. 对 postflop 加入 pot bucket、to-call bucket、SPR bucket 或 action sequence bucket。
4. 如果 64-bit layout 空间不足，应设计新的 stage-3 NLHE-specific infoset key，而不是继续挤压现有 `bucket_id` mask。

## 修复验收建议

修复后至少需要新增以下测试：

1. `InfoSetId` collision regression：
   - 构造 SB limp 后面对 BB 0.5 pot raise 和 1.0 pot raise。
   - 断言两个状态 `InfoSetId` 不相等，或 action abstraction 语义完全等价。
2. `same InfoSetId action semantics` property test：
   - 随机采样 preflop/postflop 状态。
   - 对相同 `InfoSetId` 的所有状态，断言 `legal_actions` 的抽象角色和金额桶一致。
3. 小规模训练 sanity：
   - fixed seed 训练后抽查 preflop current/average strategy。
   - BB vs limp 的 `AKo/AKs/88` 不应稳定学成 100BB all-in 纯策略。
4. 重新跑 100M/500M checkpoint 曲线：
   - LBR proxy 至少不应在 100M 后持续反弹。
   - 若实现 formal sampled BR / NashConv，应以该指标作为主收敛判断。

## 当前处理建议

在修复前，不建议继续使用 `artifacts/h3_500m_threads12` 下的 checkpoint 做策略质量判断或后续报告引用。该目录可以保留为 bug reproduction artifact。
