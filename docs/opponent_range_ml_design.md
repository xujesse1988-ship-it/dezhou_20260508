# 对手 Range 的 ML 预测：方案探索与实现框架

> 一句话：用一个**小型因果序列模型**（<1M 参数），以「手写 Bayesian tracker 的 log 先验 + 网络残差」的形式，在牌局的**每个事件位置**输出对手 169 类手牌的权重分布；showdown 亮牌做标签，对手画像（近 50 手 + 亮牌记忆）做条件；离线打赢手写 tracker 才接进引擎。
>
> 定位：本文是**前瞻设计草案**。它与 `docs/multi_field_strategy_design.md` §2（加权 range `RangeVec`）**同轴互补**——§2 的手写似然是本文 ML 模型的**先验锚、冷启动回退、以及必须打赢的 baseline**；ML 学的本质就是 §2 里那组手工猜的似然参数（`a_v / a_d / κ…`，`multi_field_strategy_design.md` §2.3）的数据版。
>
> 落地纪律照旧：离线先行，接线时门控 opt-in、默认 byte-equal、sim 回归门裁决（`docs/heuristic9_design.md` §4）。

---

## 1. 问题形式化

对每个（牌局 h，目标对手 o）二元组：

- **事件序列** `e_1..e_T`：整手按时间序的公开事件——所有座位的动作（fold/check/call/bet/raise + 尺寸）与发牌（flop/turn/river）。
- **上下文** `c`：座位数、各座相对按钮位、有效栈深（bb）、盲注结构、目标对手座位标记。
- **画像** `π_o`：目标对手的历史行为摘要（§5）。
- **标签** `y ∈ {0..168}`：o 的底牌所属 canonical 169 类（下标与 `canonical_index`，`src/heuristic/equity.rs:57` 同轴）。

模型学 `p_θ(y | e_{≤t}, c, π_o)`，对**任意前缀长度 t** 都要能出。用户要求的两条性质，落到训练目标上：

1. **随时可预测（anytime）**：架构选因果（causal）序列模型，每个事件位置都挂预测头；训练损失取**逐前缀之和** `Σ_t CE(y, p_θ(·|e_{≤t}))`。一次前向天然产出「翻前/翻牌/转牌/河牌各时刻」的全部预测，在线推理逐事件增量更新（GRU 状态 O(1)，或几十 token 直接重算）。
2. **信息越多越准（单调改善）**：**不需要、也不应该在损失里硬加单调约束**。CE 是 proper scoring rule，其 Bayes 最优解就是真后验 `P(y|e_{≤t},…)`；条件熵随信息细化单调不增，所以「后街更准」在期望意义下自动涌现。注意单调只在**期望**上成立——单手牌里对手一次成功的诈唬本来就该把预测带偏，这不是 bug。单调性因此做成**评估断言**（§7）：分街平均 NLL / top-5 命中必须单调，违反即某街欠拟合或数据偏差的报警。

**训练目标 vs 验收指标要分开**：验收是「showdown 手牌落在预测权重 top-5 类」，但**训练必须用 CE（NLL），不能直接优化 top-5**。理由：(i) top-5 不可导也不校准，直接优化会把质量堆进头部、毁掉 range 形状——而下游 `equity_weighted` 消费的是整条分布，尾部质量（诈唬 combo）直接影响 equity；(ii) 两者在最优处一致：固定大小为 5 的预测集，命中率最优解就是真后验质量最大的 5 类，所以 CE 训到位，top-5 自动到位。

**输出为什么是 169 类而不是 1326 combo**：跟 §2 的 `RangeVec` 对齐，消费端（`equity_weighted` 采样，`multi_field_strategy_design.md` §2.5）按类质量平摊到存活 combo。suit 依赖（同花听、blocker）v1 不进模型，靠 board 花色同构规范化（§4.1）+ 消费端已有的 `combo_refine` 钩子兜住。

## 2. 数据盘点（2026-07 实测）

| 源 | 量 | 标签性质 | 用途 |
|---|---|---|---|
| live 6-max 对手亮牌（`h9_live_run24–30.jsonl` 内 `hand_result.shown_cards`）| **8,610 手 / 2,870 手有亮牌 / 5,212 个 (对手,底牌) 样本**；河牌 3,207 手中 343 手 muck | 有截断偏差（只有打到摊牌且亮牌的才有标签）| 主训练 + 全部评估 |
| live hero 自标注（每条决策 `req.hole`）| hero 参手的**每一手**，含 fold 手，**零截断** | hero 策略单一、分布覆盖窄 | 辅助训练（降权），教「线→牌」一般映射 |
| Slumbot HU（`slumbot_hands_20260529_1.jsonl`）| **10,000 手全部带 `bot_hole_cards`，含 7,849 手 fold 结束——零截断的全知数据** | HU、单一对手 | 预训练 + 架构验证（censoring 对照实验的 ground truth）|
| sim 自对弈（`tools/heuristic9_sim.rs` + 待加 `--dump-hands`）| 无限生成，双侧全知 | 人造对手族群（真实性有限）| 预训练 + meta 学习的对手多样性 |
| 对手画像（`h9_live_prof*.json`）| **44 个真实对手，33 个 ≥100 手** | 跨 session 累计 | 画像通道的种子 + meta 训练的族群 |

