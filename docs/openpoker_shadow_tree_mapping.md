# OpenPoker 真实牌局到抽象影子树的映射

代码口径：2026-10-15，仓库版本 `2532705`。

`openpoker_advisor` 通过重放整手动作历史，把真实牌局映射到 blueprint（离线训练策略）对应的公共下注树。每个历史动作先换算座位和筹码单位，再翻译成当前影子节点允许的抽象动作，沿树边推进到 `node_id`。真实手牌和公共牌另外计算为牌桶，与 `node_id` 组合成 `InfoSetId`，用于查询策略。

**翻牌阶段的 blueprint 影子从翻前根节点重放而来。翻前映射形成的底池、剩余筹码和存活玩家会带入翻牌；进入翻牌时不会把影子底池重新校准成真实底池。** 实时搜索可以另外从真实筹码状态的本街起点建立子树，两者需要区分。

## 树和状态的区别

公共下注树在 advisor 启动时按 blueprint 的配置构建，包含各街的抽象下注分支。每次决策重新初始化并推进的是影子状态，树本身由这些状态共享。

| 对象 | 起始筹码 | 推进方式及用途 |
|---|---|---|
| blueprint 公共下注树 | 当前 advisor 使用每座 100BB 配置 | 编码抽象动作路径，提供节点、合法动作 tag 和子节点 |
| `real` 重放态 | blueprint 的固定起始栈，目前每座 100BB | 按历史中的具体 bet/raise 金额推进，用于校验和 blueprint 路径的输出尺寸计算 |
| `abs` 抽象影子态 | 与 blueprint 相同 | 按映射后的抽象动作及影子金额推进，携带 `current_node_id` |
| `auth` 真栈重放态 | 请求中的各座实际起始栈 | 用于实时搜索及部分兜底计算；缺少 `stacks` 时仍退回 blueprint 起始栈 |

`real` 的名称不表示它完整还原了真实码深。它保留历史中的具体 bet/raise 金额，但 Call 和 AllIn 的金额由自己的状态推导。在真实筹码偏离 100BB 时，尤其需要区分 `real` 和 `auth`。

## 从请求到策略的流程

```text
公共路径：本手全部已确认动作 → 座位和筹码归一化 → real 与 abs 双态重放 → node_id
牌的信息：真实 hole + board → hand_bucket

策略查询：(node_id, hand_bucket, street) → InfoSetId → blueprint 策略分布
动作输出：采样抽象动作 → 按输出基准态重算金额 → 换回平台单位并检查合法区间
```

### 座位和筹码归一化

满桌时以按钮为原点旋转座位：

```text
tree_seat = (op_seat + N - button_seat) % N
```

树座 `0/1/2` 对应 BTN/SB/BB，`3..N-1` 对应 UTG 起的其余位置。桌面座位数必须与加载的 blueprint 座位数一致。

在 `3 ≤ 实际发牌人数 < N` 的短桌局中，真实 BTN/SB/BB 仍映到树座 `0/1/2`，其余玩家按环序放到 CO 一侧。UTG 一侧缺少的位置作为幻影座，在重放开头依次 fold。OpenPoker 两人短桌采用单独的位置映射，目前仅支持翻前；翻后会显式返回 `short_hu_postflop`。

筹码换算使用盲注比例：

```text
scale = solver_BB / OpenPoker_BB
solver_to = OpenPoker_to × scale
```

例如平台 SB10/BB20，solver SB50/BB100，则 `scale = 5`。代码要求该比例为整数，且大小盲按同一比例对齐。`to` 是本街累计下注到的金额，不是本次新增投入。

### 每次从翻前根节点重放

`lockstep_replay` 每次都创建一个固定起始栈的 `real` 和一个位于树根的 `abs`。先执行幻影座弃牌，再遍历请求中的全部动作：

1. 将动作座位映到树座，将 `to` 换成 solver 单位。
2. 检查是否轮到该座，在 `real` 中执行具体动作。
3. 调用 `advance_shadow_by_applied`，将该动作翻译并应用到 `abs`。
4. 检查两边的下一行动者是否一致。
5. 历史结束后，检查是否轮到我方，以及双方街和请求的公共牌数量是否一致。

历史同时包含对手和我方已确认的动作。请求保存实际动作，没有保存我方上一次选择的抽象边；下一次决策时，我方历史动作也会重新映射。

例如翻牌轮到 BTN 面对下注时，影子导航路径可以是：

```text
翻前根节点
→ UTG / HJ / CO Fold
→ BTN Raise(0.5)
→ SB Fold
→ BB Call
→ 进入翻牌
→ BB Bet(0.5)
→ BTN 当前决策节点
```

