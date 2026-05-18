# 简化 NLHE InfoSet 是否需要动作历史 — 调查路径

本文档只记录"接下来要验证什么 / 怎么验证"，不记录结论。结论落地后改 `docs/status.md`。

## 出发点

`src/training/nlhe.rs::SimplifiedNlheGame::info_set` 把 `InfoSetId` 编码为：

- `bucket_id` — hole+board 的牌力桶（与下注线无关）
- `position_bucket` — 座位
- `stack_bucket` — **starting stack / BB**，整手不变（见 `src/abstraction/preflop.rs:106`）
- `betting_state` — 仅本街 raise 计数
- `street_tag` — 街
- `action_signature` — 本次决策的可选动作 role mask + bet sizing 金额桶

没有任何字段编码"前面街发生过什么"。同一 `street_tag + betting_state=Open` 下，
preflop 的 aggressor 身份、raise 深度、是否 limp 都被丢掉。

## 假设

存在两条不同的 preflop 线，到达 flop 同一 actor 决策点时，`info_set` 返回相同的
`InfoSetId`。但两条线下双方 range 分布显著不同，单一 average strategy 不可能同时
对两个 range 最优。

## 验证路径（按序执行）

### Step 1 — 写 collision 测试，证实 / 证伪假设

新增 `tests/nlhe_infoset_history_collision.rs`。构造两条 preflop 线：

- 线 A（SB 攻击）: SB raise → BB call → 进 flop。
- 线 B（BB 攻击）: SB call (limp) → BB raise → SB call → 进 flop。

使用同一 RNG seed，让发牌（hole + board）完全相同。

断言：

1. 两条线 flop 起手的 actor 相同（BB / player 1）。
2. 两条线在该决策点上 `info_set(state, actor)` 返回**相同**的 `InfoSetId`
   — 这是当前实现的行为，测试 `assert_eq` 是为了把 collision 钉成 regression。
3. 两条线 `state.action_history` 实际不同 — 证明 collision 不是因为状态本身
   退化，而是 InfoSet 编码丢信息。

通过 = collision 实锤；失败 = 假设不成立，要么 stack/pot 状态本身就把两条线
区分了（要看具体哪个字段在区分），要么测试构造错了。

### Step 2 — 决定怎么加 history（外部参考调研已完成）

Step 1 测试通过（commit `60eacec` + vultr 实测）。collision 实锤，按 `CLAUDE.md`
§1 是正确性问题，必须修。

#### 外部参考做法（按"参考价值降序"列）

1. **OpenSpiel `universal_poker`（lossless ground truth）**
   `InformationStateString` 字面包含跨街完整下注序列：
   ```
   [Round %i][Player: %i][Pot: %i][Money: %s][Private: %s][Public: %s][Sequences: %s]
   ```
   `sequences` 是按 round 用 `|` 拼接的完整 betting sequence（每 round 内是
   抽象动作字符）。`InformationStateTensor` 把动作序列拆成 2 bit/动作的 label
   `(call=10, raise=01, all-in=11, fold/deal=00)` × `max_game_length`，再单独
   带一个 `max_game_length` 长的 sizing 数组。**没有 history bucketing**——
   universal_poker 是面向 ACPC 全游戏的，infoset key 直接包含完整动作历史。

2. **Slumbot 2019（实战 NLHE CFR 实现，可比性最高）**
   不在 infoset key 上拼历史；而是**预先建一棵 abstract betting tree**。
   `betting_tree.h` 的 `Node` 结构：
   ```cpp
   class Node {
     std::unique_ptr<std::shared_ptr<Node>[]> succs_;
     int id_;
     short last_bet_to_;
     short num_succs_;
     unsigned short flags_;          // 编码 street 等
     unsigned char player_acting_;
     unsigned char num_remaining_;
   };
   ```
   每个节点一个 `id_`，按 `(player_acting, street)` 分桶 sequential 分配
   （`num_nonterminals[pa][st]++`）。CFR table 直接以 `(node_id, hand_bucket)` 索引。
   节点本身**就是抽象动作历史**——树的每条路径从 root 到当前节点，唯一对应
   一个抽象 betting sequence。`BettingAbstraction` 配置按 `(street, num_prior_bets,
   player)` 给出可用 bet sizes，因此**合法动作集本身依赖历史**，节点天然区分。

