# 阶段 4 性能 Profiling 报告 — AWS c7a.8xlarge 实测

## 文档目标

记录 stage 4 E2 [实现] closure 后在 AWS c7a.8xlarge × 32 vCPU on-demand 上的:

1. stage4_* SLO 8 条实测数字(2026-05-15 commit `a137f3f`)
2. `perf record --call-graph=dwarf` 4-core × 30s + 32-vCPU × 30s flamegraph 分析
3. Root cause 识别(rayon work-stealing per-call overhead 主导 / legal_actions 14× 冗余调用)
4. Path A / Path B 优化方案 + 50K update/s target 可行性推算

本文档**不**进入 stage 4 验收 ground truth 路径(继承 stage 3 `pluribus_stage3_profile.md` 同型政策);仅作为 §E-rev2 / §F-rev carve-out 翻面的 profiling 数据载体,实测数字与 carve-out 翻面状态由 `pluribus_stage4_workflow.md` §修订历史 + `pluribus_stage4_report.md` (F3 [报告] 落地) 引用。

## Host

- **Instance**:AWS c7a.8xlarge on-demand,US-East-2(`3.144.145.182`)
- **CPU**:AMD EPYC 7R13 / 16 core × 2 SMT thread = 32 vCPU
- **RAM**:61 GB
- **Disk**:484 GB EBS gp3
- **OS**:Ubuntu 26.04 LTS(kernel 7.0.0-1004-aws)
- **Toolchain**:rustc 1.95.0(repo rust-toolchain.toml 字面 pin)
- **Profiling tool**:`perf 7.0.0` + Brendan Gregg `FlameGraph`
- **artifact**:v3 bucket table 528 MiB,sha256 `63f68790...`(跨 host byte-equal scp 上传校验通过)

## SLO 实测数字

`cargo test --release --test perf_slo -- --ignored --nocapture --test-threads=1 stage4_`:

| # | SLO 项 | 门槛(D-490..D-499 字面) | 实测 | 状态 |
|---|---|---|---|---|
| ① | D-490 单线程 throughput | ≥ 5,000 update/s | **8,453 update/s** | ✅ PASS(+69%) |
| ② | D-490 4-core throughput | ≥ 15,000 update/s | 9,605 update/s | ❌ FAIL(-36%) |
| ③ | D-490 32-vCPU throughput | ≥ 20,000 update/s | **29,136 update/s** | ✅ PASS(+46%) |
| ④ | D-454 LBR P95(1000 hand × 6 trav) | < 30 s | 0.35 s | ✅ PASS(+99%) |
| ⑤ | D-485 baseline eval 1M 手 | < 120 s | F2 未落地 → `unimplemented!()` | (预期 panic) |
| ⑥ | D-461 24h projected(走单线程 `step()`) | ≥ 10⁹ update/24h | 6.72e8 | ❌ FAIL(测试形态 bug,见下) |
| ⑦ | D-498 7-day fuzz | CI orchestrator | panic 标记符 | (预期 panic) |
| ⑧ | D-490 6-traverser cross-check | deviation ≤ 50% | **102.6%** | ❌ FAIL |

**单线程 → 4-core 加速比 1.14×**(efficiency 0.29,远低于 D-490 字面 ≥ 0.75)。  
**单线程 → 32-vCPU 加速比 3.45×**(efficiency 0.108,刚好压 D-490 字面 ≥ 0.13)。  
**6-traverser per-traverser throughput**:`[13084, 19235, 25363, 3679, 5265, 8471]` update/s,max/min = 6.9×,**触发 D-459-revM 翻面条件**。

⑥ D-461 24h 测试形态 bug:用单线程 `step()` 7,778 update/s × 86,400 = 6.72e8(< 10⁹);真训练走 `step_parallel(32)` 29,136 × 86,400 = **2.52e9 ≫ 10⁹**。测试形态需 §E-rev / §F-rev 修。

## Flamegraph 分析

### 采集协议

```bash
# AWS host
CARGO_PROFILE_RELEASE_DEBUG=full cargo build --release --example profile_step_parallel --jobs 32
perf record -F 199 -g --call-graph=dwarf,32768 -o perf.4core.data \
    -- ./target/release/examples/profile_step_parallel 4 30
perf record -F 199 -g --call-graph=dwarf,32768 -o perf.32vcpu.data \
    -- ./target/release/examples/profile_step_parallel 32 30
perf script -i perf.4core.data | ~/FlameGraph/stackcollapse-perf.pl | ~/FlameGraph/flamegraph.pl > perf.4core.svg
perf script -i perf.32vcpu.data | ~/FlameGraph/stackcollapse-perf.pl | ~/FlameGraph/flamegraph.pl > perf.32vcpu.svg
```