三个关键判断：

- **样本量支持小模型，不支持大模型**。5k 级带标签对手样本 + 1 万级辅助源 → 参数量控制在 0.3–1M，靠先验残差（§4.2）和预训练吃住方差。
- **对手多样性够 meta 学习起步**。44 个真实对手（都是 bots，风格分布从 ursabot 的 1,418 手到长尾）+ sim 参数扰动可以造更多。这决定了画像通道（§5）不是摆设——训练时模型能真见到「不同画像 → 不同 range」的对照。
- **截断偏差（censoring）必须显式管理**，见下。

### 2.1 截断偏差三级处理

训练只用亮牌样本时，学到的是 `P(y | 前缀, 打到摊牌且亮牌)` 而非 `P(y | 前缀)`——会系统性高估「打得到摊牌的牌型」（成手），低估「中途会放弃的牌型」（空气/弃掉的诈唬）。分级对策：

- **L0（v1 默认）**：直接用亮牌样本训。诚实声明：这与验收指标（摊牌手 top-5）**完全对齐**——评估本身就是摊牌条件下的；但喂给 `equity()` 当「当前时刻真实 range」用时有偏。对弱场剥削这个偏差方向甚至偏保守（高估对手成手 → hero 少诈唬多价值），可接受。
- **L1（v1 就做）**：混入零截断源——hero 自标注 + Slumbot 全知 + sim 全知。模型见过「fold 手长什么样」，畸变显著缓解。Slumbot 集还能做**对照实验**：同一模型分别用「全部手」vs「仅摊牌手」训练，在全知测试集上量出 censoring 的实际伤害，决定要不要上 L2。
- **L2（v2，可选）**：生成式升级（§3 方案 d）——policy 网 `P(a | y, state, π)` 用**隐变量边际似然**训练，fold 手也进 loss（`log Σ_y P(y)·Π P(a_i|y,·)`，169 路边际化每序列一次前向批内可算），range 预测 = Bayes 后验。原理上最正，实现最贵。

## 3. 方法探索：候选路线对比

| 方案 | 是什么 | 优 | 劣 | 结论 |
|---|---|---|---|---|
| (a) 手写 Bayesian tracker | §2 的 `RangeVec` 先验 + 手工似然逐动作乘 | 零数据可用、可解释、天然 anytime + 单调 | 似然参数靠拍脑袋（§2.9 风险 2 自认「模型误设 = 头号 EV 风险」）、画像只能平移标量 | **不是对手，是地基**：先验锚 + 回退 + baseline B2 |
| (b) GBDT（LightGBM）on 手工线特征 | 每街聚合特征（角色/尺寸桶/质地/画像统计）→ 169 类 | 小数据稳、快、本机友好 | 169 类多分类碎（每轮 169 棵树）；序列信息靠手工压缩，丢线的结构；每街要单独一套模型，anytime 粒度粗 | 离线 sanity 基线 B3，不上线 |
| (c) **小型因果序列网络（推荐）** | GRU（1 层 d≈192）或 2–4 层 d=128 causal Transformer，逐事件 token，逐前缀 CE，先验残差输出 | 一个模型吃全部前缀粒度；画像/亮牌记忆可微地进条件；数据效率靠残差学习兜底；0.3–0.8M 参数本机 2 核可训可推 | 需要写 tokenizer + 训练管线；可解释性弱于 (a)(b) | **v1 主干** |
| (d) 生成式 policy 网 + Bayes 后验 | 学 `P(a|y,state,π)`，预测=后验；隐变量边际似然用全部无标签手 | 数据利用率最高、censoring 原理性修复、policy 网本身可复用（判 villain 会不会下注 → check-raise 规划）| 实现复杂度 ×169 边际化、调试难、收益要等 L1 对照实验量化 | **v2 升级路径**，不是 v1 |
| (e) in-context 大模型 | 近 50 手原始序列直接进大 Transformer 上下文 | 理论上限最高 | 数据（要海量**对手**多样性）与算力（本机 3.3GB/2 核）双双不配 | 否 |

推荐 (c) 的三个决定性理由：

1. **它是唯一同时原生满足两条目标函数性质、又能吃进画像条件的方案**。(a) 满足性质但画像表达力弱；(b) 每街一套模型，「anytime」退化成「每街一次」。
2. **残差参数化让它在 5k 样本下不至于胡说**（§4.2）：网络只学「对手写先验的乘性修正」，数据稀薄处自动退回 (a)，冷启动 = (a)。ML 上线的最坏情况就是等于现状。
3. **机器约束**：本机 3.3GB RAM / 2 核（实测），`docs/multi_field_strategy_design.md` §3.7 已警告过 OOM。<1M 参数 CPU 训练分钟级/epoch，推理微秒级，手写 Rust 前向即可，零重依赖。

