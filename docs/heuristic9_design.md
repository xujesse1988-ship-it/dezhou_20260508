# Heuristic9 — 启发式 9-max 无限德州扑克引擎（设计 + 迭代闭环）

> 一句话：与现有 Pluribus/CFR-blueprint 主线**完全独立**的第二条腿——一个分层、可解释、
> **座位无关（2–9 人通用）** 的纯启发式决策引擎。目标是更强的 9-max OpenPoker 竞争者，
> 通过「打 1000 手 → 看数据 → 调参/加规则 → 再打」的闭环持续升级。

## 0. 为什么是启发式（而不是再训一张 blueprint）

CFR blueprint 的代价：① 固定人数（6-max 树训死，9-max 要重训、抽象 equity 假设 1 对手）；
② 不可解释、改一处要重训几小时；③ 多人一般和下 CFR 不保证收敛 Nash、强度只能靠实测。

启发式在这里反而全是优点：
- **座位无关**：同一套规则吃 `num_seats`，2 人到 9 人零改。9-max 是它的天然主场。
- **即时可调**：一个 `HeuristicConfig` 几十个旋钮，改完即生效，无需训练。
- **可解释**：每个决策都能回溯到「equity / 底池赔率 / 质地 / 对手画像」哪一项驱动 → 数据
  分析能精确定位漏洞。
- **AI 优势**：人写启发式怕维度爆炸、怕规则打架；AI 可以一次性把**正交维度**（equity、
  位置、主动权、人数、SPR、质地、对手类型）全铺开、用统一货币（EV / equity）协调，
  把「规则打架」变成「分数加权」。

我们**不规避启发式的缺点**（如不是博弈论最优、可被针对），而是**最大化它的优点**：
弱场（OpenPoker 真人 + 杂牌 bot）里，剥削性打法 + 多人纪律 + 永不犯大错，EV 远高于
追求不可剥削。

## 1. 系统架构

```
OpenPoker server ──WS──> heuristic9_play.py (driver, 2-9 座通用) ──stdio JSON──> heuristic9_advisor (Rust)
                                                                                      │
                                                              ┌───────────────────────┘
                                                              ▼
                                                   src/heuristic/ 决策引擎
```

- **driver**（`tools/heuristic9_play.py`）：网络/token/重连/累计手历史/落日志。改自现有
  `openpoker_play.py`，但**座位通用**（不强制 6-max、不主动离 HU/9-max 桌）。
- **advisor**（`tools/heuristic9_advisor.rs`）：常驻进程，每决策点收一行 JSON、调引擎、
  出一行 `{action, amount?}`。crate 零网络依赖。**永不 panic**：任何异常从 `valid` 出安全
  合法动作（能 check 则 check，否则 fold）。
- **engine**（`src/heuristic/`）：纯函数式决策核心，与规则引擎/评估器复用同一套 `Card`/
  `HandEvaluator`，但**不依赖** CFR/blueprint/bucket 任何东西。

## 2. 决策流水线（每个决策点）

```
Request(JSON) ──> [1]GameView ──> [2]Features ──> [3]Range ──> [4]Policy ──> [5]Exploit ──> [6]Mix ──> Response
```

**[1] GameView**（`view.rs`）：把扁平的 `actions[]` 重放成下注状态——逐街分段（检测下注轮
关闭）、每座 `committed_this_street/total`、`status`（active/folded/allin）、`high_bet`、
`pot`、`to_call`、`num_active`、主动权方（last aggressor）、按钮相对位置、有效栈/SPR。
重放失败容错降级，绝不崩。

**[2] Features**（`features.rs`）：抽正交信号
- **made-hand 强度**：评估器算成手类别 + 相对档位。
- **equity**：对**建模对手 range** 的多人蒙特卡洛胜率（`equity.rs`）——整个引擎的主货币。
- **听牌势能**：同花/顺子/双卡/后门 → 落后时的反超 equity（已被 equity 吸收，额外用于
  区分价值/半诈唬）。
- **board 质地**：干湿、对子面、连张、同花可能、高张结构。
- **位置/主动权**：IP/OOP（相对仍在场对手）、是否翻前加注者。
- **人数惩罚**：active 对手越多，价值门槛越高、诈唬越少。
- **SPR**：承诺区间（低 SPR→顶对全下；高 SPR→控池/隐含赔率）。
- **底池赔率/隐含赔率**：跟注所需 equity。

**[3] Range**（`equity.rs::OppRangeModel`）：对手 range 先验 = 位置×动作的 GTO-ish 开池/
3bet/跟注矩阵（按手牌强度百分位），再被**对手画像**（VPIP/PFR/AF）收紧或放宽。

**[4] Policy**（`policy.rs`）：分街启发式专家给每个候选动作打 EV 分
- **翻前**：按位置+强度+人数+栈深决定 open/iso/3bet/4bet/call/fold；座位无关（用相对按钮
  位 + 「还剩几个未行动」泛化「EP/MP/CO/BTN」）。
- **翻后**：c-bet（按质地+人数+位置选频率与尺寸）、薄价值、半诈唬、底池赔率跟注、
  check-call/check-raise、按质地+SPR+人数选尺寸、低 SPR 承诺/stack-off。

**[5] Exploit**（`opponent.rs`）：按对手画像调——对紧手多偷、对跟注站多价值少诈唬、
对疯子多抓诈。这是「累积经验」变现的地方（弱场最大 EV 来源）。

**[6] Mix**（`policy.rs::finalize`）：分数→动作分布，受控随机（用手牌状态 seed 的 RNG）
保证不被简单针对，夹到合法集，尺寸映射到 OpenPoker `min_raise/max_raise`。

## 3. 模块清单（`src/heuristic/`）

| 文件 | 职责 |
|---|---|
| `mod.rs` | 模块根 + 公开 API `decide(&Request, &Config, &mut Opponents) -> Decision` |
| `view.rs` | `Request`/`Response` serde 类型 + `GameView` 下注重放 + 卡牌解析 |
| `equity.rs` | 多人蒙特卡洛 equity + `OppRangeModel`（range 先验/采样） |
| `features.rs` | 质地/听牌/made-hand 分类/SPR/底池赔率/位置 |
| `policy.rs` | 翻前 + 翻后策略 + 混合 + 尺寸映射 |
| `opponent.rs` | 对手画像（VPIP/PFR/AF…）+ 剥削调整 + 持久化 |
| `config.rs` | 全部可调参数（默认值 + JSON 覆盖） |

## 4. 迭代闭环（升级方法论）

```
打 1000 手(live 或 sim) ──> 落 action-log + hh-log ──> heuristic9_analyze.py 出漏洞报告
        ▲                                                          │
        └───────── 调 config / 加规则 / 改模块 <── 自对弈 sim 回归门(mbb/g 不退) ──┘
```

- **离线先行**：live 需用户 api_key；无 key 时用 `heuristic9_sim`（座位通用自对弈，
  heuristic vs 多种 baseline：random/tight-passive/loose-aggro/honest）出 mbb/g + CI，
  作为**每次改动的回归门**——改完必须 sim 不退化才上线。
- **数据分析**：`heuristic9_analyze.py` 从 hh-log 聚合：按 spot（街×位置×动作面）出
  频率/胜率、最大输池复盘、c-bet/fold-to-cbet/wtsd 等漏洞指标。
- **版本节奏**：每个 config/规则改动 = 一次 commit；live 1000 手一个里程碑，记录在
  本文档 §6 升级日志。

## 5. 正确性与安全

- **永不犯规/崩溃**：advisor 所有路径回落到 `valid` 合法动作；引擎纯函数、无 IO。
- **无 unsafe / 无全局 RNG**：遵守 crate lint（`unsafe_code=forbid`）与可复现约束
  （决策 RNG 由手牌状态显式 seed）。
- **单测**：卡牌解析、下注重放（对照已知 OpenPoker 序列）、equity 单调性（坚果=1、
  空气<对手）、合法动作夹取、对手画像收敛。
- 启发式层用 f64（equity/概率），与规则/评估器整数层隔离——不违反 invariants §1
  （该约束限定规则/评估器/抽象层）。

## 6. 升级日志

### v0.1（2026-06-20）：基础框架落地

座位通用引擎（`src/heuristic/`）+ advisor（`heuristic9_advisor`）+ 9-max driver
（`heuristic9_play.py`）+ 自对弈 sim（`heuristic9_sim`）+ 分析工具（`heuristic9_analyze.py`）
+ Rust 正确性套件（`tests/heuristic9.rs` 8 测全过）。

**正确性验证（强）**：
- **下注重建 0% 漂移**：sim 把 `GameView` 重建的 pot/to_call 与权威 `GameState` 对照
  **10 万+ 真实下注序列、漂移 0.000%**（9/6/2-max 全座）。
- **0% fallback / 0 illegal**：sim 全程出合法动作、永不兜底；真实日志 1500 决策回放 0 fallback。
- **mirror 自检 ~0**：hero×9 自对弈 3 seed {+135,−103,−54} mbb/g、均straddle 0 = 无座位偏差。
- **深码区覆盖（`--start-bb 200/400`）**：100/200/400BB 全 **0 fallback、0 重建漂移**；mirror 自检
  各深 3 seed 均落在 CI95 内（200BB {+186,−475,−604}、400BB {+141,−554,−1204} mbb/g）→ 重建/SPR
  逻辑深码不崩。注意 CI95 随深度约线性放大（100→400BB 翻倍），深码门须**加手数或放宽容差带**。

**强度（sim mbb/g，CI95）**：
| 场 | 9-max | 6-max | HU |
|---|---|---|---|
| mixed 混合场 | **+3807 ± 1656** | +5466 ± 1411 | — |
| vs station | +8094 | — | — |
| vs maniac | +2360 | — | — |
| vs tag（强对手）| +1728 | — | +249 ± 382 |
| vs nit | −369 ± 562（噪声内）| — | — |
| vs random | +23k | — | — |

**座位无关已证**：同一引擎在 9/6/2-max 全部正盈利、0 fallback、0 重建漂移——这是启发式
相对 CFR-blueprint（训死人数）的核心优势。

**真实日志画像（1500 决策回放）**：VPIP 28.7% / PFR 16.6%、按位置 open 率 LATE 43% >
MID 23%（正确位置梯度）、AF 2.25、fallback 0%。

### v0.2（2026-06-21）：live 接通 + 剥削层可测可门控 + 加力

用户提供 api_key（`.env` 的 `API_KEY=`）后首次跑通 live，并完成一轮「打→优化→打」闭环。
**OpenPoker 是 6-max（常半空/近 HU），但引擎保持 2-9 通用**（用户后续可能接 9-max 平台；
已验引擎零硬编码座数，`--seats` 仅 driver per-run 参数；live 短桌用 `--seats 2` 向上自适应）。

- **live 实测通过**：smoke 5 手连通/鉴权/出牌全绿、0 fallback；决策合理（AKo open、
  JQ 成手大注价值、垃圾牌弃）。依赖 `pip3 install --user websocket-client`。
- **迭代②（基建）**：`heuristic9_sim` 加 `--exploit`，与 live driver 同语义喂画像（稳定座名
  `p{seat}`、board 长度标街、SB/BB forced_blind、手末 `observe_hand`）→ 剥削层首次可在
  离线回归门里测/门控。默认关时 byte 级等价历史基线。**首测：剥削层对紧场欠力**（全 nit
  场 ON≈OFF≈+250 mbb/g）。`--config PATH` 同时加上（partial JSON 扫旋钮免重编）。
- **迭代③（加力）**：sim 扫 `exploit.adjust_cap` → 0.08 改 **0.12**（甜点）。回归门
  （`--exploit`, 12000h, 多 seed, 0 fallback/0 重建漂移）6-max 3-seed 均值 vs 0.08：
  station +386、maniac +256、mixed +97、nit +103、tag −65(噪声内)；9-max 同向。
  **≥0.16 对 nit 断崖**（过偷被罚 295→−520→−1975），故止于 0.12。exploit=OFF 不受影响。
- **driver 自救**：live 暴露「对手离桌独坐→server 心跳使 recv 永不超时→永久空等」（实测卡
  34min）。移植主线三件套：`already_seated`→leave+rejoin、看门狗线程（120s 无新手 rejoin）、
  断开 `_ws=None`。重启后 6 决策/40s 正常流转、画像持久载入。
