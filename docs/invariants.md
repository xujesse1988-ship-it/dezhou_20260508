# 代码层硬约束

下列规则由编译器 / clippy / `Cargo.toml` 强制，违反通不过 build。


## 1. 规则 / 评估器 / 抽象层不用浮点

- 筹码：`u64`（`ChipAmount`）
- 盈亏：`i64`
- 评估器返回整数 rank
- abstraction bucket id 是离散整数

**为什么**：CFR regret 累计上亿次，浮点漂移会污染策略。
bucket id 是索引，浮点等价比较是错的。
唯一允许浮点的位置是 CFR 内部的 σ / regret 累加（数值上无法避免）。

## 2. 不用全局 RNG

所有随机性走 `RngSource` 显式参数。
不引用 `rand::thread_rng()` / `rand::random()` / 任何 global state。

**为什么**：byte-equal 复现是发现算法 bug 的最低门槛。
一处用了全局 RNG，整个调用链不再可复现，bug 进来后找不到。

## 3. 不用 `unsafe`

`Cargo.toml`：

```toml
[lints.rust]
unsafe_code = "forbid"
```

编译期直接拒绝。

**为什么**：扑克 AI 的瓶颈不在内存安全的边角能省的几个指令上。
觉得需要 unsafe，更可能是数据结构选错了。

## 4. `ChipAmount::Sub` 下溢 panic（debug + release 都 panic）

要 saturating 行为请用 `checked_sub` 显式写。

**为什么**：筹码负数永远是 bug。silent wrap 会让错误在下游分布到 regret 表里再也找不出来。

## 5. `Action::Raise { to }` 是绝对值

`to` 是 raise 的目标金额（含已下注部分），不是增量。

**为什么**：跟 NLHE 协议惯例一致，跟 PokerKit / poker-eval / 主流牌谱格式一致。
跨参照对照时少一层换算。

## 6. 座位方向唯一约定

`SeatId(k+1 mod n_seats)` 是 `SeatId(k)` 的左邻。

所有"向左"语义（按钮轮转 / 大小盲 / odd-chip 余筹 / 摊牌顺序 / 发牌起点）共用这一条。
不要在某处反向。

## 7. 改算法必须有外部对照



## 8. `closed` 必须 hard pass

stage 验收门槛全部通过才能 close。
不允许 "closed with known deviations"。
不允许 "下个 stage 修"。

如果一个量化门槛偏离阈值 10× 以上，**停下来怀疑算法**，
不要在偏离上面继续盖新阶段
