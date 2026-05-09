# 阶段 2 实施流程：test-first 路径

## 文档目标

本文档把阶段 2（抽象层：action + information abstraction）的实施工作拆解为可执行的步骤序列。它不重复 `pluribus_stage2_validation.md` 的验收门槛，只回答一个具体问题：**在已有验收门槛的前提下，工程上按什么顺序写代码、写测试、做 review，最不容易翻车，并且能让多 agent 协作完成**。

阶段 2 与阶段 1 的最大差异：**阶段 1 有 PokerKit 做 byte-level ground truth，阶段 2 没有同等强度的开源参考**。clustering 的 bucket 边界由我们自己定，没有外部权威可对照。所以阶段 2 必须把 "内部不变量" 用满：bucket 内方差 / 间距 / 重复一致性 / preflop→flop transition 数值连续性。test-first 收益没有阶段 1 那么高，但 **determinism test-first 收益反而更高**——一旦 clustering 不可重现，阶段 4–6 全部白做。

## 总体原则

**正确性 + 确定性 test-first，性能 implementation-first**（继承阶段 1，额外强调 clustering determinism）。

- bucket 质量验收（内方差 / 间距 / 单调性）由阶段 2 自身定义，必须在 [测试] 步骤把阈值和测度方式钉死，不许 [实现] agent 顺手放宽。
- equity Monte Carlo 浮点路径与运行时整数 bucket id 路径必须 **物理隔离** 到不同子模块（`abstraction::cluster` / `abstraction::equity` 允许浮点；`abstraction::map` 禁止浮点），禁止浮点泄露到运行时映射热路径。
- 阶段 1 锁定的 `GameState` / `HandEvaluator` / `HandHistory` / `RngSource` API surface **冻结**。阶段 2 不允许顺手改阶段 1 接口；如发现确实不够用，走阶段 1 `API-NNN-revM` 修订流程。

阶段 2 的所有 bug 都会随 InfoSet / bucket 进入阶段 3+ 并被 regret signal 放大，事后几乎无法定位（与阶段 1 同型表述）。所以阶段 2 的工程预算应优先花在 "避免无知错误" 与 "守住 clustering determinism"，而不是 "做得快"。

## Agent 分工

继承阶段 1 §Agent 分工 全部表格与跨界规则：

| 标签 | Agent 类型 | 职责 | 禁止 |
|---|---|---|---|
| **[决策]** | 决策者（人 / 决策 agent） | 技术栈选型、API 契约、特征 / 聚类参数、序列化格式 | — |
| **[测试]** | 测试 agent | 写测试用例、scenario DSL、harness、benchmark 配置、bucket 质量检查器 | 修改产品代码（除测试夹具） |
| **[实现]** | 实现 agent | 写产品代码：`ActionAbstraction` / `InfoAbstraction` / `EquityCalculator` / clustering / `BucketTable` 等 | 修改测试代码 |
| **[报告]** | 报告者（人 / 报告 agent） | 跑全套测试、产出验收报告 | — |

跨界规则、`carve-out` 追认机制、`#[ignore]` full-volume 测试在下一步 [实现] 步骤实跑、CLAUDE.md 同步责任、修订历史 "追加不删" — 全部继承阶段 1 §B-rev1 / §C-rev1 / §C-rev2 / §D-rev0 / §E-rev0 / §E-rev1 / §F-rev0 / §F-rev1 / §F-rev2 提炼的处理政策。**阶段 2 §修订历史 首条新增项必须显式 carry forward 这套政策**，不重新论证。

## 工程脚手架与技术栈选择

### 沿用 Rust（继承阶段 1）

阶段 1 已锁定的 dependency 全部继承。阶段 2 候选新增依赖（A0 [决策] 锁定）：

- 自实现 k-means + EMD 距离 vs 引入 `linfa-clustering` / `kmeans` crate：D-250 锁定**自实现**——避免外部 crate 浮点行为跨版本漂移破 clustering determinism；stage 2 特征维度 ≤ 9，手工实现性能足够。
- `ndarray`：D-250 锁定**不引入**——理由同上 + 减少 dependency surface 降低 cargo audit 噪声。clustering / EMD / equity 全部用 `Vec<f32>` / `Vec<f64>` / `Vec<u8>` + 手工索引。
- `memmap2 = "0.9"`：D-255 锁定**引入**（mmap 加载是不可避免的系统接口）。
- equity Monte Carlo 仍走阶段 1 `HandEvaluator`，**不引外部** equity 库。

### Crate 布局（阶段 2 起步）

阶段 1 单 crate 多 module 已经稳定（`pluribus_stage1_workflow.md` §A0 "等接口稳定再分"）；阶段 2 仍在同一 `poker` crate 下加 module，**不分 crate**：

```
src/
├── core/             # 阶段 1 锁定，阶段 2 只读
├── rules/            # 阶段 1 锁定，阶段 2 只读
├── eval/             # 阶段 1 锁定，阶段 2 只读
├── history/          # 阶段 1 锁定，阶段 2 只读
├── error.rs          # 阶段 1 锁定；阶段 2 仅追加 BucketTableError 等枚举（不删除）
└── abstraction/      # ★ 阶段 2 新增
    ├── mod.rs
    ├── action.rs        # ActionAbstraction trait + DefaultActionAbstraction (5-action)
    ├── info.rs          # InfoAbstraction trait
    ├── preflop.rs       # PreflopLossless169
    ├── postflop.rs      # PostflopBucketAbstraction (mmap-backed)
    ├── equity.rs        # EquityCalculator (Monte Carlo + EHS / EHS² / OCHS)
    ├── feature.rs       # 特征提取（EHS² / OCHS / histogram）
    ├── cluster.rs       # k-means / EMD 距离 / clustering harness（允许浮点）
    ├── bucket_table.rs  # mmap 文件格式 + schema_version + 错误路径
    └── map/             # 运行时映射热路径子模块（禁止浮点；clippy::float_arithmetic 死锁）
        └── ...
```

`tools/`：阶段 2 新增

- `train_bucket_table.rs` CLI（offline 训练 entry point，写出 mmap artifact）
- `bucket_table_reader.py`（Python 跨语言读取参考，对照阶段 1 `tools/history_reader.py`）
- `bucket_quality_report.py`（bucket 数量 / 内方差 / 间距 直方图，CI artifact）

bucket table mmap artifact 落到 `artifacts/`（gitignore），通过 git LFS 或 release artifact 分发，**不进 git history**（阶段 2 出口 §F3 决定分发渠道）。

---

## 步骤序列

总览：`A → B → C → D → E → F`，共 6 个阶段、13 个步骤（与阶段 1 同形态）。每个阶段内部测试与实现交替推进。

```
A. 决策与脚手架        : A0 [决策] → A1 [实现]
B. 第一轮：核心场景    : B1 [测试] → B2 [实现]
C. 第二轮：聚类落地    : C1 [测试] → C2 [实现]
D. 第三轮：fuzz + 规模 : D1 [测试] → D2 [实现]
E. 第四轮：性能 SLO    : E1 [测试] → E2 [实现]
F. 收尾                : F1 [测试] → F2 [实现] → F3 [报告]
```

---

### A. 决策与脚手架

#### 步骤 A0：技术栈与 API 契约锁定 [决策]

**目标**：锁定阶段 2 全部开放决策点，给后续 [测试] / [实现] agent 一份共同 spec。

**输入**：

