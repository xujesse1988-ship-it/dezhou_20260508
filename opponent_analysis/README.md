# opponent_analysis — 离线对手可剥削性分析

设计见 `docs/temp/opponent_exploitability_framework_2026_10_20.md`。本目录是独立系统：

- 纯 Python 标准库，不引用 `src/`、`tools/` 的任何代码，不改原有代码。
- **平台之间不混合**：每个平台单独一个数据库目录、单独的群体基准、单独的报告。导入时检测文件格式，
  和指定平台不符直接报错。
- 每个平台的规则（两人局谁是按钮、摊牌是否强制亮牌、超时代打从哪里识别）写在 `platforms.py`，不共用默认值。

## 用法（在仓库根目录运行）

```bash
# 导入：给 run 目录（自动找 *_hh.jsonl 和 *_ws.jsonl），或单个 hh 文件
python3 -m opponent_analysis ingest --db artifacts/opponent_db \
    --run artifacts/openpoker_runs/run5_6cf5889_reconnect poker_manager_live_20261020_105159_128451
python3 -m opponent_analysis ingest --db artifacts/opponent_db --hh openpoker9_hh_searchon1000_prewarm_par4_b500.jsonl

# 报告：一次一个平台
python3 -m opponent_analysis report --db artifacts/opponent_db --platform poker_manager --out artifacts/opponent_report
python3 -m opponent_analysis report --db artifacts/opponent_db --platform openpoker --out artifacts/opponent_report

# 测试
python3 -m unittest discover -s opponent_analysis/tests -t .
```

数据库：`<db>/<平台>/hands.jsonl`（通过校验的手）、`rejected.jsonl`（被拒的手和原因）、`ingest_log.jsonl`
（每次导入的文件、sha256、计数）。同一 hand_id 只收一次；被拒过的手再导入会跳过，修了适配器后删掉
`rejected.jsonl` 重导即可。

输出：`report_<平台>.md`（给人看）和 `profile_<平台>.json`（每个对手每项统计的后验参数和候选漏洞）。

## 流水线

| 文件 | 作用 |
|---|---|
| `adapters.py` | 各平台原始格式 → `RawHand`。OpenPoker 的 all-in 不带金额（已知缺陷）由重放按剩余筹码补；离桌座位缺 final_stacks 时只在其已弃牌时补 |
| `replay.py` | 自带的下注轮重放：盲注、行动顺序、本轮结束、全下。行动顺序不对、金额对不上、零和不成立都拒收并记原因 |
| `spots.py` | 每个决策点 → 观测（机会口径）。超时代打的决策跳过 |
| `stats.py` | Beta 后验、按平台拟合的群体先验（矩估计）、Benjamini–Hochberg |
| `exploit.py` | 单点公式：弃牌过多 / 河牌跟注过多 / 河牌诈唬过少或过多，以及各自的保本点 |
| `cards.py` | 河牌牌力百分位（对所有可能底牌） |
| `report.py` | 群体基准、对手总表、逐人候选漏洞、与群体的显著差异、前后半程漂移 |

## 现有数据的校验结果（2026-10-20）

| 平台 | 通过 | 被拒 | 说明 |
|---|---|---|---|
| poker_manager | 1022 | 0 | hh 自带每个动作的投入、剩余筹码和超时标记 |
| openpoker | 3856 | 38 | 被拒的都是日志缺动作（例如转牌下注没记，下一个动作对不上）；无 ws 日志的 run 无法识别超时代打 |

摊牌亮牌实测：两个平台目前都是摊牌时所有仍在牌里的手全部亮出（openpoker 3018/3018 个座位，
poker_manager 127/127）。`platforms.py` 里按平台设置 `reveal_policy`；报告每次都会重新实测，
规则或实测不支持「强制亮牌」时，河牌诈唬比例分析自动停用。

## 还没做

- 设计稿 §5.2 的求解器锁定对手（L2）。
- 牌力百分位按所有可能底牌算，没有按对手在该线路上的范围加权。
- 码深分档只进了观测的维度，报告还没按码深拆表。
