# 阶段 2：抽象层的量化验证方式

## 阶段目标

阶段 2 的目标是把阶段 1 落地的真实无限注 6-max NLHE 环境压缩成可被 CFR 训练的有限博弈。本阶段不训练任何策略，只验证抽象映射在数值上正确、确定性、性能、聚类质量。

阶段 2 需要支持：

- **Action abstraction**：从无限离散 raise 金额空间压缩到有限动作集合。默认 5-action：`fold / check / call / 0.5×pot / 1×pot / all-in`；`ActionAbstractionConfig` 接口预留 1–14 个 raise size 配置扩展，但阶段 2 不实跑大配置（用户决策；`docs/pluribus_path.md` §阶段 2 字面 1–14 raise size 留作后续阶段消融对照）。
- **Information abstraction**：把 `(GameState, hole_cards)` 公开+私有信息映射到 InfoSet id。
    - **preflop**：`(52 choose 2) = 1326` 起手牌 → lossless 169 等价类（13 paired + 78 suited + 78 offsuit），并区分位置 / 有效筹码 / 前序动作。
    - **flop / turn / river**：默认每条街 500 bucket（`BucketConfig` 接口可配置，阶段 2 验收只跑 500/500/500），基于 EHS² + OCHS 等 potential-aware 特征聚类。
- **Bucket lookup table**：独立二进制 artifact，运行时 `mmap` 加载，含 `schema_version` + 自校验 BLAKE3。这条路径在阶段 6 实时搜索的 lookup 表也会复用，阶段 2 落地基础设施。

阶段 2 与阶段 1 最大边界差异：**equity 计算允许浮点**（Monte Carlo equity / EHS² / k-means centroid），但抽象映射的运行时输出（bucket id / InfoSet id）必须是整数；浮点不得渗入阶段 1 已锁定的规则路径（`GameState` / `HandHistory` / `RngSource` 显式注入）。

阶段 2 没有 PokerKit 这样 byte-level 的开源参考实现可对照——bucket 边界由我们自己的 clustering 决定。因此阶段 2 验收 **强依赖内部不变量**（bucket 内方差 / 间距 / 重复一致性）和 **阶段 1 的信任锚**（preflop 169 lossless 是无歧义的；postflop bucket 在 preflop→flop transition 上不能数值不连续）。

## 量化验证方式

### 1. Action abstraction

- 默认 5-action 配置：`{ Fold, Check, Call, BetRaise(0.5×pot), BetRaise(1.0×pot), AllIn }`。每个动作是否合法依赖当前 `GameState`：
    - 已 check 局面无 `Fold`（`Check` 可用时 `Fold` 应被剔除——`pluribus_stage2_decisions.md` D-204 锁定）。
    - `BetRaise(x×pot)` 当 `x×pot < min_to` 时按 D-205 锁定的 fallback 规则替换 `to` 为 `min_to`（不剔除）；`Bet` 与 `Raise` 由 stage 1 `LegalActionSet` LA-002 互斥决定。
    - `BetRaise(x×pot) >= committed_this_round + stack` 时 fallback 到 `AllIn { to = committed_this_round + stack }`（D-205）。
- 配置扩展接口：`ActionAbstractionConfig` 至少支持 1–14 个 raise size（任意 `pot ratio`），但阶段 2 仅默认 5-action 走完整 SLO + 全套测试；1–14 raise size 仅做 "配置可加载 + 输出确定性 + 哈希区分性" 的 smoke test（D-202）。
- 任意 `(GameState, ActionAbstractionConfig)` → 抽象动作集合的映射必须 **byte-equal 重复一致**：相同输入重复 `1,000,000` 次结果完全相同（AA-007）。
- 抽象动作集合断言：构造至少 `200` 个固定 `GameState` 场景，覆盖 open / 3-bet / 短码 / incomplete raise / 多人 all-in / showdown 临界，断言每个场景下默认 5-action 输出的合法子集与人工预期完全一致。
- off-tree action handling 接口：`ActionAbstraction::map_off_tree(real_bet) -> AbstractAction` 必须存在且签名稳定；阶段 2 仅占位实现（D-201 锁定 Pseudo-harmonic mapping (PHM)，nearest-action fallback stub），完整数值验证 + fuzz 留给阶段 6c。

