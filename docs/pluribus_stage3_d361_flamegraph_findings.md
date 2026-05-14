# Stage 3 D-361 Hot Path Flamegraph 实测发现

> Stage 4 起步并行清单 carry-forward 项 (I) "perf + cargo-flamegraph proper sampling profiler 实测真实 hot path 分布" 一次性实测产出（详 `CLAUDE.md` §"stage 4 起步并行清单 carry-forward 项"）。
>
> **触发条件**：Stage 3 E2-rev1 closure 2026-05-14（Option C accepted） + Stage 4 A0 closure 2026-05-14 后，用户授权 ad-hoc 提前实测；**不是** stage 4 E1 [测试] 主线开始（13-step workflow A1 [实现] scaffold 之前的旁路 research，0 产品代码改动）。
>
> **实测 host**：vultr `64.176.35.138` 4-core AMD EPYC-Rome / Ubuntu kernel `5.15.0-177-generic` / `perf` 5.15.199 / `cargo-flamegraph 0.6.12` + `inferno 0.12.6` / `kernel.perf_event_paranoid=1`（临时 sysctl，不持久化）。
>
> **实测 commit**：`9c7d763`（stage 4 A0 closed at `main` HEAD）；vultr build `bench/release+debuginfo` 25.7 s incremental。

## §1 流程纪要 + 关键 methodology pitfall

实测分两轮采样：

| 轮次 | workload | binary | iter | wall | raw samples |
|---|---|---|---|---|---|
| 单线程 | `tests/perf_slo.rs::stage3_simplified_nlhe_single_thread_throughput_ge_10k_update_per_s` | `target/release/deps/perf_slo-f0dadbf2dfac6456` | 100 warm-up + 10K timed | 12.58 s | 12,574 @ 999 Hz |
| 4-core | `tests/perf_slo.rs::stage3_simplified_nlhe_4core_throughput_ge_50k_update_per_s` | 同上 | 4 thread warm-up + 50K timed (12500/thread) | 16.53 s | 30,113 @ 999 Hz |

采样命令模板（DWARF unwinding + 16 KiB stack window）：

```bash
perf record -F 999 -g --call-graph dwarf,16384 -o /tmp/<name>.perf.data \
  -- ./target/release/deps/perf_slo-XXX --ignored --exact <test_name> --nocapture
```

**首轮 cargo-flamegraph 路径全部 abort**（exit code 101）— SLO test panic 在 cargo-flamegraph wrap 下让 `inferno-flamegraph` post-process 不执行；改走 `perf record` + 手装 `inferno` binary 手动 `perf script | inferno-collapse-perf | inferno-flamegraph` 三步绕过。

### §1.1 Setup pollution 修正（**重要 methodology pitfall**）

**第一版 flamegraph 全部被 setup 污染**。`canonical_enum::build_sorted_table` 是 OnceCell lazy init（`src/abstraction/canonical_enum.rs:170-180`），CLAUDE.md `n_canonical_observation` doc 注释明确："flop ~30 ms / turn ~2 s / river ~3 min on 1-CPU host"。首次 `canonical_observation_id(Flop/Turn/River)` 调用触发一次性构造大 `Vec<u128>` + `quicksort_u128` + `enumerate_groups` 全枚举 — 在单线程 12.58 s 总 wall 内占 **14.83% of samples** （`A_setup_build_sorted_table` bucket），但**不是 D-361 per-step 真实成本**。

修正方法：用 `perf script --time <start>,<end>` 切尾窗口，单线程取最后 3 s（= 2.27 s timed loop + 0.7 s panic tail），4-core 取最后 9 s（= 8.15 s timed loop + 0.85 s panic tail）。窗口内 `build_sorted_table` setup 占比降到 < 0.05%（全部已经在 setup phase 完成）。

| Bucket | 单线程全窗口 12.58 s | 单线程末 3 s 窗口 | Δ（确认是 setup） |
|---|---:|---:|---:|
| `setup_build_sorted_table` | 14.83% | 0.00% | -14.83 pp（pure setup） |
| `setup_oncelock`（非 build_sorted_table） | 1.31% | 0.03% | -1.28 pp（pure setup） |
| `panic_unwind` | 0.03% | 0.07% | +0.04 pp（panic 末尾窗口集中） |

