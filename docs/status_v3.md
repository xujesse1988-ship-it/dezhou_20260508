# 项目当前状态（v3）

## 一句话:heads-up 阶段（已收尾）

heads-up NLHE 已收尾——1B dense blueprint(vultr `artifacts/run_dense_lockfree/nlhe_es_mccfr_final_001000000000.ckpt`,
9.3 GB)对 Slumbot 近 break-even(AIVAT 10000 手 raw −85.25 / AIVAT −108.31 mbb/g,CI 跨 0 = 符合预期、未显著),
LCFR / batched-parallel / dense 后端 + v4 bucket / AIVAT 评测链全部端到端验证;**完整
heads-up 操作细节(吞吐表 / 优化历程 / 逐 run 对照 / Slumbot+AIVAT 子集诊断 / 桶表 b3sum) 见 git 历史的
`docs/status_v2.md`**(已删)+ `docs/aivat_eval.md` + `docs/temp/*`。Slumbot 对战数据采集作为收尾持续动作仍在跑。

## 当前阶段:6-max blueprint-only

主线 = **6-max(路线 A:blueprint-only + 100BB,2026-05-30 立项)**。阶段 S1–S5 量化门槛 + 决策记录 + 亲验
代码就绪度 → **`docs/six_max_nlhe_target.md`**(主线入口文档)。

**进度(2026-06-01)**:S1–S3 均已闭/近闭(规则余 100k PokerKit 重跑;S2 树规模 + A3×A4 接进生产;S3 实测 HU
单对手桶可复用)。**S4 训练已跑完一轮并独立复核**:N=3@200 1B dense 训练(基建 commit `5b793e9` + AWS run)
S4 gate PASS(vs random/call-station/overly-tight,CI95 下界全 > 0)——但**「1B 已收敛」被 run log 自身推翻**:
实 **56% 表覆盖仍线性爬**、preflop 支配对翻转 **14%** = 欠训练(`ConvergenceMonitor` 只采 169 个 preflop 根、
看不到全表)。用户提的 **N=4(1.445B infoset)/500 桶会 backfire**(都 ×infoset → 覆盖率更差);**200 桶质量实测
健康**(排除坏桶)。**真修法 = preflop reshape**(删非 SB 开池 limp + 加 2.25BB 开池档,commit `fdc66db`,
`--reshape {none|nolimp|preopen|preopen-small}`,byte-equal 守住 cross-check):nolimp 缩树 **4.2×**(55.2M infoset/1.91GiB)、
preopen 0.68×(157.9M/5.46GiB)、preopen-small 0.45×(103.0M/3.57GiB)。**nolimp 1B 已跑完(vultr,wall 7.5h)并验证诊断**:删 limp 的 4 个非盲位 preflop
支配翻转 **13.0%→0.85%(15× 干净)**、AA/KK/QQ 全 raise 1.00、limp 归零、覆盖 64.5%;**SB 故意留 limp 作对照 →
18.4%→18.5% 不动、仍 limp AA**,证因果(动了的塌/没动的不动)+ 指向 SB 需便宜开池档。nolimp 干净但偏紧(只 3.5BB 单档)。
**preopen(加 2.25BB 开池档)已在 AWS 跑到 ~2.1B 暂停**(LCFR 不可 resume;checkpoint 1B/2B 存 vultr
`artifacts/run_6max_s4_preopen/`):非盲位 1B 即干净(0.2-0.6%)**且范围放宽近 GTO**(BTN 37→44-45%、UTG 14→18%)
证实开池档修好 nolimp 偏紧;**SB 改善(limp 43→30%、AA raise 43→68%)但仍 limp AA ~32%、翻转 ~17%,1B→2B 近乎平、
证据不足不下结论**(欠训练 vs 抽象天花板未分)。⚠ **2026-06-02 GTO Wizard 真值修正:SB limp / AA-limp 是 GTO 非缺陷**
(SB 100bb cEV limp 49% / AA limp 47%)→ 不删 SB limp、不为 SB 跑 5B。

**S5 进度(2026-06-03):①② 端到端打通**。off-tree advisor 引擎落地(`src/training/blueprint_advisor.rs`:一张权威
GameState + 每 blueprint 一份抽象影子,off-tree 翻译推进;slumbot_advisor 重构为薄壳 byte-equal)。① 跨抽象 h2h
(`tools/six_max_blueprint_h2h`)实测 **nolimp×preopen 相对强度相当(双向 CI 跨 0)**;② OpenPoker 客户端
(`tools/openpoker_advisor.rs` + `tools/openpoker_play.py`)**live 连通性 smoke 已过**(账号 jesse_xu,4 手全 blueprint
驱动 0 报错)。**两个边界**:off-tree 只忠实尺寸差异(结构性 limp-gap 显式 Desync/兜底)+ lone-hero 粒度税 confound
(不能干净排内在强度);**实测码深漂移严重**(真实桌 14BB–800BB)。剩绝对强度量化(挂场数百手 + 排行榜,需用户授权时长)。
详见 `six_max_nlhe_target.md` S4 续⑥⑦ + S5 续、`temp/openpoker_client_design_2026_06_02.md` §9。