- `pluribus_stage2_validation.md`（本规划同 commit 落地）
- `pluribus_path.md` §阶段 2
- `pluribus_stage1_decisions.md`（D-001 … D-103，**只读**，禁止改）
- `pluribus_stage1_api.md`（API-NNN，**只读**，禁止改）
- `pluribus_stage1_report.md` §10 阶段 2 切换说明

**输出**：

- `docs/pluribus_stage2_decisions.md`（D-200 起编号，与阶段 1 D-NNN 不冲突）：
    - **D-200 系列：Action abstraction**
        - D-200：默认 5-action 集合数值（pot ratio）与 fallback 规则
        - D-201：off-tree action mapping 算法选定（占位 stub，stage 6c 完整验证）
        - D-202：`ActionAbstractionConfig` 1–14 raise size 序列化格式
    - **D-210 系列：Information abstraction**
        - D-210：preflop position bucket（默认 6）
        - D-211：preflop effective_stack bucket 边界（建议 `[20, 50, 100, 200, ∞] BB`）
        - D-212：preflop prior_action bucket 离散化（建议 `{first_in, raised_1, raised_2, raised_3plus}`）
        - D-213：postflop 默认 bucket 数（500/500/500，path.md ≥500 字面）
        - D-214：postflop `BucketConfig` 配置接口
    - **D-220 系列：Equity & 特征**
        - D-220：equity Monte Carlo 默认 iter 数（建议 10,000）+ 反对称容差
        - D-221：默认特征组合（EHS + EHS² + OCHS；distribution-aware histogram 留消融）
        - D-222：OCHS opponent cluster 数（建议 `N = 8`）
    - **D-230 系列：Clustering**
        - D-230：算法（k-means + EMD vs k-means + L2；建议 k-means + EMD）
        - D-231：初始化（k-means++ + 显式 RngSource seed）
        - D-232：收敛门槛（max_iter / centroid shift threshold）
        - D-233：bucket 间 EMD 阈值 `T_emd`（建议 ≥ 0.02）
    - **D-240 系列：Bucket table 文件格式**
        - D-240：magic bytes（候选 `b"PLBKT"`）+ schema_version 起步值
        - D-241：centroid 向量序列化（u8 quantized 推荐 vs f32 raw）
        - D-242：文件路径与命名（含 host 不敏感性）
        - D-243：BLAKE3 自校验位置与计算范围
    - **D-250 系列：Crate / 模块 / Cargo.toml**
        - D-250：是否引入 `ndarray` / 其它 crate
        - D-251：`artifacts/` 目录与 gitignore 策略
        - D-252：`abstraction::map` 子模块 `clippy::float_arithmetic` lint 配置
    - **D-260 系列：外部对照**
        - D-260：选定外部 abstraction 参考（OpenSpiel poker / Slumbot 公开 bucket / 自洽性）+ 对照口径
- `docs/pluribus_stage2_api.md`（API-200 起编号）：
    - `ActionAbstraction` trait + `DefaultActionAbstraction`
    - `InfoAbstraction` trait + `PreflopLossless169` + `PostflopBucketAbstraction`
    - `EquityCalculator` trait + `MonteCarloEquity`
    - `BucketTable` 文件格式 + `BucketTableError` 错误枚举
    - 与阶段 1 类型的桥接（如 `InfoSetId::from_game_state(state, hole, &abs) -> InfoSetId`）
    - 阶段 2 端到端示例代码（doc test 占位）
- `docs/pluribus_stage2_validation.md` §修订历史 首条 "A0 关闭后 D-200..D-260 锁定同步"：把 validation.md 中所有 `[D-NNN 待锁]` 占位补成实数。

**出口标准**：

- 上述两份新文档 commit，签字确认；后续修改走 `D-NNN-revM` / `API-NNN-revM` 流程。
- `pluribus_stage2_validation.md` 中所有 `[D-NNN 待锁]` 占位均补成实数。
- `CLAUDE.md` 状态同步翻为 "stage 2 A0 closed"，`tests/` / `src/` 未修改。

**工作量**：1 人周（阶段 2 决策项数显著多于阶段 1）。

**风险/陷阱**：

- 不要为阶段 4–6 的 "未来扩展" 预留过多 trait 层。阶段 2 接口稳定到能被阶段 3 消费即可。
- A0 不需要锁定 `tools/train_bucket_table.rs` 内部实现细节，只锁输入 / 输出契约。
- D-241 centroid 量化方式（u8 quantized vs f32 raw）决定跨架构 byte-equal 能否做到；建议默认 u8 quantized + 注释里写明 f32 raw 备选触发条件。

---

#### 步骤 A1：API 骨架代码化 [实现]

**目标**：把 A0 的 API 契约翻译成可编译代码骨架，让 [测试] agent 能写测试。

**输入**：

- `docs/pluribus_stage2_api.md`（A0 输出）
- `docs/pluribus_stage2_decisions.md`（A0 输出，只读约束）

**输出**（产品代码）：

- `src/abstraction/` 完整模块树 + trait 定义 + 全部方法签名
- 所有函数体 `unimplemented!()` / `todo!()` 占位
- `tests/api_signatures.rs` 追加阶段 2 trait 签名编译断言（与阶段 1 同形态：`!` 返回类型 trip-wire）
- `Cargo.toml` 追加阶段 2 dev-dep（如有）；`[lints]` 配置追加 `abstraction::map` 子模块的 `clippy::float_arithmetic` 限制（D-252）
- CI：`cargo build` / `cargo clippy --all-targets -- -D warnings` / `cargo doc` 全绿

**出口标准**：

- `cargo build` 通过，无 unused warning
- `cargo doc` 生成完整阶段 2 API 文档
- `tests/api_signatures.rs` 阶段 2 部分编译通过
- 没有任何真实业务逻辑，所有方法 panic
- 阶段 1 全套测试 `0 failed`（阶段 2 新模块不能引入回归）

**工作量**：0.5 人周。

**风险/陷阱**：

- 不要在 trait 上加泛型 / dyn dispatch "为未来扩展"。具体类型先行。
- `bucket_table.rs` 占位 `BucketTable::open(path)` 返回 `unimplemented!()` 即可，mmap 真正实现等 C2。
- `BucketTableError` 枚举变体在 A1 全部声明（即使未实现），让 F1 [测试] 能写 match 全覆盖测试而不依赖 F2。

---

### B. 第一轮：核心场景测试 + 实现

#### 步骤 B1：核心场景测试 + harness 骨架 [测试]

**目标**：写出第一批关键测试，建立全部 harness 基础设施。所有测试此时都失败（因 A1 是 unimplemented）。

**输入**：A1 的 API 骨架代码（**只读**）+ `docs/pluribus_stage2_api.md` + `docs/pluribus_stage2_validation.md`

**输出**（测试代码 + harness，**不修改产品代码**）：

A. **核心 fixed scenario 测试**（10–15 个，命名清晰，每个独立函数）：

- `action_abs_default_5_actions_open_raise_legal`
- `action_abs_fold_disallowed_after_check`
- `action_abs_bet_pot_falls_back_to_min_raise_when_below`
- `action_abs_bet_falls_back_to_allin_when_above_stack`
- `action_abs_determinism_repeat_smoke`（默认 1k 重复，full 1M 留 D1）
- `preflop_169_aces_canonical`
- `preflop_169_suited_offsuit_distinction`
- `preflop_169_position_changes_infoset`
- `preflop_169_stack_bucket_changes_infoset`
- `preflop_169_prior_action_changes_infoset`
- `info_abs_postflop_bucket_id_in_range`（C2 前用 stub bucket）
- `info_abs_determinism_repeat_smoke`（默认 1k 重复，full 1M 留 D1）

