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
`Profiler`,**只用本进程数据、不依赖过往**,driver 每手发 observe 累积 VPIP/PFR/AF)→ 收敛门(样本≥150 +
VPIP SE≤0.05 + 近窗漂移≤0.08 三条全过才剥削)→ `subgame::apply_exploit_width_prior` 在**脱锚搜索** ranges
组装处把已收敛对手 root range 朝观测翻前 VPIP 宽度凸混合(Chen 序 top-k,α≤0.8 默认 0.5,hero 不混,带 uniform
地板 + card-removal)。**默认关时全程 byte-equal**(driver 不带 names/不发 observe,advisor `exploit=None` 走既有
`_cross`);剥削 ranges 进 solve_cache_key → 开/关/画像更新自动 cache-miss。**只在档一前缀 range 上混一次,跨街后验命中时不再混**(2026-10-21 修:原先每街对后验再混 α,冲掉翻后动作信息)。仅翻前宽度 + 仅脱锚路径(翻后 AF
轴/锚定路径/Tier 3 = 后续);脱锚 prewarm 与决策同一份画像快照(2026-10-23,否则开剥削后预热 key 必 miss);`--search-force-unanchored on` 让锚定搜索也走脱锚路径(默认 off)。正确性靠 Rust 单测(opponent_profile 收敛门 / subgame tilt 方向·
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
| Leduc ES-MCCFR / LCFR-MCCFR | ✅ `ev_p0` 收敛 -0.087;ES 路径 BLAKE3 byte-equal anchor | `tools/leduc_es_mccfr_report` |
| 简化 NLHE ES-MCCFR / LCFR | ✅ LCFR 优化路径 100M LBR 1,233 → 500M 1,126(100M 即饱和) | `run_lcfr_*` on vultr |
| dense 后端 + v4 bucket | ✅ byte-equal HashMap(5 对照)+ 100M LBR 1,143 ≈ baseline(同质量);吞吐 ~2.2× 且长 run 不塌、RAM 平 5.2 GiB、ckpt 不暴涨 | `tests/dense_nlhe_trainer.rs` |
| AIVAT 评测器 | ✅ 无偏全证;真日志降方差 1.21×(精确项 deals+runout) | `tests/aivat_nlhe_*.rs` `docs/aivat_eval.md` |
| **CFR trainer / 规则引擎 6-max N-generic** | ✅ 亲验:`recurse_es` 取 `payoff(traverser)` 不取负、traverser `% n_players`;规则多人 side pot 返回 per-seat payoff 向量 | `trainer.rs:493,653` `state.rs:846–938` |
| 规则:无人能回应时禁 bet/raise(LA-009,2026-10-16) | ✅ 其余玩家全弃牌 / all-in 或都不够跟满当前最高注 → 只能 Check/Call/Fold。PokerKit 0.4.14 随机不等码深逐决策对照 16.6 万点:我方多放行 0(修前 1834+588);剩余「PokerKit 放行、我方不放行」全部是不足额 all-in 不重开(TDA 规则,PokerKit 未实现)。等码深行为不变 | `rules/action.rs` LA-009、`state.rs` `legal_action_tests` |

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