> **教训**：profile lazy-init 路径必须 windowed sampling 或显式 warm-up + 标记。直接对 SLO test 全程 perf record 拿到的 top-N hot function 列表会 mislead。stage 4 E1 [测试] 落地正式 SLO + 优化迭代时建议在 `tools/` 加一个 `profile_nlhe_step.rs` 一次性 binary：load + warm-up 200 update + sleep 2 s 标记 + 100K timed loop，配合 `perf record --delay 5000` 自动跳过 warm-up。

## §2 单线程 D-361 hot path 真实分布

vultr 4-core EPYC-Rome 单线程 ES-MCCFR 简化 NLHE `step()` 实测吞吐 **4,410 update/s**（10K iter / 2.267 s）vs D-361 字面 SLO 10,000 update/s（**43% 阈值，差 2.27×**）。

末 3 s 窗口（9.67B 加权 samples，554 unique stacks）按调用路径分桶：

| Bucket | 占比 | 代表 leaf / 调用路径 |
|---|---:|---|
| **H allocation** | **31.55%** | `alloc::raw_vec::RawVecInner::finish_grow` / `Vec::from_iter` / `realloc` / `__libc_calloc` / `__rust_alloc` / `__rust_dealloc` |
| **F game state** | **24.64%** | `SimplifiedNlheGame::next` / `info_set` / `legal_actions` |
| X 未归类（inlined per-step） | 24.52% | 大头是 stage 1 `GameState::finalize_terminal` 2.94% + stage 1 `GameState::legal_actions` 1.00% + stage 2 `DefaultActionAbstraction::abstract_actions` 4.64% + 其余 inlined Vec / iterator / RNG `refill_wide_avx2` |
| **E HashMap** | **9.23%** | `RegretTable::get_or_init` + `StrategyAccumulator::accumulate` → `hashbrown::find_inner` / `rustc_entry` |
| **C canonical_observation** | **8.02%** | `pack_canonical_form_key` + `binary_search<u128>` over lazy FLOP/TURN_TABLE |
| **D BLAKE3** | **1.27%** | `blake3_hash_many_avx2`（InfoSetId 60-bit hash 路径） |
| G ES-MCCFR scaffolding | 1.94% | `recurse_es` 入口 |
| panic_unwind | 0.07% | 末尾 SLO assert panic |

合并按"语义模块"重组（X 未归类拆到对应模块）：

| 语义模块 | 占比 | 优化路径 |
|---|---:|---|
| **Vec / alloc** | **~33%** | preallocate / SmallVec 扩展到 legal_actions + abstract_actions + finalize_terminal 临时 Vec |
| **Game state + action abstraction**（stage 1 GameState::\* + stage 2 abstract_actions） | **~35%** | 大头在 stage 1 `apply` / `finalize_terminal` / `legal_actions`（受 D-374 字面边界保护，stage 4 评估），剩余 stage 2 `abstract_actions` 4.64% 可改 in-place mut |
| **HashMap (RegretTable + StrategyAccumulator)** | 9.2% | FxHashMap / ahash 替 `std::HashMap` `RandomState` 默认 SipHash 13 |
| **canonical_observation_id** | 8.1% | per-InfoSet LRU cache（同一 (board, hole) 在 DFS 同一 traverser node × 多 player 多次查询） |
| **BLAKE3** | 1.3% | （短期不动；只有 InfoSet hashing 路径，不构成瓶颈） |
| **ES-MCCFR scaffolding** | 1.9% | recurse_es 入口本身已 inline 充分 |

## §3 4-core scaling overhead

vultr 4-core EPYC 4-thread `step_parallel` 实测吞吐 **6,132 update/s**（50K iter / 8.154 s）vs D-361 字面 SLO 50,000 update/s（**12% 阈值，差 8.2×**） — 4-core efficiency `6132 / (4 × 4410) = 0.347` ≈ **1.39× scaling**（理想 4×）。

末 9 s 窗口 4-core 加权 bucket 与单线程差值：