### 2. Information abstraction：preflop lossless 169

- 全部 `1326` 起手牌 → 169 等价类（13 paired + 78 suited + 78 offsuit），**100% 覆盖且无重叠**。每个等价类的 hole 组合数与组合数学一致：pairs `6`、suited `4`、offsuit `12`，169 类 hole 计数总和 `1326`。编号公式 D-217 锁定 closed-form（pairs 0..13 / suited 13..91 / offsuit 91..169），B1 [测试] 可在 [实现] 之前直接基于公式枚举断言。
- preflop InfoSet 完整 key：`(hand_class_169, position_bucket, stack_bucket, betting_state)`，按 D-215 统一 64-bit `InfoSetId` layout 编码（`bucket_id` 24 bit / `position_bucket` 4 bit / `stack_bucket` 4 bit / `betting_state` 3 bit / `street_tag` 3 bit / `reserved` 26 bit；preflop `street_tag = 0`，postflop 共用同一 layout）。
    - **position_bucket**：6 桶（BTN / SB / BB / UTG / MP / CO），D-210 锁定。其它桌大小（2..=9）走 "seat distance from button mod n_seats" 通用映射，仅 smoke。
    - **stack_bucket**：连续 `chips: u64` → 5 桶 `[0, 20) BB / [20, 50) BB / [50, 100) BB / [100, 200) BB / [200, +∞) BB`（D-211 锁定，左闭右开，preflop 起手时计算；postflop 沿用 preflop 起手值不重算）。
    - **betting_state**：5 状态 `{ Open, FacingBetNoRaise, FacingRaise1, FacingRaise2, FacingRaise3Plus }`，3 bit 编码，D-212 锁定。preflop / postflop 共用同一枚举：`Open` 表示 actor 可 `Check / Bet`，`FacingBetNoRaise` 表示 actor 必须 `Fold / Call / Raise`，二者合法动作集**不同**——仅以 raise count 编码会让两类局面同 InfoSetId 但合法动作集不同（IA-002 / F17 修复）。盲注本身不算 voluntary aggression（继承 stage 1 D-037）；preflop limp 不算 raise；incomplete short all-in 视为 1 次 raise。
- 同一 `hand_class_169` 在不同 `(position_bucket, stack_bucket, betting_state)` 下必须产出 **不同** InfoSet id（哈希区分性测试，碰撞率 0%）。
- preflop InfoSet mapping 重复 `1,000,000` 次哈希一致。
- `InfoSetId` reserved 位（bit 38..64，26 bit）必须全为 0（IA-007）；任何非零写入是 P0 阻塞 bug。
- 169 lossless 是阶段 2 的 **信任锚**：它是 stage 2 唯一无歧义、无聚类、无浮点的部分，因此必须 100% 正确，不允许任何已知偏差进入阶段 3。

### 3. Information abstraction：postflop bucket（flop / turn / river）

