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
