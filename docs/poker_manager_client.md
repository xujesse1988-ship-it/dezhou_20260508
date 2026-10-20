# 接入本地 poker_manager 牌桌服务

新增入口：`tools/poker_manager_play.py`，对应相邻项目
`../poker_manager_20261016/` 的玩家 WebSocket `/ws`。默认六座、10/20 盲注、
2000 买入，决策复用 `openpoker_advisor` 及现有 blueprint / 实时搜索参数。
不需要改动服务端，也不连接 OpenPoker 平台。

## 九座启动脚本（2026-10-18）

`scripts/poker_manager_live/launch.py` 与同目录 `args.json` 参考
`artifacts/openpoker_live_20261018_115450/`：默认使用该目录实际保存的 **12 线程、
100 万次迭代、8000 ms 搜索预算、1000 手**，其余为同一份九座 crop / 500 桶 / `flop2-1pot`
配置。需要 4 线程 / 10 万次 / 3000 ms 时，修改 `args.json` 对应项即可。

设置环境变量，或在本项目 `.env` 中加入：

```bash
POKER_MANAGER_TOKEN=你的本地牌桌玩家token
POKER_MANAGER_URL=ws://127.0.0.1:3000/ws
```

环境变量优先；不读取 OpenPoker 的 `API_KEY`。九座模型可接入服务端容量不超过九座的牌桌。
使用已安装 `websocket-client` 的 Python 从任意目录启动：

```bash
/tmp/poker-manager-client-venv/bin/python -u \
  /Users/shaopengxu/projects_202605_after/dezhou_20260508/scripts/poker_manager_live/launch.py
```

每次从 `scripts/` 启动会新建 `artifacts/poker_manager_live_<上海时间>/`，保存
`launch.py`、展开后的 `args.json`、`driver.pid`、`play.log` 和三份 JSONL 日志。
终端先显示运行目录和 PID，之后 Python / Rust 输出均写入 `play.log`。token 通过环境
传给入口，不写入参数文件或启动脚本。运行目录被 Git 忽略，模板脚本及配置正常版本管理。

只准备运行目录、检查或修改参数后再启动：

```bash
/tmp/poker-manager-client-venv/bin/python scripts/poker_manager_live/launch.py --prepare-only
# 根据输出的路径，修改该目录的 args.json，然后运行其中的 launch.py。
```

`--prepare-only` 不需要 token，也不加载模型或连接服务。生成目录中的 `launch.py`
直接在该目录运行并追加日志；URL 已固定在该目录的 `args.json`，切换服务时修改其中的 `--url`。
若其 `driver.pid` 对应进程仍存在，会拒绝重复启动。
后台运行可在上述启动命令前加 `nohup`、末尾加 `&`；详细输出仍在 `play.log`。

## 启动

服务端按其 README 配置 `POKER_PLAYERS` 后启动：

```bash
cd /Users/shaopengxu/projects_202605_after/poker_manager_20261016
npm start
```

本项目安装网络依赖、构建 advisor，然后启动玩家入口。以下 `YOUR_BLUEPRINT.ckpt`
及 `YOUR_BUCKET_TABLE.bin` 需替换为实际文件；blueprint 座位数、reshape、postflop-cap
和桶表必须与训练配置一致：

```bash
cd /Users/shaopengxu/projects_202605_after/dezhou_20260508
python3 -m venv /tmp/poker-manager-client-venv
/tmp/poker-manager-client-venv/bin/pip install websocket-client
cargo build --release --bin openpoker_advisor

# 使用服务端 POKER_PLAYERS 中分配给该玩家的 token；演示配置中 Alice 为 local-alice。
export POKER_MANAGER_TOKEN=local-alice
/tmp/poker-manager-client-venv/bin/python tools/poker_manager_play.py \
  --url ws://127.0.0.1:3000/ws \
  --checkpoint YOUR_BLUEPRINT.ckpt \
  --bucket-table YOUR_BUCKET_TABLE.bin \
  --seats 6 --reshape preopen --postflop-cap 3 \
  --buy-in 2000 --num-hands 100 \
  --action-log /tmp/poker_manager_actions.jsonl \
  --hh-log /tmp/poker_manager_hh.jsonl \
  --ws-log /tmp/poker_manager_ws.jsonl
```