- 默认验收配置：`flop = 500, turn = 500, river = 500`（`pluribus_path.md` §阶段 2 字面 ≥500 per street）。`BucketConfig` 接口允许每条街独立配置 bucket 数。**阶段 2 验收只跑 500/500/500**；其它配置（如 1000/1000/1000）只做 "配置可加载 + 写出 bucket table + bucket id 范围正确" 的 smoke test，不做完整 EHS std dev / EMD 阈值验收。
- 聚类输入特征（path.md 强约束 "potential-aware"），D-221 锁定 **EHS² + OCHS** 双特征 concat 作为 k-means 输入向量，`feature_set_id = 1` 对应该组合：
    - **EHS²**（Expected Hand Strength squared）— 表征手牌强度的二阶矩，捕捉 distribution shape；标量 1 维。
    - **OCHS**（Opponent Cluster Hand Strength）— 把对手手牌空间预聚类成 `N = 8` 个 cluster（D-222 锁定，Brown & Sandholm 2014 / Pluribus 实战值），手牌特征 = 对每个 cluster 的胜率向量；8 维。
    - **聚类输入向量总维度 = 9 维**（D-244 header `n_dims = 9`）。
    - **distribution-aware histogram**（path.md "可选" 字面，**不进**默认 `feature_set_id = 1`，仅作为 stage 4 消融对照接入；D-221 锁定）。
    - 纯 hand strength（非 potential-aware）**禁止单独** 用作聚类特征。
- **Bucket 占用**：每条街每个 bucket id 至少包含 1 个 canonical `(board, hole)` sample，**0 空 bucket**（D-236 空 cluster 切分路径保底）。
- **Bucket 内方差上限**（path.md §阶段 2 字面）：每条街每个 bucket 内手牌的 EHS std dev `< 0.05`。每条街出具 bucket 内方差直方图报告。
- **Bucket 间距下限**：每条街相邻 bucket id 间的 all-in equity 分布 EMD `≥ T_emd = 0.02`（D-233 锁定阈值），证明 bucket 不是噪声聚类。"相邻" = bucket id `(k, k+1)`；每条街 500 bucket → 499 对相邻；任一对 EMD `< 0.02` 视为聚类质量不足。
- **Bucket 序号单调性**：bucket id 与 bucket 内 EHS 中位数单调一致（id 递增 ⇒ EHS 中位数递增）。便于下游 CFR 调试和 fold/raise 频率监控；D-236b 训练完成后 cluster id 重编号为 "0 = 最弱 / N-1 = 最强" 保证此性质。
- **Clustering 重复一致性**：同 seed clustering 重复跑 `10` 次，bucket lookup table BLAKE3 哈希必须 byte-equal 一致。这是阶段 2 与阶段 1 同等强度的 **硬性 determinism SLO**——clustering 不能因运行时条件浮动。

### 4. 抽象映射性能 SLO

- **抽象映射运行时吞吐**（path.md §阶段 2 字面）：单线程 `(GameState, hole_cards) → InfoSet id` `≥ 100,000 mapping/s`。
- **Bucket lookup latency**：mmap 命中路径 `(street, board_canonical_id, hole_canonical_id) → bucket_id` `P95 ≤ 10 μs`。
- **Equity Monte Carlo**（默认 10,000 iter / hand）：`≥ 1,000 hand/s`。这条 SLO **仅用于离线 clustering 训练**，运行时映射热路径不允许触发 Monte Carlo（必须命中 lookup table）。
- 性能 SLO 走 `tests/perf_slo.rs::stage2_*`，与阶段 1 同形态：release profile + `--ignored` 显式触发，CI nightly 跑 bench-full + 短 bench 在 push 时跑。

阶段 1 的 7-card 评估器 SLO（≥10M eval/s）**间接**约束阶段 2 equity Monte Carlo——`10,000 iter / hand × 1,000 hand/s = 10M eval/s` 正好打满阶段 1 SLO；阶段 1 实测 `20.76M eval/s` 提供约 2× 缓冲。

### 5. Bucket lookup table 持久化与 schema

