"""CLI: python3 -m opponent_analysis {ingest,report} ..."""

from __future__ import annotations

import argparse
import sys

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

    a = ap.parse_args(argv)
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
    hands = db.load_hands(a.db, a.platform)
    if not hands:
        print(f"{a.platform}: 数据库里没有手牌", file=sys.stderr)
        return 1
    cfg = Config(min_hands=a.min_hands, min_opps=a.min_opps, bluff_pct=a.bluff_pct)
    res = analyze(hands, a.platform, db.load_rejected(a.db, a.platform), cfg)
    md, js = write_report(res, a.out)
    print(md)
    print(js)
    return 0


if __name__ == "__main__":
    sys.exit(main())
