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
python3 -m opponent_analysis report --db artifacts/opponent_db --platform wopo --out artifacts/opponent_report

# 实时：跟随正在写入的 hh 日志，增量更新画像，浏览器打开 http://127.0.0.1:8780/
python3 -m opponent_analysis live --db artifacts/opponent_db --platform poker_manager \
    --follow 'artifacts/poker_manager_live_*/poker_manager_hh.jsonl' [--hero bp3] [--port 8780]
# wopo：再加 --play-events，页面顶部多一个实时「行动指南」（见下）
python3 -m opponent_analysis live --db artifacts/opponent_db --platform wopo \
    --follow "$D/wopo_hh.jsonl" --play-events "$D/wopo_play_events.jsonl" --hero hero

# wopo 牌局回放：浏览器打开 http://127.0.0.1:8781/（见「牌局回放」一节）
python3 -m opponent_analysis history --run 'artifacts/wopo*/' [--port 8781]

# 测试
python3 -m unittest discover -s opponent_analysis/tests -t .
```

## 实时画像（`live`）

- **只读日志**：数据只来自 driver 写的 `*_hh.jsonl`（每打完一手追加一行），以及（可选）wopo driver 的事件流
  `wopo_play_events.jsonl`（行动指南用，只有 hero 自己的底牌和服务器公开给 hero 的信息）。不调用牌桌服务的任何接口
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
- 页面：（给了 `--play-events` 时）顶部的实时行动指南（见下节）、最近一手的牌桌座次（点名字看详情）、玩家总表（可排序、选列、只看在桌）、单人详情
  （单点剥削估计、近期变化、与群体的显著差异、翻前全下的亮牌、全部统计的区间条和走势图）、最近 40 手、数据质量。
- **近期口径**（`--recent-hands`，默认 1000，`report` 也有）：每个玩家自己的最近 N 手单独算一套统计和单点剥削估计，
  页面上「全部历史」和「最近 N 手」是两张玩家表（点标题可收起），单人详情两份剥削估计都列出。「近期变化」= 每项统计最近 N 手对比更早的手做两比例检验
  （两边各至少 30 次机会），所有对手所有统计一起做 Benjamini–Hochberg（错误发现率 10%）；同时给出两边桌上人数
  2–4 / 5–6 / 7–9 的占比，某一档变化超过 15 个百分点时提示可能是桌况。替代了原来的「前后半程漂移」。
  走势图按玩家自己的手牌顺序分成约 24 段（每段至少 250 手），画原始比例，阴影是最近 N 手。
  另有 `/report.md`、`/profile.json`（与 `report` 命令的输出相同）、`/api/snapshot`。

## 行动指南（wopo，`--play-events`）

hh 日志每手打完才写一行，看不到正在打的这手。wopo driver（`tools/wopo_play.py`）在写 play.log 的同时，
把每一行同时写成一条 JSON 事件（`--play-events`，缺省就是 play.log 同目录的 `wopo_play_events.jsonl`，
`--play-events off` 关闭）。`live` 加 `--play-events <文件或通配符>` 后，页面顶部显示「行动指南」：

- 当前这手：编号、庄位、盲注 / ante、状态（进行中 / 已结算 / 作废及原因）；hero 底牌、公共牌、底池；
  最新的决策建议（建议动作和金额放大显示，下面是各动作概率、来源、耗时；过期的划掉并标出原因，
  还在算时显示已等待秒数）；hero 实际怎么打的、和建议是否一致；各座位名字 / 位置 / 开局栈 / 剩余栈 /
  本手最近动作，旁边是这个玩家画像里的手数、入池率 / 翻前加注率、候选漏洞数、类型（点名字打开下面的
  单人详情）；对手牌力估计（见下）；按街分组的动作时间线；结算（各座输赢、亮牌）。
- 对手牌力估计：advisor 的范围日志（`--range-log`，driver 开了搜索就默认写在行动文件同目录的
  `wopo_ranges.jsonl`）每行带每个对手的牌力分布（`strength`：葫芦 / 三条 / 两对 / 超对或顶对 / 其它一对 / 没中等的概率）。
  `live` 默认跟随每个 `--play-events` 同目录的 `wopo_ranges.jsonl`，`--play-ranges <文件或通配符>` 另指，`--play-ranges off` 关。
  每条街显示最近一次搜索用的范围（街开始时的范围，不含这条街的动作），条形图从左到右由强到弱；旧的街折叠在下面。
  服务端只留牌力分布（1326 个权重读完就丢，不发给页面）；范围日志行大，第一次见到时读最后 64 MB。
  没有 `strength` 的旧日志不显示这一块。
- 下面是最近几手的折叠列表（一行摘要：底牌、公共牌、hero 输赢、决策点数与同 / 不同 / 无建议 / 过期次数）。
- 页面每秒轮询 `/api/play?since=<version>`，没有新事件时只回 `{"version", "unchanged": true, "now"}`。
  `now` 是服务端时间，页面用它算「已等待几秒」，所以浏览器和实盘机器时钟不同步也没关系。
- 没给 `--play-events` 时 `/api/play` 回 `{"enabled": false}`，页面不显示这个面板，其余部分照旧。

服务端只在内存里保留最近 `--play-hands`（默认 20）手，不写数据库，和画像统计无关。事件文件还不存在时等它出现；
只读到换行为止；第一次见到一个文件时只读它最后 8 MB（约几百手），所以服务重启后马上能恢复最近 20 手，
长时间的会话也不用整份重读；文件被替换时从头读，完全相同的事件去重；被挤出窗口的旧手再来的事件（例如过期决策）忽略。
通配符匹配到多个文件时按每手第一条事件的时间排序。事件按 `hand_id` 归到各手，按到达顺序排列，
服务端把它们整理成每手的状态（座位、栈、公共牌、各决策点的建议与 hero 实际动作、结算），页面只用结构化字段，
不解析文字。

### 启动（wopo 实盘）

driver 只要照旧给 `--play-log $D/play.log`，就会同时写 `$D/wopo_play_events.jsonl`，不用加参数。
画像服务（在同一台机器上跑，ssh 隧道看）：

```bash
D=artifacts/wopo_ante200_9b_exploit_live_20261029
setsid nohup python3 -m opponent_analysis live --db artifacts/opponent_db_ante100_20261029 --platform wopo \
    --follow "$D/wopo_hh.jsonl" --play-events "$D/wopo_play_events.jsonl" --hero hero \
    >> $D/opponent_live.log 2>&1 < /dev/null &