3. **DeepStack / 主流 HU NLHE solver**
   action abstraction 通常是 per-street：first action 用 half pot / pot /
   all-in，后续 action 用 pot / all-in。infoset 处理同 Slumbot：节点 = 抽象历史。

#### 结论

主流做法**不是**在 64-bit infoset key 上加 history bucketing。主流做法是
**预建抽象 betting tree**，每个 reachable 抽象动作序列得到一个唯一节点 id，
CFR table 用 `(node_id, hand_bucket)` 索引。

我们现在的 `SimplifiedNlheGame::info_set` 是 stateless 函数：它从 `GameState`
+ `bucket_table` 直接 pack 出 `InfoSetId`，没有跨节点的 tree 数据。这条架构本身
就是丢历史的根因——只看"当前状态特征"，没看"怎么走到这里"。

#### 候选方案对比

| | A. 建 PublicBettingTree | B. 扩 InfoSetId 到 128 bit hash | C. 现 26-bit `action_signature` 改塞 history label |
|---|---|---|---|
| 跟参考实现一致 | ✓ Slumbot/DeepStack 标准做法 | ✗ 没见过这么做的 | ✗ 没见过这么做的 |
| 历史 lossless | ✓ 节点 1:1 对应抽象序列 | ✓ 64-bit hash 碰撞可忽略 | ✗ 仍有损（位预算紧） |
| 改动量 | 大：新数据结构 + state 改成带 `node_id` | 中：`InfoSetId` 改 `u128`，下游全跟着改 | 小：只改 `info_set` 内部 |
| RegretTable 影响 | key 从 `InfoSetId` 改成 `(node_id, hand_bucket)`，或保留 `InfoSetId` 但语义换成 node-based | key 类型从 u64 改 u128 | 无 |
| checkpoint 兼容 | 旧 ckpt 全弃 | 旧 ckpt 全弃 | 旧 ckpt 全弃 |
| 收敛行为可证 | 标准 CFR 收敛，跟文献对得上 | 等价 lossless，但是没人这么做过，证明没现成参考 | 不 lossless，得另外证哪些 history 桶足够 |

#### 推荐：方案 A

理由：

- `CLAUDE.md` §1 / §4 "跟参考行为不一致默认是参考对" → 走 Slumbot 形态。
- checkpoint 兼容反正都要弃（任何方案都改了 key 布局），不构成 A 的额外代价。
- 改动量大但**架构上一次性对齐**，不留"以后再优化"的尾巴（§2 反追加）。
- 方案 B/C 没有外部参考佐证收敛性，等于自创编码 + 自己证收敛，违反 §1。

#### 方案 A 的最小骨架

1. 新增 `src/training/nlhe_betting_tree.rs`:
   - `PublicBettingTree` 在 `SimplifiedNlheGame::new` 时从 root 出发 DFS 枚举
     所有 reachable 抽象动作序列，每个决策节点分配 `u32 node_id`。
   - 节点存：`(street, player_acting, parent, action_taken_from_parent,
     legal_actions: Vec<AbstractAction>)`。
   - **不存** hand / board / chip values——这些走 hand bucket 旁路。
2. 改 `SimplifiedNlheState` 加 `current_node_id: u32`。`next` 沿 tree 链跳节点。
3. `info_set` 重写：
   ```
   InfoSetId = pack(hand_bucket [24 bit], node_id [N bit], street_tag [3 bit], reserved)
   ```
   `node_id` 位宽按实测树大小定（先做 sizing：跑一遍 DFS 数节点数）。
4. 删掉 `action_semantic_signature` / `compute_betting_state` / `compute_position_bucket`
   / `compute_stack_bucket` 在 simplified NLHE 这条 codepath 的调用（它们的
   信息已经全部内化到 `node_id` 里——同一 node_id 必然同 street、同
   player_acting、同合法动作集、同 prior raise count、同 starting stack）。
   `stack_bucket` 因为本来就是 starting-stack 常量（见上文），node_id 把它完全
   subsume；`position_bucket` 在 HU 范围里跟 player_acting 1:1 对应。