B. **Preflop 169 lossless 完整覆盖测试**（独立测试 crate `tests/preflop_169.rs`）：

- 枚举全部 1326 起手牌，断言：每个被映射到恰好 1 个 169 类、169 类总 hole 计数与组合数学一致（pairs 6 / suited 4 / offsuit 12，总和 1326）。
- preflop 169 是阶段 2 信任锚，**B1 必须完整覆盖**，不能拖到 C1。

C. **Equity Monte Carlo 自洽性 harness**（`tests/equity_self_consistency.rs`）：

- 反对称：`equity(A, B) + equity(B, A) ≈ 1`（容差由 D-220 锁定）
- preflop 169 类 EHS 单调性 smoke：AA 最高、72o 最低
- 阶段 2 不接入外部参考；自洽即可

D. **Determinism harness 骨架**（不开火，留待 C1 / D1 接入完整断言）：

- 同 seed clustering 重复 → bucket table 字节比对 stub
- 跨线程 bucket id 一致 stub

E. **Benchmark harness 骨架**（无 SLO 断言，留待 E1 接入）：

- criterion 配置接入阶段 2 mapping path
- 占位 benchmark：单次 InfoSet mapping、单次 equity Monte Carlo

**出口标准**：

- A 类测试编译通过、运行失败（因 `unimplemented!()`）
- B 类（preflop 169 lossless 1326 → 169 枚举）能用 stub 跑通流程；至少枚举正确性测试可独立通过（不依赖产品 stub 之外的实现）
- C / D / E 类 harness 能跑出占位结果或断言失败，流程不 panic
- 阶段 1 全套测试 `0 failed`

**工作量**：1.5 人周。

**风险/陷阱**：

- 不要一次写完所有 200+ scenario。先这 10–15 个驱动 API；后续 C1 再批量补（与阶段 1 §B1 同形态：B1 写 10 个 driving，C1 写 200+）。
- equity Monte Carlo 反对称容差 **必须 [决策] 锁定**（D-220），不要 [测试] 自己拍数。
- 不要在 B1 写 postflop bucket 质量阈值断言（C1 才接）。
- 不要在 B1 写 1M 完整 determinism（D1 才接）。

---

#### 步骤 B2：实现 pass 1，让 B1 全绿 [实现]

**目标**：用最朴素实现让 B1 全部通过。**只追求正确性，不追求性能**。

**输入**：B1 的测试代码（**只读**）+ A1 的 API 骨架（**修改产品代码以填充实现**）

**输出**（产品代码，**不修改测试**）：

- `DefaultActionAbstraction`：默认 5-action + 完整 fallback 规则（D-200）
- `PreflopLossless169`：1326 → 169 等价类完整映射（纯 combinatorial，禁止 Monte Carlo / 浮点）+ position / stack / prior_action 复合 InfoSet key
- `PostflopBucketAbstraction` **占位实现**（C2 才完整）：每条街固定返回 `bucket_id = 0`，但接口签名匹配，B1 类 A 的 in-range 断言能过
- `MonteCarloEquity`：朴素 Monte Carlo（默认 10k iter）调用阶段 1 `HandEvaluator`
- `EHSCalculator`：EHS / EHS² 朴素实现

**出口标准**：

- B1 全部测试通过（含 preflop 169 lossless 1326 → 169 枚举）
- equity Monte Carlo 反对称误差在 D-220 容差内
- 阶段 1 全套测试仍 `0 failed`（默认 + ignored 套件未受影响）

**工作量**：1.5–2 人周。

**风险/陷阱**：

- preflop 169 lossless 实现必须 **纯 combinatorial**（位运算 / 排序 + 表查），**禁止** Monte Carlo / 浮点；否则 1M 重复一致性测试在 D1 会暴露 nondeterminism。
- equity Monte Carlo 朴素实现性能很烂没关系，E2 处理。阶段 1 §B2 / §C2 / §D2 同型规则：性能在 E2，不在 B2 / C2 / D2。
- `PostflopBucketAbstraction` 占位实现的 `bucket_id = 0` 必须配 `// TODO(C2): replace stub with mmap lookup` 注释，避免 C1 测试误判其为 "已实现"。

---

### C. 第二轮：聚类落地

#### 步骤 C1：postflop 聚类质量测试 [测试]

**目标**：把测试从 B 阶段的 in-range smoke 扩展到 `pluribus_stage2_validation.md` §3 全部 bucket 质量门槛。

**输入**：B2 的实现（**只读**）+ `pluribus_stage2_validation.md`

**输出**（测试代码，**不修改产品代码**）：

- `tests/bucket_quality.rs`：
    - 每条街每个 bucket 至少 1 个 canonical sample（0 空 bucket）
    - 每条街每个 bucket 内 EHS std dev `< 0.05`
    - 每条街相邻 bucket 间 EMD `≥ T_emd`（D-233）
    - bucket id ↔ EHS 中位数单调一致
    - 1k 手 `(board, hole) → bucket id` smoke + `#[ignore]` 1M 完整版（C2 / D2 跑）
- `tests/clustering_determinism.rs`：
    - 同 seed clustering 重复 10 次 bucket table BLAKE3 一致
    - 跨线程 InfoSet mapping 一致
    - 跨架构 32-seed bucket id baseline regression guard（与阶段 1 `cross_arch_hash` 同形态）
- `tests/equity_features.rs`：
    - EHS² / OCHS 特征自洽（反对称 / 单调 / 边界）
    - OCHS opponent cluster 数与 D-222 一致
- `tests/scenarios_extended.rs`（阶段 2 版）：扩到 200+ 固定 `GameState` 场景，覆盖 open / 3-bet / 短码 / incomplete / 多人 all-in 的 5-action 默认输出
- `tools/bucket_quality_report.py`：bucket 数量 / 内方差 / 间距 直方图，CI artifact 输出

**出口标准**：

- 所有 C1 测试编译通过
- 部分测试预期失败（B2 stub bucket 不可能过 EHS std dev 门槛）— 留给 C2 修
- preflop 169 lossless 全套保持全绿（C1 不动 preflop 信任锚）

**工作量**：1.5 人周。

---

#### 步骤 C2：实现 pass 2，让 C1 全绿 [实现]

**目标**：补全 B2 stub 的 postflop bucket 实现，落地 mmap-backed bucket table。

**输入**：C1 的测试代码（**只读**）+ A0 锁定的 D-220 / D-230 / D-240

**输出**（产品代码，**不修改测试**）：

- `EquityCalculator` 完整 EHS² / OCHS 计算（朴素实现，性能 E2）
- `cluster.rs` k-means + EMD 距离实现（D-230 锁定算法 + D-231 k-means++ 显式 RngSource 初始化 + D-232 收敛门槛）
- `tools/train_bucket_table.rs` CLI：从 RngSource seed → 训练 bucket table → 写出 mmap artifact
- `BucketTable::open(path)` mmap 加载 happy path 实现（错误路径 F2）
- `PostflopBucketAbstraction::map(...)` 完整实现（mmap lookup）
- bucket table v1 schema 落地（D-240 锁定字段顺序 + D-241 centroid 量化）
- bucket table v1 artifact 与 stage-2 commit 同 PR 落到 `artifacts/`（gitignore）+ release artifact 候选（F3 决定分发渠道）