- **形态**（用户决策）：单一独立二进制 artifact，运行时 `mmap` 加载。该路径在阶段 6 实时搜索 lookup 表也会复用。bucket table artifact **不进 git history**（D-248 / D-251 `artifacts/` gitignore + git LFS / release artifact 分发）。
- 文件格式（D-244 锁定 80-byte 定长 header + 变长 body + 32-byte trailer，全部 little-endian；reader 通过 header 偏移表定位变长段，不依赖前段累积 size）：
    - **header**（80 bytes，8-byte aligned）：① `magic: [u8; 8] = b"PLBKT\0\0\0"`（D-240 锁定 5 字节 ASCII + 3 字节 zero pad）+ ② `schema_version: u32 = 1`（D-240）+ ③ `feature_set_id: u32 = 1`（D-240，对应 EHS² + OCHS(N=8)）+ ④ `bucket_count_flop / turn / river: u32 × 3`（preflop 固定 169 不存）+ ⑤ `n_canonical_flop / turn / river: u32 × 3`（preflop 固定 1326 不存）+ ⑥ `n_dims: u8 = 9`（D-221 EHS² + OCHS(N=8)）+ ⑦ `pad: [u8; 7] = 0`（8-byte 对齐填充）+ ⑧ `training_seed: u64`（D-237）+ ⑨ `centroid_metadata_offset / centroid_data_offset / lookup_table_offset: u64 × 3`（绝对字节偏移，reader 用此寻址，offset 越界 / 不递增 / 不 8-byte 对齐均 `BucketTableError::Corrupted`）。
    - **body**（变长，按 header §⑨ 偏移定位）：⑩ `centroid_metadata`（每条 postflop 街 × 9 维 × `(min: f32, max: f32)`）+ ⑪ `centroid_data`（每条 postflop 街 × bucket_count(street) × 9 维 × u8 quantized；D-241 锁定 u8 quantized 保证跨架构 byte-equal，反量化 `x = min + (q / 255.0) * (max - min)`，量化误差 ≤ 0.4% 远低于 D-233 `T_emd = 0.02`；按 D-236b 重编号顺序排列）+ ⑫ `lookup_table`（preflop `[u32; 1326]` D-239 / D-245 + flop `[u32; n_canonical_flop]` + turn `[u32; n_canonical_turn]` + river `[u32; n_canonical_river]`；entry = bucket id）。
    - **trailer**（32 bytes）：⑬ `blake3: [u8; 32] = BLAKE3(file_body[..len-32])`（D-243 自校验，eager 校验在 `BucketTable::open` 命中）。
- 加载错误路径（D-247 锁定 5 类，每条均需测试覆盖；继承阶段 1 §F1 错误路径模式）：
    - `BucketTableError::FileNotFound { path }`
    - `BucketTableError::SchemaMismatch { expected, got }`（含 v1 reader 拒绝 v2 文件，D-246）
    - `BucketTableError::FeatureSetMismatch { expected, got }`
    - `BucketTableError::Corrupted { offset, reason }`（含 magic bytes / BLAKE3 不匹配 / header §⑨ 偏移表不变量违反 BT-008）
    - `BucketTableError::SizeMismatch { expected, got }`（mmap 边界 / 截断文件）
- **v1 → v2 schema 兼容**：v1 reader 必须显式拒绝 `schema_version > 1` 文件（D-246 锁定，返回 `SchemaMismatch { expected: 1, got: 2 }`）。v2 reader 可选支持 v1 升级路径，stage 2 不要求 v2 reader 实现，留 stage 2 schema 第一次 bump 时决定。继承阶段 1 §5 schema_version 模式。
- **跨语言读取**：Python 端必须能完整读取 Rust 写出的 bucket table（D-249 锁定，与阶段 1 `tools/history_reader.py` 同形态 minimal proto3 decoder 风格）。`tools/bucket_table_reader.py` 至少 1k 个 canonical id → bucket id 跨语言比对一致。

### 6. 跨平台 / 确定性