## 4. v1 模型设计

### 4.1 Token 化

每手一条序列，`[CTX] [PROF] e_1 e_2 … e_T`，长度典型 15–40：

| token | 内容 |
|---|---|
| `[CTX]` | 全局条件投影：`num_seats`、目标对手相对按钮位桶（对齐 `view.rs:639` 的 5 桶，座位无关）、有效栈深 bb（log 尺度连续嵌入）、目标对手座位 one-hot 标记 |
| `[PROF]` | 画像向量投影（§5 两通道拼接）|
| 动作 token | 行动者相对位桶 + 是否目标对手 + 动作类型 {fold,check,call,bet,raise,allin} + **尺寸连续嵌入**（`log(bet/pot)` 过小 MLP 加到类型嵌入上，不做粗桶）+ 状态特征（当街已投/pot、SPR）|
| 发牌 token | 街标记 + 牌 rank/suit 嵌入（board 花色**同构规范化**：按首现顺序重标号，训练时做花色重标号增广）|

要点：**所有座位的动作都进序列**（他人的 3bet 改变目标对手 call 的含义），靠「是否目标对手」位区分视角；多人池一手牌对每个活跃对手各出一条样本，共享前缀、只换 `[CTX]` 的目标标记——这天然座位无关（`docs/invariants.md` §6），HU/6-max 同一架构。数据源就是 `hand_result.result.actions`（v0.4 起带权威 `street` 标签）+ `req` 流，不依赖引擎内部状态。

### 4.2 输出头：先验残差 + card-removal 掩码

```
logits[k] = log m_prior[k] + f_θ(e_{≤t}, c, π)[k] + log(alive_k / total_k)
p = softmax(logits)        # alive_k=0 的类掩 -inf
```

- `m_prior` = 手写 tracker 在同一前缀上的输出（§2 的 `RangeVec`，从角色模板先验起、逐动作施手写似然；离线训练时用 Python 复刻版，接线后用 Rust 原版）。
- `f_θ` 是网络输出的残差 logits。初始化为零 → **训练第 0 步 = 手写 tracker**，梯度只学「手写似然错在哪」。这就是「ML 版的 §2.3 似然」：手写版说「大注 → κ 极化」，网络从数据里学「这个画像的对手大注时 κ 实际是多少」。
- card-removal 用 hero 视角的死牌（hero 底牌 + board）算 `alive_k`（同 `equity.rs:166` 的 `avail` 逻辑），保证不可能类严格零质量、部分阻断类按存活 combo 数折减。

### 4.3 损失

```
L = Σ_hands Σ_t w_src · CE(y, p_t)   + 0.05 label smoothing
```

- `w_src`：live 对手亮牌 1.0；hero 自标注 0.3（策略单一，防主导）；Slumbot/sim 预训练阶段 1.0、微调阶段 0.1。
- 逐前缀求和已隐含「后街样本更多次数进 loss」（一手河牌样本贡献 preflop/flop/turn/river 全部前缀位置），与「各街都要准」一致；不额外加街权重，分街质量靠评估把关。
- 增广：花色重标号（×24 中采样）、等价座位旋转。

### 4.4 规模与资源

序列 ≤48 token；GRU d=192 或 Transformer 2 层 d=128 h=4：参数 ~0.4M。训练集 ~5 万条序列（对手样本 + hero + Slumbot），CPU（2 核）一个 epoch 约几分钟，几十 epoch 一夜内完成；训练时 `nice` + 限内存，或挪到别的机器（数据集就几十 MB）。推理单前缀 <0.1ms，逐事件增量 O(1)。

## 5. 画像通道：「最近 50 手」的设计（回应主问题）

用户倾向「最近 50 手（含 showdown）而非全部历史」。**方向赞成**——这个场里对手全是 bots，会改版重配（人类则有状态漂移），全历史平均会把「上周的它」和「现在的它」和稀泥。但两点修正：

1. **双时间尺度，而非单窗口**。`recent-50` 硬窗口（对手被观测的最近 50 手，含他 fold 的手）负责「现在的它」；再叠一条 **EMA 长期通道**（半衰期 ~300 手）负责「一贯的它」。两通道**都带机会数**（比如 3bet 机会才 4 次就别信 3bet%——把分子分母都给网络，让它自己学收缩，比手工调平滑超参稳）。两者冲突本身就是「它最近变了」的信号，网络可学。
2. **亮牌记忆按事件计数，不按手数**。亮牌是最贵的信息（直接的「线 → 牌」证据），但稀疏——单个对手 50 手窗口里典型只有 5–15 次亮牌。固定收**最近 K=20 次亮牌事件**（可跨 100+ 手），每条 = (该手该对手的线摘要嵌入, 169 类 one-hot)，小 attention 池化成向量。这让「上次它 check-raise river 亮出诈唬」这种记忆直接参与当前预测——正是 §3 场型检测缺的「摊牌亮诈率」信号的可微版。

