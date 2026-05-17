# Heads-up NLHE 求解器目标降级说明

## 文档目标

本文档记录 2026-05-17 的目标调整：项目主线从 `6-max 100BB No-Limit Texas Hold'em` 降级为 **2 人 heads-up No-Limit Texas Hold'em 求解器**，但代码结构、数据结构和规则边界仍保留扩展到 `6` 人的能力。

本文档是新目标的入口文档。旧的 [pluribus_path.md](./pluribus_path.md) 仍可作为 Pluribus / 6-max 长线参考，但不再作为当前阶段的默认验收目标。

## 术语锁定

- `2 人`：当前求解、训练、评测都先只要求 heads-up。
- `No-Limit / 无限注`：下注大小不固定，动作抽象负责把连续下注空间压缩成有限动作集合。
- `有效筹码`：求解器仍以有限有效筹码为状态变量，默认优先支持 `100BB`；后续可增加 `50BB`、`200BB` 或更深筹码 profile。
- `无限筹码`：不按数学意义上的无限 stack 建模。真实无限 stack 会让游戏树缺少可工程验收的上界，暂不进入当前目标。
- `可扩展到 6 人`：规则引擎、座位模型、hand history、抽象接口、训练 trait 不写死 2 人；但训练质量、性能门槛和正式评测先只对 2 人负责。

## 新最终目标

实现一个可复现、可训练、可评测的 heads-up NLHE 求解器：

- 支持 2 人 NLHE 规则、合法动作、side pot、showdown、hand history 回放。
- 使用抽象后的 action space 与 information abstraction 训练 blueprint strategy。
- 先完成 blueprint-only 策略闭环，再决定是否引入实时 re-solving。
- 训练、checkpoint、策略查询和评测结果在固定 seed / 固定版本下可复现。
- 架构上保留 `n_seats <= 6` 的扩展余地，避免未来从 2 人迁回 6 人时重写核心接口。

## 明确非目标

- 当前阶段不追求 Pluribus 级别 6-max 多人策略质量。
- 当前阶段不要求多人 continual re-solving、biased leaf strategies 或完整 Pluribus 论文复现。
- 当前阶段不以线上牌局自动化为目标。
- 当前阶段不把 `n_seats > 2` 的训练质量作为验收项；最多做规则 smoke / compile / API 兼容检查。

## 架构原则

### 规则层保持通用

规则引擎继续使用 `TableConfig.n_seats`、`SeatId`、按钮相对位置和玩家数组表达桌面。heads-up 是默认 profile，不是唯一 profile。

硬约束：

- `TableConfig` 应支持 `2..=6` 人。
- heads-up 盲注和行动顺序必须显式测试。
- 6 人路径允许保留 smoke 测试，避免接口退化。
- 不在规则层写死 `n_seats == 2` 的特殊分支，除非德州规则本身对 heads-up 有特殊顺序。

### 训练层先专注 2 人零和

heads-up NLHE 是 2 人零和博弈，求解与评测可以使用更直接的 exploitability / best response 指标。训练主线应先收敛到：

`Kuhn -> Leduc -> 简化 2 人 NLHE -> 抽象 heads-up NLHE blueprint`

多人 CFR 的 traverser 轮换、多人收益分配和 6-max blueprint 训练暂时降为扩展项。

### 抽象层保留人数维度

information abstraction 不应假设永远只有 2 人，但当前 bucket 特征可以先围绕 heads-up 优化：

- hand class / board texture / equity / potential。
- position bucket：优先支持 BTN/SB 与 BB。
- stack bucket：以 effective stack 为主。
- betting state：先覆盖 heads-up 常见 open / 3-bet / c-bet / check-raise 结构。

迁回 6 人时，再扩展 UTG/MP/CO/BTN/SB/BB 位置桶和多人 pot 特征。

## 阶段重排

### 阶段 H1：规则与 heads-up profile

目标：把规则环境的默认验收目标切到 heads-up，同时保留 6 人 smoke。

验收：

- 新增或明确 `TableConfig::default_hu_100bb()`。
- heads-up preflop / postflop 行动顺序测试通过。
- random play heads-up `1,000,000` 手牌无非法状态。
- hand history roundtrip / cross-language reader 对 heads-up 样例通过。
- 6-max 旧规则 smoke 仍能编译并至少覆盖发牌、盲注、结算基础路径。

