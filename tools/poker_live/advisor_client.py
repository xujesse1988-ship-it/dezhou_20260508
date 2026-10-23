"""常驻 Rust advisor 子进程（stdio JSON-lines）+ advisor 命令行参数组装。

从 openpoker_play.py 原样抽出（行为 / 命令行 / IPC 字节不变），openpoker_play 与 wopo_play 共用。
"""

import json
import subprocess


class Advisor:
    def __init__(self, binary, checkpoint, bucket_table, reshape, postflop_cap, seed,
                 extra_args=None, cropped=False, n_seats=6):
        # cropped=True：checkpoint 是按街裁切的 crop 文件 → advisor --cropped-checkpoint（缺省
        # False = --checkpoint，命令行与旧版逐字节相同）。
        ckpt_flag = "--cropped-checkpoint" if cropped else "--checkpoint"
        cmd = [binary, ckpt_flag, checkpoint, "--bucket-table", bucket_table,
               "--reshape", reshape, "--postflop-cap", str(postflop_cap), "--seed", str(seed)]
        cmd += list(extra_args or [])  # 缺口②：--search / --search-* 透传给 Rust advisor。
        self.proc = subprocess.Popen(
            cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, bufsize=1,
        )
        line = self.proc.stdout.readline()
        if not line:
            raise RuntimeError("advisor 未输出 ready 行（提前退出？看 stderr）")
        self.ready = json.loads(line)
        if not self.ready.get("ready"):
            raise RuntimeError(f"advisor ready 异常: {line!r}")
        # 座位数一致性：advisor 的 solver 座位数（= blueprint 座位数）必须等于 driver 的桌面
        # 座位数，否则每个请求都会 fallback:not_<N>max（或更糟：座位号越界被静默丢弃）。
        if self.ready.get("n_seats") != n_seats:
            self.close()
            raise RuntimeError(f"advisor n_seats={self.ready.get('n_seats')} ≠ driver --seats "
                               f"{n_seats}（blueprint / --seats 配错）")
        # 未读取的 prewarm 响应行数（prewarm 先发后弃响应；管道有序 → decide 前按计数 drain
        # 即可对齐，不会把预热响应误当决策响应）。
        self._pending_prewarms = 0

    def prewarm(self, req):
        """RoundStart 预热（--search-prewarm）：街起点、hero 行动**前**发 prewarm 请求，让
        advisor 把该街 solve 提前算进缓存（build+solve wall 藏进对手行动时间）。**不等响应**
        （等 = 阻塞 WS 消息处理，预热就白做了）；响应行在下次 decide 前 drain 丢弃。"""
        payload = dict(req)
        payload["prewarm"] = True
        self.proc.stdin.write(json.dumps(payload) + "\n")
        self.proc.stdin.flush()
        self._pending_prewarms += 1

    def observe(self, rec):
        """叠加剥削（--exploit）：每手结束把完整手历史发给 advisor 喂进进程内 Profiler
        （`{"observe":1,names,actions:[{seat,street,action}]}`）。**不等响应**（advisor 返回遥测
        ack，下次 decide 前 drain 丢弃，同 prewarm 锁步机制）。--exploit 关时 driver 不调本方法
        → IPC 流 byte-equal 旧行为。"""
        payload = dict(rec)
        payload["observe"] = True
        self.proc.stdin.write(json.dumps(payload) + "\n")
        self.proc.stdin.flush()
        self._pending_prewarms += 1  # observe 与 prewarm 共用「待 drain 的非决策响应」计数。

    def _drain_pending(self):
        while self._pending_prewarms > 0:
            line = self.proc.stdout.readline()
            if not line:
                raise RuntimeError("advisor 无响应（退出？）")
            self._pending_prewarms -= 1

    def decide(self, req):
        """req = dict（见 openpoker_advisor::Request）。返回 advisor 响应 dict
        {action, amount?, source, ...}。"""
        self._drain_pending()
        self.proc.stdin.write(json.dumps(req) + "\n")
        self.proc.stdin.flush()
        line = self.proc.stdout.readline()
        if not line:
            raise RuntimeError("advisor 无响应（退出？）")
        return json.loads(line)

    def close(self):
        try:
            self.proc.stdin.close()
        except Exception:
            pass
        try:
            self.proc.wait(timeout=10)
        except Exception:
            self.proc.kill()


