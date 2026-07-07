# 对手画像特征全集（信息采集清单 + 多维重要度）

> 一句话：把「从 server 事件流可观测的对手信号」一次性盘全，逐条解释它是什么、怎么变成一个决策，
> 并沿多个维度标注重要度。目标是**先把信息采全（采原始计数、不采衍生比率）**，策略后续再挑用哪些。
>
> 定位：本文是**采集侧设计草案 / 参照底稿**，不是实现规范。与既有文档的关系——
> - `src/heuristic/opponent.rs` / `src/training/opponent_profile.rs`：当前只采 VPIP / PFR / 全局翻后 AF，
>   本文是它的超集与升级方向。
> - `docs/opponent_range_ml_design.md`：ML range 层。本文 §D（摊牌真值）正是它的标签来源；
>   本文的 per-node 频率是它「手写 Bayesian tracker 似然」的数据化对象。
> - `docs/scenario_heuristic_design.md`：策略层的场景森林设计——本文 §0.5 的 per-opponent 节点计数表
>   与其场景 key **共享坐标系**（其 §5.2）；加权 range `RangeVec` 消费端走读见 `docs/heuristic9_strategy.md`。
>
> 落地纪律照旧（`docs/heuristic9_design.md` §4、`CLAUDE.md`）：正确性优先；任何特征进**剥削决策**前，
> 必须过 sim A/B 门（mbb/g 不劣），不能拍脑袋接。**本文只管「采什么」，不预设「怎么用」。**

---

## 0. 怎么读这份表：四个评级维度

每个特征标注四项。图例：

| 维度 | 取值 | 含义 |
|---|---|---|
| **EV 剥削价值** | ★★★ / ★★☆ / ★☆☆ | 对「更强剥削策略」的边际贡献。★★★=直接决定一类下注该不该做；★☆☆=锦上添花 |
| **样本效率** | 🟢 快 / 🟡 中 / 🔴 慢 | 收敛所需手数，由**分母大小**决定。🟢 分母≈每手（VPIP 级，百手可信）；🟡 需特定前置局面（3bet/cbet 级，几百手）；🔴 多重条件叠加、分母个位数（river 三重 barrel、分牌面后） |
| **数据来源** | 📗 动作流 / 📙 摊牌真值 / 📕 需补协议 | 📗 靠现有动作序列即可派生（日志已全采；权威源＝`hand_result.actions`，**非** `player_action` 流——后者会漏结束手动作，见 §1）；📙 需 `shown_cards`（仅 ~34% 手）；📕 现有日志缺、需补采（**仅 per-action 决策耗时一项**） |
| **条件必需** | 是 / 否 | 是否**必须**叠加 §G 的结构维度（牌面/位置/底池类型）才有意义。「是」的特征不分维度采就是噪声 |

**样本约束（贯穿全文的硬现实）**：🟡🔴 特征的分母远小于总手数——一旦再叠加牌面/位置维度，单格样本
瞬间掉到个位数。**所以本文所有条件频率的采集，都必须同时存「分母（机会数）」，并配套群体先验做小样本
收缩（见 §H）。** 没有分母和先验，🟡🔴 特征事后无法解释，采了也白采。

> 注：**「样本效率」列是先验估计；实测机会数分布见「实测样本称重」节**（用 7 个 live run 量过，
> 其中两个 a-priori 评级已按实测订正：fold-to-3bet 🟡→🔴、4bet 🔴→🟡）。样本现实也已从「33 个 ≥100
> 手对手」更新为「7 run 实含 62 个」——见该节。

---

## 0.5 核心原则：采原始计数，不采衍生比率

online profiler 应从「几个标量」升级为一张 **per-opponent 节点计数表**：

```
key   = (街, 节点类型, 面对动作, 位置, 底池类型, 牌面桶, sizing 桶)
value = { 机会数 n, 动作直方图, sizing 直方图, 关联摊牌样本指针 }
```

任一比率（VPIP、fold-to-cbet…）都是对这张表的一次查询。好处：**永不丢信息、维度可事后重切、
分母天然带着**。完整手动作序列 + `shown_cards` 另存原样（raw log 已在做）。

---

## 1. 数据可得性底账（现在采不到的只有一样）

`h9_live_run*.raw.jsonl` 已是近乎完整的事件流。逐条对照本文所需字段：

| 需要的原始信号 | 来源事件 | 状态 |
|---|---|---|
| 每动作类型 + 街 + 座位（完整序列） | `hand_result.actions`（权威） | ✅ 已采。⚠ `player_action` 流会漏**结束手的最后一个动作**（尤其 opener 对 3bet 的 fold）→ 重放必须以 `hand_result.actions` 为准，name 从 `player_action`+`table_state` 补（覆盖 100%） |
| **精确下注额 / 投入 delta** | `player_action.amount` / `.contribution_delta` | ✅ 已采（online 被裁） |
| **每动作前后底池** | `player_action.pot` / `.pot_after` | ✅ → sizing-to-pot 可算 |
| **每决策点精确栈深** | `player_action.player_stack_before/after` | ✅ → SPR / 有效栈可算 |
| 逐街牌面 | `community_cards.cards` | ✅ → 牌面质地可算 |
| 位置（button/sb/bb） | `table_state.dealer_seat` / `hand_start` | ✅ |
| **摊牌手牌** | `hand_result.shown_cards` | ✅ ~34% 手 |
| 赢家牌型描述 | `hand_result.winners[].hand_description` | ✅ 额外补一部分强度真值（即使未亮牌） |
| **每动作决策耗时（tank time）** | — | ❌ **`player_action` 无 `ts`，唯一缺口** |

**唯一「现在不补、以后永久丢」的信号 = villain 每动作的决策耗时**（§F 时序 tell 的唯一来源）。
其余全部可从现有日志离线重建。是否补采 per-action 时间戳，是本文唯一需要**现在**拍板的协议问题。

---

## A. 翻前特征（按位置 × 面对局面）

翻前样本最密（每手每人一次机会），是**最快可信**的一层。当前 `counted_pre` 逻辑只记「每人翻前第一次
自愿动作」就早退——**后续的加注链（3bet/4bet）完全不可见**，这是最该先补的盲区。