**OpenPoker advisor 支持 N 座 / 9-max(2026-10-14)**:`openpoker_advisor --seats <3..9>`(默认 6,不传时与旧版
逐位相同)。solver 座位数 / 盲注 / 码深唯一来源 = blueprint game 的 `table_config()`(`n_max_at_bb(seats,100)`,与
`train_cfr --profile six-max --seats N --stack-bb 100` 同一定义),`--seats` 与 blueprint 不符 → layout 指纹拒绝加载
(完整 ckpt 与 crop 都测了)。座位映射 `seat_map(req, N)` 统一 N 座:满桌 `(op−button)%N`;k 人短桌(3≤k<N)真实
BTN/SB/BB→树座 0/1/2、其余按环序占 CO 侧、前 N−k 个 UTG 起位置幻影先 fold;HU 仅 preflop。规则引擎逐步重放独立
验证(6/9 座全部占座子集 × 每个 button,preflop + 三街行动序)。桌面座位数 ≠ N → `fallback:not_{N}max`。
`openpoker_hh` 接受 2..=9 座。driver `tools/openpoker_play.py` 加 `--seats`(透传 + 核对 advisor ready 的 n_seats)
与 `--cropped-checkpoint` 透传。**9-max 搜索规模**(`nine_max_search_subtree_sizing`,100BB flop 根 {1pot}):limp 池
k=6/7/8/9 = 557k / 3.07M / 15.5M / 72.6M 节点,单加注池 9-way 1.47M;≥7 人 limp 池 `max_subtree_nodes` 原本是建完树
才查 → `subgame.rs` 脱锚 solve 新增「≥7 Active 先计数、超 cap 不建」闸(只在缓存未命中真要建树时数;被拒的 key 记在
`SubgameSolveCache` 里,同街再决策直接复用拒绝结果不重数;6 座不可达,逐位不变)。上线建议 `--search-max-nodes` 按此表定
(40M 下 8-way limp 15.5M 会放行、建树约 40s + 约 2.9 GB:TreeNode 184 字节/节点)。

**9-max crop blueprint 实盘抽查(2026-10-16)发现搜索正确性 bug(兜底检查已修)**:`read_current_strategy` 原先只拦「infoset
完全未访问」,拦不住平均策略只是均匀占位(strategy_sum 无记录 / 总和 0 / 恰好均匀)→ advisor 按均匀
抽样出牌(含顶对推全下 / 顶对弃牌)。本次实盘 22 次搜索决策 6 次均匀(27%),6 月旧记录 0.5%。根因 = 有效迭代太少:
子博弈每次迭代重抽 hero 底牌,生产 `live_traversers=false` 在 9 座里轮遍历者,单挑底池 1000 次只约 111 次 hero 遍历,
抽不中真实桶;离线重放 `--search-live-traversers` + 100k 迭代可解(单线程 2–6.5 s/solve)。另有 `limp_heuristic`
面对平跟弃得过紧。详见 `temp/openpoker_9max_live_spotcheck_2026_10_16.md`。平跟处理(同日,`temp/openpoker_limp_handling_research_2026_10_16.md` §6):
`limp_heuristic` 改为按位置 × 平跟人数配范围(Chen 序)、CO/BTN 可跟着平跟、没人加注时不再套底池赔率规则(修 SB 任意两张补齐);
新增 `--search-limp-as-fold on|off`(默认 off):平跟在影子里记为弃牌,平跟池翻后脱锚搜索拿到范围前缀,翻前仍走启发式。
离线回放 35 个平跟池翻后决策:在池座位有前缀信息 0 → 31,兜底未降(节点上限 / 欠训练)。同日另修两处:① blueprint 路径
也拦均匀占位(`Blueprint::strategy` 对恰好均匀 / 未访问的行返回空,决策时翻后改走锚定搜索 `search:bp_placeholder`、
翻前 / 未开搜索走兜底 `fallback:blueprint_placeholder`;range 估计对空 σ 本就按 1/n,不变);② driver 对不带 amount
的 all_in 按 `stack_before` 补投入、剩余栈记 0(修前起始栈被算成只剩已投入部分,run3 一手 BB 算成 44 → 搜索放弃)。

**wopo driver 跟上 poker_manager(2026-10-25)**。`tools/wopo_play.py`:① advisor 建议先过合法性,不能加注时的
`all_in` 改 `call`(`:allin_as_call`,同 8a321cb),其余越界动作改安全动作(`fallback:illegal_advisor_action`,记 `rejected`);
② 支持 `--exploit-4bet*`(PM-17,与 poker_manager 共用参数);③ `--hh-log` 记校验通过的整手,`opponent_analysis` 新增
`wopo` 平台(`ingest` / `report` / `live` 都能用)。只有离线自测和实测会话回放,还没上实盘。
④ 连 WS 带 token 鉴权(`Authorization: Bearer`,环境变量 `WOPO_TOKEN` 优先,其次项目根 `.env`),默认地址
`ws://64.176.55.133:3000/ws`;服务端拒绝(握手 401/403 或关闭码 4001)→ 不重连、退出码 1。已对该服务端实测:
正确 token 握手成功,错误 token 被 401 拒绝后 driver 直接退出。

**平跟池 hero 范围按 limp 规则算(2026-10-24)**。脱锚搜索估范围时,hero 翻前由 `limp_heuristic` 决定的步不再乘 blueprint σ,
改拿同一个启发式对 1326 个组合各算一遍得 0/1 系数(`PrefixReach::hero_coef`);对手平跟后翻前又加注的,去掉他的映射弃牌步。
修的是平跟池 hero 真实手牌权重接近 0 → 访问次数不够 → 放弃搜索(115653 批次 23 次里 14 次)。尚未离线重放 / 上实盘。
详见 `temp/limp_heuristic_hero_range_design_2026_10_24.md`。
同日:新增 `--search-iterations-turn` / `--search-iterations-river`(转牌 / 河牌子树迭代次数,缺省沿用全局;实盘设 200 万 / 300 万,
依据是 115653 批次转牌、河牌求解 100% 先撞 100 万上限,耗时中位 3.0 / 1.3 秒、最长 5.2 / 2.0 秒,远低于 8 秒预算);
日志加子树当前街下注菜单(play.log `menu=` 与 `search menu build ... 累计[...]`,actions.jsonl `resp.menu`)。