**出口标准**：

- C1 全部测试通过
- bucket table 默认 500/500/500 配置同 seed clustering BLAKE3 byte-identical（重复 10 次）
- 1M `#[ignore]` 完整版测试在 release profile 跑通（与阶段 1 §C2 / §D2 同形态：[实现] agent 在闭合前实跑 `--ignored` 全集合）
- 阶段 1 全套测试仍 `0 failed`

**工作量**：2–3 人周（k-means + EMD 自实现 + mmap 文件格式落地是阶段 2 主体工作量）。

**风险/陷阱**：

- k-means 初始化 / k-means++ 抽样 / EMD 距离 tie-break 必须用 `RngSource` 显式注入（继承阶段 1 D-027 / D-050）；任何隐式 `rand::thread_rng()` 是 P0 阻塞 bug。
- centroid 量化（D-241）若选 f32 raw，跨架构 byte-equal 可能破。建议 D-241 锁定 u8 quantized；如选 f32，必须配跨架构对照测试。
- `PostflopBucketAbstraction::map(...)` 必须保证浮点不进入 `abstraction::map` 子模块——所有浮点计算在 `cluster.rs` / `equity.rs` 完成，只把整数 bucket id 写入 mmap 表。

---

### D. 第三轮：fuzz + 规模

#### 步骤 D1：fuzz 完整版 + 规模化测试 [测试]

**目标**：用规模化 fuzz 把 "概率性 bug" 挤出来。

**输入**：C2 的实现（**只读**）

**输出**（测试代码 + CI 配置，**不修改产品代码**）：

- `fuzz/abstraction_smoke`：cargo-fuzz target 跑 1M 次随机 `(board, hole) → bucket id`，断言 in-range + determinism
- `tests/abstraction_fuzz.rs`：
    - 1M 次 InfoSet mapping 重复一致（默认 100k smoke + `#[ignore]` 1M）
    - 1M 个随机 `ActionAbstractionConfig` 1–14 raise size → 输出确定性
    - 100k 个随机 off-tree `real_bet` → 抽象动作映射稳定（占位实现层面，stage 6c 才完整）
- `tests/clustering_cross_host.rs`：跨架构 32-seed bucket table baseline regression guard（与阶段 1 `cross_arch_hash` 同形态）
- CI：每次 push 跑 100k 次 abstraction smoke fuzz（5 分钟内）；nightly 跑 1M 完整版 + bucket lookup throughput baseline

**出口标准**：

- 所有测试编译通过
- 运行后通常会暴露 1–3 个 corner case bug（off-tree action 边界 / k-means 浮点 NaN / EMD 退化分布 / mmap 文件 layout overflow）— 列入 issue 移交 D2

**工作量**：0.5–1 人周。

---

#### 步骤 D2：修 fuzz 暴露的 bug [实现]

**目标**：修复 D1 暴露的所有 bug，达到 1M 抽象映射 0 不一致 / 0 panic。

**输入**：D1 的测试代码 + 运行结果（**只读测试**）

**输出**（产品代码，**不修改测试**）：

- 修复 fuzz 暴露的所有 nondeterminism / 边界 bug
- Action abstraction off-tree mapping 占位实现（D-201 算法 stub，stage 6c 才完整）
- 如发现 `BucketTable` 文件格式或 `BucketTableError` 变体不够用，走 `D-NNN-revM` / `API-NNN-revM` 流程显式 bump（参考阶段 1 §D-rev0 D-037-rev1 / D-039-rev1 处理流程）

**出口标准**：

- `pluribus_stage2_validation.md` §6 跨平台 / 确定性 全部通过
- CI 100k 次 abstraction fuzz 在 5 分钟内 0 违反
- 1M 次 nightly fuzz 0 panic / 0 invariant violation

**工作量**：0.5–1 人周。

---

### E. 第四轮：性能 SLO

#### 步骤 E1：benchmark + SLO 断言 [测试]

**目标**：建立性能门槛断言。此时 SLO 大概率达不到（B2 / C2 用的是朴素实现），断言会失败 — 留给 E2 优化。

**输入**：D2 的实现（**只读**）+ `pluribus_stage2_validation.md` §8 SLO 汇总

**输出**（测试代码 + CI 配置，**不修改产品代码**）：

- criterion benchmark：
    - `abstraction/info_mapping`：`(GameState, hole) → InfoSet id`
    - `abstraction/bucket_lookup`：`(street, board, hole) → bucket_id`（mmap 命中）
    - `abstraction/equity_monte_carlo_10k_iter`
- SLO 断言（`tests/perf_slo.rs::stage2_*`）：
    - 抽象映射 `≥ 100,000 mapping/s` 单线程
    - bucket lookup `P95 ≤ 10 μs`
    - equity Monte Carlo `≥ 1,000 hand/s`（10k iter / hand）
- CI 短 benchmark（30 秒内）+ 全量 nightly + criterion baseline 对照

**出口标准**：

- 所有 SLO 断言为 "待达成" 状态
- benchmark 能跑出当前数据但断言失败

**工作量**：0.5 人周。

---

#### 步骤 E2：性能优化到 SLO [实现]

**目标**：让 E1 的 SLO 断言全部通过，**且不破坏正确性测试**。

**输入**：E1 的 benchmark + SLO 断言（**只读**）+ 当前 benchmark 数据

**输出**（产品代码，**不修改测试**）：

- bucket lookup hot path 内存布局优化（cache-friendly canonical id 编码）
- equity Monte Carlo 多线程 + SIMD 优化（如必要）
- preflop 169 mapping 走 `[u8; 1326]` 直接表（替代任何条件分支）
- `abstraction::map` 子模块持续守住 `clippy::float_arithmetic` 死锁（性能优化不允许引入浮点）

**出口标准**：

- E1 所有 SLO 断言通过
- B / C / D 全套测试仍然全绿（**性能优化引入正确性回归是阶段 1 / 阶段 2 同样最常见的翻车场景**——见阶段 1 §E-rev1）
- 1M 次 abstraction fuzz 重跑 0 违反
- 阶段 1 全套测试仍 `0 failed`

**工作量**：1.5–2 人周。

**风险/陷阱**：

- bucket lookup 优化时小心浮点泄露——任何 `f32` / `f64` 进入 hot path 会破跨架构一致性。
- preflop position bucket / stack bucket 离散化不能引入分支预测失败热点（如 `match` 链转 `[u8; ...]` 表查）。
- 阶段 1 E2 同型经验：apply 路径去 clone + 评估器换 bitmask 顺带让 1M fuzz / 1M determinism 等正确性测试加速 5–24×；阶段 2 E2 也应同时观察 D1 / C1 完整套件耗时——若 E2 让正确性套件**变慢**而非变快，是优化方向选错的早期信号。

---

### F. 收尾

#### 步骤 F1：兼容性 + 错误路径测试 [测试]

**目标**：补完最后一类测试 — schema 兼容性和异常输入。

**输入**：E2 的实现（**只读**）

**输出**（测试代码，**不修改产品代码**）：

- `tests/bucket_table_schema_compat.rs`：v1 → v2 schema 兼容性（写一个 v1 bucket table，用 v2 代码读取，验证升级或拒绝路径）
- `tests/bucket_table_corruption.rs`：byte flip 100k 次 0 panic + 5 类错误（`FileNotFound` / `SchemaMismatch` / `FeatureSetMismatch` / `Corrupted` / `SizeMismatch`）覆盖
- `tests/off_tree_action_boundary.rs`：1M 个边界 `real_bet`（0 / 1 / chip max / overflow / negative-after-cast）→ 抽象映射稳定
- `tests/equity_calculator_lookup.rs`：iter=0 / iter=1 / iter=u32::MAX 边界（与阶段 1 `evaluator_lookup.rs` 同形态）