5. Step 1 的 collision 测试翻转为 `assert_ne`。

#### 方案 A 的风险 / 待回答问题

- **树有多大？** 必须先做 sizing：从 `SimplifiedNlheGame::root` DFS 全枚举抽象
  动作序列，统计决策节点数。预期 ~10^4 到 10^5 量级（5-action × 4 街 ×
  限定 raise 深度）。若 >> 10^6 要重新考虑 abstraction granularity。
- **`DefaultActionAbstraction::abstract_actions(state)` 依赖 state**：
  legal_actions 在不同 chip 状态下可能不同（如 all-in 上限随筹码变化）。
  但抽象动作 ratio_label 是 (HALF_POT/FULL_POT/AllIn) 固定语义，对应金额由
  state 推出。树结构应按 ratio_label 序列建树，而不是按 `to` 金额，否则筹码
  连续值会让节点数爆炸。
- **是否真消除了所有 collision？** 树建完后跑 collision 测试翻转版 (`assert_ne`)
  必须通过；再加一条 "abstract action sequence → InfoSetId 单射" 的全枚举
  property test。
- **postflop 公共牌怎么处理？** Slumbot 把 board cards 走单独的 hand-bucket 维度，
  不进 betting tree。我们沿用：`info_set = (hand_bucket@street, node_id)`，
  board 信息走 `bucket_table.lookup` 输入。这一步现有代码已经是这样。

### Step 3 — 加完之后用什么证明加对了

不能只靠"测试现在 assert_ne 通过了"。需要：

- Leduc / Kuhn 类 ground truth 不适用（它们不分多街 aggressor）。
- 简化 NLHE 没有 closed-form Nash。
- 唯一可行的外部对照是：在加完 history 的版本上跑 LBR / best response
  exploitability 曲线，对比加之前。`tools/nlhe_h3_report.rs` 已经有 LBR proxy 入口。
- 量化门槛：完整 H3 gate（按 `docs/status.md` 是 100M update + 1M hands 评测）下
  exploitability 显著下降才能说"加 history 修了正确性问题"。否则可能是修了一个
  不影响收敛的死字段。

## 方案 A 执行路径

每个 Phase 是一个独立 commit。每个 Phase 跑通后才进下一个。**先做 Phase 0 sizing**，
节点数决定整个方案是否可行；可行了再开始改架构。

### Phase 0 — Sizing + Baseline 存档

目标：(1) 拿到 reachable 抽象决策节点数的真实数字，作为方案 A 可行性 hard gate；
(2) 在动任何代码前存好旧版 LBR exploitability baseline，供 Phase 5 对照。

#### 0a. Baseline 存档（动手前必做）

- 在当前 `cooldown` HEAD commit 上跑 `tools/nlhe_h3_report.rs`，存 artifacts
  到 git tracked 路径或 vultr persistent location。命令：

  ```bash
  cargo run --release --bin train_cfr -- \
      --game nlhe --trainer es-mccfr \
      --bucket-table artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin \
      --updates <BASELINE_BUDGET> --seed 0x... --threads 4 \
      --checkpoint-dir artifacts/baseline_pre_history/ --checkpoint-every <N>
  cargo run --release --bin nlhe_h3_report -- \
      --checkpoint artifacts/baseline_pre_history/<final>.ckpt \
      --artifact artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin \
      --eval-hands-per-seat <N> --lbr-probes <N> --lbr-rollouts <N> \
      --output artifacts/baseline_pre_history/h3_report.md
  ```
  `BASELINE_BUDGET` 跟 Phase 5 一致。这是大计算事件，必须先跟用户对预算 +
  provider，不擅自占用 vultr 4-core。

#### 0b. 树规模 sizing

- 新增 `tools/nlhe_betting_tree_sizing.rs`：
  - 从 `SimplifiedNlheGame::root` 开始 DFS。每到一个 `NodeKind::Player` 决策节点
    计数 +1；用 `(player_acting, street, abstract_action_sequence)` 作 dedup key
    确认无重复（HU + 抽象 path 理论上单射，但代码层面验证一遍）。
  - 报告：决策节点总数、各街分布、深度直方图、节点数对应 bit 宽度。