**平跟池启发式加 ante(2026-10-30)**。wopo 8 座 1BB ante 桌 10-29 实盘 124 个翻前决策里 57 个走 `limp_heuristic`,
原范围 / 尺寸按无 ante 写(HJ 22 面对一家平跟 10:1 也弃、SB 补齐上限 40%)。现按「ante 程度」`t = min(本手 ante 总额 / 8BB, 1)`
(发牌座位数 × 每座 ante,短桌按 `dealt_seats`)调整没人加注那一支:隔离档和被动档范围都乘 `1 + 1.1t`(封顶 100%,
被动档为 0 的 HJ 及更早位置仍不跟着平跟);隔离尺寸加 `t×BB`(跟开池尺寸走:ante-flop2 开池 4.5BB 比无 ante 1pot 开池 3.5BB 多 1BB)。
2.1 倍的依据:8 座 ante 1BB 200BB ante-flop2-3bet 9B blueprint 各位置首入范围是无 ante 9 座 flop2-1pot 9B 的 2.04–2.18 倍
(BTN 75.1% / 36.8%、CO 51.0% / 23.4%、HJ 37.6% / 17.6%;两边开池尺寸不同,倍数里含尺寸差,是估计值)。
有人加注那一支(P 档跟、底池赔率 >4 倍且 ≤30BB 才跟)不动——它用真实底池,已含 ante。无 ante 时逐位不变;hero 范围系数用同一个函数,自动一致。
离线回放 10-29 的 124 个翻前请求(本地 100BB ante-flop2 7B crop):走启发式的 57 个仍走启发式,31 个不变,26 个变化全在没人加注那一支
(弃 → 隔离 / 跟着平跟 / 补齐 15、跟着平跟 → 隔离 4、BB 过牌 → 隔离 1、隔离尺寸 +0.9–1BB 6)。测试 `limp_heuristic_ante_widens_ranges_and_size`。
尚未上实盘;入池变多,翻后脱锚搜索的决策和放弃搜索也会跟着变多。

**搜索读数门槛 + 弱牌押上全部筹码护栏(2026-10-19)**。起因:实盘 65c 在 3h9d8s 面对 2 倍池下注,搜索给 allin 0.55
推掉 100BB(对手暗三条);同一请求换 3 个种子 allin = 0.55 / 0.09 / 0.002,该信息集没解稳。① `EsMccfrTrainer::with_visit_counts`
记每个信息集作为遍历者的访问数(只记账,regret / σ / rng 逐位不变),子博弈求解一律开;`read_current_strategy` 加门槛
`SubgameSearchConfig::min_hero_visits`(advisor `--search-min-hero-visits`,默认 200,0 = 关),不足 → `low_hero_visits`
拒读:锚定 flop 退回 blueprint 同一节点(`source=blueprint:low_hero_visits`,真码深闸不过则兜底),转牌 / 河牌 /
脱锚走 `search_giveup:*:low_hero_visits`。每次搜索读数的访问数写进响应 `hero_visits`。200 来自本机近似实验
(9 座测试 blueprint + 500 桶 + 生产搜索参数):访问 ≤ 150 的读数几乎都一边倒,**生产值待按实盘 `hero_visits` 标定**。
② `--search-weak-commit-guard on|off`(默认 on):翻后弱牌(坚果度 < 0.75 且顺子 / 同花出牌 < 8)时,把全下和
「新投入 ≥ 一半剩余筹码」的下注 / 加注的概率按比例挪给过牌 / 跟注 / 弃牌,`source` 追加 `:weak_commit_guard`,
`probs` 仍记原分布。**面对下注、分布里没有 `call` 这一档时(2026-10-22 修)**挪走的概率整份给真实跟注:对手的注在树里被映成全下,
我方跟注按 AA-004-rev1 折进了 AllIn 槽,这份 allin 在树里就是跟注;修前按比例全给了弃牌(实盘 1c3c2ba0 KcQc allin .71、
f2ff9b11 66 allin .21 都被挪成弃牌,对手都是 cc)。`call` 在菜单里只是概率为 0 时规则不变。
这是针对跟注偏多的对手池的剥削,不是修均衡。另:超出菜单最大档(1pot)的下注在搜索里仍压成 1pot
(本街从起点建树,菜单与 blueprint 翻牌同为 {0.5,1}),翻牌尺寸闸转去搜索对超池注不解决尺寸问题,尚未修。