# ===========================================================================
# advisor 命令行参数（两个入口共用；只管 advisor 透传，入口自己的参数各自加）
# ===========================================================================
def add_advisor_args(p, default_seats, debug_help):
    """往 argparse parser 加 advisor 相关参数。default_seats = 入口默认桌面座位数；
    debug_help = --debug-log 的说明（driver 侧打印内容各入口不同）。"""
    p.add_argument("--advisor-bin", default="target/release/openpoker_advisor")
    # blueprint 二选一：完整 dense ckpt / 按街裁切的 crop 文件（后者需 --search --search-trigger
    # all-postflop，advisor 启动期校验）。
    p.add_argument("--checkpoint", default=None)
    p.add_argument("--cropped-checkpoint", default=None,
                   help="按街裁切的 blueprint（nlhe_dense_crop 产出）；与 --checkpoint 二选一，"
                        "需配 --search --search-trigger all-postflop")
    p.add_argument("--seats", type=int, default=default_seats, choices=range(3, 10),
                   metavar="{3..9}",
                   help=f"advisor / blueprint 座位数（实际牌桌人数支持范围由入口确定；默认 {default_seats}）")
    p.add_argument("--bucket-table", required=True)
    p.add_argument("--reshape", default="preopen")
    p.add_argument("--postflop-cap", type=int, default=3)
    p.add_argument("--seed", type=int, default=1)
    p.add_argument("--debug-log", action="store_true", help=debug_help)
    # 缺口② 实时搜索（透传给 Rust advisor；开了才送 stacks 真栈、postflop 触发面 re-solve）。
    p.add_argument("--search", action="store_true",
                   help="开实时子博弈搜索（postflop 触发面用真码深 re-solve；缺省纯 blueprint）")
    p.add_argument("--search-iterations", type=int, default=None)
    p.add_argument("--search-trigger", choices=["flop-first-unraised", "all-postflop"], default=None)
    p.add_argument("--search-time-budget-ms", type=int, default=None)
    p.add_argument("--search-lcfr", action="store_true")
    p.add_argument("--search-max-nodes", type=int, default=None)
    # range 先验平滑 λ（advisor 默认开 0.25；显式 0 = 关，A/B 对照臂用）。
    p.add_argument("--search-range-uniform-mix", type=float, default=None)
    # 脱锚搜索档一前缀 reach（advisor 默认 on；off = A/B 对照臂 / 回退到 uniform 先验）。
    # 不传 = 不透传，吃 advisor 默认（ON）。
    p.add_argument("--search-unanchored-prefix-reach", choices=["on", "off"], default=None)
    # 脱锚搜索档二′-跨街复用（advisor 默认 on；off = A/B 对照臂 / 回退到档一前缀）。
    # 不传 = 不透传，吃 advisor 默认（ON）。
    p.add_argument("--search-unanchored-cross-street", choices=["on", "off"], default=None)
    # 仅 flop 街优先 blueprint（advisor 默认 off）：on = flop 锚定面（影子同步）且本手无人 all-in、
    # hero 与每个未弃牌对手的有效码深在 60–140BB 内才走 blueprint、不搜索；不满足 / flop 脱影子
    # 仍实时搜索；turn/river 不受影响。不传 = 吃 advisor 默认（OFF）。
    p.add_argument("--search-flop-prefer-blueprint", choices=["on", "off"], default=None)
    # 搜索一律走脱锚路径（advisor 默认 off）：on = 影子同步、本该锚定搜索的决策也走脱锚搜索
    # （可叠加 --exploit；翻牌访问量不足不再退回 blueprint）。不改哪些决策搜索。不传 = 吃 advisor 默认（OFF）。
    p.add_argument("--search-force-unanchored", choices=["on", "off"], default=None)
    # 平跟当弃牌（方案 B，advisor 默认 off）：on = 非 SB 开池平跟在 blueprint 影子里记为弃牌，
    # 平跟池翻后搜索拿到范围前缀；翻前仍由 limp_heuristic 决定。不传 = 吃 advisor 默认（OFF）。
    p.add_argument("--search-limp-as-fold", choices=["on", "off"], default=None)
    # 搜索读数门槛（advisor 默认 200）：hero 信息集在 solve 里被遍历不足这么多次就不用这份解，
    # flop 退回 blueprint、转牌 / 河牌兜底；0 = 关。不传 = 吃 advisor 默认。
    p.add_argument("--search-min-hero-visits", type=int, default=None)
    # 弱牌押上全部筹码护栏（advisor 默认 on）：翻后弱牌不全下、不做投入过半剩余筹码的加注。
    # 不传 = 吃 advisor 默认（ON）。
    p.add_argument("--search-weak-commit-guard", choices=["on", "off"], default=None)
    # 深码 SPR 自适应菜单（deep_menu_for：深 {1pot} / 浅 ≤3-way {0.5,1} 全层级）。
    p.add_argument("--search-deep-menu", action="store_true")
    # 子树 solve 的 traverser 只轮子树根仍 Active 的座（弃牌座轮到 = 零学习迭代）。
    # advisor 默认 on；off = 回到全座轮转（A/B 对照臂）。
    p.add_argument("--search-live-traversers", choices=["on", "off"], default=None)
    # 子树独立桶表（如 500/500/500；blueprint 仍用 --bucket-table 的表）。
    p.add_argument("--search-bucket-table", default=None)
    # solve update 并行线程数（同预算 update ≈ ×核数；只助 solve 侧，建树仍单线程）。
    p.add_argument("--search-solve-threads", type=int, default=None)
    # RoundStart 预热：街起点（hero 行动前）让 advisor 提前 build+solve 暖缓存，
    # wall 藏进对手行动时间。driver 侧行为（advisor 按请求响应，无需自己的 flag）。
    p.add_argument("--search-prewarm", action="store_true")
    # 叠加剥削（Tier 2，exploit_strategy_design_2026_06_14）：进程内对手画像 → 翻前 range 宽度先验。
    # 需配 --search（仅挂脱锚搜索路径）。缺省关 = 全程与现网 byte-equal（不带 names、不发 observe）。
    p.add_argument("--exploit", choices=["on", "vpip", "off"], default="off",
                   help="叠加剥削三态：on=VPIP+PFR 形状（被动入池→CallBand/主动→RaiseBand，需 PFR 收敛）/ "
                        "vpip=仅 VPIP 宽度（与现网逐位 byte-equal）/ off=关（默认）；on|vpip 需 --search")
    p.add_argument("--exploit-min-hands", type=int, default=None)
    p.add_argument("--exploit-strength", type=float, default=None)
    p.add_argument("--exploit-converge-se", type=float, default=None)
    p.add_argument("--exploit-converge-drift", type=float, default=None)