### 具体动作翻译为抽象动作

Fold 和 Check 找同类合法分支；Call 优先找 Call 分支。如果没有 Call，且 `real` 执行动作后判定行动者全下，才尝试 AllIn 分支，否则返回结构性缺口。显式 AllIn 要求影子当前节点存在 AllIn 分支。

Bet/Raise 则把具体 `to` 放入**影子自己的下注状态**中计算比例。设：

```text
P = 影子当前底池
M = 影子本街最大已下注额
C = 影子当前行动者本街已下注额
T = 真实动作的 to，已换算到 solver 单位

P_after_call = P + max(M - C, 0)
x = (T - M) / P_after_call
```

普通加注区间内，按当前街的基础菜单选择相邻档位。当前底层使用 pseudo-harmonic 随机舍入；上层部分注释仍写着“最近 ratio”，应以 `DefaultActionAbstraction::map_off_tree` 的实现为准。

对于相邻档位 `A < x < B`：

```text
P(选择较小档 A) = (B - x)(1 + A) / ((B - A)(1 + x))
P(选择较大档 B) = 1 - P(选择 A)
```

实际实现使用整数 milli 比例，`x` 向下量化到千分之一。随机数由状态和金额派生，同样的状态与金额会得到同样的映射。精确命中档位时直接选该档；超出菜单端点时夹到端点。`T` 达到影子全下上限时先映到 AllIn；`T` 不高于影子下注水位时先按 Call、Check 等边界处理。

基础菜单的选择还要经过 `project_tag_onto` 投影到当前节点合法集：tag 存在则取该动作；加注档被剪掉时向上找最近的合法加注档，没有更大档才尝试 AllIn。投影取回的是影子合法动作中自带的金额，因此 `abs` 按影子金额推进。Bet/Raise 若在 `real` 中使行动者全下，也要求影子走 AllIn；此处的全下标记来自 `real`，不是独立读取服务器真栈。

### 树节点和真实牌的信息组合

`SimplifiedNlheGame::next` 按动作 tag 定位树边：

```text
tag = AbstractActionTag::of(action)
edge_idx = 当前节点 legal_actions 中 tag 的位置
child = 当前节点 children[edge_idx]
```

`Raise(0.5)`、`Raise(1.0)` 等 tag 标识抽象边，`to` 金额通过影子状态体现。到达决策节点后调用：

```rust
game.info_set_for_cards(abs.current_node_id, hole, &board)
```

翻前用 169 手牌类别，翻后把真实 `hole + board` 标准化为 observation，再查桶表得到 `hand_bucket`。最终打包 `node_id`、`hand_bucket` 和街信息，查询 blueprint 平均策略。影子初始化时发出的牌不用于这一步手牌查表。

### 抽象动作输出为平台动作

策略选择 Bet/Raise 的比例档后，`outgoing_action` 在输出基准态上重新计算对应金额：blueprint 路径使用固定起始栈的 `real`，搜索成功路径使用 `auth` 和与搜索子树一致的动作菜单。

金额随后除以 `scale` 并四舍五入，夹入平台给出的 `[min_raise, max_raise]`。平台没有合法加注区间时，当前实现可降为合法的 Call 或 Check，并在 `source` 中记录降级标记。

## 具体例子

以下按六座、`preopen` 抽象、100BB 影子栈计算，金额单位均为 BB。翻牌首次下注有半池和满池档，后续再加注的半池档被剪掉。涉及舍入分支和策略选择时，明确指定的分支是演示假设；给定请求实际走哪一档由代码的确定性种子和策略决定。

### 精确命中翻前档位

前面三人弃牌，BTN 开到 2.25BB，SB 弃牌，BB 跟注。BTN 开池前底池为 1.5BB，跟注后为 2.5BB：

```text
半池加注到 = 1 + 0.5 × 2.5 = 2.25BB
```

| 步骤 | 真实局 | 影子 |
|---|---|---|
| BTN 开池 | raise-to 2.25 | Raise(0.5)，到 2.25 |
| SB 弃牌 | Fold | Fold |
| BB 跟注 | 跟到 2.25 | Call，跟到 2.25 |
| 翻牌初始底池 | 5 | 5 |

假如真实手牌为 `AhKh`、翻牌为 `As7d2c`，就把这组牌对应的桶与上述路径到达的节点组合，查询策略。牌面影响牌桶，下注历史确定树位置。

### 翻前尺寸差异带入翻牌及输出

BTN 改为开到 3BB，开池比例为：