| 特征 | 定义 | 战略用途 | EV | 样本 | 源 | 条件 |
|---|---|---|---|---|---|---|
| VPIP | 翻前自愿入池率 | range 松紧总闸 | ★★☆ | 🟢 | 📗 | 否 |
| PFR | 翻前加注率 | 主动性；VPIP-PFR gap 判被动/激进 | ★★☆ | 🟢 | 📗 | 否 |
| 按位置 open 率 | 各位置首入加注率（UTG…BTN/SB） | 定位其开池宽度 → 反偷/3bet 决策 | ★★☆ | 🟡 | 📗 | 是 |
| 偷盲率 | CO/BTN/SB 无人入池时的开池率 | 高→我盲位加宽防守/3bet-bluff | ★★★ | 🟡 | 📗 | 是 |
| **3bet 率** | 面对一个 open 再加注 | **连续加注核心**；区分 nit/tag 最有效单指标 | ★★★ | 🟡 | 📗 | 是 |
| **4bet 率** | 面对 3bet 再加注 | 判其 4bet 是否极化 → 我 3bet-bluff 该不该做 | ★★★ | 🟡 | 📗 | 是 |
| 5bet-jam 率 | 面对 4bet 全下 | 深层加注链诚实度 | ★☆☆ | 🔴 | 📗 | 是 |
| fold-to-3bet | 被 3bet 后弃牌率 | **直接定我方 3bet-bluff 盈亏**（见下公式） | ★★★ | 🔴 | 📗 | 是 |
| fold-to-4bet | 被 4bet 后弃牌率 | 定我方 4bet-bluff 盈亏 | ★★☆ | 🔴 | 📗 | 是 |
| cold-call | 面对 open 冷跟（非 3bet） | 判其扁平范围 → squeeze 机会 | ★★☆ | 🟡 | 📗 | 是 |
| squeeze | open+caller 后 3bet | 高侵略性标记 | ★★☆ | 🔴 | 📗 | 是 |
| 盲位防守 | BB defend 率 / SB 3bet 率 | 低→我偷盲加宽；高→收敛偷盲、加厚价值 | ★★★ | 🟡 | 📗 | 是 |
| limp / limp-fold / limp-reraise | 跛入及其后续 | limp-fold 高=极弱鱼，可 iso 猛打；limp-raise=陷阱信号 | ★★☆ | 🟡 | 📗 | 是 |
| iso-raise | 对跛入者的隔离加注率 | 其惩罚 limp 的意愿 | ★☆☆ | 🔴 | 📗 | 是 |
| open sizing 分布 | 开池尺寸（bb）分布 | 尺寸 tell 的翻前入口 | ★☆☆ | 🟡 | 📗 | 否 |
| 3bet sizing | 3bet 尺寸分布 | 小 3bet 常是价值/线性 → 可读 | ★☆☆ | 🔴 | 📗 | 是 |

**剥削公式（贯穿 A/B/C 所有 fold-to-X）**：一个纯诈唬下注额 `s`、底池 `P`，当
`对手弃牌率 > s/(s+P)` 时所有诈唬即时盈利。例：2/3 pot（s=0.67P）需 >40% 弃牌。
所以 **fold-to-3bet / fold-to-cbet / fold-to-barrel 这三类，采到即可直接换算「该不该诈唬」**，
是全清单里 EV 最硬的部分。

---

## B. 翻后当进攻方（街 × 牌面 × 底池类型 × IP/OOP × 人数）

「当我方是上一条街的加注方 / 主动方时，对手怎么持续施压」。**这类特征离开牌面质地维度几乎无意义**
（A 高干燥面 c-bet 90% vs 湿三张连牌 c-bet 30% 是两个世界），故绝大多数「条件必需=是」。

| 特征 | 定义 | 战略用途 | EV | 样本 | 源 | 条件 |
|---|---|---|---|---|---|---|
| flop c-bet | 翻前加注方 flop 持续下注率 | 判其 flop 诚实度 → 我 float/raise/fold | ★★★ | 🟡 | 📗 | 是 |
| double barrel | flop cbet 后 turn 继续开火 | turn 上是否放弃诈唬 → 我 turn 抓诈 | ★★★ | 🟡 | 📗 | 是 |
| triple barrel | turn 后 river 第三枪 | river 诈唬频率（配 §D 摊牌看真值） | ★★☆ | 🔴 | 📗 | 是 |
| give-up | cbet flop 后 turn 放弃（check） | 高=其 cbet 含大量放弃诈唬 → 我 flop 多 call | ★★☆ | 🟡 | 📗 | 是 |
| delayed cbet | 过 flop、turn 才下注 | 判其「过牌≠放弃」→ 别轻易偷 | ★☆☆ | 🔴 | 📗 | 是 |
| probe | 对手过牌后我方位置前的主动下注（其视角） | 其对被让牌局面的利用度 | ★☆☆ | 🔴 | 📗 | 是 |
| donk | 非加注方主动领打 | 稀有动作，常两极（超强/乱打） | ★☆☆ | 🔴 | 📗 | 是 |
| 每街 sizing 桶 | 下注额分小/中/大/overbet | 尺寸→强度映射基座（配 §D 校准） | ★★☆ | 🟡 | 📗 | 是 |
| overbet 率 | 超池下注频率 | 极化信号；station 面前是榨价值利器 | ★★☆ | 🔴 | 📗 | 是 |

---

## C. 翻后当防守方（面对我方 / 他方施压）

「当对手是被打的一方」。这里的 fold-to-X 与 §A 的翻前 fold-to-3bet 同属最硬 EV（见 §A 公式）。

| 特征 | 定义 | 战略用途 | EV | 样本 | 源 | 条件 |
|---|---|---|---|---|---|---|
| fold-to-cbet | 面对 flop 持续下注弃牌率 | **直接定我方 cbet-bluff 盈亏**——最高频剥削点 | ★★★ | 🟡 | 📗 | 是 |
| fold-to-turn barrel | 面对 turn 第二枪弃牌率 | 定 double-barrel bluff 盈亏 | ★★★ | 🟡 | 📗 | 是 |
| fold-to-river barrel | 面对 river 第三枪弃牌率 | 定 triple-barrel bluff 盈亏 | ★★☆ | 🔴 | 📗 | 是 |
| call-down 率 | 连跟 flop+turn+river | 高=跟注站 → 砍诈、加厚薄价值（现有 station 逻辑的精细版） | ★★★ | 🟡 | 📗 | 否 |
| check-raise（逐街） | 过牌后加注频率 | 高=我 cbet 需收敛、多过牌控池；低=我可放心多桶 | ★★☆ | 🔴 | 📗 | 是 |
| raise-cbet | 直接加注我方 cbet | 同上，判其 flop 加注是否含诈 | ★★☆ | 🔴 | 📗 | 是 |
| float | call flop 后夺回主动（turn 领打/加注） | 判其反浮动 → 我 cbet 后 turn 计划 | ★★☆ | 🔴 | 📗 | 是 |
| fold-to-check-raise | 面对被加注弃牌率 | 定我方 check-raise-bluff 盈亏 | ★★☆ | 🔴 | 📗 | 是 |
| fold-to-donk | 面对领打弃牌率 | 定我方 donk-bluff（少用） | ★☆☆ | 🔴 | 📗 | 是 |

---

## D. 摊牌真值（`shown_cards` ~34% + winner 牌型描述）

**全清单里唯一带手牌真值标签的一层。** 稀（摊牌且未 muck 才有），但价值独一份：前面 A–C 的所有频率
都是「从动作猜 range」，**这一层用真牌给那些猜测纠偏**，也是 ML range 的直接标签源。

| 特征 | 定义 | 战略用途 | EV | 样本 | 源 | 条件 |
|---|---|---|---|---|---|---|
| 摊牌手牌强度分布 | 亮出的牌落在 169 类的分布 | 校验 VPIP/AF 推断（如「松但只强牌摊牌」= 假站） | ★★★ | 🔴 | 📙 | 否 |
| **按线还原真实诈唬比** | 特定激进线后亮的牌里空气占比（如 triple-barrel→bluff%） | **把「猜的 bluff 频率」换成真值**，直接定抓诈阈值 | ★★★ | 🔴 | 📙 | 是 |
| sizing↔强度真值校准 | 各 sizing 桶对应的实际摊牌强度 | 给 §E 尺寸 tell 装上真值锚 | ★★☆ | 🔴 | 📙 | 是 |
| 推断 range vs 实际亮牌校准误差 | 模型预测分布与真牌的偏差 | **直接修正模型误设**——ML range 自认头号 EV 风险 | ★★★ | 🔴 | 📙 | 否 |
| WTSD | 参翻后进摊牌的比例 | 判其「爱看牌 vs 爱弃牌」总倾向 | ★★☆ | 🟡 | 📗 | 否 |
| W$SD | 摊牌中赢钱比例 | 低=到摊牌牌太弱（诈唬型/跟注站）| ★★☆ | 🔴 | 📙 | 否 |
| WWSF | 看到 flop 后赢下底池比例 | 综合翻后攻击力 | ★★☆ | 🟡 | 📗 | 否 |