**出口标准**：所有测试编译通过；部分会失败留给 F2。

**工作量**：0.3 人周。

---

#### 步骤 F2：兼容性升级器 + 错误处理 [实现]

**目标**：让 F1 全绿。

**输入**：F1 的测试代码（**只读**）

**输出**（产品代码，**不修改测试**）：

- bucket table schema 升级器或显式拒绝路径
- 5 类 `BucketTableError` 错误路径完整实现
- off-tree action 边界硬化
- equity calculator 边界硬化

**出口标准**：F1 全绿。如发现 corruption / schema 错误前移到 `BucketTable::open` 阶段比留在 `map(...)` 路径更合理，参考阶段 1 §F-rev1 "错误前移到 `from_proto`" 模式落地。

**工作量**：0.3 人周。

---

#### 步骤 F3：验收报告 [报告]

**目标**：阶段 2 收尾，产出可交接的验收报告。

**输入**：

- 全部测试的最新运行结果（默认 + `--ignored` + nightly fuzz）
- git history
- bucket table mmap artifact + BLAKE3 哈希
- `tools/bucket_quality_report.py` 输出的直方图

**输出**（文档）：

- `docs/pluribus_stage2_report.md`（与 `pluribus_stage1_report.md` 同体例）：
    - 测试手数 / fuzz 次数 / clustering 重复次数
    - 错误数（应为 0，否则解释）
    - bucket 数量 / 内方差 / 间距 直方图（每条街一份）
    - 性能数据（所有 SLO 实测值）
    - 关键 seed 列表
    - 版本哈希（git commit + bucket table BLAKE3）
    - 已知偏离 / carve-out 现状（含跨架构 1M aspirational / 24h fuzz 7 天 self-hosted carve-out 继承 / off-tree 完整验证 stage 6c）
- git tag `stage2-v1.0`
- bucket table mmap artifact + Python 读取脚本一并发布（D-242 锁定分发渠道）

**出口标准**：验收文档所有通过标准全部满足；报告 review 通过；阶段 1 全套测试在 stage2-v1.0 commit 上仍 `0 failed`。

**工作量**：0.4 人周。

---

## 反模式（不要做）

继承 `pluribus_stage1_workflow.md` §反模式 全部条款（**不要 [测试] agent 修改产品代码 / 不要 [实现] agent 修改测试代码 / 不要过早抽象 / 不要先优化再正确 / 不要隐式全局 RNG / 不要浮点参与规则路径 / 不要过早分 crate**），叠加阶段 2 专属：

- **bucket clustering 跑出 "不可重现" 结果就放过**：是阶段 2 头号必修 bug，不许 "反正下次会一样" 过关。同 seed BLAKE3 不一致的任何案例都是 P0。
- **浮点泄露到运行时映射热路径**：clustering / equity 离线浮点 OK；运行时 `abstraction::map` 子模块必须纯整数。`cargo clippy -D clippy::float_arithmetic`（限定 `abstraction::map`）必须能过。
- **bucket 数量配置变更时不重新跑 1M determinism**：`BucketConfig` 改一次 → 全套 determinism + 1M fuzz 重跑一次，否则 `schema_version` 不能 bump。
- **OCHS opponent cluster 数从 [测试] 反推**：D-222 锁定后不可由 [测试] agent 私改。
- **跳过 preflop 169 lossless**：阶段 2 的 "信任锚"，B1 必须完整覆盖；不允许 "C1 再补"。
- **预 overengineer trait + dyn dispatch "为阶段 4–6 准备"**：A1 / B2 不允许；阶段 1 §反模式 同型经验。
- **顺手改阶段 1 API**：阶段 2 [实现] agent 发现 `pluribus_stage1_api.md` API-NNN 不够用 → 走 API-NNN-revM 修订流程，**不允许直接改阶段 1 类型签名 / 删除 / 重命名**。

## 阶段 2 出口检查清单

进入阶段 3 前必须满足以下全部条件：

- [ ] 验收文档 `pluribus_stage2_validation.md` 通过标准全部满足
- [ ] 阶段 2 验收报告 `pluribus_stage2_report.md` commit
- [ ] CI 在 main 分支 100% 绿，含：默认单元测试 + 100k abstraction fuzz + clustering determinism + bucket lookup SLO 断言 + 阶段 1 全套测试无回归
- [ ] 24 小时 nightly abstraction fuzz 连续 7 天无 panic / 无 invariant violation（继承阶段 1 carve-out 形态：GitHub-hosted matrix 必须落地；self-hosted runner 7 天与代码合并解耦）
- [ ] bucket table mmap artifact + Python 读取脚本与阶段 2 commit 同版本发布（D-242 决定分发渠道）
- [ ] git tag `stage2-v1.0`，对应 commit + bucket table BLAKE3 写入报告
- [ ] 阶段 1 全套测试 `0 failed`（阶段 2 不允许引入阶段 1 回归；stage1-v1.0 tag 在阶段 2 任何 commit 上仍可重跑通过）

## 时间预算汇总

| 步骤 | Agent 类型 | 工作量 |
|---|---|---|
| A0. 决策与契约 | [决策] | 1 周 |
| A1. API 骨架 | [实现] | 0.5 周 |
| B1. 核心测试 + harness | [测试] | 1.5 周 |
| B2. 实现 pass 1 | [实现] | 1.5–2 周 |
| C1. 聚类质量测试 | [测试] | 1.5 周 |
| C2. 实现 pass 2（k-means + bucket table） | [实现] | 2–3 周 |
| D1. fuzz 完整版 | [测试] | 0.5–1 周 |
| D2. 修 fuzz bug | [实现] | 0.5–1 周 |
| E1. benchmark + SLO | [测试] | 0.5 周 |
| E2. 性能优化 | [实现] | 1.5–2 周 |
| F1. 兼容性测试 | [测试] | 0.3 周 |
| F2. 兼容性实现 | [实现] | 0.3 周 |
| F3. 验收报告 | [报告] | 0.4 周 |

按 agent 类型汇总：

| Agent 类型 | 累计工作量 |
|---|---|
| [测试] | 4.3–5.3 周 |
| [实现] | 6.3–9.3 周 |
| [决策] + [报告] | 1.4 周 |
| **总计** | **12–16 周** |

与 `pluribus_path.md` 中 "阶段 2：2–3 人月" 估算吻合。阶段 1 实测 11.5–15 周区间内闭合（含 9 条 rev 修订 + 105 条 cross-validation 分歧修复 + 多核 host carve-out）；阶段 2 因 clustering 自实现 + bucket table mmap 落地工作量略高，预算上调至 12–16 周。如 [测试] / [实现] 两类 agent 在某些步骤可并行（如 C1 与 D1 部分准备工作可与 B2 / C2 重叠），实际墙钟时间可压缩到 9–13 周。

## 参考资料