```text
x = (3 - 1) / 2.5 = 0.8
```

映到半池开池档的概率为 `1/3`，对应 2.25BB；映到满池档的概率为 `2/3`，对应 3.5BB。假设选中了 2.25BB，SB 弃牌、BB 跟注：

| 状态 | 真实局 | 影子 |
|---|---:|---:|
| BTN 投入 | 3 | 2.25 |
| BB 投入 | 3 | 2.25 |
| SB 弃掉的盲注 | 0.5 | 0.5 |
| 翻牌底池 | **6.5** | **5** |

BB check 后，BTN 下注 3.25BB。在真实局这是半池，但在影子里计算为 `3.25 / 5 = 0.65pot`，会再次舍入到半池 2.5BB 或满池 5BB。翻前累计的差异改变了翻牌对同一金额的解释。

如果 BTN 是我方，且此时 blueprint 选择半池档，输出会按 `real` 的 6.5BB 底池计算，发出 3.25BB；平台 BB 为 20 筹码时，就是 65 筹码。下一次重放这一动作时，影子仍会把 3.25BB 解释成 0.65pot，**不保证落回我方最初选择的半池边**。

### 翻牌下注映射后的跟注继续保留差异

从双方翻牌底池都为 5BB 的节点开始，BB 下注 3.25BB：

```text
x = 3.25 / 5 = 0.65
P(半池) = (1 - 0.65) × 1.5 / (0.5 × 1.65)
        = 7/11，约 63.6%
P(满池) = 4/11，约 36.4%
```

假设选中半池，随后 BTN 跟注：

| 步骤 | 真实局 | 影子 |
|---|---:|---:|
| 翻牌初始底池 | 5 | 5 |
| BB 下注 | 3.25 | 2.5 |
| BTN 跟注投入 | 3.25 | 2.5 |
| 进入转牌时底池 | **11.5** | **10** |

影子沿 `Bet(0.5) → Call` 推进。Call 保留跟注语义，金额由各自状态推导，所以不会补回下注映射造成的差额。

### 翻牌小加注被投影到满池加注

从上例 BB 已下注、BTN 尚未跟注的时点分叉。真实 BB 下注 3.25BB，影子 BB 下注 2.5BB；BTN 现在真实加注到 8BB。

```text
影子当前底池 = 5 + 2.5 = 7.5
影子当前水位 = 2.5
BTN 本街已投入 = 0
跟注后底池 = 7.5 + 2.5 = 10
x = (8 - 2.5) / 10 = 0.55
```

| 基础档位 | 影子目标金额 | 当前节点处理 |
|---|---:|---|
| Raise(0.5) | 2.5 + 0.5 × 10 = 7.5 | 被剪掉，向上投到满池 |
| Raise(1.0) | 2.5 + 1.0 × 10 = 12.5 | 合法，保留 |

因此无论 0.55pot 先舍入到哪一档，最终都走 `Raise(1.0)`：真实加到 8BB，影子加到 12.5BB。如果 BB 再跟注，真实底池为 `5 + 8 + 8 = 21BB`，影子底池为 `5 + 12.5 + 12.5 = 30BB`。

### 翻前小再加注也需要合法集投影

BTN 开到 2.25BB，SB 加到 5BB。SB 行动前影子底池为 3.75BB、水位为 2.25BB、已投入为 0.5BB：

```text
跟注后底池 = 3.75 + (2.25 - 0.5) = 5.5
x = (5 - 2.25) / 5.5 = 0.5
```

这次金额精确命中半池档，但半池再加注被剪掉，仍要投到满池。影子最终加到 `2.25 + 5.5 = 7.75BB`。精确命中基础菜单不等于该档在当前节点合法。

### 六座桌四人发牌

假设平台座位 2 为 BTN，实际发牌座位为 `[0, 2, 4, 5]`：

| 平台座位 | 真实角色 | 树座 |
|---|---|---|
| 2 | BTN | 0 |
| 4 | SB | 1 |
| 5 | BB | 2 |
| 0 | CO，翻前第一个行动 | 5 |

树座 3 和 4 作为幻影 UTG、HJ，先依次 Fold，然后轮到树座 5。真实四人局的 CO 决策映到六人树中 UTG、HJ 已弃牌后的 CO 节点。盲注位置和行动环序通过这次重排对齐。

### 缺少动作分支或真实码深不同

**非 SB open-limp。** 真实 UTG Call 到 1BB，但 `preopen` 根节点删除了非 SB 的 open-limp。影子找不到 Call 分支，返回 `structural_gap`，不会随意选一个加注动作代替。