- **前期硬伤检测**：`analyze --hard`（fallback/该 check 却弃/弃顶级起手/value 低eq/加注越界/
  HU 跟空气）+ driver 内联即时扫。1000 手 live 实测 net **+36976 chips=+1849 mbb/g**、0 fallback。
  硬伤检测先误报 2 例（多人池 BB 防守跟注），已修为仅 HU 报。

### live 1000 手 EV 深挖：limp「漏洞」是伪命题（被回归门驳回，未改代码）

live 6-max 1940 决策显示翻前被动（VPIP46/PFR18、gap 28pt），`limp_behind` 76 手是
被支配的 offsuit K-rag（K5o/K3o/J8o…）。**假设**：按可玩性（对子/同花/同连）收紧 limp、弃 K-rag。
**为可门控，先加 sim 被动 baseline `Limper`（open-limp 宽 range，制造多人 limped 底池，0 fallback/漂移）。**
**回归门驳回**（子 agent 严测）：对那批 K-rag 做 EV 归因，竟是**强 +EV**（9max vs station +7.35bb/手、
limpy +5.01、mixed +4.90）——弱/被动场没人惩罚跛入，跛进去翻到 K 顶对就收钱。收紧反而退化
（40k×3seed：9max station −923、limpy −559，2–4σ 真退化）。故**未提交**收紧改动。
**结论**：limp K-rag 对弱/被动场（=我们实际面对的、也是赢 +1849 的原因）就是 +EV，只对**会 iso 惩罚的强场**才是漏洞。
正确归属＝**剥削层**（仅对激进 iso 对手收紧 limp），非默认改动；当前不接强场，暂不做。
「正确性>一切 + sim 门控」拦下了一个教科书正确但语境错误的 -EV 改动。Limper baseline 保留（`0bfea7a`，
让 sim 混合场更像 live 弱场，提升后续所有回归保真度）。

### live net 标度修正：盲注走过手的起始栈回落虚高（driver bug，已修）

**症状**：driver `_hand_result` 的 `net = my_final − my_start`，`my_start` 经
`hand_start_stacks_list()`——只有当 hero 在 `player_action`/`your_turn` 上被 `note_start_stack`
记过栈时才知道真实起始栈，否则**回落 `BUY_IN`（=1000）**。于是**hero 零动作的手**（盲注走过、
全弃到 hero 盲）拿不到起始栈 → `net = my_final − 1000`，hero 栈越深虚高越夸张。
**run24 实测**：56 手 pot=10 的盲注走过全部 `my_start==1000`，单手虚报上千（例：hero 坐 4847
赢 10 的走过 → `my_final=4857`、记 `net=+3857` 而非 `+10`）。整段把 net 从真实 **+106,372** 抬到
**+302,916**（虚高 +196,544 / 65%），mbb/g 从真实 **+2,659** 抬到 +7,577。**历史所有 live run 的
mbb/g 都被此 bug 系统性高估**（越深越假）。

**修复**（`heuristic9_play.py`）：dispatch 处理 `table_state`（此前被忽略但 raw 有留底）——每条快照
对各非空座喂起始栈。取 **preflop（行动前）首条**最干净：此刻各座唯一已投入＝盲注，
`start = 当前栈 + 该座盲注`，故 hero 零动作手也能拿到权威起始栈。盲注按 **紧凑指派**
（dealt 座环序，button 后第一个=SB、第二个=BB；**服务端对 2 人池亦无 HU 特例**，不能用物理座
`btn+1/btn+2` 否则把盲注派到空座）。首见即锁、不覆盖、`hand_id` 守门；天然兼容 leave/rejoin/rebuy
（用本手自己的 table_state，不挪用上手 final）。

**验证**（离线复算全 run24，2000 手）：把真实 raw 流喂回**打了补丁的 Session**，driver 自报
`net=+106,372`（+2,659 mbb/g）、与独立「stack-delta」真值**逐手 2000/2000 完全一致**；
68 手 old≠new；**0 手违反盲注走过 `|net|<=pot`**（修前 59 手 `|net|>pot`）。落在交叉校验区间内
（pot-bounded ≈ +105,872、stack-delta ≈ +108,267）。`--selftest` 加了一手盲注走过回归手
（hero 零动作、起始栈只能来自 table_state，断言 `net=+10` 且 `|net|<=pot`）守住此修复。

> 残留：稀疏/非连续占座下盲注紧凑指派仍是近似（见 backlog ①），但本修复直接用 table_state 的
> `in_hand` 座集做紧凑环序，已对 run24 全部短桌/HU 手校准为 0 越界。

### v0.3（2026-06-28）：剥削层价值放大（接线，shipped）+ 对子面再加注限深（驳回）

两项**场依赖**优化在严 sim 门控下分流——一过一驳，再次印证「教科书正确 ≠ 本场 +EV」。

**① 剥削层 value-size 放大（SHIPPED）**：弱场全是 any-two 跟注站（live 见 93o/62s/J6o 摊牌、
WTSD 43–51%）。剥削层已对分类 station 下调价值门槛（`value_shift`，薄价值）；新增**价值注/价值
加注尺寸乘子** `ExploitConfig.station_value_size_mult`（默认 **1.30**）：仅价值动作放大、诈唬不动
（放大诈唬只对不弃牌的站增亏）。仅在场上有**已分类 station** 时生效（剥削层，**非默认改动**；
无 station / 未开 exploit 时 byte-equal）。落点 `policy::value_size`（包 `pick_bet_size(true)` ×
mult，夹 `[size_small,1.6]`），由 `aggregate_exploit` 的 `any_station` 触发。
回归门（8000h，多 seed，0 fallback/0 重建漂移）：
- `--field station`：6max 6/6 seed 正（均 **+112 mbb/g**）、9max 多 seed 正（8kh 均 +48、16kh 均 +71）；
- `--field mixed`（**真实弱场**，含 1 个 station）：6max 5/6 seed 正（均 **+106**）、9max 5/6 正（均 +73）；
- `--field tag`：6max 持平（噪声内）、9max +100（部分 tag 实测 VPIP>0.45&AF<1.5 被分类 station，剥削正确触发）；
- `--field nit` / HU(seats2) station：**byte-equal**（无/未触发 station 分类，乘子=1.0，零退化）；
- 非 exploit 全座/全 field：**byte-equal** 历史基线。
`≥1.40` 时 9max station 转负（过大尺寸把站的边缘成手吓弃 + 薄价值放大方差），故止于 1.30。
**机理**：站跟更大注 → 价值注做大多榨；且 sim 的 station 对大注会按底池赔率弃 air（`pot_odds<0.2` 门），
**低估** live any-two 站（不弃）的放大收益 → sim 是保守下界。

**② 对子面非坚果价值再加注限深（REJECTED，未提交引擎改动）**：run24 最大输池（−7031）＝hero 持
非坚果两对（K7o→aces-up）在**成对面**（A A x x K）把 `value_raise` 连开进 5 层再加注战（52→306→994→
2995→6981）撞对手 trip-A。**假设**：成对面 + 本街进攻数 ≥3（深战）+ made<Monster 时把价值再加注降级跟注。
为可门控，加 sim baseline **`PairedTrap`**（松被动入池、**仅成对面持 trips+ 才升级加注战**，否则绝不诈唬加注；
非对子面同 Tag）——follow Limper 先例。**回归门驳回**：
- 漏洞在 sim **极稀疏**：trap 场 8000h 仅 ~8 次 hero 在 snr≥3 的成对面 value_raise（hero 的 equity 模型
  对大注已收紧对手 range、本就多半不再加注）；
- 对 trap 仅噪声级帮助（6max +5.5 单 seed、余 0）；
- 对**真实 `tag` 场一致小亏**（depth3 全 seed 负 −12~−46；窄到「仅两对、排除 trips」仍 5/6 seed 负 −4~−12）——
  因 tag 也用两对打成对面加注战，hero 两对再加注本是 **+EV**，限深反而漏价值（正是任务预警的「别杀掉
  非对子面/可盈利加注战」的同构陷阱，只是发生在**成对面 + 非 trap 激进对手**）；
- depth=5 时全场 byte-equal（纯 no-op）。
**结论**：机理对（深成对面战别拿非坚果 stack-off），但**对实际激进场是 wash-to-小亏**，无法证明 +EV → 驳回默认改动。
正确归属同 limp＝**剥削层**（仅对画像为被动/仅坚果建战的对手限深），非默认。`PairedTrap` baseline **保留**为
该剥削项的将来验证夹具（follow `0bfea7a` Limper 先例；不入 mixed pool，不动默认 sim 数）。

### v0.4（2026-06-29）：协议增强—driver 发权威信息，view.rs 重建对齐服务端（shipped）

三个重建正确性 bug 同根：`view.rs` 从**残缺信息**（扁平 actions 无街标、物理猜盲、偶尔低估的栈）
重建下注态，而 **driver 手里有全部权威信息却没发**。本次让 driver 把已知权威发给 advisor，
`from_request` **有则用、无则退化历史启发式**——sim 全发 `None` → 重建逐字节不变（漂移恒 0.000% by
construction），只有 live 走权威路径。commit `ad7bff2`。

**A 栈硬化（根修；此前 `c500643` 的 table_state 锚定只是抢先规避、未真修）**：
- driver `player_action`：先 `on_player_action` 再 `note_start_stack`（旧序对**首动作即 raise** 的座把
  start 锁太低——raise-to 尚未计入 committed）；
- `view.rs` raise-to 自纠：`a.to`（server 权威 raise-to）超过记录起始栈即栈被低估的铁证 → 把有效起始栈
  抬到 ≥target，而非把加注夹到错误小栈（仅信权威 `a.to`，None-fallback 的 target 是猜测、不抬栈）。

**B 逐动作街标签（治翻后「分段坍缩」）**：`HistAction.street: Option<u8>`（serde default）。全动作带
标签时按标签权威推进、**跳过启发式 `round_closed`**——幻影空座/不规则序列不再把多街塌成一跳、抹掉
`to_call`；任一缺标签整手退化历史启发式分段。driver `build_request` 给每动作打街标（镜像 observe 的
`action_streets` 映射）。

**C 权威盲座（治盲规则反 + 翻前位置桶错）**：`Request.sb_seat/bb_seat: Option<u8>`；`GameView` 存盲座，
`my_pos_bucket` 改走存储盲座而非 button-index 硬编码 HU 特例。**服务端 HU 无特例：button 投 BIG blind**
（run24 raw 坐实）。driver 用 `_compact_blinds`（紧凑指派，正确处理稀疏/空座）发权威盲座，两者皆知才发。
物理回退（sim/缺省）= HU `btn/btn+1`、多人 `btn+1/btn+2`，与历史 button-index 判定**等价**。

**验证**：`cargo test --test heuristic9` 10/10（含 2 个权威路径新测：HU button=BB、街标签修复幻影座分段
坍缩，均断言旧逻辑下**必失败**的值）；sim 漂移门 9/6/2-max × exploit × 多 seed = pot/to_call **0.000%、
fallback 0**；clippy/fmt/selftest/`build --all-targets` 全绿。**实盘 A/B**（replay run24/run25 raw 经新
driver+advisor，比对重建 `to_call` vs 服务端权威 `can_check`）：run24 旧路径 **52** 处「`facing_bet` 却
`to_call=0`」全部修正、run25 修 1、**0 回归**；新 driver 实盘上 sb/bb 座 4000/4000、postflop 街标
1902/1902 全填充。

**机理价值**：实际 EV 影响小（栈已被 table_state 锚定预修、其余被 `policy.rs` 两道防线接住），但根治了
**分析器 `pot`/`required_eq` 脏数据**（run24 ~1.3% 决策）、解锁真 HU 盘（盲规则）、并在短手/稀疏占座放大
前预修。

### Phase A 遥测（2026-07-02）：决策节点 trace（零行为变化，byte-equal，shipped）