> 采集注意（截断偏差，`opponent_range_ml_design.md` §2.1 已论证）：只有「打到摊牌且亮牌」才有标签，
> 会**系统性高估成手、低估中途弃掉的诈唬**。故 §D 任何频率的分母都是「摊牌手」，不是「所有该线手」，
> 采集时必须显式记这个分母口径，事后才不误用。对弱场剥削此偏差方向偏保守，可接受。
> `winners[].hand_description`（如 "Flush"）在**未亮全牌**时仍给一层粗强度真值，值得一并采。

---

## E. 尺寸 tell（需 `amount`+`pot`，均在日志）

多数弱对手 sizing 与手牌强度**不独立**——这是纯剥削金矿，但**单看尺寸是噪声，必须配 §D 摊牌真值
才能落成「大注=强 / 小注=弱」的映射**。故本层 EV 高低取决于摊牌样本是否够。

| 特征 | 定义 | 战略用途 | EV | 样本 | 源 | 条件 |
|---|---|---|---|---|---|---|
| 每节点 sizing 分布 | 各决策点下注额/底池比的分布 | 尺寸 tell 的原始底座 | ★★☆ | 🟡 | 📗 | 是 |
| sizing↔强度相关性 | 尺寸桶与摊牌强度的相关 | 高相关=可按尺寸读牌，极高 EV | ★★★ | 🔴 | 📙 | 是 |
| polarized vs merged | 大注是否两极（超强+诈）/ 线性 | 定我方面对其大注的 call/fold | ★★☆ | 🔴 | 📙 | 是 |
| min-raise 癖 | 偏好最小加注 | 常见弱鱼 tell（价值/试探两极） | ★☆☆ | 🟡 | 📗 | 是 |
| 尺寸恒定性 | 是否所有线同一尺寸 | 恒定=无 tell（别读）；随强度变=可剥削 | ★★☆ | 🟡 | 📗 | 否 |

---

## F. 序列 / 动态

跨街、跨手的**时间结构**。信息量大但噪声最高，且含唯一的协议缺口（时序 tell）。

| 特征 | 定义 | 战略用途 | EV | 样本 | 源 | 条件 |
|---|---|---|---|---|---|---|
| 逐街 aggression | flop/turn/river 各自的 AF（现在只有一个全局） | 定位「哪条街变诚实」→ 分街抓诈 | ★★☆ | 🟡 | 📗 | 是 |
| 多街线频率 | bet-bet-bet / check-call-call 等整线模式 | 整线 tell，比单点频率更抗混淆 | ★★☆ | 🔴 | 📗 | 是 |
| 倾斜检测 | 大输一手后 VPIP/激进度是否飙 | 抓 tilt 窗口，短时加宽抓诈/价值 | ★★☆ | 🔴 | 📗 | 否 |
| session 内适应 | 前段 vs 后段行为漂移 | 判其是否在调整 → 我方是否需换档 | ★☆☆ | 🔴 | 📗 | 否 |
| **时序 tell** | 每动作决策耗时（tank / 秒下） | 秒 call=弱封顶、长考=边缘决策 | ★★☆ | 🟡 | 📕 | 否 |

> **时序 tell 是全文唯一需要现在决定的采集缺口**：`player_action` 无 `ts`，不补则永久丢。
> 其余 F 项均可从现有日志重建。

---

## G. 结构条件维度（给 A–F 所有特征加正交维度）

G 不是「独立特征」，而是**切分 A–F 的坐标轴**。前面大量特征标了「条件必需=是」，指的就是必须沿这些轴
分桶采集。**这层决定了整套画像的分辨率上限，也是样本被稀释的根源**——每加一个维度，单格分母除以桶数。

| 维度 | 分桶 | 为什么重要 | 采集优先 |
|---|---|---|---|
| 牌面质地 | 干/湿、对子、同花面、高/低、连牌 | cbet/barrel/fold-to-cbet 离开质地几乎无意义 | ★★★（B/C 的前提） |
| 位置 IP/OOP | 有位 / 无位 | 同一人 IP 与 OOP 是两套策略 | ★★★ |
| 底池类型 | limped / SRP / 3BP / 4BP | range 宽窄与承诺度完全不同 | ★★☆ |
| 有效栈深 / SPR | 短/中/深、低/中/高 SPR | 决定承诺阈值与 stack-off 宽度 | ★★☆ |
| 人数 | 单挑池 vs 多人池 | 多人显著收紧、诈唬价值降 | ★☆☆ |

**分维纪律**：维度是乘法稀释样本。建议采集时**存最细粒度的原始计数**（见 §0.5），
但**消费时按样本量自适应回退**——某格分母不足就并桶（湿/干二分 → 单一），靠 §H 的群体先验兜冷启动。
细粒度采集是免费的（只是计数），细粒度**消费**才是噪声源。

---

## H. 元信息 / 稳健性（不是特征，是让特征可用的地基）

小样本现实（33 人 ≥100 手）下，下面四样决定 A–G 是「可用信号」还是「噪声」：

- **样本分母**：每个 per-node 计数必带机会数 `n`。消费端据 `n` 给置信区间 / 决定是否回退 GTO。
- **群体先验（层次收缩）**：先估 field 在每个节点的平均频率，个体频率朝群体先验按 `n` 收缩
  （Beta-Binomial / 经验贝叶斯）。**这是 🟡🔴 特征能早期可用的唯一办法**——冷启动打群体读，样本累积再个体化。
- **recency 衰减**：对 §F 倾斜/适应类，近窗加权（对齐现有 `converge_drift` 近窗思路）。
- **收敛门分特征定**：别复用 VPIP 的 `min_hands=150`。3bet/cbet 分母小，门要按各自机会数单独设。

---

## 实测样本称重与 field 先验（2026-07-02 · 7 run 快照）

> 用 `h9_live_run24–30.raw.jsonl`（去重 **9886 手**）重放，量第 1–2 档特征的**机会数（分母）**在
> **62 个 ≥100 手对手**上的分布。动作源＝`hand_result.actions`（完整权威、含 street），seat→name 由
> `player_action`+`table_state` 补（覆盖 100%）。脚本：`tools/opponent_feature_weigh.py`（口径见其头注，
> `python3 tools/opponent_feature_weigh.py` 可复现）。
>
> 方法学订正（已影响过一次结论）：初版用 `player_action` 事件流重放，漏掉「结束手最后一个动作」
> （尤其 opener 对 3bet 的 fold），使 fold-to-3bet / fold-to-cbet 分母**系统性低估近半**；改用
> `hand_result.actions` 后修正。这也是本文 §1 把权威源标成 `hand_result.actions` 的原因。

**样本量（机会数）分布** — 「ready 裁决」= 以「多数对手 ≥30 机会」为可个体化门槛：

