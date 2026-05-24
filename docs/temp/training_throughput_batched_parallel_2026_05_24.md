# 训练吞吐 batched-parallel 改造记录（2026-05-24）

主机：AWS c6a.8xlarge `3.144.174.32`（32 vCPU = 16 core × 2 SMT, EPYC 7R13, 64 GB）

## 起因

`docs/status_v2.md` 训练吞吐表里 c6a.8xlarge LCFR 100M 跑 4h，~6,885/s。
用户报告 c6a.8xlarge 和 c6a.16xlarge 速度没差别——多核没发挥。

## profile 旧版（B=1）

`perf record -F 999 -g --call-graph=dwarf`，32 threads，200K updates。

top self-time symbol：

| % | symbol |
|---:|---|
| 9.02 | `__schedule` (kernel) |
| 6.55 | `crossbeam_epoch::default::with_handle` |
| 5.33 | `__sched_yield` (libc) |
| 4.52 | `RegretTable::current_strategy_smallvec` |
| 4.41 | `SimplifiedNlheState::clone` |
| 4.31 | `SimplifiedNlheGame::info_set` |
| 4.02 | `crossbeam_epoch::Global::try_advance` |
| 3.47 | `SimplifiedNlheGame::next` |
| 3.19 | `drop_in_place<SimplifiedNlheState>` |
| 2.85 | `cfree` |
| 2.72 | `_raw_spin_unlock_irqrestore` (kernel) |
| 2.60 | `malloc` |
| 2.41 | `crossbeam_deque::Stealer::steal` |

汇总：~17% syscall（sched_yield + futex）+ ~13% crossbeam/rayon 调度 + ~28% 真实 ES-MCCFR work。
top -H 同时拍到 main thread 99% CPU、其余 32 worker S 态。

**根因**：`step_parallel` 一次 dispatch 只跑 `n_threads` 条 ~1 ms trajectory，调度开销跟计算同量级。

## 改动（5 文件，未 commit）

- `src/training/trainer.rs::step_parallel` 加 `batch_per_worker: usize` 参数；rayon 任务里
  `for batch_idx in 0..batch_per_worker` 跑 B 条 trajectory 再合并。stale-σ window 由 `n_active`
  变 `n_active × B`，NLHE 119M infoset 下 256² / 119M ≈ 0.06% 重访概率，可忽略。
- `tools/train_cfr.rs` 加 `--batch-per-worker` flag，default 16；尾批自动缩 B 不越 `--updates`。
- `tools/nlhe_h3_report.rs` inline trainer 也走 B=16。
- `tests/api_signatures.rs` fn 签名加 usize。
- `tests/perf_slo.rs` stage3_4core SLO 改成 `step_parallel(_, 4, 16)`，4 × 16 = 64 update/call。

跨 run 决定性 + D-362 BLAKE3 anchor 不影响（anchor 走单线程 `step`）。

## sweep 对比（peak 编译期 throughput，AWS c6a.8xlarge）

| threads | 旧 B=1 | 新 B=16 | 提升 |
|---:|---:|---:|---:|
| 1 | 2,689/s | 2,572/s | -4%（单线程不走 batched 路径） |
| 2 | 2,838/s | 3,557/s | +25% |
| 4 | 3,706/s | 5,302/s | +43% |
| 8 | 4,737/s | 7,084/s | +50% |
| 16 | 6,135/s | 8,532/s | +39% |
| 32 | 7,650/s | 9,743/s | **+27%** |

32 vCPU 相对单线程 scaling 由 2.85× → 3.79×。

实测命令与日志：`/tmp/scaling_sweep.log`（旧）、`/tmp/scaling_sweep_batched.log`（新）。

## profile 新版（B=16，32 threads）

top self-time symbol：