### 阶段 H2：2 人小博弈正确性

目标：先把训练算法钉在已知 ground truth 上。

验收：

- Kuhn exploitability 收敛到既定阈值。
- Leduc vanilla CFR / ES-MCCFR 收敛曲线可复现。
- checkpoint 恢复后策略查询一致。
- regret matching 数值边界测试通过。

### 阶段 H3：简化 heads-up NLHE

目标：在小动作集、小 bucket 的 2 人 NLHE 上完成训练闭环。

验收：

- `Game::n_players() == 2`。
- root state 使用 heads-up `TableConfig`。
- 能完成至少 `100,000,000` 次 sampled decision 更新。
- blueprint-only 策略能稳定击败 random / call-station / overly-tight 三类基线。
- LBR 或近似 best-response 指标随训练下降。

### 阶段 H4：heads-up blueprint

目标：训练第一个可用 heads-up NLHE blueprint。

验收：

- action abstraction 支持 fold/check/call、若干 pot ratio、all-in，并可配置。
- preflop 至少 lossless `169` 起手类别。
- flop / turn / river bucket 表可生成、可加载、可复现。
- first usable blueprint 至少完成 `1,000,000,000` 次 sampled decision 更新。
- 正式评测至少 `1,000,000` 手牌，输出 `mbb/g`、standard error、置信区间、按位置收益。
- 与 Slumbot 或同等级 heads-up 参考 bot 的对照作为后续质量门槛，不阻塞 first usable。

### 阶段 H5：搜索与实用化

目标：在 blueprint-only 可用之后，再决定是否引入实时 re-solving。

验收：

- blueprint-only API P95 `< 100ms`。
- search-on API 如启用，P95 目标 `< 30s`。
- 同一状态、同一 seed、同一策略版本输出完全一致。
- off-tree action mapping 有版本化算法说明和 fuzz 测试。

## 6 人扩展口

为了未来扩展到 6 人，以下接口从一开始就不能退化成 heads-up only：

- `TableConfig.n_seats`：保留人数参数，目标范围 `2..=6`。
- `GameState.players()`：继续返回动态长度玩家列表。
- `SeatId`：继续使用座位编号，不引入固定 `Hero/Villain` 替代核心状态。
- `Action` / `LegalActionSet`：不包含 heads-up 专属语义。
- `HandHistory`：继续记录完整 seat / stack / action 序列。
- `Game::n_players()`：训练 trait 仍表达人数，不把 2 人写进 trait 本身。
- payoff：内部可以利用 2 人零和优化，但 public API 应能表达多人 payoff vector。

## 当前代码迁移建议

优先做低风险迁移：

1. 新增 heads-up 默认配置，不删除 `default_6max_100bb()`。
2. 新增 heads-up 场景测试，先让新目标有硬锚点。
3. 把训练入口和简化 NLHE 默认 profile 切到 heads-up。
4. 文档和评测报告默认引用 heads-up 目标。
5. 等 heads-up 闭环稳定后，再清理或降级 6-max 专属长跑门槛。

暂不做：

- 不批量删除 6-max 测试。
- 不重写 hand history schema。
- 不把 `n_seats` 从配置中移除。
- 不把所有 seat / position 逻辑改成二人专属枚举。

## 决策记录

### D-HU-001：当前主线切换为 heads-up NLHE

当前主线目标从 6-max Pluribus-style engine 切换为 heads-up NLHE solver。6-max 仍作为架构扩展方向保留，不再作为近期验收目标。

### D-HU-002：No-Limit 使用有限有效筹码建模

当前求解器按 No-Limit 德州扑克处理下注空间，但仍使用有限有效筹码 profile。默认优先 `100BB`。数学意义上的无限 stack 不进入当前目标。

### D-HU-003：规则层通用，训练层专注 2 人

规则层保持 `2..=6` 人可配置；训练、blueprint、评测先只承诺 2 人质量。这样可以降低算法复杂度，同时避免未来扩回 6 人时重写基础设施。

### D-HU-004：旧 Pluribus 路线降为参考文档

[pluribus_path.md](./pluribus_path.md) 保留作为 6-max 长线参考。新的 heads-up 目标以本文档为准。