场景化设计（`docs/scenario_heuristic_design.md` §6 Phase A）首阶段落地：每个决策挂**只写**
trace（`src/heuristic/trace.rs`）——现有 if 分支的显式命名节点路径（22 个叶节点，清单见该文档
§3.2.1）+ 场景 key 维度（街/池型/角色/面对/人数/SPR/位置 + P0-1 派生量）+ 驱动信号（equity /
made / 实际 evaluate 过的阈值比较与频率门掷点）+ legalize 降级 before/after。`Response.trace`
经 advisor 一行 JSON 原样透传，driver 整条 resp 落 action log（`heuristic9_play.py` 零改动，
selftest 全绿）。`heuristic9_analyze.py` 新增按节点聚合 `(n, hands, EV bb/hand, CI95, 动作
分布)`（EV 口径=整手 net 摊给该手命中的所有节点，§8②；h<30 标待观察）+ 兜底率（fallback ∪
legalize 降级占比，§8④）；旧日志无 trace 优雅降级（run30 实测）。`heuristic9_sim
--trace-summary`（opt-in）打印节点热度表。P0-1 派生量同步落地（`villain_capped` /
`villains_capped` / `facing_barrel_n` / `is_delayed_cbet`，`view.rs` 只读访问器；
`i_have_initiative` 过期主动权修复仍未做——行为变更，出 Phase A 范围）。**回归门**：
9/6/2-max、`--exploit`、`--start-bb 400` 五条 8000 手基线（seed 7）**逐字节一致**
（trace 只写不读：频率门掷点绑定局部变量后比较，RNG 消费序列不变；翻前 fold 短路路径不为
trace 多算 equity）；`cargo test` 全绿（`heuristic9` 10/10 + 新增 `heuristic9_trace` 6 测、
P0-1 派生量 3 测）；fmt / clippy 全绿。

**2026-07-03 纪律更新（用户拍板）：byte-equal 兼容门退役。** 它只是 Phase A 一次性的发布
条件——Phase B 起重构**不再兼容旧行为**、以「重新做一个、做得更好」为目标，仅过质量门
（多 seed sim mbb/g 不退 + 0 fallback / 0 重建漂移 + 样本纪律），见
`docs/scenario_heuristic_design.md` §6 纪律更新。

### Phase B 场景森林重构（2026-07-04）：显式节点分派 + i_have_initiative 修复（shipped）

场景化设计（`docs/scenario_heuristic_design.md` §6 Phase B）落地——首个「重新做、做得更好」
的行为性重构（等价门已随 Phase A 退役）：

- **结构**：`preflop_decide`/`postflop_decide` 隐式 if 树重写为显式场景森林——上下文节点
  enum `SpotNode`（`PreUnopened` / `PreFacingRaise(RaiseLevel)` / `PostUnopened(PotType)` /
  `PostFacingBet(PotType)`）+ `match` 分派（编译器保证完备，每个决策点恰好一个节点）；
  trace 路径由分派结构直接组装（「分派即路径」），`Response.trace` 透传链路与 driver 零改动。
- **节点分层**：`pre.facing_raise.*` 按加注级别拆 `.vs_open/.vs_3bet/.vs_4bet`；
  `post.unopened.{value_bet,cbet_bluff}` 按池型拆 `.srp/.3bp`（3bet 池与 SRP 分参——
  设计点名项；limped/4bet+ 落父 id 兜底）。Phase A 22 叶 id 全部保留为路径前缀，现行
  叶节点 32 个。参数收进 `HeuristicConfig.nodes` 新子树（`serde(default)`，partial-JSON
  覆盖照常）；`preflop`/`postflop` 退为共享层。
- **bug 修复**：`i_have_initiative()` 过期主动权（P0-1 收尾，backlog 旧项关闭）——按街
  回溯 `street_aggressors`；失去火力的 PFA 不再高频 c-bet、check-raise 接管方获得 barrel 权。
- **扫参**：`bluff_raise_freq_mult` 0.35→0.10 shipped（三档扫描 10/11 配置一致向好、与
  run31 live 遥测互证）；float 0.25、3bp c-bet 分参默认值按 §4.3 驳回/推迟（详见场景设计
  文档 §6 落地状态）。
- **回归门**：8000 手 × 11 配置（6-max mixed/limpy/station × seed 7/11/23 + 9-max/2-max）
  vs 重构前基线 10/11 上行、1 平、全在 CI95 内；0 fallback、0 重建漂移；fmt/clippy/
  heuristic 测试套件（+7 新测）/`--selftest` 全绿。

### Phase C 第一轮（2026-07-05）：3bp c-bet 分参 shipped + float 二次驳回（scenario §6 Phase C）

首轮 §4.1 逐节点闭环。造靶基建 `7a64c45`（`tools/heuristic9_sim.rs` 新增 `WideOpener` /
`ThreeBettor` 两 3bet 池对手原型 + `--field 3bp|wideopen|3bettor`，不入 mixed pool、默认
关，follow Limper/PairedTrap 先例）。三候选按 run31/32 live 定序处理，方法论 follow
PairedTrap 驳回格式（多 seed paired 同 seed 对比 mbb/g，靶场显著向好 + 标准矩阵不退化）：

- **候选① 3bp c-bet 频率（SHIPPED，`d84cc35`）**：`CbetSplit::threebet` 默认 0.80/0.55 →
  **0.30/0.20**（srp/other 不动）。靶 = `--field wideopen --seats 2`（hero 作 3bettor 进
  翻后，`cbet_bluff.3bp` n~60/8000 手）。9 seed paired：0.30/0.20 vs srp-同参 **9/9 一致
  +23.3 mbb/g**（单调至 0 的 +32.1，保留非零留节点存活 + 平衡头寸，follow Phase B
  bluff_raise 0.10 先例）；**无 --exploit 时同向 +20** → 结构性（3bet 池 fold-equity 低：
  低 SPR、大池、villain 跟 3bet 后 range 更强更粘），非画像分类假象。标准回归矩阵 11 配置
  均值 Δ −0.68 mbb/g（limpy/station/9max/2max 恰 0，仅稀疏 mixed 3bet 池漂 ±10）、
  0 fallback / 0 漂移——参数只在 3bet 池生效未串 srp，与 run32 live `value_bet.3bp` n=24
  +45.6 的「3bet 池是好 spot」互证。**同轮驳回 value_bet.3bp 价值阈值**：ve 0.58→0.50 在
  3bp-6max 仅 +31（4/7、深在 ±950 噪声内）、wideopen-2max 反 −4，跨靶不一致；ve 收紧
  （0.61/0.64）两靶一致变差 → 默认 0.58 已近最优，不动。
- **候选② float 频率（二次驳回）**：Phase B 已驳回 0.5→0.25 一刀切（3/11 负）。本轮在高密度
  靶复核揭示**机理**：float 最优值随对手翻后粘度**反号**——wideopen-2max（粘跟站）float=0.0
  最优（+49 mbb/g 9/9），3bettor-2max（过度 barrel 后弃牌的进攻方）float=1.0 最优（+33
  8/9），且**与 exploit 无关**（含/不含 --exploit 反号方向一致）。故任何一刀切乘子必错；正确
  修法 = 按对手翻后弃牌画像门控（scenario §5.2 节点级剥削查询 / P0-3），当前 float 节点仅
  经 `bluff_mult` 消费剥削读数、而该效应 exploit 无关 → `bluff_mult` 不含此信号 → 门控须等
  P0-3 per-opponent 翻后 fold-to-stab 统计。街/人数一刀切门控不解反号（分裂由对手驱动、非
  街/人数），拒绝按代理量过拟合两个合成靶（§4.2）。
- **候选③ bluff_raise 频率（复核，无改动）**：全局频率、非池型分参。3bettor-2max 靶（n~22）
  + 标准矩阵复核：降 0.10→0.0 仅 +8~11 mbb/g（7/9、9/11）但**杀节点**（Phase B 已权衡：保
  0.10 留 live 样本通道，每 2000 手 ~5 次）；升 0.20/0.35 两处一致明确变差（0/11、1/11）。
  **0.10 复核仍甜点，维持**。画像门控（仅对高弃牌对手开）与 float 同需 P0-3 翻后 fold 统计
  → 一并推迟。

### Phase C 第二轮候选序（run33 live 复盘重排，2026-07-06）

run33（2000 手，收频验证轮：`value_bet.3bp` n=32 +41.0 仍强正、`cbet_bluff.3bp` n=2 仍饥饿、
aggregate 0 −EV 跟注 / 0 兜底）单笔最大亏 −7821 chips（−391 BB）复盘定位到一个**系统性 EV bug**，
按正确性优先升为下一轮头号候选：

- **候选①（新增，头号）成花/成顺湿板非坚果 made 的 commit 过松**。实证 hand `1b3328bc`：hero
  `Qc3c` 在 `6h2s5c8c4c`（三梅花）成 **Q 高非坚果同花**，river `post.facing.value_raise` 读
  equity **0.966 / made=Monster** → 全下 7278 输给更高（A/K 高）同花；账分 −285 BB 直接归因坏
  stack-off（另 ~105 BB 为成型同花下注的 cooler 底）。根因两条（均可泛化）：① `MadeClass::Monster`
  （`features.rs:104-109`，亲验）把所有同花/顺子+ 归一档、无坚果度区分 → 非坚果同花过
  `value_thr`/`raise_value_thr` 并触发 `pick_bet_size` 超池尺寸；② `build_opp_thresholds`
  （`policy.rs:206-253`）用**翻前起手强度**分位建对手 range + 对大注反向收紧 → 成花河牌上剔掉
  A7s/K9s 这类翻前弱却成更高同花的手、留下输给 hero 的 AA/KK 超对 → MC 高估至 0.966（数值日志确认；
  此因果机制为复盘主张、未逐行复核）。旋钮：给 `MadeClass` 加 nut-ness 折让、成花/成顺板关 value_raise
  门与超池尺寸；`build_opp_thresholds` 加板质地感知（成花河牌不按翻前强度收紧、反纳低翻前强度的同色/
  连接手）。靶：扩 `heuristic9_sim` 一个「成花板 vs float-then-raise」场做 sim 回归。
- 候选②/③/④ 顺延：② 3bp c-bet 造靶（`cbet_bluff.3bp` n=2 饥饿）；③ float 造靶（run31/32/33
  三轮一致轻负、机理为对手翻后粘度反号、须 P0-3 门控）；④ bluff_raise 造靶（0.10 复核仍甜点）。

改动仍须 paired 复核（靶场显著向好 + 标准矩阵不退化）+ sim 回归门通过后方 shipped。

### Phase C 第二轮候选①（成花板非坚果 commit）：造靶落地 + 引擎改动**驳回**（2026-07-06）

run33 复盘的头号候选，走完「造靶 → 修 → sim 回归门」闭环后**驳回引擎默认改动**——第三次
印证 limp K-rag / PairedTrap 的同一教训：**教科书正确（非坚果别 stack-off）≠ 本场 +EV**。

**造靶（保留，`--field flushtrap`）**：新增 `FlushTrap` 对手原型——松被动入池见多翻牌（宽跟
制造到河成花机会），成花/成顺板上**仅持坚果同类**（nut flush / boat，`monster_non_nut==false`）
才升级下注/加注战，把 hero 非坚果同花 value_raise 全下钓进 drawing-dead；非坚果同类被动
check/call（让 hero 价值注被更差的牌支付、把漏洞隔离在 re-raise 上）；非成花/成顺板打法同
Tag。follow Limper/PairedTrap 先例：不入 mixed pool、默认关、既有 `--field` 逐字节输出不变
（byte-equal 复核 5 配置通过）。同增只读坚果度特征 `features::monster_non_nut` / `nonnut_flush`
（+10 单测）与 `--trace-summary` 下的「非坚果同花面对下注」n 计数。

**修复设计（已实现、已 sim 回归、已还原）**：给成手加坚果度判定（同花：hero 是否持该花色
高于自身最高 hole 张、且不在 board 的牌 → 有则非坚果；顺子：monotone/成对板或有更高顺子
top → 非坚果）；policy 对**非坚果 Monster** 关 `post.facing.value_raise` 门（降级为
`odds_call`）+ 关超池尺寸。回归单测确认 `Qc3c` on `6h2s5c8c4c` 由 value_raise→call、
`Ac3c`（坚果）仍 value_raise。

**回归门数据（驳回依据）**：
- **靶场（`--field flushtrap` 2-max 200bb，窄化到「仅同花」gate）**：3/3 seed 一致向好
  **+29.4 / +34.5 / +37.7 mbb/g**——对**河牌极化加注**的对手，压制非坚果同花 stack-off 确实 +EV。
- **标准矩阵（11 配置，8000 手，`--exploit`）**：**11/11 一致退化、均值 −113.9 mbb/g**
  （mixed −25~−86、limpy −115~−174、**station −155~−245**，0 fallback / 0 漂移）。对**弱/被动/
  跟注场**（=我们实际面对的场、也是 live +1600 mbb/g 的来源），hero 非坚果同花的 value_raise
  与超池是被更差的牌（更小同花/对子/两对）**支付的 +EV 动作**，降级为默认反而漏价值。
