# OpenPoker 牌桌播放器

本机浏览器查看 `tools/openpoker_play.py --ws-log` 输出的当前牌桌，并可暂停、逐步回放或跳到上一手 / 下一手。Hero 固定在牌桌底部中央，自己的手牌始终可见；对手仅在日志记录了摊牌时翻开底牌。

## 启动

在仓库根目录运行（Python 3.10+，无第三方依赖）：

```bash
python3 table_player/server.py
```

浏览器打开 <http://127.0.0.1:8765>。默认自动选择仓库根目录和 `artifacts/` 下最近修改的 `*ws*.jsonl`，也可以在页面中切换日志或查看明确标注的虚构演示牌局。

指定当前运行的日志：

```bash
python3 table_player/server.py \
  --ws-log artifacts/openpoker_live_20261018_115450/openpoker_ws.jsonl \
  --port 8765
```

打牌脚本需包含 `--ws-log /path/to/openpoker_ws.jsonl`。播放器只读取这个文件，不需要 API key，也不连接 OpenPoker；打牌脚本保持原样。仅有 `--action-log` 或 `--hh-log` 不足以还原实时牌桌，不作为输入。

后台运行：

```bash
tmux new-session -d -s table-player 'python3 table_player/server.py'
```

在同一仓库根目录执行；停止时运行 `tmux kill-session -t table-player`。

## 页面功能

- 每秒读取新增的完整 JSONL 行；断线或日志停止更新时，保留最后状态并标明更新时间。
- 按实际快照适配 2–9 座；Hero 座位由 `table_joined`、`hand_start` 或快照中的 `hero.seat` 识别，座位变化时自动旋转牌桌。
- 显示公共牌、底池、各玩家筹码、本街投入、庄家、当前行动者、弃牌 / 全下 / 获胜状态。
- Hero 手牌同时显示在底部座位与侧栏；未收到手牌时显示空位，不猜测牌面。
- “动态”按局分组保留最近 5 局（含当前局），记录行动、Hero 手牌、各街公共牌及各玩家已公开的亮牌；换局或换桌后保留旧记录，重复快照不会重复插入牌面。
- 进度条、单步、上一手 / 下一手、0.5×–5× 回放，以及“跟随最新”；方向键单步、空格播放 / 暂停。
- 筹码 / BB 单位切换、手机窄屏布局、内置 9 人桌演示。

`--seats 9` 只控制没有服务端快照时的初始布局，实际座位数以快照为准。`--hero-seat 3` 可为缺少身份事件的日志提供初始 Hero 座位（从 0 起）；后续真实身份事件仍会覆盖它。

## 数据处理约定

输入每行形如 `{"ts": ..., "dir": "in", "raw": "{...WS 消息...}"}`。也接受直接的 WS 消息 JSONL。只显示服务端确认的动作，忽略客户端尚未确认的 `dir: out`。

- `table_state` 为筹码、座位、公共牌和当前行动者的权威快照。
- `player_action.street` 可能已经标成下一街，不能用它提前换街；使用公共牌事件和快照切换。
- `contribution_delta` / `stack_after` 优先于动作金额；结算优先使用 `total_pot`，避免把转移筹码误当整个底池。
- 处理同一手的完整 / 增量公共牌、重复消息、重连补发、先 idle 快照后结算、换手和换桌。
- 回放从稀疏检查点还原过去状态，不把未来手牌或摊牌信息带回过去。
- 追加写入的半行等待换行完成；坏行跳过并计数。文件被截短或替换时重置回放。
- 日志只读、只在本机 `127.0.0.1` 提供服务；不提供任意文件读取，不向浏览器返回原始请求头、turn token 或错误 HTML。

本工具展示日志已经观测到的信息；没有快照、截断了手牌开头或漏事件时，部分信息可能尚未齐全。实时性取决于打牌脚本继续写入 WS 日志。回放速度是事件步进速度，不模拟原始墙钟间隔。当前按所选日志缓存事件，切换来源最多保留 4 份缓存。

## 验证

```bash
python3 -B -m unittest discover -s tests -p 'test_table_player.py' -v
node --check table_player/static/app.js
```

测试覆盖 Hero / 换手 / 换桌、最近 5 局动态与历史玩家身份、牌面记录及去重、真实协议的街标记、结算顺序、无未来底牌泄漏、日志追加 / 半行 / 换文件、演示筹码守恒和 HTTP 文件访问边界。