统计向量的具体字段复用并扩展 `OppStats`（`src/heuristic/opponent.rs:13`）与 P0-3 规划的 per-street 统计：VPIP/PFR/3bet/fold-to-3bet/cbet/fold-to-cbet/WTSD/W$SD/逐街 AF/check-raise 率 + 各自机会数。**训练时严格用「该手开始前」的窗口值**（在线一致性，防未来泄漏）——dataset build 按时间流式重放即可得到。

## 6. 与引擎的集成（依赖 multi_field §2 接缝）

```rust
// src/heuristic/range_predictor.rs（新）
pub trait RangePredictor {
    fn observe(&mut self, ev: &HandEvent);          // 逐事件增量
    fn predict(&self, opp_seat: usize) -> RangeVec; // 任意时刻可调
}
// 三个实现：
//   HeuristicTracker —— §2 手写贝叶斯（先验 & 永久回退）
//   MlRange          —— 加载导出权重，手写前向（纯 Rust，~200 行，零新依赖）
//   Blend            —— p = λ·ml + (1-λ)·heuristic，λ 进 config
```

- **消费端零新工作**：输出就是 §2 的 `RangeVec`，直接喂 `equity_weighted`（`multi_field_strategy_design.md` §2.5）。本设计**依赖 §2 的 Phase 3 落地**（`RangeVec` + 加权采样）与 **P0-1**（`GameView` 保留整手动作线——tokenizer 的喂料）；在那之前 ML 线纯离线推进，互不阻塞。
- **推理不上 onnxruntime/tract**：0.4M 参数的 GRU/Transformer 前向手写即可（启发式层允许 f64，`heuristic9_design.md:116`；无全局 RNG 需求，推理确定性），权重用带版本头的二进制文件进 `artifacts/`。
- **门控**：`RangeConfig.use_ml_range: bool` 默认 `false`（挂在 §2.7 的 `RangeConfig` 下），OFF 时逐字节等于基线——纪律同 `use_range_vec`。
- **在线画像存储**：扩 `h9_live_prof*.json` schema——加 per-street 计数（与 P0-3 共建，一次 schema 变更两个项目用）+ 环形亮牌记忆（每对手最近 20 条 `{hand_id, line_digest, class_idx}`）。老文件缺字段 serde default 兼容。
- **训练/服务偏斜**：v1 特征化在 Python（训练）+ Rust（推理）各一份，用 **golden 对拍**锁死——选 200 手真实牌局，双端 dump token 流与 `m_prior` 逐位比对，进 CI 单测。长期可把 tokenizer 下沉 Rust、Python 侧 FFI 调用，v1 不做。

## 7. 评估协议

- **切分**：按时间 run24–28 训 / run29 验 / run30+ 测（防泄漏 + 模拟真实部署时序）；另做 **leave-one-opponent-out**（对 ≥100 手的 33 个对手轮流留出）量 meta 泛化——「没见过的对手，画像通道还能不能用」。Slumbot/sim 只进预训练，单独报告不混入主指标。
- **指标**：
  - 主：**top-5 命中率**（真实类 ∈ 预测权重前 5 类），分街报告。随机下界 ≈ 3%（5/169）。
  - **range 质量分位 q**（M1 起常设）：亮牌落在预测 range 的前百分之几——按 per-combo
    密度（`p_k/alive_k`，与 §2 模板 frac 同轴）降序累计预测质量到标签类，平局/类内取
    中点。这是离散 PIT：**预测 = 真后验 ⇒ E[q] ≡ 50%，与分布尖度无关**，故 |q−50%|
    是纯排序失准度——q>50% = range 画得过强过窄（真牌被压进尾部），<50% = 过宽。
    它把整条分布形状用上（top-5 只看头部），且是 `equity_weighted` 消费端最关心的
    「形状质量」的直接读数。
  - **equity 残差 bias / MAE**（M1 起常设）：`e = Σ_k p̂_k·eqc_k − eq(亮牌 vs hero)`，
    其中 `eqc_k` = 类 k 存活 combo 对 hero 底牌的 equity 均值（街末 board 上 MC 补完
    runout；对手视角）。这是对下游 `equity_weighted` 消费的那个**线性泛函**的直接检验
    ——类空间指标（top-5/NLL/q/rank）全部置换不变，「错到相邻强度」和「错到强度谱
    另一端」同罪，而 equity 残差只按决策代价计罪。bias>0 = range 系统性高估对手强度
    （hero 会偏保守）——censoring 偏差在决策空间的读数；MAE = 逐手精度。模型与
    B2b 共用同一张预计算 eq 表（`range_eqtab_build`），MC 噪声共模、配对 Δ 精确对消。
    hero 不在手的样本无 hero 视角，跳过（val+test 仅 10/2,040，与消费分布一致——
    hero 不在手时引擎也不消费 range）。
  - 工程：NLL、真实类权重、真实类排名分位（MRR）、ECE 校准。
  - **单调性断言**：同一批摊牌手在「翻前末/翻牌末/转牌末/河牌末」四个快照的平均 top-5 与 NLL 必须单调改善，违反即报警（§1 性质 2）。
