# wopo_arena：多个 advisor 本地同桌对打

本地牌桌服务，协议和 wopo 相同。每个 bot 是一对独立进程：没改过的 `tools/wopo_play.py` + 它的 advisor，
和接真实 wopo 时一样用 WebSocket 连上来，看到自己坐在 seat 0，把决策写进行动文件；牌桌读行动文件后替它出牌。

## 牌桌规则

- 8 座，50/100，ante 100，初始 200BB。配置 `bots_ante200.json`：旧版 9B、open2 9B、o75 10B、exploit 各坐 2 座，共 8 对进程。
- 开局前筹码低于 20BB 或高于 400BB，都按 200BB 重新进场（正好 20BB、400BB 不算）。盈亏按每手算，不受重置影响。
- 牌桌上不抽水；报告另算一列按 wopo 抽水模型（4% 封顶 10BB，200BB 以上的底池再加 1.5%）的结果。

**复式发牌**：一轮 8 手共用一副牌。公共牌相同，8 份底牌绑定位置（SB 一份、BB 一份……），庄位每手移一位、座次不变，
所以一轮下来每个 bot 在每个位置都用这个位置的那份牌打一次。下一轮换一副牌，座次重新随机。
牌和座次只由 `--seed`、轮号决定，同一 seed 可以复现发牌。总手数必须是 8 的整数倍。

## 运行

```bash
cd tools/wopo_exploit && cargo build --release --bin wopo_exploit_advisor && cd ../..
cargo build --release --bin openpoker_advisor
python tools/wopo_arena/arena.py --config tools/wopo_arena/bots_ante200.json \
    --out artifacts/arena_ante200_<日期> --hands 2000
```

- `python` 要装了 `websocket-client`。三个 crop 要在配置写的路径。8 个进程峰值约 54 GiB，建议 64 GiB、32 vCPU。
- **输出目录不要放到 `artifacts/wopo_*`**：那里的牌谱会被实盘画像和 `wopo_exploit_lab fit` 读进去。
- 停：`kill -TERM <arena pid>`，打完这手退出（退出码 3）；再发一次立刻停。
- 续打：同样的命令再跑一遍。进度只看 `table/wopo_hh.jsonl`，中断的那手用原来的牌重打，hand_id 带 `.N` 后缀（第 N 次重启）；
  driver 用自己的牌谱恢复对手画像。配置或 seed 不同会拒绝接着打。`--hands` 可以改大。
- 比赛下一段：新建 `--out` 目录，`--first-hand` 接着上一段的手号（牌不重复），`--warm-from` 指上一段目录以延续对手画像。
  `--flat-stacks` 让每手所有人都从初始筹码开始，不看补码线和重进线，这时同一轮的牌和码深完全相同。
- 可用内存低于 `--min-avail-gib`（默认 2）时，打完这手自动暂停，退出码 3。
- driver 中途退出、决策超时累计超过 `--max-timeouts` 次，整场中止，退出码 2。

## 输出

| 路径 | 内容 |
|---|---|
| `table/wopo_hh.jsonl` | 整桌牌谱，带 8 家底牌、`hand_no` `round_no` `stack_resets` `net_by_bot` |
| `table/wopo_play_events.jsonl` | 每一步的决策（建议、概率、来源、耗时） |
| `<bot>/` | 这个 bot 视角的 hh、ws、play.log、行动文件和范围日志，格式同实盘；seat 0 是它自己 |
| `arena.log` | 启动记录、每轮进度和警告 |
| `mem.tsv` | 每 60 秒各 driver、advisor 的内存和机器可用内存 |
| `run.json` | 配置、seed、commit、重启次数 |

```bash
python tools/wopo_arena/report.py artifacts/arena_ante200_<日期>                    # 对打报告
python -m opponent_analysis history --run 'artifacts/arena_ante200_<日期>/table/'   # 整桌回放
python -m opponent_analysis history --run 'artifacts/arena_ante200_<日期>/o75_a/'   # 单个 bot 视角，含范围
```

整桌回放勾选「提前亮出对手亮牌」才能在中途看到其他座的底牌，否则到结算那一步才翻开；每个座位的决策都显示。页面上的 hero 列固定是 seat 0，不代表某个 bot。

## 统计怎么算

报告把「轮」当一个样本算区间，同一轮的牌运对比较不起作用；两两差值也按同一轮配对。
筹码有延续，每手码深不同，一轮内的牌运不是严格抵消。

## 校验

```bash
python3 -m unittest tools/wopo_arena/test_arena.py
PYTHONPATH=.venv-pokerkit/lib/python3.11/site-packages python3 tools/wopo_arena/xcheck_pokerkit.py --hands 3000
```

- `engine.py` 和 driver 的 `tools/poker_live/table.py` 是分开写的。driver 逐条核对牌桌消息，对不上就作废这手；
  报告最后一节列出每个 driver 的作废和异常，正常应该全部为无。
- 报告再用 `opponent_analysis` 的重放逐手复核筹码。
- `xcheck_pokerkit.py` 用 PokerKit 复算随机牌局的盈亏，分池零头的分法两边不同，只允许差几个筹码。

已知差异：对手筹码都不够跟满当前注时，本桌和 driver 都允许再加注（多出来的退回），PokerKit 不允许。
真实 wopo 服务器怎么处理没有确认过。真实牌谱里也没有出现过不同人分走主池和边池的手，所以边池结算的消息格式是按平分底池的格式推的，
driver 和回放只用到 `final_stacks` 等字段，不受影响。