| 特征 | 总机会 | 中位/人 | ≥20 人 | ≥30 人 | ≥50 人 | ready 裁决 |
|---|--:|--:|--:|--:|--:|---|
| **3bet**（面对 open） | 19781 | **242** | 62 | 62 | 58 | ✅ 全员可个体化 |
| 偷盲\*（近似） | 7895 | 94 | 61 | 56 | 47 | ✅（\*位置口径待精确） |
| 盲位防守\*（近似） | 6246 | 72 | 57 | 52 | 42 | ✅（\*同上） |
| **flop cbet** | 3405 | **41** | 45 | 39 | 26 | ✅（再分牌面桶后转 🟡） |
| 4bet（面对 3bet） | 2011 | 24 | 42 | 25 | 14 | 🟡 头部够、长尾靠先验 |
| **fold-to-cbet** | 1584 | 18 | 28 | 19 | 7 | 🟡 约 1/3 对手够 |
| fold-to-3bet | 787 | 11 | 12 | 6 | 1 | 🔴 几乎全靠群体先验 |
| double barrel | 457 | 6 | 2 | 2 | 0 | 🔴 只做群体级 |

VPIP 基线：中位 513 手/人。摊牌真值（§D）：合计亮牌 6112 手，中位 68/人，46 人 ≥30 亮牌。

**场均命中率＝现成的 field 先验（新对手冷启动默认读数，个体样本累积后按 n 收缩偏离）：**

| 特征 | 3bet | fold-to-3bet | 4bet | flop cbet | fold-to-cbet | double barrel |
|---|--:|--:|--:|--:|--:|--:|
| field 均值 | 4.5% | **15.1%** | 7.7% | 33.6% | 45.6% | 53.4% |

读出的群体画像：这批 bot **3bet 很紧（4.5%）、被 3bet 极黏（只弃 15%）、cbet 偏被动（34%）**——
典型「紧-黏」型，剥削方向＝多薄价值、少对其诈唬、翻前可多偷但 3bet-bluff 慎用。
（偷盲 36% / 盲位防守 30% 位置口径近似，仅量级参考。）

**三条结论（直接指导采集与用法）：**

1. **§H 群体先验＝前置刚需，已被数据证实**：EV 最硬的 fold-to-cbet / fold-to-3bet / double barrel
   恰是样本最饥饿的三个（★★★ 却 🟡🔴）。没有「field 均值 + 按 n 收缩」，它们对多数对手无法个体化。
2. **上表命中率就是那组 field 先验**：拿来即用，无需额外训练。
3. **真实样本比预期多一倍**：`h9_live_prof.json` 仅 32 个 ≥100 手对手，而 7 run 实含 **62** 个——
   那个 profile 非本 7 run 累积。**按 run 重累积 profile 可立刻多覆盖约 30 个对手**。

**实操分档（据实测样本，落到「先采什么、怎么用」）：**

| 现在就能按人个体化 | 先 field 先验、头部对手个体化 | 短期只做群体级 |
|---|---|---|
| 3bet、flop cbet、偷盲\* | 4bet、fold-to-cbet | fold-to-3bet、double barrel |

（\*偷盲/盲位防守量级足够，但位置口径尚为近似，数值可信前需先做精确位置解析。）

---

## 汇总：按 EV 排序的采集优先级

| 档 | 特征组 | 为什么排这里 |
|---|---|---|
| **1（最硬）** | fold-to-cbet / fold-to-turn barrel / fold-to-3bet；3bet / 4bet 率；偷盲 & 盲位防守；call-down | 直接换算「诈唬/价值该不该做」（§A 公式）；翻前项样本还快 |
| **2** | flop cbet / double barrel / give-up；逐街 aggression；check-raise | 定「抓诈 vs 弃牌」；需牌面维度但仍纯动作流 |
| **3（真值锚）** | §D 摊牌全组：按线诈唬比、range 校准误差、sizing↔强度真值 | 修模型误设（头号 EV 风险）；不进决策也先采，样本稀但唯一真值 |
| **4** | §E sizing tell、overbet；open/3bet sizing | 潜在高 EV，但依赖 §D 真值锚才稳 |
| **5** | §F 动态：倾斜、适应、时序 tell | live 有真 EV，噪声最大；时序 tell 是唯一协议缺口 |
| **贯穿前置** | §G 结构维度 + §H 分母/群体先验 | 不先有这层，1–5 的条件频率都无法解释。**逻辑上排最前** |

**一句话**：先采**原始 per-node 计数（带分母）+ 完整手序列 + 摊牌**，别采比率；沿 §G 维度存最细粒度、
消费时按样本自适应并桶；配 §H 群体先验做冷启动。这样 A–F 全部特征都变成对一张计数表的查询，
信息永不丢，策略后续想用哪个都能现算，也随时能过 sim A/B 门逐个验 EV。

---

## 附：唯一需现在拍板的事

per-action 决策耗时（§F 时序 tell）是现有日志的唯一缺口——`player_action` 无时间戳。
补采需在 driver 侧给每条 villain 动作打到达时间戳（协议小改）。**不补则该信号永久丢失**，
其余所有特征都可从现有 `h9_live_run*.raw.jsonl` 离线重建，不急。

---

## 落地记录：P0-3 阶段1（采集基建）SHIPPED（2026-07-06）

> 分两阶段，照 Phase A→B 先例。**阶段1 = 纯增采集、决策路径不读 → sim byte-equal**（已落）；
> **阶段2 = 剥削接线（改行为、过 sim 质量门）—— 首个消费轮 float 门 SHIPPED（2026-07-06）**：
> `policy::float_freq_mult` 查 binding villain 在对偶 `(街.池型)` 节点的 `shrunk_fold` 单调门控
> float 频率（fold 越高 → float 越高，避开朴素 breakeven 靠造靶校准锚），解决 float 最优频率随
> 对手翻后粘度**反号**（两反号靶 9/9 ≥ 默认、+46/+51 mbb/g；标准矩阵 0/11 超 CI；exploit 关
> byte-equal）。bluff_raise / c-bet 过度诈唬门（同吃 §C fold 桶）复用同机制、留后续轮。设计全文见
> `scratchpad/p03/DESIGN.md`（分阶段计划 + 阶段2 四门接线方案 + 未决点）；shipped 数据见
> `docs/heuristic9_design.md` §6「P0-3 阶段2」。

**阶段1 采什么（`src/heuristic/opponent.rs`）**：`OppStats` 新增 `facing:
BTreeMap<String, FacingCounts>`（`#[serde(default, skip_serializing_if=is_empty)]` → 旧
`h9_live_prof*.json` 逐字节兼容）。

- **节点 key = `(街.池型)`**：`flop.srp` / `turn.3bp` / `river.srp` / … + `pre.facing_3bet`。
  池型由翻前 raise 数派生，与 hero 侧 `ScenarioKey.pot_type` 逐字同口径（§0.5/§5.2 共享坐标系）。
  止步 (街,池型)、**不铺 §G 牌面/位置/SPR**——实测 fold-to-cbet 中位仅 18/人，再叠维即噪声。
- **`FacingCounts{n,folds,calls,raises}`**（`n=folds+calls+raises`，存原始计数不存比率，§0.5 铁律）：
  - `folds/n` = §C fold-to-cbet / fold-to-turn-barrel / fold-to-river-barrel（villain **作防守方**）
    → 阶段2 喂 float / bluff_raise / cbet 过度诈唬门。
  - `raises/n` = §B raise-cbet/check-raise/river-raise 频率（villain **作进攻方**极性代理）
    → 阶段2 喂候选① 成花板非坚果 stack-off 门（真极性需 §D 摊牌，代理先造靶验）。
  - **信号归属澄清**：float/cbet 吃 §C fold 桶、候选① 吃 §B raise 桶，是**两个不同信号共用一张表**。