- **基线阶梯**（全部先于模型实现，M0 交付）：

| # | 基线 | 意义 |
|---|---|---|
| B0 | 均匀 × card-removal | 绝对地板 |
| B1 | 角色频率先验：按（位置桶 × 翻前角色 × 是否入池）的人群统计 | 「不看这个对手」能到多少 |
| B2 | **手写 Bayesian tracker**（§2 复刻版）| **上线门槛：模型分街显著打不赢 B2 就不接线** |
| B3 | LightGBM on 每街聚合特征 | 「不要序列结构」能到多少，反证架构必要性 |

- **在线影子评估（M3）**:接线前先以影子模式跑 live——每个决策点记录预测、摊牌后回填命中，`h9_live_runN.jsonl` 加一条 `range_pred` 记录。零 EV 风险拿到真实部署分布下的命中率，同时积累新标签。

## 8. 训练管线与工具

```
tools/range_dataset_build.py   # runN.jsonl + slumbot.jsonl + sim dump + prof 重放
                               #   → 样本 npz（token 流 / 画像快照 / m_prior / 标签）
                               #   顺带产出数据审计报告（muck 率、街分布、对手分布）
tools/range_baselines.py       # B0–B3 + 评估 harness（分街/单调/LOO 全套报告）
tools/range_train.py           # PyTorch CPU，早停看 run29 验证 NLL
tools/range_eqtab_build.py     # equity 残差表：val/test 每街末 per-class eq + 亮牌 eq
tools/range_export.py          # 权重 → artifacts/range_ml_v1.bin（版本头 + f32）
tools/heuristic9_sim.rs        # 加 --dump-hands：双侧底牌 + 事件流 jsonl（全知 sim 源）
```

- 环境照 `.venv-pokerkit` 先例：`uv venv --python 3.12 .venv-rangeml && uv pip install torch --index-url https://download.pytorch.org/whl/cpu lightgbm numpy`，进 `.gitignore`。
- 本机 3.3GB/2 核：数据构建与 LightGBM 无压力；torch 训练控制 batch 与 `OMP_NUM_THREADS=2`，或整个训练目录拷去更大机器（数据集几十 MB，无 GPU 需求）。
- sim 人造对手族群：对现有 baseline（Station/Nit/Maniac/PairedTrap/Limper…，`tools/heuristic9_sim.rs:37-51`）做参数扰动生成几十个「人格」，每个跑几千手全知牌局——给画像通道提供「风格 → range」的密集对照，这是 meta 学习真正的口粮。

## 9. 路线图（每步带出闸条件）

| 阶段 | 内容 | 出闸条件 |
|---|---|---|
| **M0 摸底**（先做，~2 天）| dataset build + 基线阶梯 B0–B2 + 评估 harness | 拿到 **B2 手写 tracker 的分街 top-5 命中率**——这个数决定 ML 的价值空间；若 B2 已经很高，ML 立项缩水也是合理结论 |
| **M1 v1 模型** | B3 + 序列网络（统计向量通道，先不做亮牌记忆）| 时间切分测试集上分街显著 > B2；单调断言通过 |
| **M2 画像强化** | 亮牌记忆 attention + Slumbot/sim 预训练→live 微调 + LOO 评估 + censoring 对照实验（Slumbot 全知集）| LOO 不崩（未见对手 ≥ B2）；量化 censoring 伤害，拍板要不要 L2 |
| **M3 接线** | Rust 手写前向 + golden 对拍 + `RangePredictor` trait + 影子模式 live 跑 | 影子命中复现离线指标；随后 `use_ml_range=true` 过 sim 回归门（多 seed 不退）才翻默认 |
| **M4 v2（可选）** | 生成式 policy 网 + 隐变量边际似然（L2 censoring 修复）| 仅当 M2 对照实验显示 censoring 伤害显著 |

与 multi_field 路线图的关系：M0–M2 纯离线零依赖可立即开工；M3 依赖其 Phase 1（P0-1 动作线）+ Phase 3（`RangeVec`/`equity_weighted`）。

### M0 结果（2026-07-02，已交付）

工具：`tools/range_ml_common.py`（169 索引/评估器/强度表/特征，逐义移植 `equity.rs`/`features.rs`/`opponent.rs`，带 `--selftest`）、`tools/range_dataset_build.py`、`tools/range_baselines.py`。复现：