def exploit_enabled(args):
    return args.exploit in ("on", "vpip")


def validate_advisor_args(args):
    """参数组合校验（拒绝静默：设了不生效的旗直接报错退出）。"""
    if (args.checkpoint is None) == (args.cropped_checkpoint is None):
        raise SystemExit("--checkpoint 与 --cropped-checkpoint 必须二选一")
    if args.cropped_checkpoint is not None and not args.search:
        raise SystemExit("--cropped-checkpoint 需配 --search（转牌 / 河牌没有 blueprint 数据）")
    exploit_on = exploit_enabled(args)
    if exploit_on and not args.search:
        raise SystemExit("--exploit on|vpip 需配 --search（剥削只挂脱锚搜索路径；没有搜索无处叠加）")
    if (args.exploit_min_hands is not None or args.exploit_strength is not None
            or args.exploit_converge_se is not None or args.exploit_converge_drift is not None) \
            and not exploit_on:
        raise SystemExit("--exploit-* 子旗需配 --exploit on|vpip")
    if args.search_prewarm and not args.search:
        raise SystemExit("--search-prewarm 需配 --search（拒绝静默：没有搜索就没有可预热的 solve）")
    if args.search_unanchored_prefix_reach is not None and not args.search:
        raise SystemExit("--search-unanchored-prefix-reach 需配 --search"
                         "（拒绝静默：没有搜索就没有脱锚 range 先验，否则误以为在跑 off 臂实则纯 blueprint）")
    if args.search_unanchored_cross_street is not None and not args.search:
        raise SystemExit("--search-unanchored-cross-street 需配 --search"
                         "（拒绝静默：没有搜索就没有跨街复用，否则误以为在跑 off 臂实则纯 blueprint）")
    if args.search_flop_prefer_blueprint is not None and not args.search:
        raise SystemExit("--search-flop-prefer-blueprint 需配 --search"
                         "（拒绝静默：没有搜索 flop 本就走 blueprint，设此旗无意义）")
    if args.search_force_unanchored is not None and not args.search:
        raise SystemExit("--search-force-unanchored 需配 --search（它只改搜索走哪条路径）")
    if args.search_limp_as_fold is not None and not args.search:
        raise SystemExit("--search-limp-as-fold 需配 --search"
                         "（拒绝静默：它只给翻后搜索提供范围前缀，没有搜索设了无意义）")
    if args.search_min_hero_visits is not None and not args.search:
        raise SystemExit("--search-min-hero-visits 需配 --search（它只管搜索读数）")
    if args.search_min_hero_visits is not None and args.search_min_hero_visits < 0:
        raise SystemExit("--search-min-hero-visits 须 ≥ 0")
    if args.search_weak_commit_guard is not None and not args.search:
        raise SystemExit("--search-weak-commit-guard 需配 --search（护栏挂在 advisor 搜索运行时上）")