- 阶段 2 验收门槛：`pluribus_stage2_validation.md`
- 整体路径与各阶段总览：`pluribus_path.md`
- 阶段 1 实施流程（test-first 路径，13-step 模板源头）：`pluribus_stage1_workflow.md`
- 阶段 1 9 条修订历史：`pluribus_stage1_workflow.md` §修订历史 — 处理政策与 carve-out 模板
- 阶段 1 验收报告：`pluribus_stage1_report.md` §10 阶段 2 切换说明 — 阶段 2 起步前必读
- Pluribus 主论文 §"Action and information abstraction"：https://noambrown.github.io/papers/19-Science-Superhuman.pdf
- Ganzfried & Sandholm, "Potential-Aware Imperfect-Recall Abstraction with Earth Mover's Distance"
- Brown & Sandholm, "Strategy-Based Warm Starting for Real-Time Hold'em Poker"
- OpenSpiel poker abstractions：https://github.com/google-deepmind/open_spiel

---

## 修订历史

阶段 2 实施过程中的角色边界 carve-out / `D-NNN-revM` / `API-NNN-revM` 配套追认 / 关键决策同步均在本节按时间线追加，遵循阶段 1 §修订历史 同样 "追加不删" 约定。

格式参考阶段 1 §B-rev1（B2 关闭后角色边界追认）/ §C-rev1（C2 关闭无产品代码改动 + carve-out）/ §C-rev2（carve-out 测试落地 + 实跑暴露 bug）/ §D-rev0（D2 修分歧 + scenario 测试 carve-out 追认）/ §E-rev0（E1 多核 SLO 1-CPU host carve-out）/ §E-rev1（E2 性能转绿同时正确性套件加速）/ §F-rev0（F1 错误路径结构性缺位 carve-out）/ §F-rev1（F2 错误前移到 from_proto）/ §F-rev2（F3 报告落地）。

#### A0 关闭（2026-05-09）— A-rev0

A0 [决策] 关闭。同 commit 落地：

- `docs/pluribus_stage2_decisions.md`（D-200..D-283 全锁定数值；含 D-220a / D-236b / D-228 sub-stream 派生协议）
- `docs/pluribus_stage2_api.md`（API-200..API-302 trait + 类型契约 + `EquityCalculator::equity_vs_hand` pairwise 接口 / `BucketTable` 80-byte header 偏移表 / `abstraction::cluster::rng_substream` 公开 contract）
- `docs/pluribus_stage2_validation.md` §1–§7 + §通过标准 + §SLO 汇总 全部 `[D-NNN 待锁]` 占位补成实数（与 §修订历史 首条同步）
- 本文档 §修订历史 首条（即本条）carry forward 阶段 1 处理政策
- `CLAUDE.md` 状态翻 "stage 2 A0 closed"

A0 起步起 review 子 agent 共发现 12 处独立 spec drift（F7..F18），通过 5 笔 commit 落地 11 处修正（F12 维持不修，理论 P3 工程不触发）：

| commit | batch | 修正主题 |
|---|---|---|
| `3f62842` | batch 1 | F7 / F8 / F9 / F17 — InfoSet 编码 + 类型一致性（D-215 统一 64-bit layout / `StreetTag` vs `Street` 隔离 / `BettingState` 5 状态展开 / `position_bucket` 4 bit 支持 2..=9 桌大小） |
| `96e3b9c` | batch 2 | F11 / F13 — RngSource sub-stream 派生协议（D-228 SplitMix64 finalizer + op_id 表）+ bucket table header 80-byte 偏移表（D-244 §⑨ 解决 BT-007 byte flip 变长段定位 panic） |
| `1e57942` | batch 3 | F14 — D-217 169 hand class closed-form 公式 + 12 条边界锚点表（B1 [测试] 在 [实现] 之前直接基于公式枚举断言） |
| `622204f` | batch 4 | F10 / F15 / F16 — D-206 fold-collapsed `AllIn` `betting_state` 转移澄清 / D-235 N ≤ 2_000_000 + 量化 SCALE=2^40 / D-243 schema_version vs BLAKE3 reproducibility 耦合标注（v1 only 不解决，stage 3 hook） |
| `9b7085d` | batch 5 | F18 — D-220a / EQ-001 `equity_vs_hand` pairwise 接口（反对称只在 pairwise 路径成立——`equity(hole, board, rng)` random-opp 数学上不满足反对称） |

A0 carry forward 阶段 1 处理政策清单（在 §B-rev1 / §C-rev1 / §D-rev0 / §F-rev1 提炼）：

- §B-rev1 §3：[实现] 步骤越界改测试 → 当 commit 显式追认；不静默扩散到下一步。
- §B-rev1 §4：每个步骤关闭后必须有一笔 `docs(CLAUDE.md): X 完成后状态同步` 把仓库状态、出口数据、修订历史索引补齐。
- §C-rev1：零产品代码改动的 [实现] 步骤同样需要书面 closure；测试规模扩展属于 [测试] 角色，即使 "只是改个常数"。
- §D-rev0 §1–§3：`D-NNN-revM` 翻语义时主动评估测试反弹；carve-out 范围最小化；测试文件改名 / 删除 / 大幅重写仍属 [测试] 范畴。
- §F-rev1：错误前移到序列化解码阶段（如 `from_proto` / `BucketTable::open`）是 [实现] agent 单点不变量收口的优选模式。

A0 角色边界审计：仅修 `docs/` 下 4 份文档（`pluribus_stage2_decisions.md` 起草 + `pluribus_stage2_api.md` 起草 + `pluribus_stage2_validation.md` 占位补完 + 本文档 §修订历史 首条）+ `CLAUDE.md` 状态翻面；`src/` / `tests/` / `benches/` / `fuzz/` / `tools/` / `proto/` **未修改一行**——A0 [决策] role 0 越界（继承阶段 1 §F-rev2 / §F-rev0 / §C-rev1 0 越界形态）。

补记（§A-rev1 batch 7 时同步）：A0 关闭后另一轮 review（commit `35df4f4`）落地 batch 6 修正 9 处独立 spec drift（F19..F27），其中 7 处涉及 API 签名 / 不变量收紧（详见 `pluribus_stage2_api.md` §修订历史 batch 6）+ 决策侧 9 条 D-NNN-revM（`pluribus_stage2_decisions.md` §修订历史 batch 6）。本 §A-rev0 段落起草于 commit `452fb89`（A0 闭合同步），未及更新到 batch 6 list；按 stage-1 §修订历史 「追加不删」 约定，batch 6 落地见 `CLAUDE.md` Stage 2 A0 closed 段落表格。

#### A1 关闭（2026-05-09）— A-rev1

A1 [实现] 关闭于 commit `c4107ee`（commit message 「A1 [实现] 关闭 — abstraction/ 模块树骨架 + api_signatures trip-wire + memmap2 + D-228/D-252 公开 contract」）。同 commit 落地：

- `src/abstraction/` 完整 10 文件模块树（mod / action / info / preflop / postflop / equity / feature / cluster / bucket_table / map）
- 全部公开类型 / trait / 方法签名严格匹配 `pluribus_stage2_api.md` API-200..API-302（含 batch 6 一组 rev：AA-003-rev1 / AA-004-rev1 / IA-006-rev1 / EQ-001-rev1 / EQ-002-rev1 / BT-005-rev1 / BT-008-rev1 / EquityCalculator-rev1 / BetRatio::from_f64-rev1 / `BucketTable::lookup` 签名 3 → 2 入参）
- 全部函数体 `unimplemented!()` / `todo!()` 占位（`BucketConfig::default_500_500_500()` 与 `BetRatio::HALF_POT / FULL_POT` 等 `const` 路径直接给值）
- `tests/api_signatures.rs` 追加 stage 2 trip-wire（覆盖 50+ 个公开 fn / 常量绑定，与 stage-1 `_api_signature_assertions()` 同形态）
- `Cargo.toml` 加 `memmap2 = "0.9"`（D-255）
- `src/abstraction/map/mod.rs` 顶 `#![deny(clippy::float_arithmetic)]` inner attribute（D-252）
- `src/abstraction/cluster.rs` 落地 `pub mod rng_substream { ... }`（D-228 公开 contract，含 `derive_substream_seed` 函数 + 全 15 个 op_id 常量）
- `src/lib.rs` D-253-rev1 顶层 re-export 21 个公开类型 / trait / helper + 1 个子模块（`cluster::rng_substream`）
- `CLAUDE.md` 状态翻 "stage 2 A1 closed"