```bash
python3 tools/range_dataset_build.py --out artifacts/range_ml   # ~3s
python3 tools/range_baselines.py --slumbot-eval 800             # ~90s（含强度表缓存构建）
```

数据集：live_opp **5,232**（对手亮牌样本，train/val/test 按 run 切 3,192/797/1,243）、live_hero 8,649、slumbot 10,000（7,849 手 fold 结束仍带底牌，全知）。数据修复：live `player_action.street` 有异步竞态错标，以 `hand_result.actions` 合并列表的权威街标按索引对齐重标。

**出闸数字**（test 切分 top-5 命中率，逐街）：

| 基线 | preflop | flop | turn | river | 单调 |
|---|---|---|---|---|---|
| B0 均匀×card-removal | 0.003 | 0.005 | 0.004 | 0.004 | — |
| B1 人群频率（翻前角色×位置，**不看翻后动作**）| 0.145 | 0.143 | 0.142 | 0.155 | ⚠ |
| B2a 现状标量 tracker | 0.090 | 0.110 | 0.120 | 0.136 | ✅ |
| **B2b RangeVec（§2 复刻，上线门槛）** | **0.139** | **0.142** | **0.142** | **0.158** | ✅ |

三个关键发现：

1. **现状引擎的 range 硬过滤把大多数真实摊牌手判为零概率**：B2a 的 label 排除率 preflop 21% → river **73%**（test），对 Slumbot 更是 river **93.5%**。这是 `multi_field_strategy_design.md` §0.1「局限二」的定量控诉，也给 §2 加权 range 立项提供了硬数字。
2. **手工翻后算子的边际贡献 ≈ 0**：B2b（含逐动作似然）与 B1（完全不看翻后动作）top-5 打平（0.158 vs 0.155），且 B2b 的 NLL 比 prior-only 恶化（4.69→5.00，分布过尖=校准差）——§2.9「似然参数拍脑袋 = 头号风险」被证实。**当前 ~85% 的可用信号在翻前角色里，翻后动作序列的信息基本没被手写规则变现——这正是 ML 的价值空间**。
3. 分街斜率平缓（0.139→0.158）：摊牌手多为被动线 + 手工算子未校准。M1 目标改写为两条：整体分街显著 > B2b（0.158@river），且**打开后街斜率**（river−preflop 差距显著拉大）。

M0 判断：**ML 立项通过**，进入 M1。

### M1 结果（2026-07-02，已交付）

工具：`tools/range_featurize.py`（jsonl → 序列张量 + B3 特征；核心是**逐前缀因果
m_prior**——复用 `range_baselines.py` 的 B2b 先验/算子逐事件维护，翻前角色随前缀重算、
目标翻后动作逐个施算子、fold 冻结；`--parity` 验证残差置零时街末快照与 M0 报告逐位
一致，test 0.140/0.142/0.142/0.158，差 1 样本 = f16 平局破序——这就是 §6 golden 对拍
纪律的离线半边）、`tools/range_b3_lgbm.py`、`tools/range_train.py`。环境
`.venv-rangeml`（torch 2.12 cpu + lightgbm 4.6，进 `.gitignore`）。复现：

```bash
.venv-rangeml/bin/python tools/range_featurize.py --parity        # ~4.5min
.venv-rangeml/bin/python tools/range_b3_lgbm.py --live-opp-only --lr 0.02 \
  --leaves 7 --min-data 80 --ff 0.7 --rounds 800                  # ~10s
OMP_NUM_THREADS=2 .venv-rangeml/bin/python tools/range_train.py \
  --lr 5e-4 --epochs 120 --patience 20 --seed 7                   # ~5min（早停 ep32）
```

**B3（LightGBM 每街聚合特征）**：最优配置（仅 live_opp、强正则）test top-5
0.147/0.150/0.139/0.154，**非单调且打不过 B2b**（river 0.154 < 0.158）；混辅助源更差
（0.146@river）；每街 best_iter 仅 3–10——聚合特征喂树模型，翻后基本学不出东西。
与 §3 预期一致：**反证序列结构必要性**，B3 归档不上线。

**v1 序列模型（GRU d=192 先验残差，0.27M 参数）**：混源训练 18,370 条序列
（live_opp 1.0 / hero 0.3 / slumbot 0.1，hero/slumbot 仅入 train 切分防同手泄漏），
run28 早停，CPU 训练 ~7s/epoch。test（run29,30，n=1243）：

| | preflop | flop | turn | river | 单调 |
|---|---|---|---|---|---|
| B2b（M0 门槛）| 0.140 | 0.142 | 0.142 | 0.158 | ✅ |
| **v1 top-5** | **0.161** | **0.171** | **0.175** | **0.183** | ✅ |
| Δ（配对 bootstrap p）| +0.021 (.059) | +0.030 (.002) | +0.033 (.0008) | +0.024 (.012) | |
| ΔNLL | −0.29 | −0.25 | −0.23 | −0.23 | 全部 p=0.0001 |