**短码全下。** 假设 UTG、CO 有 100BB，HJ 只有 10BB，历史为：

```text
UTG 开到 2.25BB
HJ 显式 all_in，真实到 10BB
CO 再加注到 30BB
```

真实局中这条线合法。固定 100BB 重放态收到显式 `all_in` 后，根据自己的栈执行到 100BB；随后 CO 的 raise-to 30 低于当前 100BB 水位，返回 `replay_illegal`。这里假设协议确实发送 `all_in`；发送数值 raise 的历史会走数值动作分支。

带有 HJ 实际起始栈的 `auth` 才能正确重放该例。影子失败后的翻前决策走相应启发式或 fallback；后续翻后在开启搜索并满足触发条件时，可以进入脱影子搜索。

## blueprint 影子与实时搜索子树

| 项目 | blueprint 影子导航 | 实时搜索子树 |
|---|---|---|
| 根 | 翻前根节点 | 由 `ResolveRoot` 选择本街起点或当前决策点 |
| 状态来源 | 固定 blueprint 起始栈，加抽象动作重放 | 根据请求中的实际起始栈重放得到 `auth` 和 `round_start` |
| 翻前历史的作用 | 决定翻牌节点及影子几何 | 重建真实翻牌起点，并可参与 range 估计 |
| 动作金额 | 使用影子状态下的抽象金额 | 使用搜索根状态和子树菜单形成的金额 |
| 影子失同步后 | 无法取得完整对应节点 | 满足触发条件时可走 `search:unanchored` |

在 `RoundStart` 模式下，翻牌搜索子树从翻牌这一轮起点展开。这个根状态通过从开局重放真实历史获得，但搜索树的展开不从翻前重新开始。同街后续决策可以复用已解子树，再导航到当前点。

影子仍同步时，搜索可以利用对应 blueprint 路径估计 range；影子失同步时，脱影子搜索不要求完整 blueprint 节点，默认仍可利用最后一个已同步前缀估计 range。搜索子树也存在动作抽象，其导航同样需要处理实际动作与抽象菜单的差异。

## 同步检查所保证的范围

`lockstep` 成功表示检查到的行动顺序、当前街和公共牌数量仍然对齐，不表示两边底池、SPR（剩余筹码与底池之比）或剩余筹码相等，也不表示真实码深已经被准确表达。

尺寸映射、节点动作剪枝和固定起始栈是三个不同的近似来源。前两者可能持续改变影子金额；最后一个还可能让后续行动在固定栈重放态中变得非法。结构性缺口会显式分流，不应把所有 fallback 都归因于“下注尺寸选错档”。

## 代码入口

| 关注点 | 文件与符号 |
|---|---|
| 收集双方已确认动作并构造请求 | [tools/openpoker_play.py](../tools/openpoker_play.py)：`HandState.on_player_action`、`build_request` |
| 重建 blueprint 树及校验配置 | [tools/openpoker_advisor.rs](../tools/openpoker_advisor.rs)：`build_game`、`reshape_profile` |
| 座位归一化与双态重放 | [tools/openpoker_advisor.rs](../tools/openpoker_advisor.rs)：`seat_map`、`lockstep_replay`、`lockstep_step` |
| 请求动作转为规则引擎动作 | [src/training/openpoker_hh.rs](../src/training/openpoker_hh.rs)：`hist_to_concrete` |
| 输入动作翻译及合法集投影 | [src/training/blueprint_advisor.rs](../src/training/blueprint_advisor.rs)：`advance_shadow_by_applied`、`project_tag_onto` |
| 比例计算与随机舍入 | [src/abstraction/action.rs](../src/abstraction/action.rs)：`map_off_tree`、`pseudo_harmonic_pick` |
| 抽象树边和动作剪枝 | [src/training/nlhe_betting_tree.rs](../src/training/nlhe_betting_tree.rs)：`AbstractActionTag`、`first_small_preopen_6max`、`filter_actions` |
| 树节点推进与真实牌查桶 | [src/training/nlhe.rs](../src/training/nlhe.rs)：`next`、`info_set_for_cards`、`pack_info_set_v2` |
| 真栈重放和脱影子搜索 | [tools/openpoker_advisor.rs](../tools/openpoker_advisor.rs)：`real_stacks_config`、`build_real_auth`、`decide_search_unanchored` |
| 输出金额换算 | [src/training/blueprint_advisor.rs](../src/training/blueprint_advisor.rs)：`outgoing_action`；[tools/openpoker_advisor.rs](../tools/openpoker_advisor.rs)：`action_to_response`、`raise_to_op` |
