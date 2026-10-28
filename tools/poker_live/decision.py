"""打牌决策层：组 advisor 请求、按 source 分桶计数、串行调 advisor 的后台 worker。

服务器无关：输入是牌桌层已经校验过的牌局数据，输出 advisor 响应。openpoker_play 用
make_request / tally_source；wopo_play 另用 DecisionWorker（决策在后台线程跑，消息处理不阻塞）。
"""

import queue
import sys
import threading
import time


def make_request(hole, board, button_seat, my_seat, num_seats, small_blind, big_blind,
                 actions, valid, stacks=None, dealt_seats=None, names=None, ante=0, posts=None):
    """advisor 决策请求（openpoker_advisor::Request）。键序固定（IPC 字节可比）：
    stacks / dealt_seats / names / posts 为 None（或空）、ante 为 0 时不带（advisor 退各自缺省行为）。
    posts = 本手补 live 大盲的座位（wopo 新进场 / 回座补盲）。"""
    req = {
        "hole": hole,
        "board": board,
        "button_seat": button_seat,
        "my_seat": my_seat,
        "num_seats": num_seats,
        "small_blind": small_blind,
        "big_blind": big_blind,
    }
    if ante:
        req["ante"] = ante
    req["actions"] = actions
    req["valid"] = valid
    if stacks is not None:
        req["stacks"] = stacks
    if dealt_seats is not None:
        req["dealt_seats"] = dealt_seats
    if names:
        req["names"] = names  # json.dumps：int 座位键 → 字符串键，Rust BTreeMap<u8,_> 解析。
    if posts:
        req["posts"] = posts
    return req


def tally_source(counters, fallback_reasons, src):
    """按 advisor 响应 source 分桶计数（counters 须有 fallback/search/limp_heuristic/blueprint/
    decisions 键）。blueprint=blueprint 策略；search=实时搜索解出（含 search:unanchored）；
    limp_heuristic=preflop open-limp 启发式；fallback=兜底（blueprint 结构性 fallback:* +
    搜索解不出来 search_giveup:*）。search_giveup 也以 "search" 开头，先判兜底。
    兜底原因按 source 前 80 字符归并，首次见某类打 [new-fallback]。返回桶名。"""
    src = str(src or "")
    if src.startswith("fallback") or src.startswith("search_giveup"):
        counters["fallback"] += 1
        key = src[:80]
        if key not in fallback_reasons:
            print(f"  [new-fallback decisions={counters['decisions']}] {src}", file=sys.stderr)
        fallback_reasons[key] = fallback_reasons.get(key, 0) + 1
        return "fallback"
    if src.startswith("search"):
        counters["search"] += 1
        return "search"
    if src.startswith("limp_heuristic"):
        counters["limp_heuristic"] += 1
        return "limp_heuristic"
    counters["blueprint"] += 1
    return "blueprint"


def safe_response(valid, source):
    """advisor 不可用时的安全动作：能 check 就 check、否则 fold（同 advisor safe_fallback）。"""
    return {"action": "check" if valid.get("can_check") else "fold", "source": source}


class DecisionWorker:
    """advisor 的唯一调用方：decide / prewarm / observe 按提交顺序串行执行（advisor 管道有序，
    prewarm/observe 的响应靠 decide 前 drain 对齐，必须同一线程用）。

    sync=False：后台线程执行，submit_* 立即返回，decide 结果经 callback(resp, decide_ms) 回给
    调用方（在 worker 线程里调）；消息处理线程不被搜索阻塞，hero 在搜索期间自行出牌的消息照样
    及时处理（调用方据此判结果过期）。sync=True：在调用线程里当场执行（回放 / selftest 确定性）。"""

    def __init__(self, advisor, sync=False):
        self.advisor = advisor
        self.sync = sync
        self._q = queue.Queue()
        self._thread = None
        if not sync:
            self._thread = threading.Thread(target=self._loop, daemon=True)
            self._thread.start()

    def submit_decide(self, req, callback):
        self._submit(("decide", req, callback))

    def submit_prewarm(self, req):
        self._submit(("prewarm", req, None))

    def submit_observe(self, rec):
        self._submit(("observe", rec, None))

    def _submit(self, job):
        if self.sync:
            self._run(job)
        else:
            self._q.put(job)

    def _loop(self):
        while True:
            job = self._q.get()
            if job is None:
                return
            self._run(job)

    def _run(self, job):
        kind, payload, callback = job
        if kind == "decide":
            t0 = time.monotonic()
            try:
                resp = self.advisor.decide(payload)
            except Exception as e:
                print(f"  [advisor 异常] {e} → 安全动作兜底", file=sys.stderr)
                resp = safe_response(payload.get("valid", {}), "fallback:advisor_exception")
            ms = int(round((time.monotonic() - t0) * 1000))
            try:
                callback(resp, ms)
            except Exception as e:
                print(f"  [decision callback 异常] {e!r}", file=sys.stderr)
            return
        try:
            if kind == "prewarm":
                self.advisor.prewarm(payload)
            else:
                self.advisor.observe(payload)
        except Exception as e:
            print(f"  [{kind} 异常] {e}（忽略）", file=sys.stderr)

    def close(self, timeout=None):
        """排空已提交的任务后停线程（timeout 秒内没跑完就不等了）。"""
        if self._thread is not None:
            self._q.put(None)
            self._thread.join(timeout)