- **归因定位**：value_raise 门单独降级已退化（seed7 mixed/limpy/station −55/−76/−199），超池关闭再叠加。
- **顺子先被排除**：初版 gate 含顺子时靶场反而 −100~−260——instrument 实测「非坚果顺子 value_raise」
  在干板对 Tag 风格对手是**强 +EV**（seed23 靶：23 次 value_raise 点火合计净 **+49210 chips**，多为
  T/J/K 高顺子收更差两对/暗三/小顺子）。故 gate 窄化到「仅成花板同花」，但仍未过标准矩阵门。
- **漏洞在 sim 极稀疏**（同 PairedTrap）：非坚果同花面对下注每 8000 手仅 n≈2–5、48000 手才 ~15，
  且 hero 的 equity 模型（成花板过度收紧对手 range → 读 ~1.0）令 hero 无论如何跟到底、
  value_raise 门只省下 re-raise 增量 → 净收益小、被标准矩阵的漏价值淹没。

**结论**：机理对**河牌极化加注**的对手（真 reg、或本靶）是真 -EV，但对**弱/被动跟注场是 +EV**。
正确归属 = **剥削层**（仅对画像为「河牌高频极化加注」的对手压制非坚果 stack-off，P0-3
per-opponent 河牌加注极性统计门控），非默认改动。引擎代码已还原（policy 逐字节回 HEAD、标准
矩阵 byte-equal 复核通过）；`FlushTrap` 靶 + 坚果度特征 + 单测**保留**为该剥削项的将来验证夹具
（follow `0bfea7a` Limper / PairedTrap 先例）。

### Phase C 第二轮候选②（3bp c-bet 频率）：跨靶稳健性复核 → **CONFIRMED-ROBUST（无引擎改动）**（2026-07-06）

round-1 的 `cbet_bluff.3bp` 默认 **0.30/0.20**（`d84cc35`）只在**单一** `WideOpener`（翻后粘站）靶上
验过。本轮不重扫频率（round-1 已做透、已探单调至 0），而是做**诊断驱动的跨靶稳健性复核**：float
先例（round-1 二次驳回）证明 3bp 空气决策的最优频率会**随对手翻后弃率反号**（wideopen→float 0.0；
3bettor→float 1.0，且 exploit 无关）。0.30/0.20 现在是 shipped 默认，其立论「3bet 池 fold-equity
结构性低」只有在 3bet-pot caller 粘站时才成立——须验它对**翻后爱弃**的 caller 是否也成立。

**造靶（保留，`--field 3bpfold`）**：新增 `ThreebetFolder` 对手原型——翻前**逐字节共享 `WideOpener`
分支**（宽开 + 被 3bet 后跟，喂 hero 作 3bettor 进翻后、保节点密度），翻后**高弃**（面对 c-bet 仅
TopPair+ / 好赔率强听牌继续，其余全弃）。翻前共享 → 唯一差异 = 翻后弃率（隔离变量，follow PairedTrap
「非对子面同 Tag」范式）。不入 mixed pool、默认关、既有 `--field` 逐字节输出不变（byte-equal 复核 8
配置通过）；0 fallback / 0 重建漂移。`cbet_bluff.3bp` 点火 n=28（LO）/ ~46（HI）/8000，与 WideOpener（33）同量级。

**跨靶 paired 数据（8000 手, 2-max, --exploit, paired 同 seed；LO=0.30/0.20 vs HI=0.80/0.55，mbb/g）**：

| seed | WideOpener（粘站）LO / HI / Δ | ThreebetFolder（爱弃）LO / HI / Δ | 极端 over-folder（仅两对+跟）Δ |
|---|---|---|---|
| 7  | 527.2 / 508.9 / **+18.3** | 650.6 / 659.4 / −8.8 | −18.9 |
| 11 | 623.3 / 591.2 / **+32.1** | 897.6 / 912.7 / −15.1 | −13.2 |
| 23 | 636.6 / 627.8 / **+8.8**  | 654.7 / 644.8 / +9.9 | +2.4 |
| 31 | 463.0 / 439.5 / **+23.5** | 824.8 / 812.6 / +12.2 | +5.6 |
| 47 | 573.7 / 547.6 / **+26.1** | 686.8 / 678.3 / +8.5 | −8.8 |
| 均值 | **LO +21.8（5/5 LO 优）** | **−0.66（3 正 2 负 = wash）** | −6.6（2 正 3 负，非显著） |

**结论：CONFIRMED-ROBUST，0.30/0.20 稳健，无引擎默认改动。** 与 float 的**戏剧性显著反号**（+49/+33）
不同：3bp c-bet 频率**跨对手粘度不反号**——爱弃靶 wash、连人为的极端 over-folder（连顶对都弃、每街）
也只弱 lean HI 且不过 CI（均值 −6.6，2/5 正）。而 HI（round-1 前的旧默认）在粘站靶被**清晰击败**（5/5，
LO +21.8）、在爱弃靶仅弱势不显著领先 → HI 被 dominated。机理：3bet-pot caller 的 range 已被翻前跟 3bet
**封顶为偏强**，翻后 TopPair+/两对+ 命中足够频繁 → 惩罚 hero 的多街空气 barrel，把 fold-equity 顶住；
一个**真·高弃**的 3bet caller 是自相矛盾的（它会直接在翻前弃掉 3bet）。故 round-1「3bet 池 fold-equity
结构性低」的立论**跨对手粘度成立**，非单靶过拟合。

引擎默认不动（policy/config 逐字节 = HEAD `0f2f3bd`；`--field 3bpfold` 加入后既有 field byte-equal
复核通过）。`ThreebetFolder` 靶**保留**为将来 P0-3 per-opponent 翻后 fold 统计落地后、3bp c-bet 频率
**画像门控**（若届时要对爱弃对手抬频）与 3bp c-bet **尺寸**（P0-2 结构面：现尺寸仍共享 `pick_bet_size`
层未按节点分槽；本轮判定尺寸最优方向继承与频率相同的对手依赖、须同 P0-3 信号驱动，故推迟）的验证夹具
（follow `0bfea7a` Limper / PairedTrap / `FlushTrap` 先例）。

### P0-3 阶段2（首个消费轮）：float 门节点级门控 **SHIPPED**（2026-07-06，scenario §5.2/§7）

P0-3 阶段1（`70eecb5`/`402a4e9` 采集基建 + `e74b9c1` live 采集口径前置）落 per-node 面对下注
fold 读数后的**第一个会改行为、兑现 EV** 的消费轮。两次被驳回的 float（Phase B 0.5→0.25 一刀切
3/11 负；Phase C round-1 复核揭示**反号**：粘站 wideopen float≈0 最优、爱弃进攻方 3bettor float≈高
最优，且 exploit 无关）终于被**正确修法**解决——不是全局乘子，而是**按面对 hero stab 的 villain 在
对偶 `(街.池型)` 节点的 fold-to-stab 读数单调门控**。

**机制（`src/heuristic/policy.rs`）**：float 门（`post_unopened` 内，made=Air、无听牌、无主动权、
本街无人下注）的频率乘子由 `float_freq_mult(ctx)` 决定：查 binding villain（仍在场对手里
**最粘者** = min `shrunk_fold`，bluff 需全弃故最粘者 binding）在对偶节点 `facing_node_key(街, 池型)`
（与阶段1 采集侧同口径构造 → §5.2 单一坐标系）的 `node_readout(name, node, k).shrunk_fold`。
`NodeCtx` 新接 `&OpponentTable`（原只带聚合 `adj`）。掷点仍在 gate 之后、位置不变 → RNG 消费序列
不变（守 invariants 无全局 RNG）。

**避开朴素 breakeven 陷阱**：不用 `f > s/(s+P)` 直接开关（wideopen fold 47% > 0.5-pot breakeven
33% 会误判「该 float」，但实测最优是 0——延迟/OOP stab 未弃对手续街、hero 空气续街≈0 equity →
朴素 breakeven 高估 float 盈利）。fold 只作**驱动信号 + 单调映射**，阈值/斜率**造靶校准**：
`--opp-stats` 实测 villain(p1) 在 flop.srp 的 shrunk_fold —— wideopen 47.0% / 3bettor 64.5%（清晰
discriminator），锚 `[fold_lo 0.50→mult 0.0, fold_hi 0.65→mult 3.0]` 线性插值 →
wideopen(0.47<0.50)→mult 0（float≈0，复现 +49 最优侧）、3bettor(0.645→t≈0.97)→mult≈2.9 →
float=air_bluff_base(0.30)×2.9≈0.87（float 高，复现 +33 最优侧）。turn/river fold 低（且 3bettor
那里 raise 率 22–30% 惩罚 float）→ 两靶均映射低 float（正确：延迟街 float 风险高）。

**冷启动/未知/小样本 = 现默认**（保标准矩阵不退、exploit 关逐字节一致）：`node_gated=false` /
任一对手无名（exploit 关：sim `names` 空 / live 缺名）/ binding villain `n < min_samples`(30) →
回退固定 `nodes.float_freq_mult`(0.5) = Phase B/C 旧路径。config 旋钮 `NodesConfig.float`
（`FloatGateConfig`，`#[serde(default)]`）：`node_gated`/`min_samples`/`shrink_k`/两锚（fold/mult）。

**回归门（paired 同 seed，8000 手 --exploit；node-gated=默认 ON vs 固定默认 OFF）**：

| 靶（2-max） | 语义 | node-gated ≥ 默认 | 均值 Δ mbb/g |
|---|---|---|---|
| `wideopen` | 粘跟站（float 应低）| **9/9 seed** | **+46.0** |
| `3bettor` | flop 爱弃进攻方（float 应高）| **9/9 seed** | **+51.0** |
| `3bpfold`（旁证）| fold-happy | 3/3 seed | +176~+219 |

**反号解决证明**：一个固定乘子只能赢一边；node-gated 在**两个反号靶都 9/9 ≥ 默认且均显著正**——
粘站靶靠**降** float 赢、爱弃靶靠**升** float 赢，同一门控两边都对。

**标准矩阵不退化（11 配置，node-gated vs 固定默认=baseline）**：**0/11 超 CI95、均值 Δ +14.1
mbb/g**（9 正 2 微负 −13.4/−2.9 均在 CI 内）——node-gating 对每个对手按其 fold 率自适应，方向普适
（粘→少 float、弃→多 float）故标准场也不劣反微升。**exploit 关**：9/6/2-max × seed7（±start-bb 400）
vs baseline binary(HEAD `e74b9c1`) **逐字节一致**；`node_gated=false`+exploit == baseline+exploit
逐字节一致（回退路径 = 未改旧行为）。**0 fallback / 0 pot|to_call 漂移**（全配置）。

**测试**：`policy.rs` 内 5 新单测（映射单调+夹取 / 冷启动=默认 / n 门槛回退 / 节点查询正确即查对偶
`flop.srp` / 多人池 binding=最粘者）；`heuristic9`(10) + `heuristic9_trace`(10) + `heuristic_p0_3_facing`(7)
全绿；fmt / clippy(`-D warnings`) 全绿。

**范围外（留后续轮，复用同机制）**：bluff_raise 门（全局频率、非池型分参，同需翻后 fold 门控）、
3bp/srp c-bet 过度诈唬门；多人池现取最粘者 min-fold 近似（HU 为主场做干净，多人池任一对手未知即回退
默认——保守但不误开）。

### P0-3 阶段2 第二消费轮（bluff_raise 门）：造靶落地 + 引擎改动**驳回**（2026-07-06）

float 门（same-role）成功后，尝试对 `post.facing.bluff_raise`（纯诈唬加注频率乘子
`nodes.bluff_raise_freq_mult=0.10`）复用同机制。走完整闭环（先证据后接线），判定 **REJECTED**——
不是失败，是「代理不成立」的合法结论（照候选①成花板 REJECT 先例：造靶保留、零引擎行为变更）。

**现状**：`bluff_raise` 频率 = `air_bluff_freq_base × 0.10`（clamp[0,0.4]），全局固定乘子。触发条件 =
hero 持 Air、无有效听牌、**面对 villain 下注**、`can_raise`、非河。

**杠杆（关键，与 float 的分野）**：bluff_raise 的 EV 杠杆 = villain 的 **fold-to-RAISE**（villain 作
**进攻方被加注**）。float 门 same-role 成立是因 float 杠杆 = fold-to-cbet（villain 作**防守方**），门读的
正是 fold-to-cbet；bluff_raise 若照抄，门读 `shrunk_fold`（= fold-to-cbet，villain 作**防守方**），
与真杠杆 fold-to-raise **跨角色**。且采集侧 `observe_facing` **被 re-raise 即 break、开火者不入 facing**
→ **fold-to-raise 从未进表**，门物理上读不到正确信号。