| % | symbol |
|---:|---|
| 11.60 | `SimplifiedNlheGame::next` |
| 11.10 | `SimplifiedNlheState::clone` |
| 9.62 | `SimplifiedNlheGame::info_set` |
| 9.03 | `drop_in_place<SimplifiedNlheState>` |
| 5.41 | `RegretTable::current_strategy_smallvec` |
| 3.70 | `cfree` |
| 3.40 | `malloc` |
| 2.91 | `canonical_observation_id` |
| 2.44 | `quicksort` |
| 1.57 | `GameState::finalize_terminal` |

rayon/kernel overhead 已消失（< 1.5%）。新瓶颈集中在 state 路径：clone + drop + next + info_set ≈ 40%；
alloc 7%。32 vCPU 单 update CPU 时间 vs 单线程 = 8.2×，说明 worker 内存访问仍受 cache / 带宽限制。

## 第二轮（D-378 GameState fast path + 消最后一次 clone + B 调大）

继续在 AWS c6a.8xlarge `3.144.174.32` 上沿 B=16 profile 顶部往下啃。

### 改动汇总（5 文件，未 commit）

1. **`GameState::with_rng_no_history` + `track_history` flag**（`src/rules/state.rs`）
   - CFR root 走 `with_rng_no_history` 跳 `history.actions` 的 `with_capacity(32)`
     预分配 + 每条 `apply` 的 `push`；`finalize_terminal` / `deal_board_to` 内的
     `history.board / showdown_order / final_payouts = clone()` 也跳。
   - `payouts()` 不受影响（走 `state.final_payouts` 字段，独立于 `history.final_payouts`）。
   - 公开 API `hand_history()` 仍可调用，no-history 模式下返回的 actions/board/...
     字段为空。
2. **`SimplifiedNlheGame::next` 跳 `action_history.push`**（`src/training/nlhe.rs`）
   - `game_state.track_history() == false` 时不 push，保持 SimplifiedNlheState 的
     `action_history` 全程空 Vec。`nlhe_infoset_history_collision` 测试改用
     `tree.path_to_root(current_node_id)` 验证不同分支抽象动作序列。
3. **`GameState` 内部 `Vec` → `SmallVec`**（`src/rules/state.rs` + `Player` 加 `Copy`）
   - `players: Vec<Player> → SmallVec<[Player; 9]>`，`raise_option_open: Vec<bool> →
     SmallVec<[bool; 9]>`，`board: Vec<Card> → SmallVec<[Card; 5]>`。
   - HU NLHE 全 inline 路径，clone 不 malloc/free。**实测吞吐基本无变化（GameState
     struct 变大 ~280 byte，memcpy 增加抵消了 alloc 节省）**；保留主要为了配 N>2 场景。
4. **`recurse_es_parallel` traverser fan-out 最后一次 consume state**（`src/training/trainer.rs`）
   - 原 `for action in actions { G::next(state.clone(), ...) }` n 次 clone；
     改为前 n-1 次 clone + 最后一次直接 move `state`，每 traverser 节点省 1
     次 State::clone + drop。单线程 `recurse_es` 保留原写法（D-362 BLAKE3 anchor 入口）。
5. **`tools/train_cfr.rs --batch-per-worker` 默认值保 16，新 sweet spot = 128**
   - B sweep：B=128 vs B=16 在 32 thread 1M updates 上 wall 89→82s。B=256/512 与 B=128 持平。
   - 仍在 LCFR period (1M) 的 0.4% 范围内，stale-σ window 影响 ≪ 1%。

### 实测对比（AWS c6a.8xlarge，32 threads，1M updates，包含 startup + checkpoint）

| 改动 | B | wall (s) | user (s) | steady (last 200k) | cumulative |
|---|---:|---:|---:|---:|---:|
| 第一轮 baseline | 16 | 113.2 | 550.2 | 15,171/s | 10,048/s |
| + history-skip | 16 | 101.5 | 472.9 | 16,942/s | 11,158/s |
| + SmallVec | 16 | 101.8 | 482.4 | 16,779/s | 11,220/s |
| + consume-last | 16 | 101.7 | 434.9 | 17,148/s | 11,225/s |
| + B=128 | 128 | 94.3 | 427.3 | 19,356/s | 12,208/s |