# 本机：ssh -L 8780:127.0.0.1:8780 <机器>，再开 http://127.0.0.1:8780/
```

画像服务和 driver 谁先启动都行；driver 重启后接着往同一个事件文件追加。

### 事件格式

每行一个 JSON 对象。所有事件都有：`ts`（driver 机器的 Unix 秒，3 位小数，与 play.log 行首时间同一时刻）、
`type`、`hand_id`、`table_id`、`text`（play.log 这一行去掉时间后的文字，只作对照）。play.log 每一行恰好对应一条事件，
顺序相同（driver 自测逐行核对，并且只用下面的结构化字段就能还原出每一行的文字）。

| type | play.log 行 | 字段 |
|---|---|---|
| `hand_start` | `──── <hand_id> 庄位 …` | `button`、`sb`、`bb`、`ante`（桌面筹码，0 = 无）、`hero_seat`、`n_seats`（solver 座位数） |
| `hole` | `底牌 …` | `cards`（hero 两张底牌） |
| `seats` | `座位 …`（首个快照后） | `seats`：每个发牌座 `{seat, name, start（开局栈，交 ante / 盲注前）, stack（交完后的剩余栈）, bet（已下的盲注）, hero, roles（`BTN` / `SB` / `BB` / `补盲`）}`；`button`、`sb_seat`、`bb_seat`、`posts`（补盲座）、`pot` |
| `action` | 对手动作 | `seat`、`name`、`street`、`turn`（这个动作之前已有的动作数）、`action`（fold / check / call / raise / all_in，按投入归类）、`to`（raise / all_in 的本街总额，其余 null）、`delta`（本次投入）、`to_call_before`（0 时 raise 在 play.log 写成 bet）、`stack_before`、`stack_after`、`pot_after` |
| `hero_action` | hero 实际动作（`← hero 实际行动 …`） | 同 `action`，另有 `advice`（这个决策点写进行动文件的建议 `{action, amount, text}`，没有 = null）、`match`（`same` / `diff`；没有建议 = null）。raise 和 all_in 互相算一致 |
| `board` | `翻牌 / 转牌 / 河牌 …` | `street`、`cards`（本街新发的牌）、`board`（到本街为止的全部公共牌）、`pot` |
| `hero_turn` | `>> 轮到 hero …` | `turn`（决策点编号，和后面的 `decision` / `hero_action` 对应）、`street`、`pot`、`to_call`、`valid`（`can_check` / `can_call` / `can_raise` / `min_raise` / `max_raise`，本街总额）、`hole`、`board`、`table_seq` |
| `decision` | `>> 决策 …` | `turn`、`street`、`pot`、`to_call`、`valid`、`action`、`amount`（raise 的本街总额）、`all_in`（all_in，或 raise 到最大额）、`advice`（play.log 里的建议文字）、`probs`（`[[动作, 概率]]`，兜底动作为 null）、`chosen`、`source`、`decide_ms`、`stale`（过期原因全文；null = 已写进行动文件）、`stale_kind`（`hero_acted` / `hand_over` / `hand_dead`）、`written`、`rejected`（advisor 原建议不合法被换成安全动作时的原回复） |
| `dead` | `本手作废：…` | `reason`、`tag`（冒号前的类别）。开局消息本身不合法时只有这一条，没有 `hand_start` |
| `result` | `结算 …` | `stacks`：每个发牌座 `{seat, name, start, final, net}`；`pot`、`board`、`winners`、`shown`（服务器 `hand_result` 的原样字段）、`notes`（结算核对不过的说明）、`server_ts` |

`decision` 是 worker 线程回调写的，可能在下一手已经开始后才到（`stale_kind = hand_over`），按 `hand_id` 归回原来那手。

`/api/play` 返回 `{enabled, version, max_hands, hands, files, counts, errors, patterns, now}`；`hands` 新的在前，每手是
`{hand_id, table_id, first_ts, last_ts, partial（开头没读到）, button, sb, bb, ante, hero_seat, hole, seats, board,
street, pot, turns, dead, result, hero_net, status（playing / done / dead / error）, events}`，`seats` 里的 `stack` 跟到最近一个动作
（结算前），`turns` 每项 `{turn, street, pot, to_call, valid, ts, decision, hero_action}`。某手的事件整理不出来时
`status = error`，另带 `error`（异常信息），其余字段同样齐全（只填了时间和 table_id），错误也记进 `errors`；页面上这手
显示一行错误说明，其它手照常刷新。

数据库：`<db>/<平台>/hands.jsonl`（通过校验的手）、`rejected.jsonl`（被拒的手和原因）、`ingest_log.jsonl`
（每次导入的文件、sha256、计数）。同一 hand_id 只收一次；被拒过的手再导入会跳过，修了适配器后删掉
`rejected.jsonl` 重导即可。

- **一个平台目录同时只能有一个进程写**（`ingest` 或 `live`，锁文件 `write.lock`，里面是持锁进程的 pid）。
  `live` 在跑时对同一库 `ingest` 会直接报错退出（退出码 2），先停 `live` 再导入。`report` 只读，不受影响。
  读库时按 hand_id 去重，加锁之前并发写入留下的重复行不会重复计数。
- 入库前除了重放校验，还要求底牌和公共牌格式正确、不重复（否则 `bad_cards`），并把这一手的统计提取跑一遍，
  出错记 `spots_error`。这样库里的每一手都保证 `report` 和 `live` 能处理，不会因为一手坏数据整库打不开。

输出：`report_<平台>.md`（给人看）和 `profile_<平台>.json`（每个对手每项统计的后验参数和候选漏洞）。

**金额单位**：画像统计里的金额（报告、`profile.json`、`/api/snapshot`，以及页面上的牌桌座次、玩家表的累计 / 每百手盈亏、
单人详情的剥削估计收益、翻前全下码深、最近手牌的底池和我方盈亏）一律是大盲，每手按这一手自己的大盲换算
（wopo 盲注会变；ante 算进底池和输赢）。数据库和日志里存的仍是原始筹码。顶部的行动指南不换算，照 play.log 显示原始筹码。

## 牌局回放（wopo，`history`）

在浏览器里逐步回放打完的 wopo 手牌。只读文件，不碰数据库：

- 手牌来自 driver 的 `wopo_hh.jsonl`（`--run` 给 run 目录、hh 文件或通配符，每次请求重新展开，文件变了就重读）。
  同目录有 `wopo_play_events.jsonl` 时，hero 每个决策点带上建议、和实际是否一致、advisor 的动作概率 / 来源 / 耗时
  （过期的建议也列出来），每个动作带上和上一个动作的时间差；有 `wopo_ranges.jsonl` 时显示每条街开始时的对手牌力估计。
  10-31 之前的 run 只有 hh，回放照常，只是没有建议和用时。
- 每手从开局筹码重新模拟（ante、盲注、补盲，然后逐个动作的 `contribution_delta`），每一步都有各座剩余栈、本街下注、
  是否弃牌 / 全下和底池；最后一步用服务器 `hand_result` 的 `final_stacks`，未跟注退回的部分单独标出。模拟出的剩余栈和
  hh 里每个动作的 `stack_before` 对不上时在页面「数据说明」里列出（现有 394 手没有一处对不上）。
- 每手同时跑一遍 `ingest` 用的适配器 + 重放校验；没通过（例如全下底池额外扣款造成的 `not_zero_sum`）只显示警告，
  照样能回放。driver 的 `result_notes` 也列在「数据说明」里。
- hh 没收的作废手（约 8%）不能回放，页头只给个数（事件流里开了局、hh 里没有的手）。
- 动作和事件流按顺序一一对应（座位、动作、投入都相同）；从第一个对不上的地方起不再挂建议和用时，并在「数据说明」里写明。
- 页面：左边手牌列表（按 run 过滤；搜索玩家名 / 编号 / 底牌，如 `AK`、`AKs`、`7h7c`；只看我方入池 / 摊牌 / 有对手亮牌 /
  和建议不同 / 到了翻后 / 有警告；按时间、我方输赢、底池排序；列表底部合计我方输赢 BB，没扣抽水）；右边牌桌（我方固定在下方），
  逐步播放，动作记录点一行跳到那一步，下面是结算表（开局、投入、分得、结束、输赢）。对手亮牌默认到结算才翻开，
  页头可以选提前亮出。金额可切换筹码 / BB（每手按自己的大盲）。键盘 ←/→ 单步，空格播放，↑/↓ 换手。
  地址带 `#key=<hand_id>&step=<n>`，可以直接发链接定位到某一手的某一步。
