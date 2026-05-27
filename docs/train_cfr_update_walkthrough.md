# train_cfr 一次 update 的代码走读

对照源码逐行梳理 `train_cfr` 的**一次 update** 过程，重点标出容易写错的地方。
配套的概念可视化见 `docs/es_mccfr_update_explainer.html`（注意那份是高层示意，且部分细节停留在
Phase 3 之前的旧 info_set layout——以本文件 + 源码为准）。

讲解对象 = 默认单线程路径（`--threads 1`），最能代表算法本身。多线程 / dense 后端的差异在末尾单列。

## 调用链

```
train_cfr drive 主循环          tools/train_cfr.rs:306-350
  └─ trainer.step(rng)          → EsMccfrTrainer::step          trainer.rs:530-546
       └─ recurse_es(...) DFS                                   trainer.rs:644-726
            ├─ RegretTable::current_strategy_smallvec           regret.rs:115-143
            ├─ sample_discrete                                  sampling.rs:90-123
            └─ SimplifiedNlheGame::{info_set,legal_actions,next,payoff}   nlhe.rs
       └─ update_count += 1; maybe_lcfr_rescale()               trainer.rs:543-544
```

CLI 层（`drive`）只管 batching、checkpoint 节奏、report，不碰算法。
**一次 update = 一次 `recurse_es` 从 root 到所有叶子的 DFS。**

## 逐步走一遍 `step`（`src/training/trainer.rs:530-546`）

### 1. 选 traverser（alternating）

```rust
let traverser = (self.update_count % n_players) as PlayerId;   // :533
```

HU 下 update 0→玩家0、update 1→玩家1、2→0……（D-307 alternating）。
`update_count` 是断点续训的唯一锚——resume 后从 checkpoint 的 `update_count` 继续，
奇偶性必须接得上，否则 traverser 轮换错位。

### 2. 造 root（唯一消费随机性的地方）

```rust
let root = self.game.root(rng);                                 // :534
```

`SimplifiedNlheGame::root`（`nlhe.rs:300-319`）调 `GameState::with_rng_no_history`，
**在这里一次性 Fisher-Yates 发两手底牌 + 5 张 runout board + post blinds**（51 次 `next_u64`）。
关键后果：简化 NLHE **没有 in-game chance node**——board 在街切换时由 `deal_board_to`
从已发好的 `runout_board` 取出，不再碰 rng（`nlhe.rs:487-489`）。所以 `current()` 只会返回
`Player`/`Terminal`，`chance_distribution` 被调到就 panic（`nlhe.rs:493-504`）。

### 3. DFS `recurse_es`（`trainer.rs:644-726`）—— 返回 traverser 视角的 cfv

**Terminal**（`:653`）：`G::payoff(&state, traverser)`。`payoff`（`nlhe.rs:506-522`）取
`payouts()` 里该 seat 的**净 PnL**（`awards - committed`），不是 gross。零和。

**Chance**（`:654-661`）：`sample_discrete` 采 1 个 outcome，**`pi_trav` 不变**。
NLHE 走不到；Kuhn/Leduc 走这里。

**Player(actor)**（`:662-724`）：

```rust
let info = G::info_set(&state, actor);
let actions = G::legal_actions(&state);
let n = actions.len();
regret.get_or_init(info.clone(), n);                    // :667 锁定 action_count
let sigma = regret.current_strategy_smallvec(&info, n); // regret matching
```

`current_strategy_smallvec`（`regret.rs:115-143`）：`R⁺=max(R,0)`，`ΣR⁺>0` 就归一化，
否则**回退均匀分布** `1/n`。

分两支：

**(a) actor == traverser**（`:672-698`）—— 枚举展开：

```rust
let weighted = sigma.iter().map(|s| pi_trav * s);   // strategy_sum 权重
strategy_sum.accumulate(info.clone(), &weighted);   // :677-678
for each action i:
    cfv_i = recurse_es(next, traverser, pi_trav * sigma[i], ...)  // :683-690
let sigma_value = Σ sigma[i]*cfv_i;                  // :693
let delta = cfvs.map(|c| c - sigma_value);          // :696  ← 注意没有 pi_opp
regret.accumulate(info, &delta);                    // :697
return sigma_value;
```