A1 出口数据（commit `c4107ee` 实测）：`cargo build --all-targets` ok / `cargo clippy --all-targets -- -D warnings` ok / `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` ok / `cargo fmt --all --check` ok / `cargo test`（默认）104 passed / 19 ignored / 0 failed across 16 test crates（与 stage-1 baseline byte-equal，A1 不引入测试回归——抽象层 fail-on-call 行为留待 B1 [测试] 测试代码触发）。

A1 角色边界审计：仅写 / 改 stage 1 锁定外的产品代码与配置（10 新文件 `src/abstraction/**` + `src/lib.rs` re-export + `Cargo.toml` 依赖 + `tests/api_signatures.rs` 追加 stage 2 trip-wire）；`src/core/` / `src/rules/` / `src/eval/` / `src/history/` / `src/error.rs` / `proto/` / `benches/` / `fuzz/` / `tools/` **未修改一行**——A1 [实现] 0 越界。`tests/api_signatures.rs` 触动以 §A1 §输出 第 3 条「同 commit 同步签名 trip-wire」+ 继承 stage-1 §B-rev1 §3「测试 trip-wire 同步责任由 [实现] 承担」处理政策追认（避免 B1 [测试] 起步前签名漂移可能性，相同 commit 同步原则与 stage-1 同型）。

##### A1 关闭后 review 措辞收尾 batch 7

A1 闭合 commit `c4107ee` 落地后，review 抽查发现 4 处文档措辞观察（O1..O4），其中 3 处属 doc-only 修正、1 处保留（B2 占位 forward-looking 不动）。本 batch **0 spec 变化、0 公开签名变化、0 不变量变化、0 测试回归、0 角色越界**——仅同步 doc / 注释 / CLAUDE.md 措辞，不走 `API-NNN-revM` 流程（无 API 契约改动）。

| 观察 | 类型 | 处理 |
|---|---|---|
| O1：`pluribus_stage2_api.md` §2 `InfoAbstraction::map` trait doc + §F21 carve-out 文 + §F21 影响 ④ 三处把 stage 1 `GameState::config()` getter rev 触发责任压在 A1，与实现现实（A1 选择保守 defer）冲突 | doc-only | 三处统一改为「B2 [实现] 在落地实际逻辑时触发，A1 阶段仅产签名编译不依赖该 getter」。详见 `pluribus_stage2_api.md` §修订历史 batch 7 |
| O2：`src/abstraction/mod.rs` line 14/16「模块私有」简写措辞与 `pub mod feature; pub mod cluster;` 声明语义不符 | doc-only（rust 注释）| 改写为「D-254 不在 `lib.rs` 顶层 re-export，仅经 `poker::abstraction::*` 路径访问」与同文件 line 22-24 解释段落一致 |
| O3：`PreflopLossless169 { _opaque: () }` / `PostflopBucketAbstraction { table, _opaque: () }` 用 `_opaque: ()` 字段做 opaque marker | 风格 | **保留不修**——`_opaque: ()` 是 B2 即将填充真实状态字段的命名 struct 占位，改成 unit / tuple struct 在 B2 又要换回命名字段 struct 纯属 churn |
| O4：`CLAUDE.md` line 136 / 172「全 14 个 stage 2 类型 / trait / helper」计数不准，实际 21 项 + 1 子模块 | doc-only | 改为精确计数 21（action 7 + info 4 + preflop 2 + postflop 2 + equity 3 + bucket_table 3）+ 1 子模块（`cluster::rng_substream`）|

batch 7 触发文件：`docs/pluribus_stage2_api.md`（§2 trait doc + §F21 两处 + §修订历史 batch 7 子节）+ `src/abstraction/mod.rs`（line 14/16 注释）+ `CLAUDE.md`（line 136 / 172 + A1 closed 段落补 batch 7 行）+ 本文档 §A-rev1 batch 7 子节（即本节）。

A1 + batch 7 角色边界审计：`src/abstraction/{action,info,preflop,postflop,equity,bucket_table,cluster,feature,map/mod}.rs` 公开 trait / 类型 / 方法签名 / `unimplemented!()` 占位 / `#![deny(clippy::float_arithmetic)]` inner attr / D-228 op_id 常量 **未修改一行**；`tests/api_signatures.rs` trip-wire **未修改一行**；`Cargo.toml` / `Cargo.lock` 依赖列表 **未修改一行**——0 公开签名漂移、0 trip-wire 漂移、0 测试回归。

下一步：B1 [测试]（核心场景测试 + harness 骨架）→ B2 [实现] → C1 [测试] / C2 [实现]（聚类落地）→ D1 / D2（fuzz + 规模）→ E1 / E2（性能 SLO）→ F1 / F2 / F3（收尾），按 §步骤序列 13-step 顺序推进。

#### B1 关闭（2026-05-09）— B-rev0

B1 [测试] 关闭。同 commit 落地 §B1 §输出 全部 5 类：

- **A 类核心 fixed scenario 测试**（10 + 7 = 17 个 `#[test]`，跨 `tests/action_abstraction.rs` + `tests/info_id_encoding.rs` 两文件）：
    - `tests/action_abstraction.rs`：`action_abs_default_5_actions_open_raise_legal` / `action_abs_fold_disallowed_after_check` / `action_abs_bet_pot_falls_back_to_min_raise_when_below`（按 default 100BB 配置下 0.5×pot 几乎总满足 ≥ min_to 的工程现实，断言改为**结构性不变量** `Raise.to ≥ min_to`，与 AA-003-rev1 ① 等价；具体数值 fallback 场景留 C1 200+ scenarios）/ `action_abs_bet_falls_back_to_allin_when_above_stack`（短码 BB stack=450 面对 UTG raise，1.0×pot=650 超 stack，AllIn { to = 450 }，AA-003-rev1 ②）/ `action_abs_determinism_repeat_smoke`（AA-007 1k smoke，full 1M 留 D1）+ D-202-rev1 / `BetRatio::from_f64-rev1` 量化协议 4 条断言（half-to-even / 越界 None / DuplicateRatio / RaiseCountOutOfRange）+ §7 桥接 `AbstractAction::to_concrete` 字段提取断言。
    - `tests/info_id_encoding.rs`：`preflop_169_aces_canonical` / `preflop_169_suited_offsuit_distinction` / `preflop_169_position_changes_infoset` / `preflop_169_stack_bucket_changes_infoset`（D-211-rev1 / API §9 InfoAbstraction::map 配套约束影响 ③ 字面要求 100 BB / 200 BB / 50 BB 三种 TableConfig 桶分配 3 / 4 / 2 断言）/ `preflop_169_prior_action_changes_infoset`（D-212 BettingState 5 状态 FacingBetNoRaise vs FacingRaise1 区分性）/ `info_abs_postflop_bucket_id_in_range`（`#[ignore]`，B2 决定 `PostflopBucketAbstraction` stub 构造路径后取消 ignore）/ `info_abs_determinism_repeat_smoke`（IA-004 1k smoke）/ `info_id_reserved_bits_must_be_zero`（IA-007 bit 38..64 全零）。