- **facing 重建**：`observe_hand` 内新写轻量 villain replay（`observe_facing`），从有序
  `(name, street, action)` 派生，**无需 enrich `ObservedAction`、无协议改动**（二值 fold-to-bet
  不需 amount/seat；name 一手内唯一）。口径与 `tools/opponent_feature_weigh.py` `replay`
  一致（7-run 实测校准）。sim/live 喂入路径均已满足唯一前提「actions 时序排列」。
- **先验 + 收缩**：`node_field_prior`（fold-to-cbet 0.456 / fold-to-3bet 0.151，实测快照；
  turn/river fold 无实测→占位）；`node_readout(name,node,k)` 返回 `(n, raw, shrunk)`，
  `shrunk=(folds+prior·k)/(n+k)`，`k=20` 默认。**决策路径不调用**（阶段1 零行为变化）。
- **可观测**：`heuristic9_sim --opp-stats` dump 每对手每节点 `(n, raw/shrunk fold%, raise%)`。

**验证门（全过）**：
- **byte-equal**：9/6/2-max × `--exploit` × seed 7 × 8000 手，改动前后 **pot/to_call 0.000% 漂移、
  fallback 0、hero 净盈亏/CI 逐字节一致**。现有 `h9_live_prof.json`（44 名）加载通过、旧字段不丢。
- **采集正确性（fixture 方向自证，`--opp-stats` dump，flop.srp fold-to-cbet）**：
  `ThreebetFolder` **81.6%** / `Nit` **80.8%**（爱弃，高）≫ `Station` **54.1%** / `WideOpener`
  **47.0%**（粘跟，低）——方向完全符合预期。收缩数学实测正确（nit n=3 raw 100% → shrunk 52.7%
  向 0.456 先验收缩）。
- 单测 `tests/heuristic_p0_3_facing.rs`（7 测：计数/池型/逐街/fold-to-3bet/方向/收缩数学/后向兼容）
  + `heuristic9`(10) + `heuristic9_trace`(10) + `heuristic_p0_1_action_line`(8) 全绿；fmt / clippy 全绿。

**阶段2 前置未决点（DESIGN.md §6）**：① ~~live driver `observe_record` 现用 `player_action` 流，
按 §1 铁律会漏结束手最后动作~~ **✅ 已完成（2026-07-06，前置 #3）**：`observe_record` 改用
`hand_result.actions`（权威完整、含结束手末动作），缺失时回退旧 `player_action` 流；driver-only 改动，
advisor observe schema（`actions:[{seat,street,action}]`+`names`）不变、Rust 决策路径未碰。
**实测校订（诚实记录，与本节旧「低估近半」表述冲突）**：对本机 raw 日志（run24-30，去重 10484 手）跑
A/B（真实 advisor observe 路径 + weigh.replay 双独立复算），换源后 fold-to-3bet 机会数 1020→1020、
fold-to-cbet 4761→4761、VPIP/PFR 机会数漂移 <0.01%，**分母几乎零恢复**——因这批日志的 `player_action`
消息在 `hand_result` 前基本全达（每 2000 手仅缺 ≤22 条动作）。故「低估近半」在**当前 raw 日志 + 当前
weigh.replay 逻辑上不复现**；该数值疑似源自更早一版 weigh 脚本的重建 bug。换源仍保留：它是**权威源、
更稳健**（免受未来 server/driver 时序变化导致终结动作漏采），且零害（分母等于或略升、name 覆盖实测
100%）。前置 #3 的价值＝正确性/健壮性，而非分母恢复；② turn/river fold 先验须扩测 weigh.py 校准；
③ 候选① raise 极性代理样本饥饿（river raise 稀有），短期近群体级偏坚果假设。

---

## 落地记录：P0-3 阶段2 第二消费轮（bluff_raise 门）**REJECTED**（2026-07-06）

float 门（首个消费轮）SHIPPED 后，尝试对 `post.facing.bluff_raise` 复用同机制（本清单 line 314/326-327
预留「同吃 §C fold 桶」）。走完整闭环后判定 **REJECTED**——**信号归属预判有误、代理不成立**（诚实修订）：

- **float vs bluff_raise 的角色分野**：float 杠杆 = villain **作防守方**的 fold-to-cbet（§C），门读 §C
  fold 桶 = **same-role**（成立）。bluff_raise 杠杆 = villain **作进攻方被加注**的 **fold-to-RAISE**
  （属 §B「villain 进攻方极性」一类，**非** §C 防守方 fold），门却只能读 §C fold-to-cbet = **跨角色代理**。
- **采集侧硬约束**：`observe_facing`（`opponent.rs:270`）被 re-raise 即 break + 开火者不入 facing
  → **fold-to-raise 从未进 `facing` 表**。门物理上读不到 bluff_raise 的真信号。
- **造靶实证**（`DonkFold`/`DonkStick`，隔离变量 = fold-to-raise）：两靶 villain fold-to-cbet
  **flop.srp 逐字节相同 33.8%**，但 bluff_raise 最优频率**相反**（DonkFold 高频最优 +135~+266 mbb/g、
  DonkStick 低频最优 −289~−300，3 seed paired 一致）。门读 fold-to-cbet 返回相同乘子 → 无法剥削反号。
  「作防守方弃牌倾向」与「作进攻方被加注弃牌倾向」是**独立两轴**。
- **正确修法（列 §B/§H 扩展）**：先扩 `observe_facing` 采 **fold-to-raise**（进攻方被 re-raise 的首次
  响应，当前 break 掉），再照 float same-role 门控 bluff_raise。**在此之前，bluff_raise 不做画像门控**，
  维持固定 `bluff_raise_freq_mult=0.10`（sweep 证对 sticky/常见场近最优）。
- 造靶夹具 `DonkFold`/`DonkStick` 保留待 fold-to-raise 采集落地。诊断/数据见
  `scratchpad/p03_bluff_raise/DIAGNOSIS.md` + `docs/heuristic9_design.md` §6。

**信号归属订正表**（覆盖 DESIGN.md §2 旧表 bluff_raise 行；采集侧现状随 fold-to-raise 落地更新）：

| 消费门 | 真杠杆 | villain 角色 | 清单归属 | 采集侧现状 |
|---|---|---|---|---|
| float 频率 | fold-to-cbet | **防守方** | §C fold 桶 | ✅ 已采（same-role，SHIPPED）|
| **bluff_raise 频率** | **fold-to-raise** | **进攻方被加注** | **§B**（非 §C）| ✅ 已采（`*.facing_raise`）→ same-role 门控 **SHIPPED**（2026-07-06，双向门，读 `facing_raise_node_key`）|
| **cbet 过度诈唬（srp）** | fold-to-cbet | 防守方 | §C fold 桶 | ✅ 已采（same-role，**SHIPPED**）|
| **value_bet 尺寸（srp，单向放大）** | fold-to-cbet（仅门/回退用；ship = **平放大**与读数无关） | 防守方 | §C fold 桶 | ✅ 已采（same-role）→ **平放大 A SHIPPED**（2026-07-07，`CbetValueSizeGateConfig` 默认 flat ×1.5、subsume 画像 station 1.30x；A/B 对比证节点微分不占优含 nit，见下）|
| **候选① river 极化门（value_raise 降级 / 领打压尺寸）** | **摊牌 range 极性（flush-only）** | **进攻方（下注/加注）** | **§D 摊牌**（§B raise 桶仅代理） | ✅ **§D 已采+live 接线**（`*.attack_showdown.nonnut_flush`，SHIPPED）→ **§D 门重做（第四次）REJECTED**（2026-07-06，纹理盲 §D 决策时冷启动/稀释、靶正与标准 0/11 互斥，处方 §G 成花纹理维，见下） |