**PM-17 翻前 4bet 剥削(2026-10-22,默认关)**。起因:实盘全部批次合计我方开局被 cc 3bet 390 次、弃 53%(−747 BB),
cc 被 4bet 弃 96.5%(139/144)(`temp/poker_manager_live_overall_bp2_bp3_analysis_2026_10_22.md`)。新增
`src/training/fourbet_exploit.rs`:进程内按玩家名统计 3bet 率和「3bet 后被 4bet 的弃牌率」,两项都按新观测衰减
(λ3 0.98 / λf 0.9),两道门(P(3bet 率 > 20%) ≥ 0.95;P(弃牌率 > 保本线) ≥ 0.8,弃牌率先验均值
clamp(1 − 0.05/p̂3, 0.45, 0.95)、权重 4)都过时,advisor 在翻前 blueprint 路径「我方开局、恰好一人非全下 3bet、其余都弃」
的点把 fold 概率 × convert(默认 1)挪给 raise1pot,`source` 追加 `:exploit_4bet`,`probs` 仍记原分布,响应新增
`fourbet` 遥测(点位命中就填,门没开也填)。advisor `--exploit-4bet on|off`(不要求 `--search`)+ `--exploit-4bet-convert`
/ `-fold-decay` / `-3bet-decay`;driver `poker_manager_play.py` 同名参数,开启时请求带 names、observe 带
`contribution_delta` / `to_call_before`。`decide` 改为 `decide_with(..., None)` 的转调,关闭时逐字节不变。
离线回放(`temp/pm17_online_detector_replay_2026_10_22.md`):每进程从零统计,cc 出现后 56–193 手放开,普通玩家 0 次误判;
Rust `tools/fourbet_replay` 与 Python 回放在 11 批 hh 上逐点一致(390 个点,尾概率差 < 1e-14)。
设计 `temp/pm17_preflop_4bet_exploit_design_2026_10_22.md`。首轮实盘 bp2 2000 手(2026-10-22):第 66 手开门、只对 cc 开,
我方 4bet 43 次净 +406.5 BB,cc 弃 45 / 全下 2(`temp/poker_manager_live_20261022_bp2_204941_pm17_analysis_2026_10_23.md`)。
**跨进程续用(2026-10-23)**:advisor / driver `--exploit-4bet-state <path>`,启动读状态文件接着统计(不存在 → 从零),
每手 observe 后原子写回;浮点存成最短十进制串,按位还原;`fourbet_replay --load-state / --save-state` 用同一格式,
可从历史 hh 生成初始状态。204941 那批拆成两段续跑与一次跑完的状态文件逐字节相同,回放与实盘遥测 99 个点的统计一致。

**深码全下抽样门槛(2026-10-20)**。起因:poker_manager 实盘 922fe989 翻牌顶对 SPR≈21,搜索给 allin 0.055,刚过 5% 剪枝线
被抽中,157BB 推进 7.5BB 的底池;该批 SPR ≥ 6 的 187 次翻后决策里 allin ≥ 5% 只有 3 次(0.174 / 0.080 / 0.055),全是一对顶对。
抽样前的全下门槛改成按翻后 SPR 分档:`postflop_spr` = min(hero 剩余栈, 未弃牌对手最大剩余栈) / 跟注后底池(出牌基准态:
搜索 = 真栈 auth,blueprint = 100BB 影子),SPR ≥ 6 时 allin < 20% 去掉,否则仍是 5%;翻前不变。只改抽样不改求解,
`probs` 仍记原分布。动了策略,poker_manager 实盘累计从这一版起重开一段。

**低频弃牌抽样门槛(2026-10-23)**。起因:bp2 实盘 8c63f318 河牌 AA 面对全下,脱锚搜索给跟注(AllIn 槽)0.952 / 弃牌 0.048,
抽中弃牌,只差 482 跟约 2960 的底池。`prune_for_sampling` 抽样前把 < 5% 的 `Fold` 也去掉(不分 SPR,翻前翻后、blueprint /
搜索一样),与全下门槛同时生效;只改抽样不改求解,`probs` 仍记原分布。此前两批 bp2 实盘翻后抽中 < 10% 弃牌 3 次(4.8% / 2.5% / 0.09%)。

**S6 进度(2026-06-03):实时搜索推进,MVP subgame-solver 核心已落地 + vultr 验证**(分支 `6max-rts-mvp`
commit `cf9efdb`,**未并入 6max**)。设计成文 `temp/realtime_search_design_2026_06_03.md`(文献调研+代码复用映射+
对抗验证):方法定型 = Pluribus/Modicum tabular depth-limited search(非 DeepStack/ReBeL)。三件落地:①
`PublicBettingTree::build_subtree`(以中途 public state 为根建 betting 子树,`nlhe_betting_tree.rs`);②**加性**
`GameState::resample_hidden`(克隆中途态+保留公共牌/下注态+重发隐藏牌,终局走权威 `payouts()` → **S1 不受影响**,
`rules/state.rs`);③ `SubgameNlheGame impl Game`(delegate SimplifiedNlheGame、仅重写 root()=resample+子树 →
复用 `EsMccfrTrainer` 零改,`training/subgame.rs`)。**6a 收尾已落地**(commit `a8f1b96`):④ `subgame_search`
接 `blueprint_advisor.rs:421`(`should_search` flop 未起注首决策点触发 + 失败回落 blueprint;`Contestant.search`
默认 None = byte-equal 旧行为) + `SearchObserver` 计搜索触发/fallback;⑤ 探针 `tools/six_max_search_probe`
(同一 blueprint 拆 search-on vs search-off,出 mbb/g+CI95)。**vultr 验证**:lib 71/0/8 无回归;真 1B nolimp
ckpt smoke(600 手)plumbing 健康——desync=0/illegal=0、search 真触发(fallback 1.5%)、加载无 OOM;mbb/g CI
600 手太宽不可判。子树实测:6-max first_small(3) flop=4434 节点 < cap 8000(探针不被误拒)。**§5b range
去 confound 已落地**(用户选路线乙,commit `ac3968b`):root 按 blueprint 沿历史累乘 reach 的 per-seat marginal
range 加权采样(非 uniform)→ 探针**升级成真正 §2 判别器**(解 blueprint 真 range 下子博弈);新增规则层
`resample_hidden_with_holes` + `estimate_range`(逐街 re-bucket) + range-weighted root + `--uniform-range`
A/B。仍在近似 = marginal/桶粒度 + 欠迭代噪声(MVP 解到真实终局、小子树无叶子近似,故「无 blueprint 叶子」非
confound)。vultr lib 74/0/8;range-on vs uniform smoke 均 desync=0/search 触发/无 OOM(600 手 CI 仍宽)。
**vultr 中样本首信号**(24k 手,§10.3):range-on −71.6 CI[−232,+89] / uniform −81.6,两 arm CI 跨 0 = 不退化、
range≈uniform、§2 灾难失败未现。**放宽触发面实验**(§10.4,commit `996879b`):加 `SearchTrigger{FlopFirstUnraised,
AllPostflop}` + 任意节点现算 `(entrants,raises_on_street)`(`live_entrants`+`raises_on_current_street` 沿
decisions_on_path 数当前街进攻,补多档计数缺口)。**实测 all-postflop 朴素放宽显著退化**(24k −192 CI[−376,−8.3];
12k @3000 −426 / @12000 −310 仍负 → **非迭代噪声、结构性**,退化集中盲位)→ 根因 = MVP 从当前决策点独立重解、
mid-round 撞 §6 #1/#2 landmine(非 round-start 重解+无 within-round 冻结);flop-first 因 = round-start 恰好正确。
**根因已隔离**(all-postflop range vs uniform A/B):24k range −192 vs uniform −501 → ① §5b range **大幅 help +310**
(不是退化源、反是修正,价值被放宽触发面揭示)② 残余 −192 = §6 landmine 非 range(迭代扫排噪声+本 A/B 排 range)。
默认 trigger 设回 FlopFirstUnraised(安全),AllPostflop 留研究 opt-in。详见 `temp/realtime_search_design_2026_06_03.md`
§10.2–10.4 + target S6。