- **同 toolchain + 同 seed → bucket table BLAKE3 一致**：与阶段 1 §6 同等强度。
- **跨 host 重新跑 clustering 同 seed → byte-identical bucket table**：这是阶段 2 **头号不变量**——clustering 不可重现意味着上游全部抽象不可信，下游 CFR 训练永远在调一个会动的 ground truth。
- **显式 RNG**：clustering 内部任何 k-means 初始化 / k-means++ 抽样 / EMD 距离 tie-break 必须显式接 `RngSource`（继承阶段 1 D-027 / D-050）。任何隐式 `rand::thread_rng()` 调用是阶段 2 的 P0 阻塞 bug。
- **跨架构（x86_64 ↔ aarch64）一致性**：捕获 32-seed bucket id baseline regression guard（与阶段 1 `cross_arch_hash` 同形态）。**1M 手 bucket id 跨架构 byte-identical 是 aspirational，不是阶段 2 出口门槛**（继承阶段 1 D-051 / D-052 跨架构现状）。
- **抽象层运行路径浮点边界**：
    - **不允许浮点** 进入 `(GameState, hole_cards) → InfoSet id` 的最终查表步骤。bucket id 是 `u32`，hole canonical id / board canonical id 是 `u32`。
    - **允许浮点** 在 clustering 离线训练（k-means / EMD 距离）和 equity Monte Carlo 计算路径，但浮点结果 **必须** 在写入 bucket lookup table 之前转为整数 bucket id；运行时映射只读 mmap 表，不重算浮点。
    - 工程约束：`abstraction::map` 子模块必须能通过 `cargo clippy --all-targets -- -D warnings -D clippy::float_arithmetic`（其它子模块如 `abstraction::cluster` / `abstraction::equity` 不强制）。

### 7. 与阶段 1 的不变量边界

继承阶段 1 全部不变量（**无浮点（规则路径） / 无 `unsafe` / 显式 RNG / 整数筹码 / `SeatId` 左邻一致性 / Cargo.lock 锁版本**），并在抽象层显式划分：

- 阶段 1 锁定的 `GameState` / `HandEvaluator` / `HandHistory` / `RngSource` API surface **冻结**。阶段 2 只新增上层 `ActionAbstraction` / `InfoAbstraction` / `EquityCalculator` / `BucketTable` 接口，**不修改阶段 1 类型签名**。
- 阶段 1 `pluribus_stage1_api.md` API-NNN 锁定的方法签名变化必须走阶段 1 API-NNN-revM 修订流程；阶段 2 实施期间发现 API-NNN 不够用 → 走 API-NNN-revM 显式 bump，**不允许阶段 2 [实现] agent 顺手改阶段 1 API**。
- 阶段 1 `RuleError` / `HistoryError` 错误枚举只允许追加变体，不允许移除（继承阶段 1 §修订历史 §F-rev1 错误前移到 `from_proto` 模式）。
- 阶段 2 引入的浮点（equity Monte Carlo / k-means centroids）**仅用于离线训练 + 文档输出**；运行时映射热路径必须证明限定到 `abstraction::map` 子模块的 `clippy::float_arithmetic` 检查能过。

### 8. 性能 SLO 汇总

为方便阶段 2 验收和后续阶段调用，将性能门槛集中列出：

| SLO | 阈值 | 路径 / 备注 |
|---|---|---|
| 抽象映射吞吐（运行时） | 单线程 `≥ 100,000 mapping/s` | `(GameState, hole) → InfoSet id`，path.md §阶段 2 字面 |
| Bucket lookup latency | `P95 ≤ 10 μs` | mmap 命中路径，单次查表 |
| Equity Monte Carlo（离线） | `≥ 1,000 hand/s`（10k iter / hand） | 仅 clustering 训练路径 |
| Clustering 重复一致性 | 同 seed 重复 `10` 次 BLAKE3 一致 | 头号 stage-2 不变量 |
| Bucket id determinism | 1,000,000 次重复哈希一致 | 跨线程 + 单线程 |
| Bucket 内 EHS std dev | `< 0.05` per bucket | path.md §阶段 2 字面 |
| Bucket 间 EMD | `≥ 0.02` | D-233 锁定 `T_emd = 0.02` |

## 通过标准

阶段 2 通过标准如下：