让另外两个演示账号提供对手（不要同时用 Alice，重复连接会替换玩家会话）：

```bash
cd /Users/shaopengxu/projects_202605_after/poker_manager_20261016
BOT_TOKENS=local-bob,local-carol npm run demo
```

浏览器打开 `http://127.0.0.1:3000/viewer` 可观察牌局。入口只连接玩家 `/ws`，
决策数据中没有查看器提供的对手底牌。

`--seats` 指模型座位数，必须与 blueprint 一致。`--seats 9` 可接入服务端 2–9 座牌桌，
例如六座桌无需改成九座：请求仍使用九座模型空间，`dealt_seats` 只包含实际发牌座位，
由 advisor 复用短桌幻影座映射。服务端容量超过模型座位数或座位编号越界时停止。
盲注按每手服务端 `hand_start.blinds` 读取，买入仅接受服务端规定的 1000–5000。

裁切 blueprint 将 `--checkpoint` 换成 `--cropped-checkpoint`，并加：

```bash
--search --search-trigger all-postflop --search-time-budget-ms 5000
```

支持已有 `--search-*`、`--search-prewarm`、`--exploit on|vpip|off`、`--seed`
及 `--debug-log` 参数，完整列表见 `python3 tools/poker_manager_play.py --help`。
URL 也可通过 `POKER_MANAGER_URL` 配置；token 可通过 `--token`（别名 `--api-key`）传入。
直接运行 `tools/poker_manager_play.py` 不会自动读取本项目或服务端 `.env`；
上面的 `launch.py` 启动脚本会读取本项目 `.env` 中的本地牌桌配置。

## 接入约定及限制（2026-10-18）

- 服务端广播的 `player_action.amount_mode=incremental`，`contribution_delta` 为本次投入。
  入口维护每街累计投入，把 raise 转成 advisor 要求的绝对 `to`；call / all-in 使用明确增量，
  短跟注不会按桌面最高注错误扣款。起始筹码从首个快照的 `stack + bet` 取得。
- `community_cards.cards` 为新增公共牌，逐街追加；新街重置本街投入。
- 只有 `your_turn` 或重同步恢复快照的 `hero.turn_token` 授权出牌。
  普通快照及 `action_delay` 等待期间不出牌；序号跳跃不触发重同步。
- 后台线程运行 advisor，主线程持续收消息。距离行动截止不足 0.5 秒时仍未算完，
  优先 check，否则 fold；后到的搜索结果作废。发送前检查动作是否在服务端合法列表内，
  raise 必须为区间内整数总额。
- 重连用 `connected.table_id` 找回原桌，先按顺序补公共事件、再核对快照，
  不重复扣筹码。恢复期间的实时消息暂存。相同 token 已发送的动作不重复计算；
  重连后该 token 仍有效时，用同一 `client_action_id` 和 payload 重发。
- 冷启动重连或公共回放被截断时，快照不足以还原整手下注历史：当前手安全 check/fold，
  下个 `hand_start` 恢复 solver。不会用全手牌查看器补历史。
- **现有多人 advisor 不支持本服务的标准两人局**（服务端两人局 BTN=SB，OpenPoker
  adapter 的两人局假设 BTN=BB）。两人局标记 `fallback:standard_heads_up_unsupported`，
  全街 check/fold；三人及以上调用 solver。模型参数支持 3–9，服务端两座桌可接入但走此兜底。
  这是多人入口的兼容边界，不改变本项目 heads-up 求解器的产品目标。
- 默认完成 100 手后发 `leave_table` 退出，可用 `--num-hands` 调整。筹码用尽时结束，
  不自动补币；余额不足、非法动作、配置不符会报错停止。鉴权失败或会话被替换不循环抢回连接。
  意外断线默认最多连续重连 10 次，可用 `--reconnect-attempts` 调整；Ctrl-C 后由服务端释放保留座位。