| Bucket | 单线程 % | 4-core % | Δ |
|---|---:|---:|---:|
| **I rayon idle wait** | **0.00%** | **15.49%** | **+15.49 pp**（新出现） |
| J rayon other | 0.00% | 0.75% | +0.75 pp |
| C canonical_obs | 8.06% | 5.74% | -2.32 pp（被 rayon idle 挤压） |
| E HashMap | 9.23% | 7.33% | -1.90 pp |
| F game_state | 28.34% | 22.75% | -5.59 pp |
| H alloc | 24.63% | 18.38% | -6.25 pp |

4-core 代表 idle stack（按 sample 数最高）：

```
__sched_yield << rayon_core::registry::WorkerThread::wait_until_cold << wait_until_cold << stage3_simplifi
```

`rayon` worker pool 15.5% 全部 idle 等 work-steal（226 µs/update 单任务在 4-thread × `rayon::par_iter_mut` 路径下 task 粒度过细 → OS thread schedule overhead 不可摊销）。这是 E2-rev1 commit `5c39989` `step_parallel` 从 `std::thread::scope` 改 rayon long-lived pool 后 4-core efficiency 卡在 1.78× 的主因（F3 `pluribus_stage3_workflow.md` §修订历史 E2-rev1 段落记录 1.78× 来自 commit-time vultr 实测，本 doc commit 时复测 1.39× 在 noise 内）。

## §4 e2_rev1_profile microbench 结论修正

`tests/e2_rev1_profile.rs` 7-test diagnostic（E2-rev1-profile carve-out 2026-05-14 落地，详 CLAUDE.md `Stage 3 E2-rev1-profile carve-out` 段）vultr microbench 列出 candidate (1)(2)(3) 全 "wishful thinking" — flamegraph 实测**部分**翻面：

| 原 microbench 结论 | flamegraph 实测 | 评级 |
|---|---|---|
| (1) `state borrow 替 clone` 收益 ≈ 0（state_clone 181 ns 占 < 0.1%） | state_clone 单 call 181 ns 确实 < 0.1%，**但** `SimplifiedNlheGame::next` 整个调用链（含 state.clone + apply）占 24.64% — 单 state_clone 不是问题，整个 game_state path 是。 | **部分对 / 误导** — 拒绝 state_clone borrow 重构正确，但不应推出 "game_state 路径无优化空间" |
| (2) `canonical_observation_id 缓存` 收益 ≈ 0（info_set_postflop 693 ns 占 < 0.5%） | flamegraph 末 3 s 窗口 `C_perstep_canonical_obs` = **8.06% of update time**（**非 0.5%**），且包含 `binary_search<u128>` + `pack_canonical_form_key`。 | **错误** — microbench 测的是单 call 成本，没乘以 ES-MCCFR DFS 每 update 调 ~125 leaf + ~150 internal node 的频率（CLAUDE.md `Stage 3 E2-rev1-profile carve-out` 段提到的 "~125 leaves + ~150 internal calls/update"）。caching 有 5-6% 收益空间。 |
| (3) `bucket_table mlock` 收益 ≈ 0（mmap warm 后 lookup 已快） | flamegraph C_perstep_canonical_obs 内 `binary_search<u128>` 是 fast（mmap warm 后无 page fault），**确认** mlock 无收益。 | **对** — mlock 是非优化 |

**Root cause of (2) microbench 误导**：cost-per-call (693 ns) × calls-per-update (300+) = 210 µs ≈ 95% of 226 µs total update cost — 但 microbench 端 only measured single-call latency 并直接除 218 µs full_update 得 < 0.5%，忽略 call frequency。结论 (2) 与 flamegraph 8.06% 看似冲突仅因 microbench 公式 (`single_call_ns / single_update_ns`) 是错的，正确公式应 `single_call_ns × calls_per_update / single_update_ns`。

> **flamegraph 8.06% 是 lower bound** — `pack_canonical_form_key` 在很多 stack 上由于 inlining 没有自己的 frame，部分时间被算到 caller (`SimplifiedNlheGame::info_set`) 的 24% F bucket 内。真实 canonical-observation 路径估计 8-12%。

## §5 对 stage 3 D-361 vs stage 4 D-490 SLO 的含义