- `live --platform wopo` 也挂了同一个页面：`/history`，读的就是 `--follow` 的 hh 文件，打完一手刷新列表就能看到；
  画像页页头有「牌局回放」链接，行动指南里已结算的手有「回放」链接。
- 接口：`/api/history/hands`（列表）、`/api/history/hand?key=<key>`（一手的全部步骤）。同一个 hand_id 出现在多个 run 里时，
  后面的 key 是 `<hand_id>@<run 目录名>`。

## 翻前全下的亮牌

每个玩家自愿的翻前全下（主动推，或跟注全下；超时代打的不算）单独记一条：局面（按他行动前的加注次数：未开池 /
面对开池 / 面对 3bet / 面对 4bet 及以上，以及前面有几人跟）、位置、起手码深、有效码深（他和当时还在牌里的
其他人里最大筹码取小）、当时在牌里的人数；他亮了牌就附上手牌（169 类，如 `A5s`）。报告第 5 节、页面单人详情、
`profile.json` 的 `preflop_allin` 都有，页面玩家表另有「翻前全下 亮/总」一列。没人跟的推全下不会亮牌，
所以手牌样本偏向被跟注的那些。wopo 的 `hand_result.shown_cards` 实测包含摊牌时所有在牌里的对手
（2026-10-28 两轮 114/114），但不含我方自己的牌。