- Cargo.toml 注册 `[[bin]]`。
- 本机 `cargo build --release --bin nlhe_betting_tree_sizing` + `cargo fmt --check`
  + `cargo clippy --release --bin nlhe_betting_tree_sizing -- -D warnings`。
- vultr `cargo run --release --bin nlhe_betting_tree_sizing` 跑出实数。

**决策门**：

- 节点数 < 10^6 → 进 Phase 1。
- ≥ 10^6 → 停下报数字，找用户商量是否压抽象（候选：限制 max_raises_per_street、
  或去掉 HALF_POT、或限定 board texture-aware 子树）。

### Phase 1 — PublicBettingTree 数据结构 + 建树

前置：Phase 0 通过。

- 新增 `src/training/nlhe_betting_tree.rs`，定义 `PublicBettingTree` / `TreeNode`
  / `NodeId` / `Child`。
- 实现 `PublicBettingTree::build(config: &TableConfig) -> PublicBettingTree`：
  - DFS，节点按 ratio_label 序列匹配（不按 `to` 金额）。
  - 子节点分类 `Decision / StreetTransition / Terminal`。
  - 不依赖 board/hole，建树用任一合法 RNG seed 占位（验证 board 独立性）。
- 单测（同模块 `#[cfg(test)] mod tests`）:
  - root 节点存在、player_acting = SB、合法动作集包含 Fold / Call / Raise / AllIn。
  - 每个非 Terminal 子节点的 `parent` 链 walk 回 root。
  - 节点总数与 Phase 0 sizing 工具读数一致。
  - 同 ratio_label 序列只产出唯一 `node_id`（property test：随机抽 1000 条
    路径，按 ratio_label 序列 dedup 后 node_id 集合大小等于 dedup 后路径数）。
- 本机 build + fmt + clippy 全绿；vultr `cargo test --test ...` 全套绿。

不暴露给 `SimplifiedNlheGame` 之外的模块（`src/training/mod.rs` 不 `pub use`）。

### Phase 2 — SimplifiedNlheState 内化 node_id

前置：Phase 1 通过。

- `SimplifiedNlheState` 加 `current_node_id: NodeId` + `tree: Arc<PublicBettingTree>`。
  `action_history` 暂时**保留**（postflop LBR proxy 可能用），Phase 3 后再决定删不删。
- `SimplifiedNlheGame::new` 一次性 `build` 树并 `Arc` 起来。
- `SimplifiedNlheGame::root` 设 `current_node_id = tree.root_id`。
- `SimplifiedNlheGame::next` 沿 `node.children[action_idx]` 跳转；invariant：
  `concrete action 必须命中 node.legal_actions 里某条 ratio_label`；
  不命中 panic（CFR 走 abstract 动作集，正确情况下永远不应该不命中）。
- **info_set 此 phase 不动**，仍走旧 layout。目的：把 state 改造跟 info_set 重写
  分两个 commit，让 phase 2 是 pure refactor、phase 3 是 pure 行为变更。
- 全套测试（含 Kuhn / Leduc / `cfr_simplified_nlhe` / `nlhe_infoset_history_collision`）
  应保持当前状态：collision 测试仍 `assert_eq` 通过（info_set 没换语义）；其它
  绿。任何意外失败 → 停下查根因，不 carve-out。

### Phase 3 — 重写 info_set + 翻转 collision 测试

前置：Phase 2 通过。

- 重写 `SimplifiedNlheGame::info_set`：
  - 新 layout `pack_v2(hand_bucket, node_id, street_tag)` —— 字段位宽按 Phase 0
    实测 node_id 上限定。
  - 删 simplified NLHE codepath 上对 `compute_position_bucket` /
    `compute_stack_bucket` / `compute_betting_state` / `action_semantic_signature`
    的调用（**保留**这些函数本身，stage-2 通用 `InfoAbstraction::map` 路径
    `src/abstraction/preflop.rs` / `postflop.rs` 还在用）。
- 新增 `pack_info_set_v2` / `unpack_*_v2` —— **不**复用 stage-2 `pack_info_set_id`
  的字段语义。Stage-3 NLHE 走自己的 packer，跟 stage-2 通用层解耦。
