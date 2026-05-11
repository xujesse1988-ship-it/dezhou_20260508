[external_compare] INFO: OpenSpiel (pyspiel) not installed; 走纯本地 169 类生成 fallback (D-263 不要求安装)
[bucket_table_reader] WARN: 'blake3' Python package not installed; skipping trailer integrity check (pip install blake3 to enable)
# Stage 2 External Abstraction Compare (D-260 / D-261 / D-262 / D-263)

目标：F3 [报告] 起草时一次性接入对照 sanity 脚本（D-263），对照 169 lossless 等价类成员集合（D-261 字面 「可能不同顺序但 169 类成员一致」）。

- 对照路径：纯本地 169 类生成 fallback
- 5-action 默认配置：D-200 锁定 `{ Fold, Check, Call, BetRaise(0.5×pot), BetRaise(1.0×pot), AllIn }` 与 path.md §阶段 2 字面对齐 ✓ (文字对照，无数据源)

## Preflop 169 lossless class membership

| Partition | ours | ref | common | equal | expected |
|---|---:|---:|---:|---|---:|
| paired | 13 | 13 | 13 | ✓ | 13 |
| suited | 78 | 78 | 78 | ✓ | 78 |
| offsuit | 78 | 78 | 78 | ✓ | 78 |

**结论**：169 类成员集合 byte-equal（13 paired + 78 suited + 78 offsuit）。D-262 P0 阻塞条件**不触发**。

## Postflop bucket

D-261 字面：「**不**做 postflop bucket 一一对照（OpenSpiel postflop 默认配置与我方 500/500/500 不同，且 bucket 边界本就因 cluster seed 不同而异）」。

## Slumbot

D-260 字面：「Slumbot bucket 数据获取不确定，**不强求**接入；如未来 stage 4 训练时发现 abstraction 质量与公开 bot 显著偏离，追加 D-260-revM 重新评估接入工作量」。本 sanity 不做 Slumbot 对照。

## Rust D-217 closed-form artifact round-trip

- artifact preflop lookup table 1326 hole_id → hand_class_169 enumeration:
  - paired classes: 13/13 (每类 6 hole 组合 uniform: ✓)
  - suited classes: 78/78 (每类 4 hole 组合 uniform: ✓)
  - offsuit classes: 78/78 (每类 12 hole 组合 uniform: ✓)
  - over-id classes (>=169): 0 (expect 0)
  - total: 1326/1326
- **Rust D-217 closed-form ↔ Python local 169 类 byte-equal partition counts ✓**