**叠加剥削 Tier 2 已落地(2026-06-19;CLI 2026-06-21 合并为三态 `--exploit on|vpip|off`,默认 `off`:`on`=VPIP+PFR 形状/`vpip`=仅 VPIP 与现网 byte-equal/`off`=关,原 `--exploit-pfr-shape` 子旗删除)**:进程内对手画像(`src/training/opponent_profile.rs`
`Profiler`,**只用本进程数据、不依赖过往**,driver 每手发 observe 累积 VPIP/PFR/AF)→ 收缩(2026-10-29 替代原 150 手 / SE / 漂移三道门:
VPIP/PFR 按 k=50 手伪样本往人群均值拉,强度 ×n/(n+k))→ `subgame::apply_exploit_width_prior` 在**脱锚搜索** ranges
组装处把有画像的对手 root range 朝观测翻前 VPIP 宽度凸混合(Chen 序 top-k,α≤0.8 默认 0.5,hero 不混,带 uniform
地板 + card-removal)。**默认关时全程 byte-equal**(driver 不带 names/不发 observe,advisor `exploit=None` 走既有
`_cross`);剥削 ranges 进 solve_cache_key → 开/关/画像更新自动 cache-miss。**只在档一前缀 range 上混一次,跨街后验命中时不再混**(2026-10-21 修:原先每街对后验再混 α,冲掉翻后动作信息)。仅翻前宽度 + 仅脱锚路径(翻后 AF
轴/锚定路径/Tier 3 = 后续);脱锚 prewarm 与决策同一份画像快照(2026-10-23,否则开剥削后预热 key 必 miss);`--search-force-unanchored on` 让锚定搜索也走脱锚路径(默认 off)。正确性靠 Rust 单测(opponent_profile 收缩 / subgame tilt 方向·
恒等·card-removal·`exploit=None` byte-equal / advisor parse+names) + driver selftest 场景 9(observe IPC);
**live EV 测不出剥削(§0 前提 B)**。**§6 决策级 demo 已做**(`six_max_exploit_ab`,真 10B preopen blueprint,96 deal:剥削触发 94-98%、LOOSE≠TIGHT 95.7%/TV 0.32=决策响应观测宽度、betting 质量 GTO0.606→LOOSE0.652→TIGHT0.589 单调——证机制响应+方向,非 EV)。详见 `temp/exploit_strategy_design_2026_06_14.md` §9。
**§10.5 round-start re-solve 已落地——实测推翻 §10.4 的 §6-landmine 归因**(commit `8fde9bc`,vultr lib **76/0/8**):
实现 `ResolveRoot{CurrentDecision,RoundStart}`(默认 RoundStart;从 betting-round 起点建子树+within-round 导航+
round-stable seed 给一致性)。**两 control 复现 harness**:current-decision×all-postflop=−192(=§10.4 byte-equal)、
flop-first=−72。**主 A/B(24k@3000)**:round-start×all-postflop **−407** vs current-decision −192——round-start **更差**
(5/6 位一致),根因 = deep-node 欠训练(ARM3 判别器:flop-first 读 root 训透=−62 中性,all-postflop 读深层节点欠训练=−407;
fallback 6.1%>3.7%)。**迭代扫(12k)**:current-decision −426/−310/−273、round-start −527/−366/−287 @{3k,12k,24k}——
两模式随 iters 改善但**收敛到负 plateau ~−270/−287,CI 上界仍<0**。**结论(修正 §10.4)**:① **§6 round-start **不是**退化的杠杆**
(等迭代更差、高迭代持平,不 beat current-decision);② all-postflop 退化 = 欠训练(iters 修一截)+ **残余结构 ~−270(§2 materialized:
近似 marginal-range+桶粒度的子博弈在 mid-street 劣于 1B blueprint 自身响应);③ **瓶颈 = blueprint/抽象质量(§2),非搜索 root**——
flop-first(干净点、训透)中性不亏、all-postflop 弱基底反退化。**战略岔路(待拍板)**:甲 强化 blueprint / 6b biased-leaf /
丙 收尾 flop-first-only / 丁 更好 range 建模。详见设计 §10.5。