**造靶（隔离变量，follow PairedTrap「非对子面同 Tag」）**：`tools/heuristic9_sim.rs` 新增
`DonkFold`/`DonkStick` 两 archetype + `--field donk_fold|donk_stick`（不入 mixed pool、默认关，既有
`--field` 5 靶逐字节等价已验）。两靶**共享**翻前松被动 + 翻后 donk-lead 宽（喂 hero Air 面对下注密度：
标准靶 bluff_raise 仅 15-18 takes/8000，donk 靶 m=0.10 → 35、m=2.0 → 441-473，机会 ≈1100/8000）+
防守方 fold-to-cbet 规则；**唯一分叉 = fold-to-raise**（自己 lead 被 hero 加注后：DonkFold 仅 Strong+
继续、其余全弃 = 高 fold-to-raise；DonkStick 仅 Strong+ 加注、其余全 call = 低 fold-to-raise）。

**sim 数据**：
- **纪律 A（sweep 反号，成立）**：`bluff_raise_freq_mult` ∈ {0,0.1,0.5,1,2} paired 同 seed{7,11,23}
  8000 手。DonkFold 单调↑（seed7 290→556，3 seed 均 +135~+266 mbb/g，高频最优）；DonkStick 单调↓
  （seed7 1575→1286，3 seed 均 −289~−300，低频最优）。**反号真实**——一个固定乘子只能赢一边。
- **纪律 B（代理失效，决定性）**：两靶 villain(p1) 的 fold-to-cbet（门实际读的 `shrunk_fold`）
  **flop.srp 逐字节相同 33.8%（n=1266）**、turn.srp 21.9/22.0、river.srp 24.1/27.2；而最优 bluff_raise
  频率**相反**。门读 fold-to-cbet 对两靶返回**相同乘子** → 无法剥削反号。villain 的「作防守方弃牌倾向」
  与「作进攻方被加注弃牌倾向」是**独立两轴**，本靶隔离证明可任意解耦。

**判定 = REJECTED**：反号真实但门读的 fold-to-cbet **不能代理** fold-to-raise（跨角色）。**正确修法**
（非本轮）= 先扩采集 fold-to-raise（Phase-1 §B/§H：`observe_facing` 记进攻方被 re-raise 的首次响应），
再照 float same-role 门控。现状固定 `bluff_raise_freq_mult=0.10` 对 sticky/常见场近最优（sweep 低频侧
最优），**维持不动、零引擎改动**。造靶 `DonkFold`/`DonkStick` 保留为 fold-to-raise 采集落地后的门控
验证载荷。诊断全文 `scratchpad/p03_bluff_raise/DIAGNOSIS.md`。

### P0-3 采集侧扩展：fold-to-raise（villain 进攻方被加注）**SHIPPED**（2026-07-06）

bluff_raise 门 REJECT 的「正确修法」落地：**先扩采集 fold-to-raise**（bluff_raise/候选① 的真杠杆），
门控留后续轮。纯增采集、决策路径不读 → **sim byte-equal**（Phase-1 纪律）。**引擎决策零改动**
（`policy.rs` 未碰），只动采集（`opponent.rs`）+ dump（`heuristic9_sim.rs`）+ tests + docs。

**采什么**：`observe_facing` 新增翻后 fold-to-raise 循环——把翻前 `pre.facing_3bet`（opener 面对站住
3bet 的首次响应）**镜像到翻后每街**：街内**开火者**（首个 raiser = 领打/c-bet 方）面对站住的 re-raise
（街内恰好 2 个加注）的**首次响应**。既有 fold-to-cbet 循环在防守方 re-raise 处 `break` 且开火者本身
不入 facing → fold-to-raise 从未进表；本轮补的正是这个（**独立循环、独立 key，与 fold-to-cbet 采集
互不干扰** → fold-to-cbet 逐字节不变）。

**5 设计决策**：
1. **key** = `<街>.<池型>.facing_raise`（`flop.srp.facing_raise` 等），新 `pub facing_raise_node_key`
   = `facing_node_key` 的 `.facing_raise` 兄弟（§G 扩展口子，与采集共用同一构造器，未来 hero 侧门复用）。
   存入**同一** `facing: BTreeMap`（新 key，**不加结构字段**）→ 旧 `h9_live_prof*.json` serde 逐字节兼容。
2. **前缀碰撞（必踩陷阱）**：`flop.srp.facing_raise` 会被 `starts_with("flop.")` 抢先匹配到 fold-to-cbet
   先验 0.456（错）→ `node_field_prior` 加 `ends_with(".facing_raise")` **专用前置分支**（照 doc「长/专用
   先判」）。无实测（weigh.py 无 fold-to-raise replay）→ 保守占位 **0.30**（进攻方被加注弃牌率通常低于
   面对首注，**占位待实测校准**）。
3. **收缩**复用 `shrunk_fold_rate`（Beta-Binomial）；`raises` 桶 = 开火者被加注后 re-raise（4bet/3bet-back）
   = 更高极性信号，一并采/dump。
4. **止步首次响应**（`recorded` 守卫，照 pre.facing_3bet，不追 4bet/5bet 链）。
5. **多街** flop/turn/river 各自节点，独立 per-street 循环。

**自校验（造靶方向铁证；无 weigh.py 参照，用 DonkFold/DonkStick 隔离 fold-to-raise）**：
`--field donk_{fold,stick} --seats 2 --exploit --opp-stats --hands 8000 --seed 7`。两靶防守方
fold-to-cbet + donk-lead 规则逐字节相同，唯一分叉 = 自己 lead 被加注后响应。读数：

| 靶 | `flop.srp.facing_raise` raw_fold (n) | 聚合 `*.facing_raise` raw_fold (n) |
|---|---|---|
| **DonkFold** | **93.5%** (n=154) | **86.7%** (n=369, folds 320) |
| **DonkStick** | **0.0%** (n=154) | **0.0%** (n=510, calls 449) |

**n 两靶相同（flop.srp 154）**——同 lead+防守规则 → 开火者面对加注机会数相同，只**响应**不同
（folds 320 vs calls 449）= 正是隔离变量。方向成立 → 采集接对了 actor（进攻方开火者）与时点（面对
re-raise）。byte-equal：基线 vs 改动默认输出 **0/15 diff**（5 field × 3 seats）。

**解锁**：bluff_raise 门（本清单 §6 REJECT，真杠杆 = fold-to-raise）+ 候选① river 极化门现有真信号可读
（`facing_raise_node_key` 查询、same-role 门控），门控接线留后续消费轮。`DonkFold`/`DonkStick` 造靶留作
门控验证载荷。诊断/数据 `scratchpad/p03_fold_to_raise/DIAGNOSIS.md`。

### P0-3 阶段2 第三消费轮（srp cbet 过度诈唬门）：节点级门控 **SHIPPED**（2026-07-06）

float 门（same-role SHIPPED）后、bluff_raise 门（跨角色 REJECT）后的第三个消费轮，命中**干净
same-role**：`post.unopened.cbet_bluff.srp` = hero 作 PFR 进攻方 c-bet 空气、villain 作防守方面对
→ 杠杆 = villain **fold-to-cbet**，门读的正是 §C fold 桶（同 float 轴，区别于 bluff_raise 的跨角色
fold-to-raise）。两矛盾先验（float 反号 vs 候选② 3bp robust）经造靶实测判定 = **srp 反号（像 float）**。

**现状（门前）**：srp c-bet 诈唬频率 = `lerp(cbet.srp.freq_dry, cbet.srp.freq_wet, wetness) × bluff_mult
× multiway_cut`，clamp[0,0.95]，默认 **0.80/0.55** 全局固定。

**纪律 B（代理有效，此轮成立）**：复用 float 门造靶（无新靶）——WideOpener（粘站）vs ThreebetFolder
（`--field 3bpfold`，爱弃），翻前逐字节共享、唯一分叉 = 翻后 fold-to-cbet。`--opp-stats` 实测 villain(p1)
在 **flop.srp shrunk_fold：wideopen 47.9%（n=1222）/ 3bpfold 79.6%（n=2821）= 分化 31.7pp**（比 float
校准 47/64.5 更宽）。节点密度 cbet_bluff.srp **636 / 655 每 8000 手**（远密于 float 168 / 3bp cbet 33）。
分化 + 密度双达标，且最优频率方向与门读轴对齐 → 代理 VALID。

**纪律 A（sweep 反号，决定性）**：只扫 `nodes.cbet.srp`（3bp 保 0.30/0.20 不动），5 档
{0/0, 0.20/0.14, 0.40/0.28, 0.80/0.55, 0.95/0.65} × paired 同 seed{7,11,23} × 8000 手 --exploit：

| freq 档 | WideOpener（粘 47.9%）均值 | ThreebetFolder（弃 79.6%）均值 |
|---|---|---|
| 0/0 | **676.3** | 868.2 |
| 0.20/0.14 | 675.5 | 896.8 |
| 0.40/0.28 | 663.7 | 906.7 |
| 0.80/0.55（默认） | 635.4 | 926.2 |
| 0.95/0.65 | 615.7 | **929.8** |

**粘站单调 ↓（低频最优）、爱弃单调 ↑（高频最优），3/3 seed 一致 → 反号真实。** 与候选②
**3bp c-bet 跨靶 robust（无反号）**成鲜明对比——**池型（srp vs 3bp）正是分野**：srp cbet_bluff 结构上
像 float（villain 防守 range 未封顶、fold-to-cbet 变异大 → 反号），3bp 池 caller range 被翻前跟 3bet
封顶偏强（fold-equity 顶住 → robust）。任何全局固定 srp 频率必错一边。

**门设计（锚独立重标定，`CbetBluffGateConfig`）**：cbet 是持续下注、默认频率高（0.80）恰坐**爱弃
最优端** → 可剥削 EV 全在**粘站降频**侧 → 乘子上限 = 1.0（= 回退），门**只降不升**。锚
`[fold_lo 0.50→mult_lo 0.15, fold_hi 0.75→mult_hi 1.0]`：wideopen 0.479 → 乘子 0.15 → freq≈0.12/0.08
（落粘站最优带，vs 默认 +40）；3bpfold 0.796 → 夹取 1.0 → 保默认高频（爱弃最优）。**仅门控 srp**
（`pot != SingleRaised` → 乘子 1.0，3bp robust 保 0.30/0.20 + limped/other 不动）。冷启动/未知/n<30/
`node_gated=false` → 乘子 1.0 = `CbetSplit` 现固定频率（byte-equal 安全，同 float 结构）。`policy.rs`
四函数照 float 四函数写（`map_fold_to_cbet_mult`/`cbet_bluff_binding_readout`/
`cbet_bluff_mult_from_readout`/`cbet_bluff_freq_mult`）；gate 插在该叶 `roll = rng.unit()` **之前**、
掷点位置绝对不动 → RNG 消费序列不变。

**回归门（paired 同 seed，8000 手 --exploit；node-gated=默认 ON vs OFF=baseline）**：

| 门 | 结果 |
|---|---|
| 靶字段 wideopen（粘，应降频）| ON−OFF **3/3 seed，均值 +42.3** |
| 靶字段 3bpfold（弃，应保默认）| **2/3 seed 正，均值 +12.9**（门减 turn/river barrel，3bpfold 那两街更粘）|
| 标准 11-config（6-max {mixed,limpy,station}×{7,11,23} + 9-max s7 + 2-max s7）| **0/11 超 CI95、均值 +28.6**（粘站 Station/Limper 场受益）|
| exploit 关（9/6/2-max × s7 × ±start-bb 400）vs HEAD `4893739` 二进制 | **6/6 逐字节一致** |
| `node_gated=false`+exploit vs HEAD | **3/3 逐字节一致**（回退 = 旧行为）|
| 0 fallback / 0 pot\|to_call 漂移（含深码 200/400BB）| **全配置 0** |

**反号解决证明**：一个固定 srp 频率只能赢一边；node-gated 在两个反号靶都 ≥ OFF——粘站靠**降**频赢、
爱弃保默认（已最优）。cbet 默认高频恰坐爱弃端 → 门捕获全部粘站侧 EV，爱弃/未知逐字节保守不动。