**(b) actor != traverser（opponent）**（`:699-723`）—— 采样收缩：

```rust
let nonzero = actions.zip(sigma).filter(|(_,p)| *p > 0.0); // :708-713 剔零概率
let sampled = sample_discrete(&nonzero, rng);              // :719
return recurse_es(next, traverser, pi_trav, ...);          // :722  pi_trav 不变
```

对手节点**既不累 regret 也不累 strategy_sum**，只采样 1 个动作往下走。

### 4. 收尾

```rust
self.update_count += 1;        // :543
self.maybe_lcfr_rescale();     // :544
```

`maybe_lcfr_rescale`（`trainer.rs:352-367`）：vanilla 时 `lcfr_period_size=None` 直接返回；
开了 LCFR 才在 period 边界把 regret + strategy_sum 整表 `× n/(n+1)`。

## 容易出错的细节

### ① External sampling 的 regret delta 不能乘 `pi_opp`（最核心的坑）

对比两份代码：

- ES：`delta = c - sigma_value`（`trainer.rs:696`）
- Vanilla：`delta = pi_opp * (c - sigma_value)`（`trainer.rs:245`）

ES 里对手/chance 的 reach 概率是**靠"是否采样到该节点"隐式提供的**，再显式乘一次 `pi_opp`
就把 reach 权重平方化了（注释 `:694-695`、`:676`）。从 vanilla 抄代码过来最容易在这里翻车。
同理 ES 里**根本没有 `pi_opp` 这个参数**——`recurse_es` 签名只有 `pi_trav`（`:644-650`）。

### ② chance / opponent 节点绝不更新 `pi_trav`

chance（`:660`）、opponent（`:722`）递归都原样传 `pi_trav`；只有 traverser 自己的动作才乘
`sigma[i]`（`:689`）。`pi_trav` 的语义是"traverser 自己走到这里的 reach"，对手怎么走不进它。
乘错了 average strategy 权重就偏。

### ③ average strategy 的更新位置/权重是一种约定，别和 Lanctot 原版混用

本实现在 **traverser 自己的节点**累 `strategy_sum += pi_trav·σ`（`:677-678`）。
Lanctot 2009 经典 external sampling 是在**对手节点**累 `+= σ`（权重 1）。两种都收敛
（Kuhn/Leduc exploitability 测试背书），但**半边抄一种、半边抄另一种就会错**。
注意 `regret.rs:261` 注释写的 "`+= σ(I,a)`" 是简写，真实代码带 `pi_trav` 权重——
照注释改代码会引入 bug。

### ④ 对手采样前必须剔除零概率动作（`:708-713`）

`sample_discrete` 断言每个 `p>0`（`sampling.rs:97-100`）。regret matching 后某些动作 σ 严格为 0
很常见，不 filter 直接喂进去就 panic。filter 后剩余 σ 仍 sum=1（`:706` 容差）。

### ⑤ regret 向量下标 ↔ 动作 ↔ tree child 必须三方对齐

`legal_actions`（`nlhe.rs:414-427`）和建树（`nlhe.rs:167`）共用**同一个**
`nlhe_action_abstraction()`（`nlhe.rs:83-85`）。一旦两处动作集合/顺序不一致，regret 的
`Vec<f64>` 下标会和实际动作错位，而且**静默错**（数值不 panic，只是学错策略）。
bet-size 扩张时只许改这一处函数。

### ⑥ `next()` 必须先查 tree edge，再 `apply`（`nlhe.rs:443-457`）

```rust
let edge_idx = node.legal_actions.position(|t| *t == tag);  // 先按 tag 定位
let child = node.children[edge_idx];
next_state.game_state.apply(concrete);                       // 后 apply
```