- **B 类 preflop 169 lossless 完整 1326 → 169 枚举测试**（`tests/preflop_169.rs`，5 个 `#[test]`）：阶段 2 信任锚（§B1 line 228 字面）。`preflop_169_anchor_table_closed_form`（D-217 12 锚点公式独立验证）+ `preflop_169_lossless_complete_coverage_closed_form`（1326 起手枚举 → 169 类全覆盖 / hole 计数 6/4/12 / 段长 13/78/78，**完全独立**于 `PreflopLossless169` stub）+ `preflop_169_lossless_via_stub`（12 锚点 stub 比对，B2 driver）+ `preflop_169_lossless_full_via_stub`（1326 完整 stub 比对，B2 driver）+ `preflop_169_hole_count_in_class_complete`（169 类 hole_count_in_class stub-driven）。
- **C 类 equity Monte Carlo 自洽性 harness**（`tests/equity_self_consistency.rs`，9 个 `#[test]` 全部 `#[ignore]`，B2 落地 `MonteCarloEquity` 后取消 ignore）：EQ-001-rev1 反对称按街分流（river / turn / flop 严格 1e-9 + preflop strict 双 RngSource 同 sub_seed 1e-9 + preflop noisy 10k iter 0.005 + preflop noisy 1k iter 0.02）+ preflop EHS 单调性 smoke（AA vs 72o）+ EQ-005 deterministic 1k 重复 byte-equal + EquityError 4 类错误路径（OverlapBoard / InvalidBoardLen / OverlapHole）。容差由 D-220a-rev1 锁定，[测试] 不自己拍数。
- **D 类 clustering determinism harness 骨架**（`tests/clustering_determinism.rs`，3 active + 4 ignored）：D-228 op_id 命名空间分类 + 全局唯一性两条 const-only 测试（不依赖 stub）+ rng_substream 模块路径编译验证 + 4 条 `#[ignore]` 骨架（SplitMix64 byte-equal / 32 sub_seed 区分性 / clustering BLAKE3 byte-equal 占位 / 跨线程 bucket id 一致占位，C2/D1 接入完整）。
- **E 类 criterion benchmark harness 骨架**（追加到 `benches/baseline.rs` per D-259 命名前缀 `abstraction/*`，与 stage-1 5 条 bench 共存）：`abstraction/info_mapping/preflop_lossless_169` 单次 (GameState, hole) → InfoSetId + `abstraction/equity_monte_carlo/flop_1k_iter` 单次 equity 1k iter。无 SLO 阈值断言（E1 才接到 `tests/perf_slo.rs::stage2_*`）。`cargo bench` 触到这两个 bench 时 panic（unimplemented），与 stage-1 §E1 落地前 SLO 全 fail 同形态。

B1 出口数据（commit 落地实测；本机 1-CPU AMD64 debug profile）：

- `cargo build --all-targets` ok / `cargo fmt --all --check` ok / `cargo clippy --all-targets -- -D warnings` ok / `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` ok。
- `cargo test --no-run` 编译 21 test crates（stage-1 16 + stage-2 5 新增）成功，`tests/api_signatures.rs` trip-wire（A1 落地的 50+ 签名绑定 + D-228 全 15 op_id 常量绑定）byte-equal 不变，stage 2 公开 API 0 签名漂移。
- `cargo test --no-fail-fast`（默认）：109 passed / 20 failed / 33 ignored across 21 test crates。其中：
    - **stage-1 baseline 16 crates 维持** `104 passed / 19 ignored / 0 failed`，与 stage1-v1.0 tag byte-equal（D-272 不退化要求满足）。
    - **stage-2 B1 新 5 crates** `5 passed / 20 failed / 14 ignored`：A 类 17 panic on unimplemented（§B1 §出口 line 248 字面 "A 类测试编译通过、运行失败（因 unimplemented!()）" ✓），B 类 2 closed-form 独立通过 + 3 stub-driven panic（§B1 §出口 line 249 字面 "至少枚举正确性测试可独立通过（不依赖产品 stub 之外的实现）" ✓），C/D 类 14 ignored + 3 const-only active 通过（§B1 §出口 line 250 字面 "C / D / E 类 harness 能跑出占位结果或断言失败，流程不 panic" ✓——ignored 路径默认不触发 unimplemented panic）。
- `cargo bench --bench baseline` 未触发（B1 不要求实跑；E1 才接 bench-quick / bench-full CI 路径）。

B1 角色边界审计：本 commit 仅写 / 改 `tests/`、`benches/`、`docs/` 与 `CLAUDE.md`：

- 新增 5 个 stage-2 测试文件：`tests/action_abstraction.rs` / `tests/info_id_encoding.rs` / `tests/preflop_169.rs` / `tests/equity_self_consistency.rs` / `tests/clustering_determinism.rs`。
- 修订 1 个 stage-1 [测试] bench 文件：`benches/baseline.rs` 追加 `bench_abstraction_info_mapping` / `bench_abstraction_equity_monte_carlo` 两 group + `criterion_group!` 列表追加（D-259 命名前缀 `abstraction/*`，与 stage-1 5 条 bench 共存，stage-1 既有 `bench_eval7` / `bench_simulate` / `bench_history` 实现 0 修改）。
- 修订 `docs/pluribus_stage2_workflow.md` §修订历史 追加本 §B-rev0 + `CLAUDE.md` 状态翻 "stage 2 B1 closed"。
- `src/`、`Cargo.toml`、`Cargo.lock`、`fuzz/`、`tools/`、`proto/` **未修改一行**——B1 [测试] 0 越界（继承 stage-1 §B-rev1 §3 / §C-rev1 / §D-rev0 0 越界形态）。

§B-rev0 carry forward 处理政策（与 §A-rev0 / §A-rev1 一致）：

- §B-rev1 §3：[测试] 步骤越界改产品代码 → 当 commit 显式追认；不静默扩散到下一步。本 commit 0 越界，无追认事项。
- §B-rev1 §4：每个步骤关闭后必须有一笔 `docs(CLAUDE.md): X 完成后状态同步` 把仓库状态、出口数据、修订历史索引补齐。本 commit 落地。
- §C-rev1 / §D-rev0 / §F-rev1 既往政策保持继承不变。

下一步：B2 [实现]（让 B1 全绿，按 §B2 §输出 列表落地 `DefaultActionAbstraction` / `PreflopLossless169` / `PostflopBucketAbstraction` 占位实现 / `MonteCarloEquity` 朴素实现 / `EHSCalculator` 朴素实现）。B2 出口 `cargo test`（默认）必须把本 §B-rev0 实测 109 passed → 至少 109 + 17 (A 类 panic 转通过) + 3 (B 类 stub-driven panic 转通过) - 1 (info_abs_postflop_bucket_id_in_range 仍 ignored 直到 B2 stub 路径设计) = ≥ 128 passed，并验证 §B2 §出口 line 281 "equity Monte Carlo 反对称误差在 D-220 容差内"（B2 取消 9 条 C 类 #[ignore] 后跑通）。具体计数留 B2 [实现] 实测核对。