**测试**：`policy.rs` 内 5 新单测（`cbet_gate_tests`：映射单调+夹取 / 冷启动=乘子 1.0 / node_gated 关 /
n 门槛回退 / 节点查询正确即查对偶 flop.srp / 多人池 binding=最粘者）；`heuristic9_trace`(10) +
`heuristic_p0_3_facing`(7) 全绿；fmt / clippy(`-D warnings`) 全绿。**未跑全量 cargo test**。
诊断/数据 `scratchpad/p03_cbet_bluff/DIAGNOSIS.md`。

**范围外（留后续轮）**：srp c-bet **尺寸**按对手粘度分槽（现共享 `pick_bet_size`）；turn/river barrel
频率独立门（现随 srp 锚同映射）；3bp cbet 门（候选② robust，须新反号证据才动）。

### P0-3 阶段2 第四消费轮（bluff_raise 门，重做）：节点级门控 **SHIPPED**（2026-07-06）

上轮（commit 4893739）对 `post.facing.bluff_raise` 复用 float 机制被 **REJECT**——真杠杆 = villain
**fold-to-RAISE**（villain 作**进攻方/开火者被加注**，§B），但当时门只能读 fold-to-cbet（villain 作
**防守方**，`facing_node_key`）= **跨角色代理**，两靶 fold-to-cbet 逐字节相同 33.8%、最优频率却相反 →
无法区分 → REJECT。commit fac17a8 扩采 fold-to-raise（节点 key `<街>.<池型>.facing_raise`，via
`facing_raise_node_key`）后，本轮门读**真信号** = **same-role**（杠杆 fold-to-raise，门读 fold-to-raise）
→ 上轮 REJECT 记录写明的「正确修法」落地。

**现状（门前）**：bluff_raise 频率 = `air_bluff_freq_base(ctx,multiway) × bluff_raise_freq_mult`
（clamp[0,0.4]），`bluff_raise_freq_mult=0.10` 全局固定。触发 = hero 持 Air、无有效听牌、**面对 villain
下注**、`can_raise`、非河。

**唯一实质区别 vs float/cbet 门** = binding readout 的节点 key 用 `facing_raise_node_key`（fold-to-raise，
villain 进攻方被加注）而非 `facing_node_key`（fold-to-cbet，villain 防守方）。这正是上轮跨角色 →
本轮 same-role 的信号源分野。四函数（`map_fold_to_bluff_raise_mult` / `bluff_raise_binding_readout` /
`bluff_raise_mult_from_readout` / `bluff_raise_freq_mult_gated`）照 float 四函数写（回退 = 固定
`bluff_raise_freq_mult`，非 cbet 的 1.0）；gate 插在该叶 `roll = rng.unit()` **之前**、掷点位置绝对不动。

**纪律 A（反号，当前树 058c1f5 亲自复现）**：`bluff_raise_freq_mult`∈{0,2}×paired seed{7,11}×8000 手
2-max --exploit——DonkFold（高 fold-to-raise）单调↑（+260/+231）、DonkStick（低 fold-to-raise）单调↓
（−372/−356），4/4 一致。**反号真实**，一个固定乘子只能赢一边。

**纪律 B（代理有效 = 本轮 vs 上轮决定性差异）**：门读 `facing_raise` 与真杠杆 fold-to-raise **同轴**。
`--opp-stats` 实测 villain(p1) `flop.srp.facing_raise` shrunk_fold：**DonkFold 86.7%（raw 93.8, n=161）
/ DonkStick 3.3%（raw 0.0, n=161）= 分化 83.4pp**（turn.srp 74.5% / 3.4%），且分化方向与最优频率对齐
（高 fold-to-raise → 高最优频率）→ **代理有效、能剥削反号**。对照上轮门读 fold-to-cbet **两靶逐字节相同
33.8%（0 分化，死信号）**——这就是「解耦→同轴」的决定性区别。

**门设计（双向，锚独立标定，`BluffRaiseGateConfig`）**：bluff_raise 默认低频（0.10）、最优频率随
fold-to-raise 反号 → 门须**双向**（爱弃升频剥削 / 粘站降频止损），乘子范围 `[0.0, 2.0]`（**不套 cbet 上限
1.0**）。锚 `[fold_lo 0.30→mult_lo 0.0, fold_hi 0.80→mult_hi 2.0]`：DonkStick 3.3%<0.30 → 0.0（下调）；
DonkFold flop 86.7%>0.80 → 2.0、turn 74.5%→1.78（上调）。低锚 0.30 = fold-to-raise 占位先验 →「典型/
未分化」villain 落地板 0.0（保守，仅对**明确爱弃** fold>0.30 才升频，照 float 哲学）。**门控全池型**
（区别于 cbet 仅 srp：fold-to-raise 全池型都采、bluff_raise 无 3bp robust 例外）。冷启动/未知/n<30/
`node_gated=false` → 回退固定 `bluff_raise_freq_mult`(0.10) = 现默认行为（byte-equal 安全 + 保节点存活）。

**回归门（paired 同 seed，8000 手 --exploit；node-gated=默认 ON vs OFF=node_gated:false=HEAD）**：

| 门 | 结果 |
|---|---|
| 靶字段 donk_fold（高 fold-to-raise，应升频）| ON−OFF **3/3 seed，均值 +145.3**（+182/+151/+102）|
| 靶字段 donk_stick（低 fold-to-raise，应降频）| **2/3 seed 正，均值 +7.6**（+10.8/+16.7/−4.7 噪声内；0.10→0.0 对粘站仅微增）|
| 标准 11-config（6-max {mixed,limpy,station}×{7,11,23} + 9-max s7 + 2-max s7）| **0/11 超 CI95**（多数 Δ=0.0，facing_raise 标准场稀疏→回退 0.10 门不激活；wash = 预期窄剥削）|
| exploit 关（field×seats{2,6,9}）vs HEAD `058c1f5` 二进制 | **15/15 逐字节一致** |
| `node_gated=false`+exploit vs HEAD 二进制 | **12/12 逐字节一致**（回退 = 旧行为）|
| 深码 400BB exploit-OFF vs HEAD | **4/4 逐字节一致** |
| 0 fallback / 0 pot\|to_call 漂移（ON gate 活跃路径）| **全配置 0.00% / 0.000%** |

**反号解决证明**：一个固定乘子只能赢一边；node-gated 在两个反号靶都 ≥ OFF——爱弃靶靠**升**频赢
（+145.3 强）、粘站靶靠**降**频赢（+7.6 微，默认 0.10 已近粘站最优）。上轮 REJECT 的正解落地。

**测试**：`policy.rs` 内 5 新单测（`bluff_raise_gate_tests`：映射单调**双向**+夹取 / 冷启动=固定 0.10（非
1.0）/ node_gated 关 / n 门槛回退 / **节点查询用 facing_raise key 而非 facing key**（决定性）/ 多人池
binding=最粘者）；`heuristic9_trace`(10) + `heuristic_p0_3_facing`(12) 全绿；fmt / clippy(`-D warnings`)
全绿。**未跑全量 cargo test**。诊断/数据 `scratchpad/p03_bluff_raise2/DIAGNOSIS.md`。

**范围外（留后续轮）**：bluff_raise **尺寸**按对手粘度分槽（现共享 `pick_bet_size`）；候选① river 极化门
（现有 `facing_raise` 真信号可读，同 same-role 机制）；fold-to-raise 先验 0.30 待 weigh.py 实测校准。

### P0-3 阶段2 第五消费轮 = 候选① river 极化门（画像门控版，第三次尝试）：引擎改动**驳回**（2026-07-06）

候选①（成花板非坚果 stack-off）的**第三次尝试**。前两次（全局降级）死在标准矩阵 11/11（−113.9）；
REJECT 记录归因 = 剥削层画像门控。本轮试图用 villain 河牌极性代理**只对极化对手**降级
`post.facing.value_raise`（非坚果 Monster → odds_call），对弱场保 value_raise。走完整闭环，判定
**REJECTED**——§B 频率代理不跟真极性 + 样本饥饿，非失败（照 bluff_raise v1 / 候选① 成花板先例）。

**机制**：river + `monster_non_nut(hole,board)` + binding villain 河牌极性信号达阈 → 降级 value_raise
为 call（直接路由到 call 叶、无 RNG 消费）。信号候选 = `river.<池型>.facing_raise` 的 fold-to-raise 或
raise-to-raise。已实现（`RiverPolarityGateConfig` + policy 消费 `monster_non_nut`/`nonnut_flush` + 四函数
照 bluff_raise 门写、唯一区别读 shrunk_raise），sim 复核后**逐字节还原 HEAD**。

**信号判别（20000 手 2-max，villain=p1，`river.srp.facing_raise`，k=20）**：

| 场 | shrunk_raise | raw_raise | shrunk_fold | raw_fold | n |
|---|---|---|---|---|---|
| **FlushTrap（-EV 靶）** | **44.9%** | 72.4% | 12.2% | **0.0%** | 29 |
| **Tag（+EV 标准 reg）** | **45.2%** | 64.3% | 9.7% | **0.0%** | 42 |
| Maniac | 9.6% | 10.5% | 23.2% | 21.9% | 105 |
| Station | 0.6% | 0.0% | 3.4% | 0.0% | 158 |
| Limpy | 1.1% | 0.0% | 6.3% | 0.0% | 75 |
| Nit | 3.4% | 0.0% | 20.7% | 0.0% | 9 |

**代理风险 #1（频率 ≠ 真极性，致命）**：① **fold-to-raise 死信号**——FlushTrap villain raw fold-to-raise
**0%** = station/tag/nit（成花板持 nut 被加注**从不弃、再加注**；非坚果被动**不下注**→ 不进 facing_raise
分母），bluff_raise 门那条轴在此无分化。② **raise-to-raise 判被动弱场、但与 Tag 撞车**——FlushTrap 44.9%
vs Tag 45.2%，**FlushTrap < Tag → 无阈值只点火 FlushTrap 而不点火 Tag**；非坚果 value_raise 对 Tag 是
+EV（干板非坚果顺 REJECT 记录 +49210/23 次）→ 降级 -EV。raise-aggression ≠ nut-polarity。

**代理风险 #2（纹理无关）**：节点 key 无 §G 纹理维。FlushTrap raise-to-raise 44.9% **主由非成花板 Tag
式攻击贡献**（成花板在聚合中罕见）→ 分开 FlushTrap 与 station 的是**通用攻击性、非成花板极性**——正是
#1 里 Tag 撞车的机理来源。

**靶场 + 标准矩阵（成败关键，paired ON vs OFF，chips，seed 7/11/23/12345）**：

| 门 | 结果 |
|---|---|
| FlushTrap `monster_non_nut` GATED @200bb | 0 / −15601 / −10443 / +8（**HURTS**，含顺子）|
| FlushTrap `monster_non_nut` ALWAYS @200bb | +486 / −34159 / −31156 / −27542（顺子降级大亏）|
| FlushTrap **flush-only** ALWAYS @200bb | 0 / 0 / +3378 / +8645（**HELPS**，复现旧 REJECT 记录、验证 harness）|
| FlushTrap **flush-only** GATED(min15,thr0.30) @200bb | 0 / 0 / 0 / **+1802**（样本饥饿仅 ~1 次点火）|
| FlushTrap **flush-only** GATED @100bb | 0 / 0 / 0（从不点火）|
| **标准 11-config @8000 手 --exploit** | **0/11 非 byte-equal（空洞通过：门 8000 手内从不点火）** |
| exploit 关（field×seats）ON vs node_gated=false | **5/5 逐字节一致** |

**样本饥饿（第三独立阻碍）**：river `facing_raise` 稀（villain 须开火+被加注+响应、限河；FlushTrap
river.srp.facing_raise n=29、river.limped n=12/20000）→ 门在 8000 手标准回归窗**从不点火**（标准 0/11 =
**空洞通过**，非「正确门控守住」）。漏洞本身极罕见：非坚果 flush value_raise **3 次/20000 手**。附带发现：
原漏洞手 `1b3328bc`（−285BB）主漏 = **领打超池**（`post.unopened.value_bet`，本门不碰），scope 太窄。

**判定 = REJECTED**：SHIP 四条——(a) 强分化+方向对齐 **半met**（分开被动弱场、但与 Tag 不分开）；
(b) 靶场向好 **FAIL**（样本饥饿至 ~1 次点火，flush-only GATED 仅 +1802/4 seed）；(c) 标准 0/11 **空洞
通过**；(d) exploit 关 byte-equal PASS。**(a) 受损 + (b) 失败 → 不满足四条全过**。引擎逐字节还原 HEAD
（`git diff HEAD -- src/` 空）。诊断全文 `scratchpad/cand1/DIAGNOSIS.md`。