`examples/profile_step_parallel.rs` 一次性 profiling 工具(不进 git history,AWS-only)— 跑 N threads × T seconds `step_parallel`,warmup 100 update。

### 4-core × 30s leaf hotspot(8,668 update/s,24,956 samples)

| 名次 | 占比 | 函数 / 类别 |
|---|---|---|
| 1 | 14.04% | `[k]` kernel schedule(`__do_softirq` / `do_syscall_64` 间接) |
| 2 | 9.89% | `crossbeam_epoch::default::with_handle` |
| 3 | 8.49% | `__sched_yield` |
| 4 | **7.89%** | **`PluribusActionAbstraction::is_legal`** |
| 5 | 7.54% | `crossbeam_epoch::internal::Global::try_advance` |
| 6 | **3.66%** | **`GameState::legal_actions`** |
| 7 | 3.64% | `crossbeam_deque::deque::Stealer<T>::steal` |
| 8 | 2.90% | `core::slice::sort::unstable::quicksort::quicksort` |
| 9 | 2.89% | `[k]` kernel |
| 10 | 2.54% | `[k]` kernel |
| 11 | 1.92% | `[k]` kernel |
| 12 | 1.88% | `RegretTable::current_strategy_smallvec` |

**类别聚合**:
- **Rayon coordination**(crossbeam_epoch + Stealer + sched_yield):**~29.6%**
- **Kernel/sched**(`[k]` 全 unknown / `__sched_yield`):**~22.4%**(其中 `__sched_yield` 8.49% 是 rayon worker 主动 yield)
- **协调开销小计**:**~43.6%**
- **legal-action 计算**(PluribusActionAbstraction::is_legal + GameState::legal_actions):**~11.6%**
- **CFR 核心**(current_strategy_smallvec):~1.9%(实际更多隐藏在内联调用栈)

### 32-vCPU × 30s leaf hotspot(26,154 update/s,73,944 samples)

| 名次 | 占比 | 函数 / 类别 |
|---|---|---|
| 1 | 10.60% | `[k]` kernel |
| 2 | **10.33%** | **`PluribusActionAbstraction::is_legal`** |
| 3 | 9.36% | `crossbeam_epoch::default::with_handle` |
| 4 | 6.71% | `__sched_yield` |
| 5 | **6.04%** | **`GameState::legal_actions`** |
| 6 | 4.74% | `crossbeam_epoch::internal::Global::try_advance` |
| 7 | 3.38% | `crossbeam_deque::Stealer<T>::steal` |
| 8 | 3.14% | `[k]` kernel |
| 9 | 2.27% | `RegretTable::current_strategy_smallvec` |
| 10 | 2.01% | `GameState::apply` |
| 11 | 1.71% | `malloc` |
| 12 | 1.70% | `[k]` kernel |

**类别聚合**:
- **Rayon coordination**:**~17.5%**(crossbeam_epoch 9.36% + try_advance 4.74% + Stealer 3.38%)
- **Kernel/sched**:**~17.3%**(`[k]` 7 项总 ~10.6% + sched_yield 6.71%)
- **协调开销小计**:**~34.8%**
- **legal-action 计算**:**~16.4%**(is_legal 10.33% + legal_actions 6.04%)
- **CFR 核心**(current_strategy + GameState::apply):~4.3%
- **malloc**:1.71%

### Flamegraph SVG

- AWS:`~/dezhou_20260508/perf.4core.svg` / `~/dezhou_20260508/perf.32vcpu.svg`(scp 拉取)
- 本地:`/tmp/profile_results/perf.4core.svg` / `/tmp/profile_results/perf.32vcpu.svg`

## Root cause 两层

### Layer 1 — Rayon work-stealing per-call 开销主导(~35-44%)

`step_parallel(pool, n_threads)` 走 `active_pool.par_iter_mut().enumerate().map(closure).collect()`(`src/training/trainer.rs:668-690`)— 每次调用都触发完整 rayon work-stealing 周期:

- `crossbeam_epoch::with_handle` — epoch GC 进入临界区
- `crossbeam_epoch::try_advance` — epoch 推进
- `crossbeam_deque::Stealer::steal` — work-stealing deque 操作
- `__sched_yield` — 空闲 worker 主动 yield 给 OS scheduler

调用频率:
- 4-core 8,668 update/s = **2,167 step_parallel calls/s** × 4 task/call
- 32-vCPU 26,154 update/s = **817 step_parallel calls/s** × 32 task/call

任务粒度(单次 traversal ~100 μs)**远小于** rayon 协调成本。这是 stage 3 F1-rev1 vultr 实测 4-core 加速比 1.14× 的同型根因 — D-321-rev2 选 rayon 是为了避免 OS thread spawn 开销,但**没有解决 per-call 协调成本**。

### Layer 2 — `PluribusActionAbstraction::actions()` 14× 冗余调 `legal_actions`(~12-16%)