### §5.1 stage 3 D-361 字面 SLO（10K / 50K update/s）

按 §2 §3 优化潜力组合估算：

| 优化 | 预期收益 | 备注 |
|---|---:|---|
| Vec / alloc 减半 | ~15% | preallocate + SmallVec 扩到 abstract_actions / legal_actions / finalize_terminal |
| canonical_observation_id LRU cache | ~5-6% | per-InfoSet cache，DFS 同 node 多 traverser visit 复用 |
| FxHashMap 替 std::HashMap | ~2-3% | RegretTable + StrategyAccumulator 都换 |
| abstract_actions in-place Vec reuse | ~3% | mut buffer 复用 |
| **合计** | **~25-30%** | → 5,500-5,750 update/s |
| rayon task batching | 4-core 额外 +20-30% | idle 15.5% → 5% |

**单线程**：4,410 → 5,500-5,750 update/s — **仍达不到** D-361 字面 10K SLO（差 1.74×）。

**4-core**：6,132 → 11,000-15,000 update/s（× 优化 1.27× × scaling 1.39→2.5 = 2.25×）— **仍达不到** D-361 字面 50K SLO（差 3.3×）。

D-361 字面 SLO 单 / 双 fail 状态在 stage 3 出口 → stage 4 carry-forward 维持。要真正解，需要其一：

1. **破 D-301 lock** → outcome sampling MCCFR 替 external sampling（每 update node 数从 ~275 降到 ~60-100，3× throughput）— 需 D-301-revM 翻面 + 重算 stage 3 D-340/D-341 收敛阈值 + 走 stage 3 F1-rev0 [测试] 重测 Kuhn `< 0.01` exploitability + Leduc `< 0.1` exploitability + NLHE 10M anchor BLAKE3 重锚（D-362 anchor 重算）— 字面冲突 path.md，需用户授权。
2. **跨 D-374 模块边界** → 改 stage 1 `GameState::apply` / `finalize_terminal` / `legal_actions` micro-opt — 字面违反 stage 3 D-374，需用户授权 + stage 1 全 16 integration crate byte-equal 不破前提下小心 ship（最危险路径）。

### §5.2 stage 4 D-490 降标 SLO（5K / 15K / 32-vCPU 20K update/s）

stage 4 A0 lock 时 D-490 已写明 "stage 3 D-361 退化 1/2" — 字面下界：

| host | stage 3 D-361 | stage 4 D-490 |
|---|---:|---:|
| single-thread | ≥ 10K update/s | **≥ 5K update/s** |
| 4-core | ≥ 50K update/s | **≥ 15K update/s** |
| 32-vCPU | — | **≥ 20K update/s** |

按 §5.1 估算优化后 5,500-5,750 update/s 单线程 → **D-490 单线程 5K 达标**；4-core 11,000-15,000 update/s → **D-490 4-core 15K 临界达标**。

> 注意 stage 4 主算法已切到 **Linear MCCFR + Regret Matching+**（D-400 / D-401 / D-402）+ Pluribus 字面 14-action（D-420）+ NlheGame6 6-player（D-410）。14-action 路径长度比 stage 3 5-action 长 2-3×（D-490 lock 段落明文 "因 14-action + 6-player 路径长度 2-3×"）— stage 3 4,410 update/s 单线程在 stage 4 NlheGame6 + 14-action 切换后会自然降到 ~1500-2000 update/s，**stage 4 起步 baseline** 而不是 stage 3 出口 baseline。本 doc §5 收益估算只对 stage 3 SimplifiedNlheGame 有效，对 stage 4 NlheGame6 需 stage 4 E1 [测试] 重新 baseline。

## §6 优化候选清单（stage 4 起步并行清单 (I) 后续）

按 §5.1 收益估算 × 难度 × 风险排序：

