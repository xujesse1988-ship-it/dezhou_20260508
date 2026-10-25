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
python3 -m opponent_analysis report --db artifacts/opponent_db --platform poker_manager --out artifacts/opponent_report [--hero bp3]
python3 -m opponent_analysis report --db artifacts/opponent_db --platform openpoker --out artifacts/opponent_report

# 实时：跟随正在写入的 hh 日志，增量更新画像，浏览器打开 http://127.0.0.1:8780/
python3 -m opponent_analysis live --db artifacts/opponent_db --platform poker_manager \
    --follow 'artifacts/poker_manager_live_*/poker_manager_hh.jsonl' [--hero bp3] [--port 8780]

# 测试
python3 -m unittest discover -s opponent_analysis/tests -t .
```

## 实时画像（`live`）

- **只读日志**：数据只来自 driver 写的 `*_hh.jsonl`（每打完一手追加一行）。不调用牌桌服务的任何接口
  （包括公开的 `/api/hands`，它会给出所有座位的底牌，不用）。
- `--follow` 给路径或通配符（加引号），每 2 秒重新展开一次，bot 换批次新建的 run 目录会自动接上。
  只读到换行为止，写了一半的行留到下次；坏行计数后跳过；文件被截断或替换时从头重读。
- 每行和 `ingest` 走同一套适配器 + 重放校验，写进同一个数据库（按 hand_id 去重），所以重启后从数据库续上，
  跟随的文件重读一遍只是去重。同一手被两个我方 bot 都记了时只收第一份。
- 有新手时最多每 5 秒（`--refresh`）重算一次画像；结果和对同一数据库跑 `report` 完全一致（有测试覆盖）。
  新读到的手比已有的更早（例如后来才加进一个旧文件）时按时间顺序整体重建，保证漂移检验的前后半程划分正确。
- 河牌牌力百分位缓存在 `<db>/<平台>/river_pct_cache.json`（每个约 0.1–0.2 秒），重启不用重算。
- 我方名字 = 每手日志的记录座位，加上 `--hero` 给的名字（日志没被跟随的我方 bot），都不计入对手和群体基准。
  `report` 也有 `--hero`，给同样的名字时两边结果一致。
- 默认只监听 `127.0.0.1`。在实盘机器上跑时用 ssh 隧道看：`ssh -L 8780:127.0.0.1:8780 <机器>`，再开本机浏览器。
- 页面：最近一手的牌桌座次（点名字看详情）、玩家总表（可排序、选列、只看在桌）、单人详情
  （单点剥削估计、近期变化、与群体的显著差异、全部统计的区间条和走势图）、最近 40 手、数据质量。
- **近期口径**（`--recent-hands`，默认 1000，`report` 也有）：每个玩家自己的最近 N 手单独算一套统计和单点剥削估计，
  页面上「全部历史」和「最近 N 手」是两张玩家表（点标题可收起），单人详情两份剥削估计都列出。「近期变化」= 每项统计最近 N 手对比更早的手做两比例检验
  （两边各至少 30 次机会），所有对手所有统计一起做 Benjamini–Hochberg（错误发现率 10%）；同时给出两边桌上人数
  2–4 / 5–6 / 7–9 的占比，某一档变化超过 15 个百分点时提示可能是桌况。替代了原来的「前后半程漂移」。
  走势图按玩家自己的手牌顺序分成约 24 段（每段至少 250 手），画原始比例，阴影是最近 N 手。
  另有 `/report.md`、`/profile.json`（与 `report` 命令的输出相同）、`/api/snapshot`。

数据库：`<db>/<平台>/hands.jsonl`（通过校验的手）、`rejected.jsonl`（被拒的手和原因）、`ingest_log.jsonl`
（每次导入的文件、sha256、计数）。同一 hand_id 只收一次；被拒过的手再导入会跳过，修了适配器后删掉
`rejected.jsonl` 重导即可。

- **一个平台目录同时只能有一个进程写**（`ingest` 或 `live`，锁文件 `write.lock`，里面是持锁进程的 pid）。
  `live` 在跑时对同一库 `ingest` 会直接报错退出（退出码 2），先停 `live` 再导入。`report` 只读，不受影响。
  读库时按 hand_id 去重，加锁之前并发写入留下的重复行不会重复计数。
- 入库前除了重放校验，还要求底牌和公共牌格式正确、不重复（否则 `bad_cards`），并把这一手的统计提取跑一遍，
  出错记 `spots_error`。这样库里的每一手都保证 `report` 和 `live` 能处理，不会因为一手坏数据整库打不开。

输出：`report_<平台>.md`（给人看）和 `profile_<平台>.json`（每个对手每项统计的后验参数和候选漏洞）。

## 流水线

| 文件 | 作用 |
|---|---|
| `adapters.py` | 各平台原始格式 → `RawHand`。OpenPoker 的 all-in 不带金额（已知缺陷）由重放按剩余筹码补；离桌座位缺 final_stacks 时只在其已弃牌时补 |
| `replay.py` | 自带的下注轮重放：盲注、行动顺序、本轮结束、全下。行动顺序不对、金额对不上、零和不成立都拒收并记原因 |
| `spots.py` | 每个决策点 → 观测（机会口径）。超时代打的决策跳过 |
| `stats.py` | Beta 后验、按平台拟合的群体先验（矩估计）、与群体比较用精确二项检验、Benjamini–Hochberg |
| `exploit.py` | 单点公式：弃牌过多 / 河牌跟注过多 / 河牌诈唬过少或过多，以及各自的保本点。下注额按跟注方能跟上的部分算（超出他筹码的部分不算） |
| `cards.py` | 河牌牌力百分位（对所有可能底牌） |
| `report.py` | 群体基准、对手总表、逐人候选漏洞、与群体的显著差异、前后半程漂移 |
| `live.py` + `live.html` | 跟随 hh 日志、增量累加（`report.Accumulator`）、HTTP 页面 |

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