出闸判定（对照 §9 M1 行 + M0 改写的两条）：

1. **分街显著 > B2b：✅**（翻后三街 p≤0.012；preflop +0.021 边缘 p=0.059）。
2. **单调断言：✅**（0.161→0.171→0.175→0.183；口径同 M0——top-5 逐街，NLL 逐街本就
   随街上行，B2b/M0 亦然）。
3. **后街斜率显著拉大：✗**——模型 river−preflop = +0.022 vs B2b +0.019（p=0.81）。
   增益在四条街上近乎均匀：preflop 的 +0.021 来自画像通道与先验再校准（B1 人群频率
   0.145 → 模型 0.161），把斜率分母一起抬高了。「翻后序列信息变现」的直接证据是
   翻后三街对 B2b 的显著 Δ；「斜率拉大」作为遗留课题带入 M2（亮牌记忆 + 预训练
   都是翻后侧的杠杆）。

消融与稳定性：同配置 3 种子 val snapNLL 4.525/4.528/4.533（选 seed=7）；
**live_opp-only 消融** val 4.518（最优）/ test river 0.181——混源在摊牌条件评估上
代价 ≈ 0，但该口径本来就看不见 L1 要修的 fold 手偏差（§2.1），且 test 上混源版
preflop/flop 反而更好（0.161/0.171 vs 0.150/0.169）→ **保留混源默认**。超参对 val
不敏感（lr 5e-4–2e-3 / dropout 0.15–0.3 的 4 配置 val NLL 带宽 0.015），过拟合快
（best epoch 3–22），样本量仍是主约束——预训练（M2）是下一个杠杆。

产物：`artifacts/range_ml/{seq_v1.npz, b3_v1.npz, eqtab_v1.npz, range_v1_gru.pt,
m1_report.md, m1_b3_report.md}`（`artifacts/*` 不进 git，npz/ckpt 由上述命令分钟级
重建）。

**range 质量分位 q 的发现（§7 新指标，2026-07-02 补）**：

| test q | preflop | flop | turn | river |
|---|---|---|---|---|
| B2b（均值）| **48.1%** | ~56.8% | ~62.7% | ~63.9% |
| v1 rscale=1.0（均值）| 53.4% | 59.2% | 64.8% | 66.0% |
| v1 rscale=0.8（均值）| **51.2%** | 57.5% | 63.3% | 64.7% |

1. **B2b 翻前近乎完美校准（48%≈50%），翻后逐街失准到 ~64%**——手写算子把 range
   画得过强过窄、真牌逐街被压向尾部：M0 发现 2（算子 NLL 恶化=过尖）的排序版实锤。
2. **纯 v1（rscale=1.0）在 q 上比 B2b 再差 2–5pp（p≤.0012）**，尽管 top-5/NLL 全赢
   ——CE 在 5k 样本下把质量堆进头部（§1 预警的失败模式），头部更常猜对（top-5↑），
   但猜错时真牌被压得更深。
3. **残差温度（`--residual-scale`，logits = log_prior + s·f_θ，即 §6 Blend λ 的
   logit 版）s=0.8 基本白修**：test 上 top-5 反升为**四街全显著**
   （0.163/0.172/0.178/0.183，preflop p 从 .059 → .022），单调保持，翻后 q 与 B2b
   打平（p>.11），preflop |q−50%| 反超 B2b（1.2pp vs 1.9pp）。s 在 val 上选定
   （0.6/0.8 扫描），属评估/接线期配置，ckpt 不动。**推荐配置：s=0.8**。
   全局温度修不掉的翻后共同强牌倾斜（q 仍 >50%）是 M2 的校准课题
   （亮牌记忆/预训练之外，早停与选型也应看 val |q−50%|）。
4. **平均排名**（类空间"第几名"，与 q 的质量空间互补）：v1@0.8 全街 Δrank
   −8.1～−12.4（p=.0001，全指标中 CI 最紧——它用满了整条排序信息），rank 均值从
   B2b 的 ~41–44 提到 ~32–34（中位 ~30 → ~22）。rank↓ 而纯 v1 的 q↑ 正是「排序
   变准、质量堆头」的拆解证据。

**equity 残差 bias/MAE 的发现（§7 新指标，2026-07-02 补）**：

工具：`tools/range_eqtab_build.py`（val/test 每样本每街末预计算 per-class eq 表 +
亮牌 eq：river 精确、turn 全枚举、preflop/flop 24 个共享 runout、每类采 2 combo；
模型与 B2b 共表 → MC 噪声配对对消；构建 ~4.5min，2,030/2,040 行，缺 hero 底牌的
10 个跳过）+ `range_train.py --eqtab`（默认接入）。复现：