def advisor_extra_args(args, warm_profile_hh=None):
    """透传给 advisor 的额外命令行（顺序与旧 openpoker_play.main 逐字节相同）。
    warm_profile_hh = 暖启动剥削画像的 HH 文件（仅 exploit 开时透传；caller 判存在性）。"""
    extra = []
    if args.seats != 6:
        # 缺省 6 不透传（advisor 默认 6，命令行与旧版逐字节相同）。
        extra += ["--seats", str(args.seats)]
    if args.search:
        extra.append("--search")
        if args.search_iterations is not None:
            extra += ["--search-iterations", str(args.search_iterations)]
        if args.search_trigger is not None:
            extra += ["--search-trigger", args.search_trigger]
        if args.search_time_budget_ms is not None:
            extra += ["--search-time-budget-ms", str(args.search_time_budget_ms)]
        if args.search_lcfr:
            extra.append("--search-lcfr")
        if args.search_max_nodes is not None:
            extra += ["--search-max-nodes", str(args.search_max_nodes)]
        if args.search_range_uniform_mix is not None:
            extra += ["--search-range-uniform-mix", str(args.search_range_uniform_mix)]
        if args.search_unanchored_prefix_reach is not None:
            extra += ["--search-unanchored-prefix-reach", args.search_unanchored_prefix_reach]
        if args.search_unanchored_cross_street is not None:
            extra += ["--search-unanchored-cross-street", args.search_unanchored_cross_street]
        if args.search_flop_prefer_blueprint is not None:
            extra += ["--search-flop-prefer-blueprint", args.search_flop_prefer_blueprint]
        if args.search_force_unanchored is not None:
            extra += ["--search-force-unanchored", args.search_force_unanchored]
        if args.search_limp_as_fold is not None:
            extra += ["--search-limp-as-fold", args.search_limp_as_fold]
        if args.search_min_hero_visits is not None:
            extra += ["--search-min-hero-visits", str(args.search_min_hero_visits)]
        if args.search_weak_commit_guard is not None:
            extra += ["--search-weak-commit-guard", args.search_weak_commit_guard]
        if args.search_deep_menu:
            extra.append("--search-deep-menu")
        if args.search_live_traversers is not None:
            extra += ["--search-live-traversers", args.search_live_traversers]
        if args.search_bucket_table:
            extra += ["--search-bucket-table", args.search_bucket_table]
        if args.search_solve_threads is not None:
            extra += ["--search-solve-threads", str(args.search_solve_threads)]
    # 叠加剥削透传（仅 on|vpip 时；advisor 侧 guard 再校验需 --search）。三态原样透传给 advisor。
    if exploit_enabled(args):
        extra += ["--exploit", args.exploit]
        if args.exploit_min_hands is not None:
            extra += ["--exploit-min-hands", str(args.exploit_min_hands)]
        if args.exploit_strength is not None:
            extra += ["--exploit-strength", str(args.exploit_strength)]
        if args.exploit_converge_se is not None:
            extra += ["--exploit-converge-se", str(args.exploit_converge_se)]
        if args.exploit_converge_drift is not None:
            extra += ["--exploit-converge-drift", str(args.exploit_converge_drift)]
        if warm_profile_hh:
            extra += ["--warm-profile-hh", warm_profile_hh]
    # --debug-log 透传给 advisor：打印决策流水线 + range/solve 中间数据（与 --search 正交，
    # blueprint 路径也打）。driver 侧的牌局中间数据 + advisor 内部数据由同一旗一起开。
    if args.debug_log:
        extra.append("--debug-log")
    return extra


def start_advisor(args, extra):
    cropped = args.cropped_checkpoint is not None
    return Advisor(args.advisor_bin, args.cropped_checkpoint if cropped else args.checkpoint,
                   args.bucket_table, args.reshape, args.postflop_cap, args.seed,
                   extra_args=extra, cropped=cropped, n_seats=args.seats)