- 默认 5-action `ActionAbstraction` 在 `100,000` 个随机 `GameState` 上输出合法且非空抽象动作集合，`0` 例例外；`200+` 个固定场景与人工预期 100% 一致。
- preflop 169 lossless 等价类全部 `1326` 起手牌 100% 覆盖、无重叠（D-217 closed-form 公式）；`(hand_class_169, position_bucket, stack_bucket, betting_state)` InfoSet key `1,000,000` 次重复哈希一致；`hand_class_169` 跨 `(position_bucket, stack_bucket, betting_state)` 哈希碰撞率 `0%`。
- postflop bucket 默认 `500/500/500` 配置：每条街 `0` 空 bucket、bucket 内 EHS std dev 全部 `< 0.05`、相邻 bucket 间 EMD 全部 `≥ T_emd = 0.02`（D-233）；bucket id ↔ EHS 中位数单调一致（D-236b 重编号保证）。
- bucket lookup table 同 seed 重复 clustering `10` 次 BLAKE3 byte-equal；跨 host 重跑 clustering 同 seed byte-equal。
- 单线程抽象映射吞吐 `≥ 100,000 mapping/s`；bucket lookup `P95 ≤ 10 μs`；equity Monte Carlo `≥ 1,000 hand/s`。
- bucket table v1 → v2 schema 兼容路径覆盖；corrupted bucket table `100,000` 次 byte flip `0` panic；5 类 `BucketTableError` 错误路径全部命中。
- 阶段 1 `GameState` / `HandEvaluator` / `HandHistory` / `RngSource` 接口未受阶段 2 修改；阶段 1 全套测试（123 `#[test]` across 16 crates，默认 104 active / 19 ignored）`0 failed`，stage1-v1.0 tag 在阶段 2 任何 commit 上仍可重跑通过。
- 与外部 abstraction 参考做 sanity 对照：D-260 锁定 "**自洽性优先 + OpenSpiel 轻量对照**"——主验收依赖内部不变量（preflop 169 lossless / bucket 内方差 < 0.05 / 相邻 bucket EMD ≥ 0.02 / clustering BLAKE3 byte-equal / 1M mapping determinism）；F3 验收报告附带 OpenSpiel poker abstractions preflop 169 类与 5-action 默认配置对照（D-261 锁定口径：169 类**成员**集合相等比对，**不**要求编号顺序一致；postflop bucket 不做一一对照）。Slumbot bucket 数据获取不确定，**不强求**接入。OpenSpiel 169 类成员集合若出现 `≥ 1` 类不一致视为 stage 2 P0 bug（D-262）；bucket 数量 / postflop 边界差异不阻塞，仅在 F3 报告标注。外部对照 sanity 脚本 `tools/external_compare.py` 在 F3 [报告] 起草时一次性接入，stage 2 主线工作（A1..F2）不依赖 OpenSpiel（D-263）。

## 阶段 2 完成产物

- `ActionAbstraction` trait + `DefaultActionAbstraction`（5-action）+ off-tree mapping 占位实现。
- `InfoAbstraction` trait + `PreflopLossless169` + `PostflopBucketAbstraction`（mmap-backed）。
- `EquityCalculator`：基于阶段 1 `HandEvaluator` 的 Monte Carlo equity，支持 `EHS / EHS² / OCHS`。
- `BucketTable` 二进制格式（含 `schema_version` + `feature_set_id` + BLAKE3 自校验）+ Rust 写入器 / 读取器 + Python 跨语言读取参考（用于阶段 7 评测脚本）。
- `tools/train_bucket_table.rs` CLI：从 RngSource seed → 训练 → 写出 mmap artifact，支持 `BucketConfig` 配置不同 bucket 数。
- 一套 abstraction 测试（preflop 169 lossless / postflop bucket 质量 / 确定性 / 跨平台 / 性能 SLO / schema 兼容 / 错误路径）。
- 一份阶段 2 验收报告 `pluribus_stage2_report.md`：bucket 数量 / 内方差 / 间距 直方图 / 性能 SLO 实测值 / 关键 seed 列表 / 版本哈希（git commit + bucket table BLAKE3）/ 已知偏离。
- git tag `stage2-v1.0` + bucket table mmap artifact + Python 读取脚本同版本发布。