> **bluff_raise 门 SHIPPED（重做 = 上轮 REJECT 的正解）**：上轮门读 fold-to-cbet（跨角色）两靶逐字节
> 相同 33.8%、无法区分反号 → REJECT；本轮门读 `facing_raise`（same-role）实测 DonkFold 86.7% vs DonkStick
> 3.3%（分化 83.4pp、方向与最优频率对齐）→ 代理有效。双向门（乘子 [0.0,2.0]，爱弃升频 / 粘站降频）、
> 门控全池型。靶改善 donk_fold 3/3 +145.3 / donk_stick 2/3 +7.6（两反号靶皆 ≥ OFF）、标准 0/11 超 CI、
> exploit 关 byte-equal 全过。详见 `docs/heuristic9_design.md` §6 + `scratchpad/p03_bluff_raise2/DIAGNOSIS.md`。

> **候选① river 极化门 REJECTED（第三次尝试，画像门控版；2026-07-06）**：非坚果 Monster 面对河牌下注、
> 用 villain 河牌极性代理门控 `post.facing.value_raise` 降级为 call。实测（20000 手 2-max，villain=p1，
> `river.srp.facing_raise` k=20）：**① fold-to-raise 死信号**——FlushTrap villain raw fold-to-raise **0%**
> = station = tag = nit（bluff_raise 门那条轴在此无分化）。**② raise-to-raise 判被动弱场、但与 Tag 撞车**
> ——FlushTrap **44.9%** vs Tag **45.2%**（station 0.6/limpy 1.1/nit 3.4/maniac 9.6）；FlushTrap<Tag →
> **无阈值只打靶不打 reg**，而非坚果 value_raise 对 Tag 是 +EV（降级 -EV）。raise-aggression ≠ nut-polarity
> = **代理风险 #1 致命**。**③ 纹理无关**（代理风险 #2）：44.9% 主由非成花板 Tag 式攻击贡献 → 分开的是
> 通用攻击性、非成花板极性。**④ 样本饥饿**：river `facing_raise` 稀（n=12–29/20000），门在 8000 手标准
> 窗从不点火（标准 11-config 0/11 = **空洞通过**）；flush-only GATED 20000 手仅 +1802（1/4 seed@200bb）。
> 引擎逐字节还原 HEAD。**处方 = §D 摊牌采集**（villain 河牌攻击时摊牌 range 是否含非坚果，唯一分开
> FlushTrap 纯坚果攻击 vs Tag 混合价值攻击）± §G 成花纹理维。详见 `docs/heuristic9_design.md` §6。

> **候选① river 极化门 REJECTED（第四次尝试，§D 摊牌信号；2026-07-06）**：③ 的处方（§D 摊牌）落地做门——
> 读 binding villain `river.<池型>.attack_showdown` 的 `raw_nonnut_flush`（< thresh 且 showdowns≥min → 极化）
> → 降级 hero 非坚果同花攻击（领打超池压尺寸 + value_raise 降级 call）。**§D 稳态聚合确实对**（FlushTrap
> river.srp flush-only **0.0% 全 seed 结构性** ≪ Tag 3.5% / Station 6.1% / Maniac 2.8%），但**决策时失效**：
> **① 冷启动**——`raw_nonnut_flush` 累积率，非极化 villain 头 ~15–29 摊牌读 0%（还没亮非坚果同花）→ 误判极化
> （实测 2max-mixed-s7 误伤者是 **Tag**，稳态 3.5%/9.7% 皆超阈值、纯冷启动）；**② 纹理盲稀释**——率分母含
> 非成花板摊牌 → 松 air-heavy 场（Random）成花摊牌稀、limped 0.0% 撞 FlushTrap。**min_samples sweep 无干净窗口**：
> min=15 靶 3/3 正(+663/+862/+1254 chips)但标准 **2/11 误伤**(−1245/−320，破 0/11 硬门)；min≥30 标准干净但**靶激活
> 坍塌+seed23 转负**（互斥）。byte-equal PASS（node_gated=off == HEAD 二进制 4/4）。增益全来自领打超池压尺寸
> （value_raise 侧 spot n=1/8000 死码），+4~+8 mbb/g 与标准误伤同量级、edge 边际。引擎逐字节还原 HEAD。
> **处方 = §G 成花纹理维细化 §D**（`river.<池型>.flush_board.attack_showdown`：只在成花板计摊牌 → 治稀释+冷启动）。
> 详见 `docs/heuristic9_design.md` §6 + `scratchpad/cand1_v4/DIAGNOSIS.md`。

---

## 落地记录：P0-3 采集侧扩展 fold-to-raise（villain 进攻方被加注）SHIPPED（2026-07-06）

bluff_raise REJECT 记录里的「正确修法」落地：**先扩采集 fold-to-raise，再门控**。本轮 = 采集，
门控留后续消费轮。纯增采集、**决策路径不读 → sim byte-equal**（Phase-1 纪律，引擎决策零改动）。

**采什么（`src/heuristic/opponent.rs`）**：`observe_facing` 新增翻后 fold-to-raise 循环——把翻前
`pre.facing_3bet`（opener 面对站住 3bet 的首次响应）**镜像到翻后每街**：街内**开火者**（首个 raiser =
领打/c-bet 方，§B「villain 作进攻方」）面对站住 re-raise（街内恰好 2 个加注）的**首次响应**。既有
fold-to-cbet 循环在防守方 re-raise 处 `break` 且开火者不入 facing → fold-to-raise 从未进表；本轮独立
循环补之（不改既有循环 → fold-to-cbet 采集逐字节不变）。

- **节点 key = `<街>.<池型>.facing_raise`**（`flop.srp.facing_raise` / `turn.3bp.facing_raise` / …）。
  新 `pub facing_raise_node_key(street, pot_type)` = `facing_node_key` 的 `.facing_raise` 兄弟（§G 扩展
  口子：复用点分基座 + 缀语义后缀），与采集共用**同一构造器**（照 float 门用 `facing_node_key` 先例，
  未来 hero 侧 bluff_raise 门直接复用）。存入**同一** `facing: BTreeMap`（新 key，**不加结构字段**）→
  旧 `h9_live_prof*.json` serde 逐字节兼容（`facing` 本就 `#[serde(default, skip_serializing_if)]`）。
- **`node_field_prior` 前缀碰撞（必踩陷阱，已处理）**：`flop.srp.facing_raise` 以 `flop.` 起始 → 会被
  `starts_with("flop.")` 抢先匹配到 fold-to-cbet 先验 0.456（**错**，那是防守方 fold）。加
  `ends_with(".facing_raise")` **专用前置分支**（照 §H/`node_field_prior` doc「长/专用先判」）。fold-to-raise
  **无实测**（`opponent_feature_weigh.py` 无对应 replay）→ 保守占位 **fold=0.30**（进攻方被加注弃牌率
  通常低于面对首注，因下注方有范围；**占位待实测校准**，别照抄 0.456）；raise 先验沿用 0.05。
- **收缩 + 极性**：复用 `shrunk_fold_rate`（Beta-Binomial）；`raises` 桶 = 开火者被加注后 re-raise
  （4bet/3bet-back）= 更高极性信号，一并采/dump。**止步首次响应**（`recorded` 守卫，照 pre.facing_3bet）。