**处方（正确修法，留后续轮）**：① **§D 摊牌采集（首选）**——记录 villain 河牌**攻击时的摊牌牌力构成**
（攻击 range 含非坚果吗），唯一能分开 FlushTrap（纯坚果攻击）与 Tag（混合价值攻击）；频率/fold/raise 率
都做不到。② **§G 成花纹理维**（`river.<池型>.flush3.facing_raise`）缓解代理风险 #2，但仍需 §D 解 #1
（Tag 在成花板也用坚果攻击）。③ **scope 订正**：真杠杆是**领打超池尺寸**（`post.unopened.value_bet` 非
坚果 Monster），非 `post.facing.value_raise`；且必须 **flush-only**（顺子降级 -EV）。造靶夹具
（FlushTrap + `monster_non_nut`/`nonnut_flush`）保留。

### P0-3 §D 摊牌采集（villain 攻击方摊牌牌力构成）**SHIPPED**（2026-07-06）

候选① 第三次 REJECT 处方 #1 落地：**§D 摊牌采集**——记 villain **河牌攻击时摊牌牌力构成**（攻击
range 含非坚果吗），唯一能分开 FlushTrap（纯坚果攻击）与 Tag（混合价值攻击）；REJECT 坐实的 §B 频率
代理（raise/fold 率）**撞车**（`river.srp.facing_raise` FlushTrap 44.9% ≈ Tag 45.2%）§D 解得开。**本轮
只采集、不接门**（候选① 门重做用 §D 是后续轮）。纯增采集、决策不读 → **sim byte-equal**；**`policy.rs`
零改动**（只动 `opponent.rs` 采集 + `heuristic9_sim.rs` enrich/dump + tests + docs）。照 fold-to-raise
（`fac17a8`）范式。

**采什么**：per-villain，keyed by `<街>.<池型>.attack_showdown`（河牌为主，flop/turn 一并采）。villain
在该街作**进攻方**（bet/raise）**且到摊牌亮牌**时，其牌在 **final board** 上的成手牌力构成——存原始计数
（§0.5 铁律）：`{showdowns, nonnut, nonnut_flush, by_made_class[6]}`，非坚果率查询时现算。

**5 设计决策**：
1. **新字段**（非复用 facing map）：值类型 `ShowdownCounts` ≠ `FacingCounts` → 新
   `OppStats.attack_showdown: BTreeMap<String,ShowdownCounts>`，`#[serde(default, skip_serializing_if)]`
   → 旧 `h9_live_prof*.json` 逐字节兼容。
2. **key** = `<街>.<池型>.attack_showdown`，新 `pub attack_showdown_node_key` = `facing_node_key` 的
   `.attack_showdown` 兄弟（§G 扩展口子，采集/未来 gate 共用同一构造器）。
3. **前缀碰撞：天然无**（比 fold-to-raise 更干净）——§D key 存**独立 map** + **独立 readout**
   （`showdown_readout`）→ **不经 `node_field_prior`**（§D 无摊牌率先验，全原始计数 + 现算率）→ 结构上无从
   碰撞，故**不动 `node_field_prior`**（fold-to-raise 共用 facing map 才需 `ends_with(".facing_raise")`
   前置分支）。
4. **两档非坚果度**：`nonnut`（`monster_non_nut`，含同花+顺子）**被 FlushTrap 成顺非成花板的非坚果顺子
   value_raise 污染**（FlushTrap 仅**成花板**纯坚果建战、成顺板打法同 Tag）；`nonnut_flush`（**flush-only**）
   才是候选① 真杠杆（照 REJECT 处方 #3「必须 flush-only，顺子降级 -EV」）——FlushTrap 从不攻击非坚果同花
   → flush-only ≈0。
5. **enrich（真正工作量）**：`opponent.rs` **不依赖 `features`/`eval`**——sim 用 `features` 现算
   `made_rank`/`nonnut`/`nonnut_flush` 喂 `ShowdownObs`；`observe_showdown(actions, showdowns)` 从 actions
   重建 pot_type + per-villain per-street 攻击标记（同 observe_facing 口径），归牌力到节点。摊牌判定：终局
   board 满 5 张 且 ≥2 座保有 `hole_cards`（弃牌者 apply 时 `hole_cards`→None）。**多街归属**：triple-barrel
   摊牌 → flop/turn/river 三节点各记一次（同 final-board 牌力；river 主用、flop/turn 副）。

**选择偏差（显式）**：`showdowns` 分母只含「攻击被跟到摊牌」的手——被弃的攻击无亮牌 → §D range 是
**摊牌子集**、非全攻击 range（inventory §D 截断偏差）。对候选① 无害：FlushTrap 坚果同花攻击会被 hero
非坚果同花跟到摊牌（正是要度量的 spot）。

**自校验（造靶铁证；FlushTrap vs Tag 隔离攻击极性，2-max villain=p1，20000 手 seed 7）**：

| 靶 p1 | `river.srp.attack_showdown` nnflush (n) | `river.limped` nnflush | `nonnut`（含顺子，被污染） |
|---|---|---|---|
| **FlushTrap**（纯坚果攻击） | **0.0%** (0/142) | **0.0%** (0/72) | 11.3% / 20.8% |
| **Tag**（混合价值攻击） | **6.3%** (12/189) | **20.3%** (15/74) | 14.8% / 37.8% |

**flush-only 决定性分化**：FlushTrap river 攻击摊牌 flush-only 非坚果率 **恰 0%**（`nnflush` 桶两 river
节点全 0——从不攻击非坚果同花）≪ Tag **6.3% / 20.3%**。**这正是 §B 频率代理做不到的**（raise-to-raise
两者 44.9%≈45.2% 撞车）→ §D 处方兑现的铁证。注：`nonnut`（monster，含顺子）被 FlushTrap 成顺板非坚果
顺子攻击污染（11.3% vs 14.8%，弱分化）→ 印证决策 #4，唯 flush-only 给净信号。

**验证门（全过）**：byte-equal 基线（HEAD=7646c2d）vs 改动默认输出（无 `--opp-stats`）**0/30 diff**
（field {mixed,flushtrap,tag,3bpfold,donk_fold} × seats {2,6,9} × exploit {ON,OFF}，--hands 4000 --seed 7）；
`policy.rs` 零改动。单测 `heuristic_p0_3_facing` 新增 6（§D counts/actor、direction、flush-only 隔离顺子、
key/no-field-prior、multistreet、backward-compat）→ 18 全绿；`heuristic9_trace`(10) 绿；fmt/clippy 绿。
**未跑全量 `cargo test`**（遵约束）。

**解锁**：候选① river 极化门重做——现可读 `river.<池型>.attack_showdown` 的 **flush-only 摊牌极性**
（`raw_nonnut_flush_rate`）门控 `post.facing.value_raise` / `post.unopened.value_bet`（REJECT 处方 #3
scope 订正：真杠杆是领打超池尺寸 + flush-only），门控接线留后续消费轮。造靶 FlushTrap/Tag 留作门控验证载荷。
诊断/数据 `scratchpad/p03_showdown_d/DIAGNOSIS.md`。

### P0-3 §D **live 接线**（`shown_cards` → 生产 attack_showdown）**SHIPPED**（2026-07-06）

上轮（`bad0396`）§D **只在 sim 侧喂**——live 生产的 `h9_live_prof*.json` 里 `attack_showdown` 全空 → 候选①
门若接 §D 在 live **永不激活**（死码）。本轮把 live 观测管线接上：driver 捕获摊牌底牌 → advisor 用 `features`
分类坚果度 → 调 `observe_showdown`。**live §D 生产现可用**，候选① 门重做（下一轮）才有意义。**只动 driver +
advisor observe 路径**——sim（`heuristic9_sim.rs`）、`policy.rs`/任何门、§D 采集结构（`opponent.rs`
`ShowdownCounts`/`observe_showdown` 签名）**全零改动**，无 pass-through（`bot.opponents` 已 `pub`，直接调）。

**三处接线**：
1. **driver `observe_record()`（`tools/heuristic9_play.py`）**：observe 载荷增 `board`（= `self.board`，
   由 `community_cards` 事件累积；关键实测——**`hand_result` 不带 board/community_cards（皆 null）**，且
   all-in 后 turn/river 经 `community_cards` 事件补齐至 5 张）+ `shown_cards`（= `hand_result.shown_cards`，
   逐座亮牌底牌 map；**非摊牌手/muck → 不带该字段**）。
2. **advisor `ObserveRecord`（`tools/heuristic9_advisor.rs`）**：`#[serde(default)] board: Vec<String>` +
   `#[serde(default)] shown_cards: BTreeMap<u8, Vec<String>>`（serde 自动把字符串座位键 `"2"`→u8）。
3. **advisor `feed_observe()`**：`observe_hand` 后，`shown_cards` 非空且 `board` 满 5 张 → 每座 `view::parse_card`
   解析 + **`features::made_class/monster_non_nut/nonnut_flush`（与 sim §D enrich 逐字段同函数、同 final board、
   同 nut-ness）** 算 `ShowdownObs` → `bot.opponents.observe_showdown(&observed, &showdowns)`。

**离线验证（无网络，全跑通）**：
- **crafted 端到端**：villain 河牌 bet 亮 `QhJh` on `Kh7h2h5c9d`（非坚果同花）→ `villain river.srp.attack_showdown
  = {showdowns:1, nonnut:1, nonnut_flush:1, by_made_class[Monster]:1}`；hero（仅 call）不入表 → 攻击方归属 +
  serde 座位键解析双证。
- **raw-log replay（金标准，`h9_live_run33b.raw.jsonl`）**：重建 846 手（261 手带 `shown_cards`，30.9%）→
  **17 名真实对手 attack_showdown 填充**（Sparky 55 摊牌、PaD_AI 37、JoaoOracle 33…），river 聚合 nnflush
  3.8%(3/80)。**独立 Python 判定**（纯 flush + nut-ness，不复刻引擎）river 攻击方非坚果同花 = **3 手，与 advisor
  river nnflush 逐一对上**（`allInAmazonia 5d3d`/`PaD_AI 9hQh`/`hero 8hQc`；两手坚果同花正确判 nnf+0）→
  证 live 分类 == sim 分类、整管线真数据跑通。
- **decision byte-equal**：HEAD(`bad0396`) vs 改动 advisor，1586 条真实 live Request 同 args → **响应 md5 逐字节
  相同**（观测路径不碰决策）。
- **backward-compat**：旧格式 observe（无新字段）→ 无 §D、不 crash、`observe_hand` 照常；`board<5` 守门跳过；
  旧 `h9_live_prof.json`（44 名）加载 + round-trip 语义无损。
- 跑 `heuristic_p0_3_facing`(18) / `heuristic9_trace`(10) 全绿；fmt/clippy 绿；debug+release 编译过。**未跑全量
  `cargo test`**（遵约束）。

诊断/数据 `scratchpad/p03_showdown_d_live/DIAGNOSIS.md`。

**live 接线入口（后续轮，本轮不做）**：live `hand_result.shown_cards` = 逐座底牌 map（如
`{"0":["Td","5d"],"2":["8c","9d"]}`），850 手中 262 手（所有摊牌手）都有；driver 当前**未解析**。后续轮：
driver observe 路径解析 `shown_cards` + final board → 用 `features` 现算 `made_rank`/`nonnut`/`nonnut_flush`
→ 喂 `observe_showdown`（同 sim enrich 口径，opponent.rs 采集侧零改动即复用）。schema 详见
`opponent_features_inventory.md` §D 落地记录。

### P0-3 阶段2 第五消费轮 = 候选① river 极化门（**§D 摊牌信号，第四次重做**）：引擎改动**驳回**（2026-07-06）

候选①（成花板非坚果 stack-off）的**第四次**尝试。前三次：①②全局降级死于标准矩阵 11/11（−113.9）；
③（画像门控，`river.srp.facing_raise` §B 频率代理）死于**撞车**（FlushTrap raise 率 44.9% ≈ Tag 45.2%）。
③ REJECT 处方 = **§D 摊牌采集**（`bad0396` 采、`0613410` live 接线，SHIPPED）：§D flush-only 非坚果率**能分开**
纯坚果攻击（FlushTrap river.srp **0.0%** 全 seed 结构性）与混合价值攻击（Tag 3.5%）。本轮用这个**对了的信号**做门。
判定 **REJECTED**——§D 稳态聚合对、但**决策时**跨不过 make-or-break，非失败（照候选①③ / bluff_raise v1 先例）。