**6-max 范式切换**:多人一般和 → CFR 不保证收敛 Nash、**LBR/exploitability 失去理论意义**(只当诊断,质量以
实测对战为准)、无"训到 floor 就停"、无强 6-max 公开参考对手(不像 Slumbot 之于 HUNL)。详见 target 文档。

## 算法正确性(验证完成的基础,6-max 直接复用)

| 项目 | 状态 | 依据 |
|---|---|---|
| Kuhn / Leduc Vanilla CFR | ✅ 收敛 closed-form `-1/18` / exploit `<0.1` | `tests/cfr_kuhn.rs` `cfr_leduc.rs` |
| Leduc ES-MCCFR / LCFR-MCCFR | ✅(2026-10-25/26 修正)平均策略改回非遍历者节点、权重 1 累加(`240bb1a` 起误挪到遍历者节点,修前 Leduc 100 万次后停在 exploit 0.15–0.18 平台):两人 200 万次 5 种子 0.0127–0.0146,2000 万次 0.0037 ± 0.0003;n ≥ 3 同样改(3 人 Kuhn 300 万次 NashConv 0.0026–0.0068,旧写法 0.0072–0.110)。只有轮转里仅 1 个座位时保留旧写法(此时无偏)。子树求解器 `SubgameTrainer`(2026-10-26):根所在街的行(读数和跨街后验只读这条街)每隔一段迭代扫一遍子树、直接累加精确平均(各座位按自身 reach),其余街走非遍历者节点;见 `docs/temp/es_mccfr_leduc_convergence_issue_2026_10_25.md` §7–9 | `tests/es_mccfr_leduc.rs` `tests/es_mccfr_multiplayer.rs` |
| 简化 NLHE ES-MCCFR / LCFR | ✅ LCFR 优化路径 100M LBR 1,233 → 500M 1,126(100M 即饱和) | `run_lcfr_*` on vultr |
| dense 后端 + v4 bucket | ✅ byte-equal HashMap(HU 5 对照 + 6-max 1 对照;2026-10-26 两个后端平均策略一起改到非遍历者节点后重新成立,见下方 ES-MCCFR 行)+ 100M LBR 1,143 ≈ baseline(同质量,旧写法时测);吞吐 ~2.2× 且长 run 不塌、RAM 平 5.2 GiB、ckpt 不暴涨 | `tests/dense_nlhe_trainer.rs` `tests/six_max_a3a4_trainer.rs` |
| AIVAT 评测器 | ✅ 无偏全证;真日志降方差 1.21×(精确项 deals+runout) | `tests/aivat_nlhe_*.rs` `docs/aivat_eval.md` |
| **CFR trainer / 规则引擎 6-max N-generic** | ✅ 亲验:`recurse_es` 取 `payoff(traverser)` 不取负、traverser `% n_players`;规则多人 side pot 返回 per-seat payoff 向量 | `trainer.rs:493,653` `state.rs:846–938` |
| 规则:无人能回应时禁 bet/raise(LA-009,2026-10-16) | ✅ 其余玩家全弃牌 / all-in 或都不够跟满当前最高注 → 只能 Check/Call/Fold。PokerKit 0.4.14 随机不等码深逐决策对照 16.6 万点:我方多放行 0(修前 1834+588);剩余「PokerKit 放行、我方不放行」全部是不足额 all-in 不重开(TDA 规则,PokerKit 未实现)。等码深行为不变 | `rules/action.rs` LA-009、`state.rs` `legal_action_tests` |
| 规则 + 训练:每座 ante(2026-10-27) | ✅ 规则引擎原本就支持(发盲注前人人交、进底池不算本轮下注、不足额即 all-in),此前从未测过。现 `train_cfr --ante <chips>`(BB=100,上限 100,两个 profile 都可用;`nlhe_dense_crop` 同名参数)。下注尺度按含 ante 的底池现算,建树不改(6-max 1pot 开池 350→350+6×ante)。dense ckpt / crop 指纹在 ante≠0 时把 ante 算进 hash(ante=0 与旧公式逐位相同,旧文件照常加载):实测 3 座 20BB ante 10 与 ante 0 树形状完全相同(411 节点),不加 ante 会静默串读。PokerKit 跨验证:`pokerkit_replay.py` 原传 `ante_trimming_status=False`(PokerKit 的大盲 ante 模式,把所有 ante 都算进主池)→ 改 True;6-max ante 10 100 手 + 随机座位数 / ante / 等栈深 200 手 + 长跑 2,000 手全一致,手算期望值见 `tests/ante_rules.rs`。**未修的既有分歧(ante=0 也有)**:不等栈随机桌上 ① 多人 all-in 后 replay 没把 PokerKit 走到终局(脚本缺口);② 全员 all-in 时摊牌顺序不同;③ side pot 奇数筹码归属差 1–2 chip。**wopo 实盘 ante(2026-10-27)**:`TableConfig.ante_exempt`(不交 ante 的座位 bitmask,默认 0;history proto tag 7,0 不编码 → 旧 history 字节 / 哈希不变)。没人跟的注改按下注额(总投入 − 自己交的 ante)退:人人同额 ante 时与旧逐位相同(无 ante 2,000 手 PokerKit 复验一致),有座位豁免时豁免座多下的那份退回;只有弃牌者到达的层归在场者中投入最多的人(PokerKit 对此未定义、会断言失败,本引擎约定)。豁免组 PokerKit 200 手除这类外全一致。advisor `--ante <solver 筹码>` 按带 ante 的定义建 game 加载 blueprint、ready 报 ante;请求 ante × scale 须等于 blueprint ante(否则 `fallback:ante_mismatch`);短桌幻影座在真实侧(实时搜索 / lockstep 翻译尺寸)不交 ante,blueprint 影子仍按满桌。`wopo_play --ante <桌面筹码> --blueprint-ante <solver 筹码>`:校验层按「人人先交 ante、ante 不算本轮下注」核对,比例不符 / 交 ante 即全下 → 作废;hh 行带 ante,`opponent_analysis` 重放会扣 ante。flop2-1pot cap4 树:8 座 ante 1BB = 27 万节点 / 两表 3.95 GiB(无 ante 8 座 43.3 GiB),但翻前只有 1pot(≈11.5BB)开池档,与 ante 桌常见 2–3BB 开池差很远。**`--reshape ante-flop2`**(用户定开池 4.5BB):翻前菜单 {1/3,1}pot + `preflop_open_small_only`,开池只有 1/3pot(8 座 ante 1BB = 450 chips,SB 433)、3bet+ 1pot、禁非 SB limp、无开池 all-in;flop {0.5,1}、turn / river {1} 同 flop2-1pot。`drop_small_reraise` 由「删 0.5pot 再加注」改成「删小于 1pot 的再加注」,既有菜单小档都是 0.5pot → 逐节点相同(四个与 sizing 工具对数的节点数测试照过)。8 座 ante 1BB:cap4 101 万节点 / 两表 15.53 GiB(比 flop2-1pot 大 3.9 倍,主要来自便宜开池后的 4 人翻牌底池;cap3 只有 2.69 GiB)。ante-flop2 7B(2026-10-28,100.55.54.60 训练)翻前看下来开池范围合理(UTG 26.6% → BTN 70.6%),但 3bet 只有 1pot(有位置约 23BB)一档,BTN 对 CO 开池只 3bet 1.2%、AA 也平跟。**`--reshape ante-flop2-3bet`**:翻前菜单 {1/3,0.5,1}pot + 新规则 `preflop_half_3bet`(0.5pot 只给首次再加注:开池删它、3bet 保留、4bet+ 照常删;默认关,既有 reshape 逐节点不变,ante-flop2 仍 1,013,656 节点),BB 对 UTG 4.5BB 开池 3bet = 13.25BB / 22BB。8 座 ante 1BB:cap4 218 万节点 / 两表 32.70 GiB / 翻前 + 翻牌裁切 3.26 GiB;cap3 34 万节点 / 5.09 GiB | `tests/ante_rules.rs`、`tests/cross_validation.rs` `cross_validation_pokerkit_ante_*`、`nlhe_dense_checkpoint.rs` `from_indexer`、`nlhe_betting_tree.rs` `reshape_ante_flop2_small_open_only`、`nlhe.rs` `ante_flop2_opens_at_four_and_half_bb`、advisor `ante_flop2_open_size_and_small_open_translation`、`reshape_ante_flop2_3bet_half_pot_only_on_3bet`、`ante_flop2_3bet_sizes`、advisor `ante_flop2_3bet_half_pot_three_bet` |
| 规则 + 实盘:补盲(2026-10-28) | ✅ wopo 新进场 / 回座玩家开局补 1BB live 盲注(实测 + 用户确认:算本轮下注、行动顺序不变、没人加注时可过牌),修前 driver 开局见非盲注座有下注就整手作废(ante-table 头 16 手作废 4 手)。`TableConfig.live_posts`(补盲座 bitmask,只能标非盲注座;history proto tag 8,0 不编码 → 旧字节 / 哈希不变;子树求解缓存 key 带前缀写入)。driver `table.py` 非盲注座开局 bet = 1BB 记为补盲(补盲即全下 / 补的不是 1BB 仍作废),请求 / hh 带 `posts`;advisor 真实侧(lockstep 重放、实时搜索)按补盲建局,blueprint 影子不变;补盲者没人加注时过牌 = 影子里的平跟(有 Call 走 Call,no-limp 树走平跟当弃牌);能免费过牌时策略给弃牌 → 出 check(`:fold_as_check`)。`opponent_analysis` 重放扣补盲。同时修 driver 结算核对:wopo `final_stacks` 会列 sitting_out 座,多出的未发牌座不再记 `result_seats` 备注、只核对发牌座守恒。**PokerKit 跨验证不了**(它把 BB 之后的强制注当 straddle,改行动顺序),靠手算单测 + 实盘 132 手整手重放(各街底池 / 结算与服务端一致)。 | `tests/live_post_rules.rs`、advisor `seat_map_live_posts` `live_post_fold_stays_anchored` `live_post_check_maps_like_limp` `hero_poster_never_folds_free_option`、subgame `live_posts_enter_solve_cache_key`、`wopo_play.py --selftest` 第 12 组(实盘 4 手)、`opponent_analysis` `test_live_post_hands_ingest` |
| advisor 码深 + 开池映射(2026-10-28) | ✅ advisor `--stack-bb <BB>`(默认 100,范围 [2,1000],同 `train_cfr`)按它建 blueprint 桌面,ready 行码深 ≠ 100 时报 `stack_bb`;`wopo_play --stack-bb` 透传,`advisor_client` 核对 ready 码深,不一致启动报错(poker_manager 不传 = 100)。两道码深闸改成按 blueprint 起始码深的比例:翻牌优先 blueprint 区间 60%–140%、翻前面对全下低于 60% 走兜底启发式(100BB 时与旧的 60–140BB / 60BB 逐位相同,200BB = 120–280BB / 120BB)。off-tree 投影 `project_tag_onto` 加第三种情形:无更大合法加注、也无 AllIn → 向下取最大合法加注(ante-flop2 / ante-flop2-3bet 开池节点只有 1/3pot 且无开池全下,真实开池偏大被映到 0.5 / 1pot 时原先整手 `structural_gap` 失同步);只接管原先 Err 的情形,`nlhe_replay` 副本同改。44 机离线回放 155 个实盘请求:翻前 `structural_gap` 20 次 → 0(19 次转 blueprint),其余 74 个翻前决策逐条不变;200BB ante-flop2-3bet 4B crop 加载正常,advisor 峰值 9.2 GB。**已知未修**:真实码深远深于 blueprint 时,blueprint 选 AllIn 按桌面 max_raise 全推(回放 KTs 424BB 推满),面对深码全下仍按影子跟注(T8s 跟 235BB);原先这些手多被大开池失同步挡掉 | advisor `big_open_projects_down_to_small_open` `stack_bb_arg_and_proportional_stack_gates` |