- **可观测**：`heuristic9_sim --opp-stats` per-node dump 已自动含 `*.facing_raise`（读 `facing.keys()`），
  另加 **fold-to-raise 跨对手聚合行**（`n/folds/calls/raises/raw_fold%`）作方向自校验载荷。

**验证门（全过）**：
- **byte-equal**：基线（commit 4893739）vs 改动默认输出（无 `--opp-stats`）**0/15 diff**
  （field {mixed,3bpfold,flushtrap,3bp,donk_fold} × seats {2,6,9}，--exploit --hands 4000 --seed 7）；
  pot/to_call 漂移 0.000%、fallback 0。`policy.rs` 零改动（grep 证只查 `facing_node_key`=基座节点）。
- **采集方向（造靶铁证；无 weigh.py 参照 → DonkFold/DonkStick 隔离 fold-to-raise）**：两靶防守方
  fold-to-cbet + donk-lead 规则逐字节相同，唯一分叉 = 自己 lead 被加注后响应。8000 手 seed7：
  DonkFold `flop.srp.facing_raise` fold **93.5%**（聚合 86.7%）≫ DonkStick **0.0%**（聚合 0.0%）；
  **n 两靶相同（flop.srp 154）**——同规则 → 机会数相同，只响应不同（folds 320 vs calls 449）= 隔离变量。
- 单测 `tests/heuristic_p0_3_facing.rs` 新增 5（counts/actor、direction、prior-no-collision、
  stops-at-first、backward-compat-roundtrip）→ 12 全绿；`heuristic9_trace`(10) 绿；fmt/clippy 绿。
  **未跑全量 `cargo test`**（遵约束）。

**解锁**：bluff_raise 门（真杠杆 fold-to-raise）+ 候选① river 极化门现有 same-role 真信号可读
（`facing_raise_node_key` 查询、照 float same-role 门控），门控接线留后续消费轮；`DonkFold`/`DonkStick`
造靶留作门控验证载荷。诊断/数据 `scratchpad/p03_fold_to_raise/DIAGNOSIS.md` + `heuristic9_design.md` §6。

---

## 落地记录：P0-3 §D 摊牌采集（villain 攻击方摊牌牌力构成）**SHIPPED**（2026-07-06）

候选① 第三次 REJECT 处方 #1 落地：**§D 摊牌采集**——记 villain 河牌**攻击时摊牌牌力构成**（攻击
range 含非坚果同花吗），唯一能分开 FlushTrap（纯坚果攻击）与 Tag（混合价值攻击）。REJECT 坐实的 §B
频率代理**撞车**（`river.srp.facing_raise` FlushTrap 44.9% ≈ Tag 45.2%）§D 解得开。**本轮只采集、不接门**
（候选① 门重做用 §D 是后续轮）。纯增采集、**决策路径不读 → sim byte-equal**（Phase-1 纪律，`policy.rs`
零改动）。照 fold-to-raise（`fac17a8`）范式。

**采什么（`src/heuristic/opponent.rs`）**：新 `observe_showdown(actions, showdowns)` —— 为每个亮牌 villain，
把其 **final board** 成手牌力归到「该 villain 在各街作**进攻方**（bet/raise）」的 `<街>.<池型>.attack_showdown`
节点。存原始计数（§0.5 铁律）`ShowdownCounts{showdowns, nonnut, nonnut_flush, by_made_class[6]}`，非坚果率
查询时现算。

- **新字段（非复用 facing）**：值类型 `ShowdownCounts` ≠ `FacingCounts` → 新 `OppStats.attack_showdown:
  BTreeMap<String,ShowdownCounts>`，`#[serde(default, skip_serializing_if)]` → 旧 `h9_live_prof*.json` 逐字节
  兼容。新 `pub attack_showdown_node_key(street,pot_type)` = `facing_node_key` 的 `.attack_showdown` 兄弟
  （§G 扩展口子，采集/未来 gate 共用同一构造器）。
- **前缀碰撞：天然无**（比 fold-to-raise 更干净）——§D key 存**独立 map** + **独立 readout**
  （`showdown_readout`）→ **不经 `node_field_prior`**（§D 无摊牌率先验，全原始计数 + 现算率）→ 结构上无从
  碰撞，故 **`node_field_prior` 零改动**（fold-to-raise 共用 `facing` map 才需 `ends_with(".facing_raise")`
  前置分支）。
- **两档非坚果度（关键，§D/§G 交叉）**：`nonnut`（`monster_non_nut`，含同花+顺子）**被 FlushTrap 成顺非成花
  板的非坚果顺子 value_raise 污染**（FlushTrap 仅**成花板**纯坚果建战、成顺板打法同 Tag）；`nonnut_flush`
  （**flush-only**）才是候选① 真杠杆（照 REJECT 处方 #3「必须 flush-only，顺子降级 -EV」）——FlushTrap 从不
  攻击非坚果同花 → flush-only ≈0。
- **enrich = 真正工作量**：`opponent.rs` **不依赖 `features`/`eval`**——sim 用 `features` 现算
  `made_rank`/`nonnut`/`nonnut_flush` 喂 `ShowdownObs`。摊牌判定：终局 board 满 5 张 且 ≥2 座保有
  `hole_cards`（弃牌者 apply 时 `hole_cards`→None）。**多街归属**：triple-barrel 摊牌 → flop/turn/river 三
  节点各记一次（同 final-board 牌力；river 主用、flop/turn 副）。
- **可观测**：`heuristic9_sim --opp-stats` 新增 §D per-node dump（`showdowns/nonnut(nn%)/nnflush(nnf%)/by_made`）
  + **river `*.attack_showdown` 跨对手聚合行**（`nonnut`+`nnflush`）作方向自校验载荷（2-max 聚合含
  hero(p0)+villain(p1)，纯 villain 信号看 per-opponent p1 行）。

**选择偏差（显式）**：`showdowns` 分母只含「攻击被跟到摊牌」的手——被弃的攻击无亮牌 → §D range 是
**摊牌子集**、非全攻击 range（§D 截断偏差）。对候选① 无害：FlushTrap 坚果同花攻击会被 hero 非坚果同花
跟到摊牌（正是要度量的 spot）。

**验证门（全过）**：
- **自校验（造靶铁证；FlushTrap vs Tag 隔离攻击极性，2-max villain=p1，20000 手 seed 7）**：FlushTrap
  `river.srp.attack_showdown` **flush-only 非坚果率 0.0%**（0/142；river.limped 0/72）≪ Tag **6.3%**（12/189；
  river.limped 20.3% = 15/74）。**这正是 §B raise 频率代理做不到的**（44.9%≈45.2% 撞车）→ §D 处方兑现铁证。
  `nonnut`（含顺子）被污染（FlushTrap 11.3% vs Tag 14.8%，弱分化）→ 印证 flush-only 才是净信号。
- **byte-equal**：基线（HEAD=7646c2d）vs 改动默认输出（无 `--opp-stats`）**0/30 diff**（field
  {mixed,flushtrap,tag,3bpfold,donk_fold} × seats {2,6,9} × exploit {ON,OFF}，--hands 4000 --seed 7）；
  `policy.rs` 零改动。
- 单测 `tests/heuristic_p0_3_facing.rs` 新增 6（§D counts/actor、direction、flush-only 隔离顺子、
  key/no-field-prior、multistreet、backward-compat）→ 18 全绿；`heuristic9_trace`(10) 绿；fmt/clippy 绿。
  **未跑全量 `cargo test`**（遵约束）。