- 日志默认关闭，指定路径则追加。动作日志记录 `{req, resp, sent, decide_ms}`；WS 日志沿用
  `{ts, conn, dir, raw}` 格式。HH 为本地 `poker_manager_hh: 1` 格式，保留服务端完整
  `result`（含 `total_pot`、`payouts`、`final_stacks`），不能直接当作 OpenPoker AIVAT 输入。

## 验证

离线回归无需账号、checkpoint 或 WebSocket 依赖：

```bash
python3 -m unittest discover -s tests -p test_poker_manager_play.py -v
cargo test --no-run
```

覆盖增量下注、短跟注 / 全下、公共牌累计、行动间隔、令牌去重、重连回放、冷启动 / 截断回放、
超时与过期结果、人数 / 盲注参数、非法 advisor 动作、手数上限和结算日志。

可选真实服务协议回归（需要 Node.js、服务端已安装 `ws`、Python 已安装 `websocket-client`，
以及本机监听端口权限）：

```bash
POKER_MANAGER_PROJECT=/Users/shaopengxu/projects_202605_after/poker_manager_20261016 \
  /tmp/poker-manager-client-venv/bin/python -m unittest discover \
  -s tests -p test_poker_manager_play.py -v
```

此测试通过 `tests/common/poker_manager_server.mjs` 启动随机端口、独立测试账号的服务实例，
运行真实入口 CLI 和九座测试 advisor，在六座服务端完成三手。它验证网络及协议，不代表 solver 棋力评测。

2026-10-18 联调记录：完整 Python 回归 32 项通过；`cargo test --no-run`、
`cargo build --release --bin openpoker_advisor`、`cargo fmt --all --check` 通过。
另用本地六人桌 `preopen` 10B checkpoint / 200 桶表、关闭搜索完成 10 手及 26 次决策，
无服务端 `error` / `action_rejected`、10 手筹码历史完整。其中 blueprint 18 次，
已有 advisor 的 `blueprint_placeholder` 兜底 2 次、`structural_gap` 兜底 6 次；
这些抽象 / 策略边界仍需按原 advisor 的搜索及训练流程处理，本次接入不改变算法。

### 2026-10-18 座位容量修订

初版误将 `--seats` 与服务端快照容量要求为严格相等，导致九座模型不能进入六座桌。
修订为服务端容量不超过模型座位数即可；与 OpenPoker driver 一样，在模型空间中保留
真实座位编号及 `dealt_seats`，额外空位只补起始栈占位，不伪造发牌玩家或动作。
回归验证六座快照与额外三个空位的九座快照生成完全相同的 advisor 请求，并保留标准两人局的明确兜底。

### 2026-10-19 自动补码与重新入座修订

新增可选 `--auto-rebuy`。不开启时保留上文的出局即退出行为；开启后，客户端入座成功时
发送 `set_auto_rebuy(enabled=true)`，收到 `busted` 后等待本人的 `player_left`，再查询
同一服务的 `/api/me`，按账户实际余额重新入座。手数跨多次买入累计，达到 `--num-hands`
后退出，不再重新入座。服务端偏好保留为开启状态。

账户余额至少 1000 时，买入 `min(--buy-in, 余额)`，通常仍为 2000；余额为零且已开启
服务端自动补码时，等待 `auto_rebuy_scheduled` / `rebuy_confirmed`，不绕过冷却。
默认服务端每次补充 1500，因此该次按 1500 买入。余额为 1–999 或尚未开启服务端偏好时，
通过带 `request_id` 的 `rebuy` 补充；断线后重发相同 ID，避免重复增加账户余额。
补码只增加账户余额，收到确认后才重新入座，不在牌局中途给桌上筹码加码。