## 进入阶段 3 的门槛

只有当阶段 2 所有通过标准全部满足，才能进入 MCCFR 小规模验证（`pluribus_path.md` §阶段 3）。bucket 质量任何缺陷都会以 regret signal 形式被阶段 4–6 放大，事后几乎不可定位（阶段 1 出口报告 §1 同型表述：阶段 1 任何规则错误进入抽象层会被放大；阶段 2 任何 bucket 错误进入 CFR 会被进一步放大）。**阶段 2 不允许带已知 bucket 损坏 / 重复 clustering 不一致进入阶段 3**。

阶段 1 与阶段 2 共有的 carve-out（与代码合并解耦，不阻塞下一阶段起步）：

- 跨架构 1M 手一致性（仅 32-seed baseline 强制；x86 ↔ aarch64 1M 手 byte-equal 是 aspirational）。
- 24 小时夜间 fuzz 在 self-hosted runner 连续 7 天无 panic（阶段 1 `nightly.yml` GitHub-hosted matrix 已落地；阶段 2 直接挂 abstraction fuzz target）。
- 阶段 2 新增 carve-out 候选（A0 [决策] 决定是否纳入 stage 2 出口或 stage 3 起步并行）：
    - 1–14 raise size 完整配置 sweep（仅默认 5-action 强验收；扩展配置 smoke）。
    - distribution-aware histogram 特征消融对照（默认 EHS² + OCHS 强验收；histogram 留消融）。

## 参考资料

- Pluribus 主论文：https://noambrown.github.io/papers/19-Science-Superhuman.pdf  §"Action and information abstraction"
- Pluribus 补充材料：https://noambrown.github.io/papers/19-Science-Superhuman_Supp.pdf  §S2–S3
- Ganzfried & Sandholm, "Potential-Aware Imperfect-Recall Abstraction with Earth Mover's Distance in Imperfect-Information Games"（EHS² / EMD-based clustering 经典）
- Brown & Sandholm, "Strategy-Based Warm Starting for Real-Time Hold'em Poker"（OCHS 特征起源）
- OpenSpiel poker abstractions：https://github.com/google-deepmind/open_spiel
- Slumbot 公开 bucket（如可获取）作为外部参考

---

## 修订历史

本文档遵循与 `pluribus_stage1_validation.md` / `pluribus_stage1_decisions.md` §10 / `pluribus_stage1_api.md` §11 相同的 "追加不删" 约定。决策性修订仍以 `D-NNN-revM` 为主导（在 `pluribus_stage2_decisions.md` §10 修订历史落地，编号从 D-200 起以避免与 stage-1 D-NNN 冲突），本节只记录 validation.md 自身的措辞同步。

阶段 2 实施期间的角色边界 carve-out 追认（B-rev / C-rev / D-rev / E-rev / F-rev 命名风格继承阶段 1）落到 `pluribus_stage2_workflow.md` §修订历史，本节不重复记录。