## 6-max 复用 / 需重做(亲验 file:line,2026-05-30)

**✅ 已 N-generic 直接复用**:规则引擎(side pot / showdown / per-seat payoff,`state.rs:846–938`,
`config.rs::default_6max_100bb()`)、CFR trainer(ES-MCCFR/LCFR alternating traverser,`trainer.rs:493,653`)、
InfoSetId position 已留 4 bit(支持 0..15 座,`abstraction/map/mod.rs`)、dense 后端 / checkpoint(不绑人数)。

**⚠️ 中等改动**:`SimplifiedNlheGame` 硬编码 `n_seats=2`(`nlhe.rs:310`)→ 参数化 + 重建 betting tree(HU
240,096 节点 / 119.7M infoset 是 2 人数,6-max 暴涨,S2 量);动作抽象扩到**按位置**;Game trait 零和约束
(`game.rs:120`)推广为"全玩家和=0"。

**❌ 重活(Pluribus 论文难点)**:① 抽象层 equity/OCHS 假设 **1 对手**(`equity.rs:39,79`)→ 多人 equity +
重聚类桶(**最大未知数,S3 先验证**);② 评测重构——LBR `probe_idx%2`(`lbr.rs:5`)多人失义、AIVAT 单对手要
推广多对手、要新 baseline + 解决无强参考对手。