合计：steady-state `15,171 → 19,356` = **+27.6%**；cumulative `10,048 → 12,208` = **+21.5%**；
1M updates wall `113.2 → 94.3 s` = **-16.7%**；user CPU `550 → 427 s` = **-22.4%**（per update
真实 CPU 工作下降 1/5）。

### 新 profile（B=128，32 threads，500K updates）

| % | symbol |
|---:|---|
| 15.39 | `SimplifiedNlheGame::next` |
| 10.10 | `SimplifiedNlheGame::info_set` |
| 9.87 | `SimplifiedNlheState::clone` |
| 8.29 | `drop_in_place<SimplifiedNlheState>` |
| 8.17 | `RegretTable::current_strategy_smallvec` |
| 3.26 | `canonical_observation_id` |
| 3.19 | `cfree` |
| 2.45 | `malloc` |
| 1.80 | `GameState::finalize_terminal` |
| 1.67 | `DefaultActionAbstraction::abstract_actions` |
| 1.50 | `GameState::apply` |
| 1.39 | `StrategyAccumulator::accumulate` |
| 1.38 | `hashbrown::rustc_entry` |

clone+drop = 18.16%（旧第二轮 28.13% / 第一轮 20% 翻 perf 上），absolute CPU 时间下降更显著。

### 试过但没收益（已回退）

- **`LazyLock` cache 全局 `DefaultActionAbstraction`**：B=128 + LazyLock 测得
  18,659/s steady（vs 不 cache 19,356/s），噪声内甚至略差。原因猜测：原本
  `Vec<BetRatio>`（3 entry，24 byte）的 alloc cost 已经被分摊；每次访问改 atomic
  load `LazyLock` 反而引入 contention（32 thread × 高频访问同一 cacheline）。
  已删 LazyLock，回到 stack-alloc 路径。

### 下一步候选（第二轮结束时）

1. **state apply/unapply 替 clone**（潜在最大收益，工作量大）— clone+drop 仍 18%，
   只有真正消除 clone 才能再降。需要 `GameState::undo(undo_info)` API。
2. **`info_set` postflop bucket lookup 缓存**：同一 trajectory 内 `canonical_observation_id`
   + `bucket_table.lookup` 走同一 board × hole 多次（同节点不同 traverser 角度）。
   可在 SimplifiedNlheState 上缓存 last computed `(board_hash, hole_hash) → bucket`。
3. **`Game::legal_actions` 返回 SmallVec 而非 Vec**（trait 改动，影响 Kuhn / Leduc / NLHE
   所有实现）— 节省每节点 1 次 Vec alloc。
4. **commit 当前 +27% steady / +21% cumulative，更新 status_v2.md 训练吞吐表**。

### 改动文件路径速查（第二轮新增 / 改）

```
src/core/mod.rs                Player 加 Copy derive
src/rules/state.rs             GameState SmallVec + with_rng_no_history + track_history flag
src/training/nlhe.rs           SimplifiedNlheGame::root 走 no_history；next 跳 action_history.push
src/training/trainer.rs        recurse_es_parallel traverser 最后一次 consume state
tests/nlhe_infoset_history_collision.rs  改用 path_to_root 验证两条分支
```

perf data：`/tmp/perf_v3.data`（B=128，32 threads）。基线 `/tmp/perf_t32_b16.data` 在 AWS host。

## 第三轮（hand_bucket cache + next CSE + 跳 history alloc + legal_actions move-out）

继续沿 B=128 32t profile 顶部往下啃。第二轮 commit `cc70ecc` 当 baseline。

### 改动汇总（commit 链 `cf13496..2f90218`）

