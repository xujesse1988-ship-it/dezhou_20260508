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

### 下一步候选

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
