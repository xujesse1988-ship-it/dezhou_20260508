#!/usr/bin/env python3
"""按 args.json 启动本地牌桌客户端；--prepare-only 只生成独立运行目录。"""

import argparse
from datetime import datetime
import importlib.util
import json
import os
from pathlib import Path
import runpy
import shlex
import shutil
import sys
from zoneinfo import ZoneInfo


def settings(root):
    """环境变量优先，其次项目 .env；仅读取本地牌桌专用配置，不执行 shell。"""
    values = {}
    path = root / ".env"
    if path.is_file():
        for line in path.read_text().splitlines():
            line = line.strip()
            if line.startswith("export "):
                line = line[7:].lstrip()
            name, sep, value = line.partition("=")
            name = name.strip()
            if (sep and name in ("POKER_MANAGER_TOKEN", "POKER_MANAGER_URL")
                    and name not in os.environ):
                parts = shlex.split(value, comments=True)
                if len(parts) != 1 or not parts[0]:
                    raise SystemExit(f"{name} must be a non-empty value in .env")
                values[name] = parts[0]
    for name in ("POKER_MANAGER_TOKEN", "POKER_MANAGER_URL"):
        if name in os.environ:
            values[name] = os.environ[name]
    return values


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--prepare-only", action="store_true",
                        help="生成 launch.py / args.json 后退出，不加载 advisor、不连接牌桌")
    opts = parser.parse_args()
    source = Path(__file__).resolve()
    source_dir = source.parent
    root = source_dir.parent.parent
    os.umask(0o077)
    config = settings(root)
    token = config.get("POKER_MANAGER_TOKEN")
    if not opts.prepare_only:
        if not token or any(c.isspace() for c in token):
            raise SystemExit("请在环境变量或项目 .env 中设置 POKER_MANAGER_TOKEN（使用牌桌服务的 token）")
        if importlib.util.find_spec("websocket") is None:
            raise SystemExit("当前 Python 缺少 websocket-client；请先安装，或使用已安装该依赖的 Python 环境")

    args = json.loads((source_dir / "args.json").read_text())
    if not isinstance(args, list) or not all(isinstance(arg, str) for arg in args):
        raise SystemExit("args.json 必须是命令行参数字符串数组")
    if any(arg in ("--token", "--api-key") or arg.startswith(("--token=", "--api-key="))
           for arg in args):
        raise SystemExit("请通过 POKER_MANAGER_TOKEN 配置凭据，不要将 token 写入 args.json")

    if source_dir.parent == root / "artifacts":
        run_dir = source_dir
    else:
        stamp = datetime.now(ZoneInfo("Asia/Shanghai")).strftime("%Y%m%d_%H%M%S_%f")
        run_dir = root / "artifacts" / f"poker_manager_live_{stamp}"
        run_dir.mkdir(parents=True, mode=0o700)
        shutil.copyfile(source, run_dir / "launch.py")
        (run_dir / "launch.py").chmod(0o700)

    url = config.get("POKER_MANAGER_URL", "ws://127.0.0.1:3000/ws")
    args = [arg.replace("{run_dir}", str(run_dir)).replace("{url}", url) for arg in args]
    (run_dir / "args.json").write_text(json.dumps(args, ensure_ascii=False, indent=2) + "\n")
    print(f"运行目录：{run_dir}", flush=True)
    if opts.prepare_only:
        print(f"启动命令：{shlex.join([sys.executable, '-u', str(run_dir / 'launch.py')])}")
        return

    pid_file = run_dir / "driver.pid"
    if pid_file.exists():
        try:
            pid = int(pid_file.read_text().strip())
            if pid > 0:
                os.kill(pid, 0)
                raise SystemExit(f"driver.pid 中的进程 {pid} 仍存在，请使用新的运行目录")
        except (ValueError, ProcessLookupError):
            pass
        except PermissionError:
            raise SystemExit(f"driver.pid 中的进程 {pid} 仍存在，请使用新的运行目录") from None
    os.chdir(root)
    os.environ["POKER_MANAGER_TOKEN"] = token
    pid_file.write_text(str(os.getpid()) + "\n")
    print(f"driver PID：{os.getpid()}；日志：{run_dir / 'play.log'}", flush=True)
    with (run_dir / "play.log").open("a", buffering=1) as log:
        # Redirect descriptors as well: the Rust advisor inherits stderr.
        sys.stdout.flush()
        sys.stderr.flush()
        os.dup2(log.fileno(), 1)
        os.dup2(log.fileno(), 2)
        sys.path.insert(0, str(root / "tools"))
        sys.argv = [str(root / "tools" / "poker_manager_play.py"), *args]
        runpy.run_path(sys.argv[0], run_name="__main__")


if __name__ == "__main__":
    main()