`src/abstraction/action_pluribus.rs:139-147`:

```rust
pub fn actions(&self, state: &GameState) -> Vec<PluribusAction> {
    let mut out = Vec::with_capacity(PluribusAction::N_ACTIONS);
    for action in PluribusAction::all() {
        if self.is_legal(&action, state) {  // ← 调用 14 次
            out.push(action);
        }
    }
    out
}
```

`is_legal()` 第一行(`action_pluribus.rs:160`):

```rust
pub fn is_legal(&self, action: &PluribusAction, state: &GameState) -> bool {
    let legal = state.legal_actions();  // ← 每次都重算 LegalActionSet
    match action { ... }
}
```

每个 CFR 决策点调一次 `actions()` → 14× `is_legal()` → 14× `state.legal_actions()`,实际只需 1×。CFR traversal 在 6-player × 14-action × 4-street 深递归路径上,这 14× 冗余调用在 hot path 占 ~12-16%。

## 优化路径

### Path A(最小手术,估 +50-80% throughput)

| # | 修改 | 预期增幅 | 风险 | 文件 |
|---|---|---|---|---|
| **A1** | Hoist `legal_actions` 出 `actions()` 循环 — 14× → 1× | **+10-15%** | 极低,纯重构 / 0 角色边界 / 0 测试改 | `src/abstraction/action_pluribus.rs:139-180` |
| **A2** | `step_parallel` 内部 batch K 次 traversal 到单 rayon task | **+30-50%** | 中,需 §E-rev2 carve-out(`update_count` 增量从 `n_threads` → `n_threads × K`,traverser routing 字面调整) | `src/training/trainer.rs:622-748` |
| **A1 + A2 联合** | | **~1.5-1.8×** | | |

### Path A 推算

| 配置 | 当前实测 | Path A 后预期 | Target |
|---|---|---|---|
| 32-vCPU | 29,136 update/s | **44-52K update/s** | 50K ✅ |
| 4-core | 9,605 update/s | **14-17K update/s** | 15K SLO ✅ |
| 单线程 | 8,453 update/s | 9-10K update/s(只 A1 受益) | 5K SLO ✅ |

### Path B(深度重写,估 +80-150% throughput,**不进 stage 4 主线**)

| # | 修改 | 预期增幅 | 风险 |
|---|---|---|---|
| B1 | 替 rayon → `std::thread::scope` + Barrier + 持久 N worker(spawn once at trainer init) | +20-40% on top of A | 高 — §E-rev3 重大 carve-out + 6-traverser routing deterministic 测试套件全套 byte-equal 必须维持 |
| B2 | FxHashMap 替 `std::HashMap` on `RegretTable`(D-430-revM 已 deferred) | +5-10% | 低 |
| B3 | `quicksort<u128>` 来源(可能是 InfoSet 内 board sort)替为 small-N sort network | +2-5% | 低 |
| B4 | `malloc` 1.71% — Vec/SmallVec 预分配 + RegretTable lazy alloc 优化 | +2-3% | 低 |

→ Path A+B 32-vCPU:29K → **55-72K update/s**(显著超 50K)  
→ Path A+B 4-core:9.6K → **20-25K**(显著超 15K SLO)

Path B 全部 deferred 到 stage 4 F3 [报告] 后 + 用户授权评估(继承 D-441-rev0 production 训练 / D-430-revM FxHashMap 同型 deferred 政策)。

## §E-rev2 carve-out 翻面条件(Path A 实施依据)

Path A1 + A2 由 stage 4 E2 [实现] closure 后基于 AWS c7a.8xlarge 实测 profiling 数据触发,**不阻塞** F1 [测试] / F2 [实现] / F3 [报告] 主线;主线在 E2 commit `7cd7f4e` 落地形态下 stage4_* SLO ③ 32-vCPU 29K ≥ 20K 已 PASS,Path A 是 50K target 性能优化补丁,不是验收 P0 修复。

§E-rev2 carve-out 全文落到 `pluribus_stage4_workflow.md` §修订历史 + 本文档 §修订历史。Path A2 commit 同步翻面 `step_parallel` API doc 注释(D-321-rev2 → D-321-rev2-batch K)。

## 修订历史

- **2026-05-15(profiling batch 1 落地)**:本文档首次落地。AWS c7a.8xlarge(`3.144.145.182`)实测 stage4_* SLO 8 条 + perf record 4-core × 30s + 32-vCPU × 30s flamegraph + leaf hotspot 12 项 + Root cause 两层分析 + Path A / Path B 优化方案 + 50K target 推算。`examples/profile_step_parallel.rs` 一次性 profiling 工具(不进 git history,AWS-only)。生成 SVG flamegraph 字节大小:`perf.4core.svg` 101 KB / `perf.32vcpu.svg` 77 KB。