| 优化 | 收益 | 难度 | 风险 | 触发位置 |
|---|---:|---|---|---|
| **preallocate `legal_actions` / `abstract_actions` 输出 Vec**（caller-provided `&mut Vec`） | 8-12% | 低 | 0（不改语义） | stage 4 E2 [实现] |
| **canonical_observation_id LRU cache**（per-InfoSet（board, hole）→ id） | 5-6% | 中 | 低（缓存 invalidation 简单：(board, hole) 是 immutable observation） | stage 4 E2 [实现] |
| **FxHashMap 替 std::HashMap**（RegretTable + StrategyAccumulator） | 2-3% | 低 | 0（D-373-rev2 已引 SmallVec, 加 fxhash crate 同型决策走 D-373-rev3） | stage 4 E2 [实现] |
| **SmallVec 扩到 finalize_terminal 临时 Vec** | 3-5% | 低 | 0（已有 D-321-rev2 SmallVec 模式） | stage 4 E2 [实现] |
| **rayon task batching**（多 update 合一 task，task 粒度从 226 µs 提到 2-5 ms） | 4-core efficiency 1.39→2.5 = +20-30% | 中 | 中（D-321-rev1 thread-local accumulator + batch merge 语义要保 BLAKE3 byte-equal） | stage 4 E2 [实现] |
| **改 D-301 → outcome sampling MCCFR** | 100-200% throughput | 高 | 高（D-301 lock 翻面 → 重测 D-340/D-341 收敛 + D-362 NLHE 10M anchor 重锚） | 用户授权 D-301-revM |
| **改 D-374 → 跨 stage 1 micro-opt `GameState::apply`** | 10-20%（apply 占 ~25%） | 高 | 极高（破 stage 1 全 16 integration crate byte-equal） | 用户授权 D-374-revM |

## §7 Artifacts（vultr + local 落地物）

### vultr 上保留（用户授权延 cleanup，下轮 stage 4 E1 [测试] 仍可用）

```
~/dezhou_20260508/perf.data                              # 1.9 GB — 4-core 全 16.53 s 采样
/tmp/single_thread.perf.data                              # 200 MB — 单线程全 12.58 s 采样
~/.cargo/bin/cargo-flamegraph + flamegraph + inferno-*    # cargo install 装的工具链
```

### local `/tmp/` 上拉回（用户浏览器打开 SVG）

```
/tmp/flame_single_thread_last3s.svg     535 K  — 单线程末 3 s 干净窗口（推荐先看）
/tmp/flame_4core_last9s.svg             712 K  — 4-core 末 9 s 干净窗口
/tmp/flame_single_thread_clean.svg      509 K  — 单线程全窗口（含 setup 污染，baseline 对照）
/tmp/flame_4core.svg                    684 K  — 4-core 全窗口
/tmp/flame_single_thread.svg            235 K  — 第一版 cargo flamegraph 输出（bench iter_with_setup 污染，仅做 methodology pitfall 案例）

/tmp/single_thread_last3s.folded        259 K  — 末 3 s 折叠数据
/tmp/4core_last9s.folded                593 K  — 末 9 s 折叠数据
/tmp/single_thread.folded               349 K  — 单线程全窗口折叠数据
```

折叠格式：`<frame1>;<frame2>;...;<frameN> <count>`，便于 grep / awk / 二次分类（本 doc §2 §3 数据全部由 `python3` 解析 `.folded` 文件得出）。

### 复现命令（vultr 上）

```bash
# 单线程
perf record -F 999 -g --call-graph dwarf,16384 -o /tmp/single_thread.perf.data \
  -- ./target/release/deps/perf_slo-<HASH> \
  --ignored --exact stage3_simplified_nlhe_single_thread_throughput_ge_10k_update_per_s --nocapture

# 4-core
perf record -F 999 -g --call-graph dwarf,16384 -o ./perf.data \
  -- ./target/release/deps/perf_slo-<HASH> \
  --ignored --exact stage3_simplified_nlhe_4core_throughput_ge_50k_update_per_s --nocapture

# 切尾窗口（避开 build_sorted_table OnceCell 一次性 setup 污染）
perf script -i /tmp/single_thread.perf.data --time <end-3.0>,<end> | \
  inferno-collapse-perf > /tmp/single_thread_last3s.folded
inferno-flamegraph < /tmp/single_thread_last3s.folded > /tmp/flame_single_thread_last3s.svg
```

`<end>` 通过 `perf script -i <file> 2>/dev/null | awk '{print $3}' | tail -1` 取最后 sample timestamp。