在独立运行目录的 `args.json` 数组中增加 `"--auto-rebuy"` 即可启用。该选项不改变
checkpoint、九座模型、实时搜索或买入上限；其他离桌原因和鉴权失败仍停止，不抢回被替换的会话。

### 2026-10-20 码深漂移离场

实盘 20261020_105159 一批 1000 手，我方栈从 100 BB 涨到最高 848 BB 一直坐着不动：翻前按 100 BB 的
blueprint 打，翻牌 62 次锚定搜索里 58 次因为码深门不过改走搜索
（`docs/temp/poker_manager_6max_live_20261020_105159_analysis_2026_10_20.md` PM-1）。
因此移植 OpenPoker driver 的码深漂移离场：

- 新增 `--stack-band-bb LO,HI`，默认 `80,180`（与 `tools/openpoker_play.py` 相同），`off` 关闭。
  每手 `hand_result` 后，我方栈不在 `[LO×BB, HI×BB]`（含端点，BB 取本手盲注）就发一次 `leave_table`。
- 服务端在本手结束前会回 `leave_pending`、继续发牌，照常打；以本人的 `player_left`（或 `not_at_table`）
  为离桌完成，然后按买入（开 `--auto-rebuy` 时按账户余额）重新 `join_lobby`。离桌期间不重复发 `leave_table`。
- 断线重连时如果还在原桌、离桌没完成，重发一次 `leave_table` 再 resync。
- 只有**入座时栈在区间内**才会因漂移离桌。服务端自动补码一次 1500（75 BB），低于 80 BB 的下限；
  如果不加这条，按 1500 入座后每手都会离桌。
- 栈归零仍走原来的 `busted` → `player_left` 流程，不经过这里。达到 `--num-hands` 时只发收尾的那一次 `leave_table`。

### 2026-10-20 对手全下时 advisor 的 `all_in` 改成跟注

实盘 20261020_164346（bp3）8db1dbc5：对手全下、我方筹码更深、其余人都已弃牌，服务端只给 `fold` / `call`。
advisor 在没有加注区间时会回一个不带金额的 `all_in`（`tools/openpoker_advisor.rs` `action_to_response` 的 `Action::AllIn` 分支），
driver 判它不在合法动作里、按兜底弃牌，**AA 被弃**（`docs/temp/poker_manager_live_20261020_164346_analysis_2026_10_20.md` PM-8）。

- advisor 只有在服务端没给 `raise` 时才回 `all_in`；这时服务端也没给 `all_in`、但给了 `call`，说明我方能投入的最多就是跟注，
  driver 改发 `call`，`source` 末尾加 `:allin_as_call`。服务端给了 `all_in` 时照常发 `all_in`。
- 两样都没有（例如只能过牌）仍走原兜底；`fallback:illegal_advisor_action` 的日志里新增 `rejected`，记下 advisor 原始回复
  （`action` / `amount` / `source`），以后不用再靠读代码推断。
- 搜索路径不受影响：同一局面下搜索会按真实局面把全下换成跟注（bp2 e400a9ce 已验证），出问题的只有翻前 blueprint 路径。

### 2026-10-21 静默失联检测

实盘 bp2 在 2026-10-20 23:57:38 后停止收消息，进程与本地 TCP 仍显示存活，
服务端账户却已离桌。旧版把每次接收超时都当作正常等待，没有检测半开连接，
因此一直留在旧连接上，未进入已有的重连流程。

- 连续 15 秒没有收到数据或控制帧时发送 WebSocket Ping；10 秒内没有收到匹配的 Pong，
  关闭旧连接并进入原有的重连流程。使用单调时钟，避免系统时钟校准影响超时。
- 服务端 Ping 仍由 `websocket-client` 自动回复；Ping / Pong 不送入 JSON 牌局处理。
  正常空桌等待只要心跳有回应就不会触发重连。
- WS 日志新增 `heartbeat_ping`、`heartbeat_pong`、`heartbeat_timeout` 事件。
  重连沿用当前手数、原桌恢复和自动补码逻辑；鉴权失败或会话被替换仍会停止。