- **2026-05-09（A0 [决策] 关闭同步）**：A0 [决策] 落地 `docs/pluribus_stage2_decisions.md`（D-200..D-283）+ `docs/pluribus_stage2_api.md`（API-200..API-302）+ 5 笔 review 修正 batch（commits `3f62842 / 96e3b9c / 1e57942 / 622204f / 9b7085d`，闭合 11/12 spec drift = F7+F8+F9+F10+F11+F13+F14+F15+F16+F17+F18；F12 维持不修——理论 P3，feature 精度 ~5e-3 远高于 d2 量化失效阈值 1e-12，工程不触发）。本文档 §1–§7 + §通过标准 + §SLO 汇总全部 `[D-NNN 待锁]` 占位补成实数（与 `pluribus_stage2_decisions.md` §10 修订历史首条同步）：
    - §1 action：D-200 默认 5-action `{ Fold, Check, Call, Bet/Raise(0.5×pot), Bet/Raise(1.0×pot), AllIn }`；D-201 off-tree PHM stub；D-204 `Check` 局面剔除 `Fold`；D-205 `x×pot < min_to` fallback 到 `min_to`、`>= committed + stack` fallback 到 `AllIn`；D-209 输出顺序固定。
    - §2 InfoSet key：D-210 6 桶 position（`{ BTN, SB, BB, UTG, MP, CO }`）；D-211 5 桶 stack `[0, 20) BB / [20, 50) / [50, 100) / [100, 200) / [200, +∞)`，左闭右开；D-212 5 状态 betting_state `{ Open, FacingBetNoRaise, FacingRaise1, FacingRaise2, FacingRaise3Plus }`（**preflop / postflop 共用**，从 4 桶 prior_action 扩到 5 状态以区分 BB-after-limp 与 first-in-non-BB，IA-002 / F17 修复）；D-215 统一 64-bit `InfoSetId` layout（24+4+4+3+3+26 bit）；D-217 hand_class_169 closed-form 公式（pairs 0..13 / suited 13..91 / offsuit 91..169）+ 12 条边界锚点表。
    - §3 postflop bucket：D-213 默认 `flop = 500 / turn = 500 / river = 500`；D-214 `BucketConfig` 每条街 ∈ [10, 10_000]；D-220 `iter = 10_000`；D-220a EQ-001 反对称容差按街分流（postflop 1e-9 / preflop Monte Carlo 0.005 with iter=10k）；D-221 `feature_set_id = 1` = EHS² + OCHS(N=8)，n_dims=9；D-222 OCHS `N = 8`；D-223 EHS / EHS² 计算口径；D-227 EHS² rollout（river 0 / turn 46 / flop 1081）；D-230 k-means + L2；D-233 `T_emd = 0.02`；D-235 k-means++ 量化 SCALE=2^40 + N ≤ 2_000_000；D-236b 训练完成后 cluster id 重编号为 "0 = 最弱 / N-1 = 最强"。
    - §5 bucket table：D-240 magic = `b"PLBKT\0\0\0"` 8 字节；D-241 centroid u8 quantized；D-244 80-byte 定长 header + 变长 body + 32-byte BLAKE3 trailer + §⑨ 三段绝对偏移表（解决 BT-007 byte flip 在变长段定位失败导致 panic 的结构性缺口，F11/F13 修复）；D-247 5 类 `BucketTableError`；D-249 Python 跨语言 reader。
    - §6 跨平台：D-228 RngSource sub-stream 派生协议（SplitMix64 finalizer + op_id 表，F11 修复）；D-237 `(BucketConfig, training_seed, feature_set_id)` 同组合 BLAKE3 byte-equal；D-251 `artifacts/` gitignore；D-252 `abstraction::map` 子模块 `clippy::float_arithmetic` 死锁。
    - §7 外部对照：D-260 自洽性优先 + OpenSpiel 轻量对照；D-261 preflop 169 类成员集合相等（不要求编号顺序）；D-262 `≥ 1` 类不一致视为 P0；D-263 F3 [报告] 一次性接入 `tools/external_compare.py`。
    - §通过标准 同步：preflop key 字段名 `prior_action` → `betting_state` 与 D-212 / D-215 对齐。
    - §SLO 汇总 同步：`Bucket 间 EMD` 阈值由 "D-NNN 待锁数" 锁为 `D-233 锁定 T_emd = 0.02`。
    本节首条由本 commit 落地，与 `pluribus_stage2_decisions.md` §10 修订历史首条 + `pluribus_stage2_workflow.md` §修订历史首条 + `CLAUDE.md` "stage 2 A0 closed" 状态翻面同步。