**已实现（sim 复核后逐字节还原 HEAD）**：`RiverPolarityGateConfig`（挂 NodesConfig：`node_gated / min_samples /
nonnut_flush_thresh`）+ policy 首次消费 `showdown_readout.raw_nonnut_flush`（**唯一区别** vs float/cbet/bluff_raise
门读 facing/facing_raise 的 `node_readout`）。两漏点：① 领打超池 `post.unopened.value_bet` → 压尺寸
（`suppress_overbet` 绕过 Monster 超池升级、回落常规 value size）；② `post.facing.value_raise` → 降级 call
（直接路由 call 叶、不穿下游 RNG）。多人池保守（全活对手须一致极化）。10 内联单测全绿。

**关 A 标准场 §D rate（2-max p1 villain, 8000 手 seed7, `river.*.attack_showdown` flush-only nnflush%）**：

| 场 | river.srp nnflush%(n) | river.limped nnflush%(n) |
|----|-----------------------|--------------------------|
| **FlushTrap** | **0.0% (66)** | **0.0% (33)** |
| Random | 1.4% (71) | **0.0% (37)** ← limped 撞车 |
| Maniac | 2.8% (178) | 8.6% (35) |
| Tag | 3.5% (85) | 9.7% (31) |
| Station | 6.1% (114) | 14.3% (28) |
| Limpy | 6.5% (46) | 33.3% (21) |

FlushTrap srp 0.0% 全 seed 结构性（0/36..0/69，从不攻击非坚果同花）。**srp 池可分**（0% vs ≥1.4%）、
**limped 池不可分**（松 air-heavy 场成花摊牌稀 → 0% 撞 FlushTrap，纹理盲）。

**决定性证据 = min_samples sweep（thresh=0.01，标准 vs 靶 paired ON/OFF chips Δ）——无干净窗口**：

| min_samples | flushtrap Δ[s7,s11,s23] | mixed6-s7 Δ | mixed2-s7 Δ |
|-------------|-------------------------|-------------|-------------|
| **15** | **+663/+862/+1254** | **−1245** | **−320** |
| 30 | +273/+160/**−291** | 0 | 0 |
| 50 | 0/0/**−291** | 0 | 0 |
| 80 | 0/0/0 | 0 | 0 |

min=15 靶场 3/3 正但标准 **2/11 误伤**（−1245/−320，落 CI95 内但破 0/11 硬门）；min≥30 标准干净但**靶场激活
坍塌 + seed23 转负**（门连靶都伤）。**没有任何 min 同时靶正 3/3 + 标准 0/11。**

**根因两条（纹理盲 §D 的语境失效，正是 §8④/REJECT 预警的失效模式）**：
1. **冷启动累积偏差**：`raw_nonnut_flush` 是累积率，非极化 villain 头 ~15–29 摊牌读 0%（还没亮过非坚果同花）
   → 冷启动窗误判极化。2max-mixed-s7 误伤 villain 是 **Tag**（稳态 3.5%/9.7% 皆超阈值），纯冷启动致误伤 →
   srp-only / 调阈值都治不了。
2. **纹理盲稀释**：率分母含非成花板摊牌 → 松场成花摊牌稀 → 率压到 0%、limped 撞 FlushTrap。
- **min_samples 分不开**：FlushTrap 可信样本窗（~15–30）恰覆盖标准场冷启动误伤窗；抬 min 杀靶场激活。

**靶场增益来源 + 稀疏（诚实）**：3/3 正**全来自领打超池压尺寸**（leak 1）——`--trace-summary` 证 `value_raise
点火` ON==OFF（未降级）→ facing-bet 非坚果同花 river spot **n=1/8000**（value_raise 侧实为死码）。增益
+4~+8 mbb/g 与标准误伤 −2~−8 同量级、edge 边际；live 9-max 每 villain river 攻击摊牌更稀（run33b ~5/villain）
→ 门更慢热。

**byte-equal（PASS，证门正确、REJECT 是 EV 权衡非 bug）**：node_gated=off full-stdout md5 == HEAD 二进制
**4/4 逐字节相同**（mixed-6/flushtrap-2/mixed-9/station-6）；标准 8/11 config ON==OFF Δ=0；0 fallback/0 漂移。

**判定 = REJECTED**：SHIP 四条——(a) 靶场向好 min=15 met 但**破 0/11 硬门**；(b) 标准 0/11 仅在 min≥30 达成、
但此时靶场激活坍塌+转负（互斥）；(c) byte-equal PASS；(d) 激活可行但 value_raise 侧死码、增益边际。
→ **(a)(b) 互斥不可兼得**。引擎逐字节还原 HEAD（`git diff HEAD -- src/` 空）。**处方 = §G 成花纹理维细化 §D**
（`river.<池型>.flush_board.attack_showdown`：只在成花板计摊牌 → 分母不被稀释、率反映「成花板上攻不攻非坚果
同花」→ 分开 FlushTrap 结构性 0% 与松场成花板会亮非坚果、且治冷启动）。这是 §D 采集侧改动，留后续轮
（一轮一改）。造靶 FlushTrap/Tag + 坚果度特征 + 单测（造靶夹具）保留。诊断全文
`scratchpad/cand1_v4/DIAGNOSIS.md`。

### P0-3 阶段2 第六消费轮（srp c-bet **价值下注尺寸** 门）：**CONFIRMED-ROBUST（无反号，零引擎改动）**（2026-07-06）

srp c-bet **频率**门（第三消费轮，戏剧反号 SHIPPED）的**尺寸**姊妹课题：把 hero c-bet 价值注**尺寸**按
binding villain 节点级 fold-to-cbet 连续调整（拟：粘站放大榨价值 / 爱弃缩小诱跟）。杠杆同轴（value_bet.srp
= hero 进攻方价值下注、villain 防守方面对 → 读 flop.srp fold-to-cbet）。**本轮非白纸**：现画像
`ExploitAdjust.value_size_mult` 已对分类 `Station` 放大 1.30x（`ExploitConfig::station_value_size_mult`），
但粗（离散）、单向（只放大）、非节点级。判定 bar = 节点级连续 **beats 画像级 station 1.30x**，非只 beats 平尺寸。

**造靶（复用频率门 wideopen/3bpfold，无新靶）**：flop.srp shrunk_fold = WideOpener 47.5% / ThreebetFolder
79.6%（分化 32.1pp）；`value_bet.srp` 点火 1648/1080 每 8000 手（密度足）。**⚠ 关键前提发现**：两靶
`station_value_size_mult` 1.00↔1.30 toggle **EV 逐字节不变** → 两靶均**不分类 Station**（宽开→高 PFR/AF
≥1.5），现画像乘子对其 = **1.0**（非 1.30）——画像分类看翻前 VPIP/AF，漏「翻后粘 + 翻前激进」对手。

**纪律 A（sweep 反号，决定性）= 无反号**。constant-mode（gate 恒定乘子、仅作用 value_bet.srp 叶）8000 手
2-max --exploit，5 seed 均值（mbb/g）：

| value_size mult | WideOpener（粘 47.5%）| ThreebetFolder（爱弃 79.6%）|
|---|---|---|
| 0.60 | 720.5 | 933.8 |
| 1.00 | 651.0 | 938.2 |
| 1.40 | 868.9 | 956.6 |
| 1.60 | **961.4** | 955.9 |

粘站**清晰要大**（1.60 最优，+310 vs 1.0，单调↑）；爱弃**wash**（933–957，跨度 23 ≪ CI95 ±270，最优 1.2/1.4
仍**大**，**不要小**）。**最优尺寸不随粘度反号**——粘要大、爱弃不反向。本轮核心假设「爱弃→缩小」**证伪**。
与频率门（爱弃清晰高频 / 粘站清晰低频，戏剧反号）根本不同；与候选②（3bp 频率 robust，爱弃 wash）**同型**。

**纪律 B（beats 画像）= shrink 方向 ≈0**。默认反号门锚 `[0.45→1.45, 0.80→0.70]` ON vs OFF（=画像）paired：
WideOpener Δ **+202**（3/3 正）、ThreebetFolder Δ **+8.9**（1/3 负）。**拆解**：WideOpener 的 +202 **全来自门
把粘站映到大尺寸（放大方向）= 与画像 station boost 同向**（画像做的正是「粘→放大」，只是这靶漏被分类）；门
**独有的 shrink（爱弃→小 ~0.71）** sweep 已证劣于放大 → 均值 +8.9、1/3 负 ≈ 0。**本轮唯一新 EV 路径（爱弃
缩小）实测证伪。** 放大那半增益固定大乘子/画像 boost 即可拿，节点级反号门无独有 EV。

**机理**：价值注（非诈唬）尺寸对所有对手「越大越不亏」——对爱弃对手，任何尺寸它都弃弱牌（省的边际弃牌本
就付不多），它不弃的是像样牌、会付更大注 → 大注不劣。故不像频率（诈唬 EV 直接随弃率反号）。一个固定大尺寸
赢两边（正是画像 1.30x 的方向）。

**判定 = CONFIRMED-ROBUST，零引擎改动。** policy/config `git checkout` 还原 HEAD（`9e7aff1`，`grep` 无 gate
残留、引擎逐字节 = HEAD）→ 标准 11-config / byte-equal / 深码门按构造成立（同一二进制）。造靶保留。**引申
（非本轮行动）**：两靶未分类 Station 却强受益于放大（WideOpener +310）= **画像分类覆盖盲区**（漏翻后粘/翻前
激进），补法 = **单向** value-size boost 或放宽 station 分类，是另一 feature（须独立证 beats 画像 + 过标准场含
station 避双重放大 + 节点差异化落 CI），留后续轮。诊断全文 `scratchpad/p03_cbet_value_size/DIAGNOSIS.md`。

### 迭代 backlog（下一步，按优先级）

1. ✅ **已 v0.4 解决**：稀疏占座 compact 重映射。driver 用 `_compact_blinds` 发权威盲座 + 逐动作街
   标签，`view.rs` 按标签分段、按权威盲座定位 → 幻影空座不再扰乱分段/位置（实盘 A/B 坐实，0 回归）。
2. **剥削对纯 nit 场仍欠力**：adjust_cap 已到安全上限 0.12（再高 nit 断崖）。要进一步榨干
   紧场需结构改动——把「模型对手紧（助我弃）」与「利用其高弃率（助我攻）」解耦，因为同一
   equity 既驱动跟注又驱动下注，对手收紧会同时压住我方进攻。非全 nit 的真实混合场剥削已正收益。
3. **equity 遥测统一标度**：翻前 fold 短路返回 vs-random `strength`、翻后返回 vs-range
   equity，`heuristic9_analyze.py` 的「弃了 +equity」标记会混标度误报。统一成 vs-range。
4. **混合/平衡**：`MixingConfig.temperature` 目前只用频率门，可加边界动作 softmax 混合
   降可剥削性。
5. **live 数据闭环**：表有活人时跑满 1000 手 → `heuristic9_analyze.py` 出漏洞报告 → 迭代。
   当前平台活跃度低（实测仅 1 对手），数据攒得慢。

### 如何升级（操作）

```bash
# 1) 离线回归门（改 config/规则后必跑，确认不退化）
cargo build --release --bin heuristic9_sim
./target/release/heuristic9_sim --hands 8000 --seats 9 --seed 7   # 100BB：mbb/g 不退 + 0 fallback/漂移
# 1b) 深码区回归门（码深漂移到 200/400BB 仍须 0 fallback/0 重建漂移；mirror 自检落 CI95 内）
./target/release/heuristic9_sim --hands 8000 --seats 9 --seed 7 --start-bb 200
./target/release/heuristic9_sim --hands 8000 --seats 9 --seed 7 --start-bb 400
./target/release/heuristic9_sim --hands 8000 --seats 9 --seed 7 --start-bb 400 --mirror  # 深码无座位偏差

# 2) live 打 1000 手（需 api_key）
python3 tools/heuristic9_play.py --api-key KEY --num-hands 1000 \
    --advisor-bin target/release/heuristic9_advisor --seats 9 \
    --action-log h9_actions.jsonl --exploit --opp-profile h9_prof.json

# 3) 看数据 → 定位漏洞
python3 tools/heuristic9_analyze.py h9_actions.jsonl

# 4) 调 config（JSON 覆盖默认）或加规则 → 回 1)
./target/release/heuristic9_advisor --config my_cfg.json ...   # 例：收紧 open_early
```