1. **info_set hand_bucket per-street cache**（`cf13496` / `src/training/nlhe.rs`）
   - `SimplifiedNlheState` 加 `info_set_cache: AtomicU64`（packed
     `street_plus_one(u8) | set_mask(u8) | bucket0(u16) | bucket1(u16)`）。
   - 同一 trajectory 内 (street, actor) 不变 → (board, hole) 不变 → hand_bucket
     必相同；命中跳 postflop `canonical_observation_id` 二进制搜索 +
     `bucket_table.lookup`。
   - 街切换时 packed `street_plus_one` mismatch 自动失效，不需要显式 invalidate。
   - Atomic 仅为满足 `Game::State: Sync` bound（`Cell` 不 Sync）；实际 State 由
     单 worker 拥有，Relaxed load/store 等价普通 mov。
   - Clone 由 derive 改手写（AtomicU64 非 Clone）。

2. **`SimplifiedNlheGame::next` 复用 tree.node 查表**（`c1d36f5` / `src/training/nlhe.rs`）
   - 原 `next_state.tree.node(...)` 调两次（先取 edge_idx，再读 `children[idx]`）。
     `Arc<PublicBettingTree>` deref 阻塞 LLVM alias 分析，CSE 不一定生效。
   - 改一次 `let node = tree.node(...)`，把 `child` Copy 出来释放 borrow。

3. **D-378 fast path 跳 history.hole_cards / board 分配**（`4e7b3a2` /
   `src/rules/state.rs`）
   - `pot_winners` 改读 `players[idx].hole_cards`：`compute_payouts` 把
     `contenders` 已过滤 Folded，`players[].hole_cards` 必为 Some。
     `history.hole_cards` backup 只 replay 需要。
   - `with_rng_opts` 在 `!track_history` 时 `history.hole_cards` / `history.board`
     留 `Vec::new()` 不分配；track_history 路径保留 `vec![None; n]` /
     `Vec::with_capacity(5)`，replay 语义不变。

4. **`Game::legal_actions` move-out 内部 Vec**（`2f90218` /
   `src/abstraction/action.rs` + `src/training/nlhe.rs`）
   - `AbstractActionSet` 加 `into_actions(self) -> Vec<AbstractAction>` 直接 move
     `self.actions`。
   - `SimplifiedNlhe::legal_actions` 由 `set.as_slice().to_vec()`（额外 alloc +
     memcpy 6 × 24B）改 `.into_actions()`。

### 实测对比（AWS c6a.8xlarge `3.144.174.32`，32 threads，B=128，1M updates，3 次中位 elapsed / steady）

| 改动 | wall (s) | user (s) | elapsed (s) | steady last 200k (/s) | vs baseline elapsed | vs baseline steady |
|---|---:|---:|---:|---:|---:|---:|
| 第二轮 baseline `cc70ecc` | 107.2 | 596.5 | 62.1 | 16,112 | — | — |
| + info_set cache `cf13496` | 105.6 | 558.0 | 59.1 | 16,942 | -4.8% | +5.1% |
| + next CSE `c1d36f5` | 95.7 | 545.4 | 56.6 | 17,649 | -8.9% | +9.5% |
| + fast-path skip `4e7b3a2` | (noisy) | (noisy) | 56.6 | 17,654 | -8.9% | +9.5% |
| + legal_actions move `2f90218` | (noisy) | (noisy) | 56.0 | 17,880 | **-9.8%** | **+11.0%** |

`elapsed` = trainer report 最后一行 `elapsed=`（纯训练时间，剔除 startup +
checkpoint 写入）。`wall` 包 checkpoint，11 GB RegretTable 写到 tmpfs 噪声大
（30–60s 波动），第 3/4 步增量看 elapsed 更准。

### 确定性

- 50K updates × 2 次 `--threads 1 --seed 0xcafebabe` checkpoint SHA-256 byte-equal
  （`92c12bd8d774...bed5c81`）。
- `cfr_simplified_nlhe` 全部 release 测试 pass（含 `nlhe_infoset_history_collision`
  / `nlhe_infoset_semantics`）；`trainer_error_boundary` / `checkpoint_round_trip` /
  `cfr_fuzz` / `api_signatures` 全 pass。