- 翻转 `tests/nlhe_infoset_history_collision.rs`：`assert_eq` → `assert_ne`，
  注释里说明翻转节点（commit hash）。
- 新增 `tests/nlhe_betting_tree_injection.rs`：枚举树上所有 (decision_node, hole)
  组合的代表抽样（hole 不全枚举，固定一手代表牌即可），断言不同 node_id 一定
  得到不同 InfoSetId。
- **旧测试 `tests/nlhe_infoset_semantics.rs`** 测的是 H3 旧 action_signature 26-bit
  layout 区分金额。新方案下"金额语义不同"是因为 node_id 不同（不同路径），不再靠
  action_signature。需要审视这个测试：
  - 如果旧测试构造的两条路径在新方案下也是不同 node_id → 用 node_id 替代
    action_signature 的断言，测试改写但语义保留。
  - 如果旧测试的两条路径在新方案下恰好同 node_id → 旧测试本身就是 false positive
    （没有真区分），按 `CLAUDE.md` §2 "代码错了改代码 / 不写已废弃保留"直接删。
- 全套测试 vultr 跑通；`cargo test --release -- --ignored` 也跑。

### Phase 4 — checkpoint schema bump + 工件清理

前置：Phase 3 通过。

- `InfoSetId` 字段语义换了，旧 ckpt 全部失效。按 `CLAUDE.md` §2 直接弃：
  - 升 `SCHEMA_VERSION`（`src/training/checkpoint.rs`）。
  - 旧 schema 拒绝 load（已有 D-356 多 game 不兼容拒绝机制）；不写迁移。
- 删 `artifacts/h3_smoke/` 下的旧 checkpoint（Phase 3 之前训的）。
- 改 `docs/status.md`：简化 NLHE 那一行的 BLAKE3 anchor 失效，等 Phase 5 重跑后填新值。

### Phase 5 — Convergence 证据（外部对照）

前置：Phase 4 通过 + Phase 0 baseline 已存档。这一步是按 `CLAUDE.md` §1
"正确性必须外部对照证据" 的硬要求。

**Phase 0 已经存了旧版 baseline**（见 Phase 0 第二步）。这里跑新版：

- 新版（Phase 4 完成后）：同 update budget + 同 seed + 同 bucket table 跑一次
  `tools/nlhe_h3_report.rs`，拿 LBR exploitability 曲线，存 artifacts。
- 对比新旧 artifacts：
  - LBR exploitability 显著下降（量化阈值预先定：≥ 30% 相对下降，依据 Slumbot
    / DeepStack 文献报告的 history-aware vs history-blind solver 量级差距）。
  - 若下降 < 10% 或反而上升 → 方案 A 没修对，回头看 node_id 单射是否真单射，
    或 abstraction 本身太粗。
- 改 `docs/status.md`：简化 NLHE 行从 "1K finite strategy + BLAKE3 byte-equal smoke"
  升级到 "LBR exploitability < X chips/game @ 100M update"。旧 BLAKE3 anchor
  失效，删除，不写"已废弃"。
- 改 `docs/nlhe_infoset_history_investigation.md`：本文档整体作废（结论已落地到
  `docs/status.md`）；按 `CLAUDE.md` §2 直接 `git rm`，不留"已结束"标记。

**算力预算门**：Phase 5 的"100M update + LBR"按 H3 闭环工具 smoke 经验估算 wall
time 显著 > 1h。按 `feedback_high_perf_host_on_demand.md` 必须事先跟用户商量
provider（AWS / vultr 升级 / Hetzner）+ 预算，不假设 vultr 4-core 能跑完。Phase 0
存 baseline 时同步把这个预算估算报给用户。

## 范围外

- 不在本文档讨论 6-max history。stage 3 简化 NLHE 是 HU 范围（D-313）。
- 不在本文档讨论 perf。按 `CLAUDE.md` 反模式 §1，正确性确认前不做 perf。
- 不在本文档讨论 checkpoint schema 兼容。如果改 `InfoSetId` 位 layout，旧
  checkpoint 全部作废，按 `CLAUDE.md` §2 "不写已废弃保留"直接弃。