> **现状(2026-06-03,上为 05-30 初评快照)**:⚠️ 中等改动**已做**(P4 `08b3edc` 参数化 `new_with_abstraction`,
> n_seats 由 config 驱动)。❌① **已消解**(S3 实测 HU 单对手桶可复用进 A3×A4 ≤3-way,不需重做多人 equity)。
> ❌② **评测已重构**:S5 off-tree advisor 引擎(`blueprint_advisor.rs`)+ ① 跨抽象 h2h + ② OpenPoker 客户端
> (强参考对手缺口已接、live smoke 已过)。剩 = AIVAT 多对手(S5③,按需) + 绝对强度量化挂场。

## 关键代码入口

- CFR/LCFR-MCCFR:`src/training/trainer.rs`(`EsMccfrTrainer` / `recurse_es` / `recurse_es_parallel` /
  `maybe_lcfr_rescale`;`with_lcfr_period(P)`)。Brown & Sandholm 2018 Discounted MCCFR(arxiv 1809.04040)。
- NLHE state + 树:`src/training/nlhe.rs` + `nlhe_betting_tree.rs`;按街动作抽象 `src/abstraction/action.rs`
  (`StreetActionAbstraction`,单一来源 `nlhe.rs::nlhe_action_abstraction()`)。
- 抽象:preflop 169 lossless `src/abstraction/preflop.rs`;equity/OCHS `src/abstraction/equity.rs`;
  InfoSetId 打包 `src/abstraction/map/mod.rs`。
- 评测:LBR `src/training/lbr.rs`;baseline `src/training/nlhe_eval.rs`;AIVAT `src/training/aivat*.rs`。
- **S5 跨抽象 advisor 引擎**:`src/training/blueprint_advisor.rs`(一张权威 GameState + 每 blueprint 一份抽象影子,
  off-tree 翻译推进:`outgoing_action` / `advance_shadow_by_applied` / `play_cross_abstraction_hand` /
  `evaluate_cross_abstraction_h2h`)。薄壳:① h2h `tools/six_max_blueprint_h2h`;② OpenPoker
  `tools/openpoker_advisor.rs` + `tools/openpoker_play.py`(WS driver);HU `tools/slumbot_advisor.rs`(复用同核)。
- 规则:`src/rules/`(config / state / side pot / showdown)。

代码结构:`src/{rules,hand_eval,abstraction,training}/` + `tests/`(cargo test)+ `tools/`(诊断/训练 binary)。

## 持久 artifact + 主机

- **vultr `~/dezhou_20260508/artifacts/`**:1B dense ckpt(`run_dense_lockfree/`,9.3 GB,Slumbot 续跑 +
  6-max baseline 候选)、HU bucket 表(1000 / 500,HU equity;b3sum/EVR 见 git 历史)。
- 主机表:

| host | 角色 | 状态 |
|---|---|---|
| vultr 64.176.35.138 (4 vCPU / 11.67 GiB) | 持久存储 + 短测试 | 长期持有;**跑不动 NLHE 训练**(3M update 进 swap) |
| AWS(按需起/停,IP 每次变) | 训练 | HU 用 c6a.8xlarge(32 vCPU);6-max 大概率不够,待 S2 sizing 定更大机 |

## 构建 / 测试

命令见 `CLAUDE.md`。性能/正确性 SLO + BLAKE3 anchor 在 `cargo test --release -- --ignored`;可选 PokerKit 跨验证(6-max S1 要用)。

## 文档维护规则

- 工作笔记 / 临时数据 → `docs/temp/*.md`。
- 本文档 = 当前代码真实状态入口;主线验收目标在 `docs/six_max_nlhe_target.md`。