## 流水线

| 文件 | 作用 |
|---|---|
| `adapters.py` | 各平台原始格式 → `RawHand`。OpenPoker 的 all-in 不带金额（已知缺陷）由重放按剩余筹码补；离桌座位缺 final_stacks 时只在其已弃牌时补 |
| `replay.py` | 自带的下注轮重放：盲注、行动顺序、本轮结束、全下。行动顺序不对、金额对不上、零和不成立都拒收并记原因 |
| `spots.py` | 每个决策点 → 观测（机会口径）。超时代打的决策跳过 |
| `stats.py` | Beta 后验、按平台拟合的群体先验（矩估计）、与群体比较用精确二项检验、Benjamini–Hochberg |
| `exploit.py` | 单点公式：弃牌过多 / 河牌跟注过多 / 河牌诈唬过少或过多，以及各自的保本点。下注额按跟注方能跟上的部分算（超出他筹码的部分不算） |
| `cards.py` | 河牌牌力百分位（对所有可能底牌） |
| `report.py` | 群体基准、对手总表、逐人候选漏洞、与群体的显著差异、前后半程漂移、翻前全下的亮牌 |
| `live.py` + `live.html` | 跟随 hh 日志、增量累加（`report.Accumulator`）、HTTP 页面；`PlayFeed` / `hand_view` 跟随 wopo 事件流（行动指南，`/api/play`） |
| `history.py` + `history.html` | wopo 牌局回放：hh 每手重新模拟成逐步状态，挂上事件流里的建议和范围日志里的牌力估计（`history` 命令，`live` 里的 `/history`） |

