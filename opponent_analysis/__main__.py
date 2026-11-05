"""CLI: python3 -m opponent_analysis {ingest,report,live,history} ..."""

from __future__ import annotations

import argparse
import sys

from . import aliases as aliases_mod
from . import db
from .platforms import PLATFORMS
from .report import Config, analyze, write_report


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(prog="opponent_analysis", description="离线对手可剥削性分析（每个平台独立）")
    sub = ap.add_subparsers(dest="cmd", required=True)

    ing = sub.add_parser("ingest", help="把手牌日志校验后存入平台数据库")
    ing.add_argument("--db", required=True)
    ing.add_argument("--platform", choices=sorted(PLATFORMS), help="可选；与文件实际格式不符时报错")
    src = ing.add_mutually_exclusive_group(required=True)
    src.add_argument("--run", nargs="+", help="run 目录（自动找 *_hh.jsonl 与 *_ws.jsonl）")
    src.add_argument("--hh", help="单个 hh 文件")
    ing.add_argument("--ws", help="配合 --hh：OpenPoker ws 日志，用于识别超时代打")

    rep = sub.add_parser("report", help="生成某个平台的报告")
    rep.add_argument("--db", required=True)
    rep.add_argument("--platform", required=True, choices=sorted(PLATFORMS))
    rep.add_argument("--out", required=True)
    rep.add_argument("--min-hands", type=int, default=Config.min_hands)
    rep.add_argument("--min-opps", type=int, default=Config.min_opps)
    rep.add_argument("--bluff-pct", type=float, default=Config.bluff_pct)
    rep.add_argument("--recent-hands", type=int, default=Config.recent_hands,
                    help="「最近」= 每个玩家自己的最近 N 手")
    rep.add_argument("--hero", nargs="*", default=[],
                    help="另外算作我方的名字（日志没被导入的我方 bot），不计入对手和群体基准")
    rep.add_argument("--aliases", default=None,
                     help="对手别名表（JSON，同一个人的多个名字合成一个画像，如 tools/poker_live/wopo_aliases.json）；"
                         "数据库照旧存原名，缺省不合并")

    lv = sub.add_parser("live", help="跟随正在写入的 hh 日志，实时更新画像并开 HTTP 页面")
    lv.add_argument("--db", required=True)
    lv.add_argument("--platform", required=True, choices=sorted(PLATFORMS))
    lv.add_argument("--follow", nargs="+", required=True,
                    help="hh 日志路径或通配符（加引号），每次轮询重新展开，新的 run 目录会自动接上")
    lv.add_argument("--hero", nargs="*", default=[],
                    help="另外算作我方的名字（日志没被跟随的我方 bot），不计入对手和群体基准")
    lv.add_argument("--aliases", default=None,
                    help="对手别名表（JSON，同一个人的多个名字合成一个画像，如 tools/poker_live/wopo_aliases.json）；"
                         "数据库照旧存原名，缺省不合并")
    lv.add_argument("--host", default="127.0.0.1")
    lv.add_argument("--port", type=int, default=8780)
    lv.add_argument("--poll", type=float, default=2.0, help="读日志间隔（秒）")
    lv.add_argument("--refresh", type=float, default=5.0, help="有新手时重算画像的最短间隔（秒）")
    lv.add_argument("--min-hands", type=int, default=Config.min_hands)
    lv.add_argument("--min-opps", type=int, default=Config.min_opps)
    lv.add_argument("--bluff-pct", type=float, default=Config.bluff_pct)
    lv.add_argument("--recent-hands", type=int, default=Config.recent_hands,
                    help="「最近」= 每个玩家自己的最近 N 手")
    lv.add_argument("--play-events", nargs="+", default=None,
                    help="wopo driver 的 --play-events 事件文件（路径或通配符，加引号）：页面顶部显示实时行动指南；"
                         "缺省不显示")
    lv.add_argument("--play-hands", type=int, default=20, help="行动指南保留的最近手数（含当前手）")
    lv.add_argument("--play-ranges", nargs="+", default=None,
                    help="advisor 的 --range-log 文件（路径或通配符）：行动指南显示对手每条街的牌力分布；"
                         "缺省 = 每个 --play-events 同目录的 wopo_ranges.jsonl（driver 的默认位置），off 关")

    hs = sub.add_parser("history", help="wopo 牌局回放：浏览器里逐步回放打完的手（读 hh 日志，有事件流时带上建议）")
    hs.add_argument("--run", nargs="+", required=True,
                    help="run 目录或 wopo_hh.jsonl（可用通配符，加引号）；同目录的 wopo_play_events.jsonl / "
                         "wopo_ranges.jsonl 有就一起读")
    hs.add_argument("--host", default="127.0.0.1")
    hs.add_argument("--port", type=int, default=8781)
    hs.add_argument("--preflop", action="store_true",
                    help="同时在 /preflop/ 提供 blueprint 翻前策略查看（拉起 target/release/preflop_viewer，"
                         "加载约 30 秒、峰值约 1.6 GiB）")

    a = ap.parse_args(argv)
    if a.cmd == "live" and a.play_hands < 1:
        ap.error("--play-hands 须 ≥ 1")  # exits with status 2 and the usage line
    a.alias_table = None
    if a.cmd in ("report", "live"):
        try:
            a.alias_table = aliases_mod.load(a.aliases)
        except (OSError, ValueError) as e:
            ap.error(f"--aliases 读不了：{e}")
    try:
        return _run(a)
    except db.DatabaseBusy as e:
        print(f"数据库正被另一个进程写入：{e}", file=sys.stderr)
        return 2


def _run(a) -> int:
    if a.cmd == "history":
        from .history import serve as serve_history

        serve_history(a.run, a.host, a.port, preflop=a.preflop)
        return 0
    if a.cmd == "live":
        from .live import default_range_patterns, serve

        cfg = Config(min_hands=a.min_hands, min_opps=a.min_opps, bluff_pct=a.bluff_pct, recent_hands=a.recent_hands)
        if a.play_ranges == ["off"] or not a.play_events:
            ranges = None
        else:
            ranges = a.play_ranges or default_range_patterns(a.play_events)
        serve(a.db, a.platform, a.follow, a.host, a.port, cfg, a.poll, a.refresh, a.hero,
              play_patterns=a.play_events, play_hands=a.play_hands, range_patterns=ranges,
              aliases=a.alias_table)
        return 0
    if a.cmd == "ingest":
        jobs = []
        if a.run:
            for d in a.run:
                jobs.append(db.find_run_files(d))
        else:
            jobs.append((a.hh, a.ws))
        for hh, ws in jobs:
            e = db.ingest(a.db, hh, ws, a.platform)
            print(f"{e['platform']}: {hh} -> {e['counts']}")
        return 0
    hands = [a.alias_table.hand(h) for h in db.load_hands(a.db, a.platform)]
    if not hands:
        print(f"{a.platform}: 数据库里没有手牌", file=sys.stderr)
        return 1
    cfg = Config(min_hands=a.min_hands, min_opps=a.min_opps, bluff_pct=a.bluff_pct, recent_hands=a.recent_hands)
    res = analyze(hands, a.platform, db.load_rejected(a.db, a.platform), cfg, a.hero)
    md, js = write_report(res, a.out)
    print(md)
    print(js)
    return 0


if __name__ == "__main__":
    sys.exit(main())