apply 之后 `current_player` 可能换人或进 Terminal，tag 查表依赖动作本身、不依赖筹码值——
顺序反了就定位到错节点。

### ⑦ info_set 把 node_id 打进高位，根除跨街 collision（`nlhe.rs:334-412`）

`pack_info_set_v2` 把 26-bit `node_id` 放进 `InfoSetId` bits 38..64（`nlhe.rs:98-120`）。
`debug_assert node.player_acting == actor`（`:340-343`）是"CFR 走错节点"的 trip-wire。
还有个 per-street `info_set_cache`（`:354-409`）：同街同 actor 的 (board,hole) 不变才命中，
**街切换靠 `street_plus_one` mismatch 自动失效**——这套缓存失效逻辑写错会让 postflop
用到 preflop 的 bucket。

### ⑧ `next()` 不消费 rng（`nlhe.rs:432`、`:487-489`）

随机性全在 root。如果在 decision transition 里碰了 rng，就破坏了"无独立 chance node"模型，
也破坏跨 run 的 BLAKE3 byte-equal（同 seed 必须逐字节一致）。`sample_discrete` 恰好消费
1 次 `next_u64`（`sampling.rs:110-111`）——多消费/少消费都会让整条 trajectory 漂移。

### ⑨ traverser 节点用 `state.clone()` 枚举，最后一个才能 move

单线程 `recurse_es` 每个分支都 `state.clone()`（`:682`）。并行版 `recurse_es_parallel`
做了优化：前 n-1 个 clone、最后一个 move 原 state（`trainer.rs:793-816`）。
**若把 move 用在非最后分支，后续 sibling 就拿不到 state**。

### ⑩ LCFR rescale 同时作用 regret 和 strategy_sum，因子 `n/(n+1)`（`trainer.rs:358-365`）

只 rescale 一个表（除非显式走 `with_lcfr_period_strategy_only`）、或因子写成 `(n+1)/n`
都会反向。且 LCFR period **不存 checkpoint**——resume 后强制回 vanilla（`trainer.rs:618-624`），
CLI 在 build 阶段就拒绝 `--resume + --lcfr-period`（`train_cfr.rs:191-197`、`:216-222`）。

### ⑪ 多线程 stale-σ 是有意为之，但有边界

`step_parallel`（`trainer.rs:456-526`）里一个 batch 内所有 trajectory 都读 **pre-dispatch 的
共享只读 σ**，delta 攒到线程本地、批末按 tid 升序 playback 合并（`:510-517`）。
NLHE 119M infoset 下重访概率 ~0.06%，可忽略；但 **Leduc 只有 288 infoset，必须走单线程
`step`**（`:443-444` 注释），否则 stale-σ 偏置显著。`drive` 里 `--threads 1` 才走 `step`，
`>1` 走 `step_parallel`（`train_cfr.rs:308-327`）。

### ⑫ batch 收尾的 floor 除法不能 round-up（`train_cfr.rs:317-318`）

```rust
let n = threads.min(remaining).max(1);
let batch = (remaining / n).min(batch_per_worker).max(1);   // floor，绝不越 args.updates
```

注释（`:313-316`）明确：`div_ceil` 会 round-up 越过目标 update 数。尾数留到下一轮 `n` 缩小后
`batch=1` 精确命中。

## 后端差异速记

- **dense 后端**（`--dense`，`DenseNlheEsMccfrTrainer`）：把 `HashMap<InfoSetId, Vec<f64>>`
  换成两张扁平 `Vec<f64>`，算法逻辑同型，同 seed 下与 HashMap 后端 byte-equal
  （`tests/dense_nlhe_trainer.rs` 5 个对照）。checkpoint 走 dense raw v3 格式，与 HashMap ckpt
  不互通。
- **多线程**（`step_parallel`）：见易错点 ⑪。数值决定性来源 = DFS 顺序（rng 决定）+ tid 顺序
  （rayon `par_iter_mut().enumerate().collect()` 保 index 顺序），与单线程 `step` 不保证
  byte-equal（D-362 anchor 测试只跑单线程）。
