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

### 迭代 backlog（下一步，按优先级）

1. **稀疏占座 compact 重映射**（driver，对 6-max 短桌 + 9-max 稀疏都重要）：v1 满环假设，
   非连续物理座会多模拟「幻影座」扭曲位置/人数。主线 `openpoker_play.py` 已有 `dealt_seats`/
   `stream:state` 占座推断方案可移植。
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