## 现有数据的校验结果（2026-10-20）

| 平台 | 通过 | 被拒 | 说明 |
|---|---|---|---|
| poker_manager | 1022 | 0 | hh 自带每个动作的投入、剩余筹码和超时标记 |
| openpoker | 3856 | 38 | 被拒的都是日志缺动作（例如转牌下注没记，下一个动作对不上）；无 ws 日志的 run 无法识别超时代打 |

wopo（2026-10-25 接入）：hh 来自 `tools/wopo_play.py --hh-log`，只记 driver 自己逐条核对通过、打完的手
（中途接入、消息对不上、两人局的手都不记），所以动作和投入可以直接用。还没有实盘数据，只用实测会话
`tools/testdata/wopo_session_2026_10_16.jsonl` 的 7 手验证过（`tests/data/wopo_hh_2026_10_16.jsonl`）。
两点限制：服务器动作消息里没有超时代打标记（每手记 `forced_unknown`）；实测会话里没有摊牌，不知道摊牌牌面
在 `hand_result` 里怎么给，所以暂不读亮牌、`reveal_policy` 先按 `may_muck`（河牌诈唬比例分析停用），抓到
带摊牌的会话后再补适配器。

摊牌亮牌实测：两个平台目前都是摊牌时所有仍在牌里的手全部亮出（openpoker 3018/3018 个座位，
poker_manager 127/127）。`platforms.py` 里按平台设置 `reveal_policy`；报告每次都会重新实测，
规则或实测不支持「强制亮牌」时，河牌诈唬比例分析自动停用。

## 还没做

- 设计稿 §5.2 的求解器锁定对手（L2）。
- 牌力百分位按所有可能底牌算，没有按对手在该线路上的范围加权。
- 码深分档只进了观测的维度，报告还没按码深拆表。