**解锁**：候选① river 极化门重做——现可读 `river.<池型>.attack_showdown` 的 **flush-only 摊牌极性**
（`raw_nonnut_flush_rate`）门控（REJECT 处方 #3 scope 订正：真杠杆 = 领打超池尺寸 + flush-only），门控接线
留后续消费轮。诊断/数据 `scratchpad/p03_showdown_d/DIAGNOSIS.md` + `heuristic9_design.md` §6。

### live 接线入口 **已接线 SHIPPED**（2026-07-06）：`hand_result.shown_cards` → live §D 采集

上轮（`bad0396`）§D **只在 sim 侧喂**（`heuristic9_sim.rs` enrich），live 生产的 `h9_live_prof*.json`
里 `attack_showdown` 全空 → 候选① 门若接 §D 在 live 永不激活（死码）。本轮把 live 观测管线接上，**live 生产
现可用**，与 sim 逐字段同口径。**只动 driver + advisor observe 路径**（sim/policy/门/§D 采集结构 `opponent.rs`
全零改动，无 pass-through：`bot.opponents` 已 `pub`）。

**三处接线**：
1. **driver `observe_record()`（`tools/heuristic9_play.py`）**：observe 载荷增 `board`（终局 5 张公共牌 =
   `self.board`，由 `community_cards` 事件累积，含 all-in 后 runout 补齐）+ `shown_cards`（`hand_result.shown_cards`，
   逐座亮牌底牌 map；**非摊牌手 / muck → 不带该字段**，advisor 天然无 §D）。`_hand_result` 传 `msg.get("shown_cards")`
   （照 `result_actions` 同款）。
2. **advisor `ObserveRecord`（`tools/heuristic9_advisor.rs`）**：加 `#[serde(default)] board: Vec<String>` +
   `#[serde(default)] shown_cards: BTreeMap<u8, Vec<String>>`（serde 自动把字符串座位键 `"2"` 解析为 u8）。
3. **advisor `feed_observe()`**：`observe_hand` 后，若 `shown_cards` 非空且 `board` 解析满 5 张——对每个
   `(seat, cards)` 用 `view::parse_card` 解析 + **`features::made_class/monster_non_nut/nonnut_flush`**
   （**与 sim §D 逐字段同口径**）算 `ShowdownObs{name: names[seat], nonnut, nonnut_flush, made_rank}` →
   `bot.opponents.observe_showdown(&observed, &showdowns)`。`observe_showdown` 内部按每街 attack + 攻击方
   专属归属，非攻击者/非摊牌者不入表。

- **schema**：逐座底牌 map，key = 座位号字符串、value = 两张牌串数组，如
  `{"0":["Td","5d"],"2":["8c","9d"]}`。`hand_result` **不带 `board`/`community_cards`**（皆 null）→ board 走
  driver 累积 `self.board`（all-in 后 turn/river 经 `community_cards` 事件补齐至 5 张，已实测）。
- **覆盖**：仅摊牌且未 muck 才有。**截断偏差同 sim**：只有到摊牌亮牌的攻击有标签。

**离线验证（无网络，全跑通）**：
- **crafted 端到端**：villain 河牌 bet + 亮非坚果同花（`QhJh` on `Kh7h2h5c9d`）→ dump profile
  `villain river.srp.attack_showdown = {showdowns:1, nonnut:1, nonnut_flush:1, by_made_class:[...,1]}`；
  hero（仅 call）不入表。serde 字符串座位键→u8 解析对。
- **raw-log replay（金标准，`h9_live_run33b.raw.jsonl` 24718 行）**：重建 846 手 observe 记录（含 261 手
  ≈30.9% 带 `shown_cards`）→ 喂 advisor → **17 名真实对手 `attack_showdown` 填充**（Sparky 55 摊牌、PaD_AI 37、
  JoaoOracle 33…）；合计 showdowns=339、river 聚合 nnflush 3.8%（3/80）。**独立 Python 判定**（不复刻引擎、纯
  flush + nut-ness 检查）river 攻击方非坚果同花 = **3 手，与 advisor river nnflush 逐一对上**：`allInAmazonia`
  `5d3d` on `8dQd4h4d3c`、`PaD_AI` `9hQh` on `AsQd2h8h7h`、hero `8hQc` on `Jc7c6s8cAc`；两手坚果同花
  （`Ah`/`Ac` 高）正确判 nnf+0。
- **decision byte-equal**：HEAD(`bad0396`) vs 改动 advisor，1586 条真实 live Request 同 args →
  **响应 md5 逐字节相同**（观测路径改动不碰任何决策）。
- **backward-compat**：旧格式 observe（无 `board`/`shown_cards`）→ 无 §D、不 crash、`observe_hand` 照常记；
  `board<5` → §D 守门跳过；旧 `h9_live_prof.json`（44 名）在新 advisor 加载 + round-trip 语义无损。

诊断/数据：`scratchpad/p03_showdown_d_live/DIAGNOSIS.md` + `heuristic9_design.md` §6。

---

## 落地记录：P0-3 阶段2 第三消费轮（srp cbet 过度诈唬门）**SHIPPED**（2026-07-06）

订正表末行「cbet 过度诈唬」兑现为**首个跨角色代理有效 + 反号成立的 same-role 消费门**（继 float 后
第二个 shipped 引擎门）。`post.unopened.cbet_bluff.srp`（hero 作 PFR 进攻方 c-bet 空气、villain 防守）
的 srp 频率由 `policy::cbet_bluff_freq_mult` 按 binding villain 在对偶 `(街.srp)` 节点的 `shrunk_fold`
单调门控（乘子锚 `[0.50→0.15, 0.75→1.0]`，独立重标定；上限 1.0 = 只降不升）。

- **纪律 B（代理有效，此轮成立）**：门读 flop.srp fold-to-cbet 与真杠杆 fold-to-cbet **同轴**
  （same-role）。造靶两靶（WideOpener 粘 47.9% / ThreebetFolder 弃 79.6%，§C fold 桶）**强分化 31.7pp**，
  且最优频率方向**随该轴对齐**（高 fold → 高最优频率）→ 门**能**剥削（区别于 bluff_raise 的解耦）。
- **纪律 A（反号成立）**：sweep（8000 手 2-max，只扫 `nodes.cbet.srp`）粘站单调↓（低频最优）、爱弃
  单调↑（高频最优），3 seed paired 一致。**srp 反号（像 float）**；候选②已证 **3bp robust（不反号）**
  → **仅门控 srp、3bp 保 0.30/0.20 不动**（池型正是分野）。
- **sim 回归门全过**：靶字段 wideopen ON−OFF **3/3 +42.3**、3bpfold **2/3 +12.9**；标准 11-config
  **0/11 超 CI95、均值 +28.6**（粘站 Station/Limper 场受益于 srp 空气降频）；exploit 关 6/6 逐字节 =
  HEAD、gate_off fallback 3/3 逐字节 = HEAD；深码 200/400BB 0 fallback/0 漂移。
- 复用阶段1 §C fold 桶 + `facing_node_key`/`pot_type_name` 同口径（零采集侧改动，未碰 opponent.rs）。
  造靶复用 float 门的 `wideopen`/`3bpfold`（无新靶）。数据/判定见 `scratchpad/p03_cbet_bluff/DIAGNOSIS.md`
  + `docs/heuristic9_design.md` §6。