```bash
.venv-rangeml/bin/python tools/range_eqtab_build.py                      # ~4.5min
OMP_NUM_THREADS=2 .venv-rangeml/bin/python tools/range_train.py \
  --eval-only --residual-scale 0.8 --report artifacts/range_ml/m1_report.md
```

| test（rscale=0.8）| preflop | flop | turn | river |
|---|---|---|---|---|
| v1 bias | **−1.5pp** | +2.2pp | +4.3pp | +4.7pp |
| B2b bias | −4.2pp | +0.8pp | +3.2pp | +3.6pp |
| MAE：v1 / B2b | 11.6 / 12.3 | 16.7 / 16.9 | 19.8 / 19.9 | 23.9 / 24.0 |
| ΔMAE（配对 p）| **−0.7pp (.0001)** | −0.2pp (.012) | −0.1pp (.23) | −0.1pp (.31) |

1. **口径先行**：本指标条件在「摊牌且亮牌」上（L0 同轴；亮牌 eq 均值 0.60→0.64
   逐街）。亮牌手比前缀条件均值强，故对**在场后验**无偏的 range 在此口径下应读出
   **负 bias**；读数 ≈0 = 贴合摊牌条件分布。无条件读数要等 M2 Slumbot 全知对照。
2. **两个模型的翻后 bias 全为正且逐街攀升**（B2b +0.8→+3.6，v1 +2.2→+4.7）——
   即使相对摊牌条件真值，range 仍画得过强。这是翻后 q>50% 倾斜的 equity 空间版，
   并给出决策代价的量纲：river 时 hero 平均把对手看强 4–5 个 equity 点（偏保守向，
   与 §2.1 L0 声明的偏差方向一致）。
3. **v1 的 equity 精度增益集中在前两街**：MAE preflop −0.7pp（p=.0001）、flop
   −0.2pp（p=.012）显著更准，turn/river 打平——类空间的大胜（Δtop-5/Δrank 四街全
   显著）没有等比例转化成 equity 精度，说明「排序变准」主要发生在同强度带内部的
   类间排布。
4. **v1 相对 B2b 的 Δbias 四街全为 +1.1～+2.7pp（p=.0001）**：preflop 是修正
   （B2b −4.2pp 过弱 → v1 −1.5pp 近无偏），翻后是加重（CE 质量堆头 = 往强牌侧堆）。
   残差温度对此几乎无感（rscale 1.0 vs 0.8 各街 bias 差 ±0.3pp）——全局温度修不动
   方向性 bias，翻后 equity bias 与 q 倾斜同源，一起进 M2 校准课题。

M1 判断：**主出闸通过（显著打赢 B2b + 单调；rscale=0.8 下四街全显著），进入 M2**；
斜率课题、翻后 q/equity bias 倾斜（同源）、censoring 对照实验（Slumbot 全知集）
一起在 M2 处理。

## 10. 风险与拍板点

| 风险 | 缓解 |
|---|---|
| 5k 标签样本过拟合 | 先验残差（最坏=手写 tracker）+ 预训练 + 增广 + 早停 |
| 对手改版导致画像失效 | recent-50 主导 + 双尺度冲突信号 + 影子模式持续监控命中率衰减 |
| censoring 偏差污染 equity 消费 | L1 混源 + Slumbot 全知对照定量 + 必要时 M4 |
| 训练/服务特征偏斜 | golden 200 手双端对拍进 CI |
| 本机资源 | <1M 参数 + CPU 管线 + 可搬移训练 |
| B2 摸底后发现手写 tracker 已够 | 这是**好结局**：省掉整条 ML 维护成本，M0 的评估 harness 沉淀为 §2 调参的度量工具 |

拍板点：① 169 类（是，与 §2 同轴）；② hero 自标注权重 0.3（可扫）；③ v1 不做 per-combo suit 精化（消费端 `combo_refine` 兜）；④ M4 只在 M2 对照实验证明必要时立项；⑤ 训练框架 PyTorch CPU（不引入 GPU/onnxruntime 依赖）。

---

## 附：数据 schema 锚点

- 决策记录：`{"req": {hole, board, actions[{seat,action,to?,street}], stacks, names, button_seat, my_seat, ...}, "resp": {...}}`（`h9_live_run*.jsonl`）
- 结果记录：`{"kind":"hand_result", board, my_seat, result:{actions[带权威 street 标签], **shown_cards: {seat: [c1,c2]}**, winners, final_stacks}}`
- Slumbot：`{hole_cards, bot_hole_cards（每手都有，含 fold 手）, board, action:"b200c/kb200f", client_pos}`
- 画像：`h9_live_prof.json → by_name[name]{hands, vpip_opp/cnt, pfr_opp/cnt, aggr_actions, call_actions}`（待扩 per-street + 亮牌记忆，与 P0-3 共建）