- 项目级 BLAKE3 anchor test
  `simplified_nlhe_es_mccfr_fixed_seed_repeat_3_times_blake3_identical_1m_update`
  期望 artifact path `..._seed_cafebabe_v3.bin`，实际是 `..._seed_cafebabe_schemav3.bin`
  → 自动 skip（pre-existing 测试配置不一致，与本轮改动无关）。

### 新 profile（`/tmp/perf_v5.data`，B=128 32t 500K updates，HEAD=`4e7b3a2`）

`2f90218` 改动未单独取 profile（增量小，预期 `Vec::from_iter 2.29%` 消失，
clone / drop / RegretTable 排布不变）。

| % | symbol |
|---:|---|
| 14.92 | `SimplifiedNlheGame::next` |
| 11.02 | `SimplifiedNlheGame::info_set` |
| 8.47 | `SimplifiedNlheState::clone` |
| 8.11 | `drop_in_place<SimplifiedNlheState>` |
| 7.57 | `RegretTable::current_strategy_smallvec` |
| 3.30 | `cfree` |
| 2.29 | `Vec::from_iter`（→ `legal_actions::to_vec()`，`2f90218` 后消） |
| 2.20 | `canonical_observation_id` |
| 2.02 | `malloc` |
| 1.77 | `GameState::clone` |
| 1.72 | `GameState::finalize_terminal` |
| 1.46 | `DefaultActionAbstraction::abstract_actions` |
| 1.33 | `GameState::apply` |
| 1.27 | `hashbrown::rustc_entry` |

### 试过但回退（项目 invariant 拒绝）

- **`Arc<BucketTable>` / `Arc<PublicBettingTree>` → `*const T`**：profile 显示
  `drop_in_place 8.11%` 主要来自两条 Arc 字段的 fetch_sub on shared cacheline
  （32 thread × ~13M atomic ops/s contended）。换 raw ptr 跳引用计数后预期
  ~5% 进一步提升。但 `Cargo.toml [lints.rust] unsafe_code = "forbid"`（D-026 /
  D-027 阶段 1 整路径 invariant），`unsafe impl Send/Sync` + `unsafe { &*ptr }`
  被 lint 拒绝，已 revert。如果未来允许 contained unsafe（per-crate
  `#[allow(unsafe_code)]`），这是下一个最高 ROI 入口。

### 下一步候选（第三轮结束时）

1. **state apply/unapply 替 clone**（潜在最大收益，工作量大）— clone+drop 仍 ~16%，
   需要 `GameState::undo(undo_info)` API + `recurse_es_parallel` 改 push/pop 路径。
2. **`Game::legal_actions` 返回 SmallVec 而非 Vec**（trait 改动，影响 Kuhn /
   Leduc / NLHE 所有实现 + api_signatures 测试）— 估 1-2%。可与
   `AbstractActionSet { actions: Vec<...> }` 内部换 `SmallVec<[..; 8]>` 联动，
   再消 `abstract_actions` 那次 `Vec::with_capacity(6)`。
3. **更新 `docs/status_v2.md` 训练吞吐表**：c6a.8xlarge 32t LCFR 100M 估算
   从原 ~4 h（6,885/s）下移到 ~93 min（按 cumulative `1M / 120s` 估），
   或 ~155 min（按 steady 17,880/s 估，含 cumulative 起步段降效）。

### 改动文件路径速查（第三轮新增 / 改）

```
src/abstraction/action.rs      AbstractActionSet::into_actions(self) -> Vec<AbstractAction>
src/rules/state.rs             with_rng_opts !track_history skip history.hole_cards / board alloc；pot_winners 改读 players[]
src/training/nlhe.rs           SimplifiedNlheState info_set_cache: AtomicU64；info_set 缓存命中跳 postflop lookup；next CSE tree.node；legal_actions move-out
```

perf data：`/tmp/perf_v4.data`（cache + CSE，HEAD=`c1d36f5`）、`/tmp/perf_v5.data`
（fast-path 完成，HEAD=`4e7b3a2`）。基线 `/tmp/perf_t32_b16.data` 在 AWS host。
