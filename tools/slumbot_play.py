#!/usr/bin/env python3
"""Slumbot HUNL API driver（docs/temp/slumbot_api_bridge_plan_2026_05_29.md M5/M6）。

fork 自 ericgjackson 的 sample_api.py：Login/NewHand/Act/ParseAction 原样保留（端点
用真实 /slumbot/api/...），把 "naive check/call" 那段换成调常驻 Rust advisor
（tools/slumbot_advisor.rs）拿 incr。累计 winnings → mbb/g + 95% CI。

两种模式：
  --selftest         本地 mock：不连 slumbot.com，用 ParseAction + call-station 对手 +
                     随机发牌，把若干完整手局跑通 advisor（M5 验收：跑通一手）。
  （默认）           真实联机 slumbot.com 打 --num-hands 手（M6，需用户授权对外服务）。

网络全在本文件（Python），Rust crate 零网络依赖。
"""

import argparse
import json
import math
import random
import subprocess
import sys
import time

# `requests` 只在真实联机路径用，延迟到 NewHand/Act/Login 内 import——这样离线
# --selftest（M5）在没装 requests 的机器上也能跑。

host = 'slumbot.com'

NUM_STREETS = 4
SMALL_BLIND = 50
BIG_BLIND = 100
STACK_SIZE = 20000


# ===========================================================================
# ParseAction（移植自 sample_api.py，逐行保留；修了原文 'Illegal fold' 的 set 笔误）
# ===========================================================================
def ParseAction(action):
    """返回 dict：st / pos（下一动者，-1=手结束）/ street_last_bet_to / total_last_bet_to
    / last_bet_size / last_bettor。'error' 键表示解析失败。"""
    st = 0
    street_last_bet_to = BIG_BLIND
    total_last_bet_to = BIG_BLIND
    last_bet_size = BIG_BLIND - SMALL_BLIND
    last_bettor = 0
    sz = len(action)
    pos = 1
    if sz == 0:
        return {
            'st': st, 'pos': pos, 'street_last_bet_to': street_last_bet_to,
            'total_last_bet_to': total_last_bet_to, 'last_bet_size': last_bet_size,
            'last_bettor': last_bettor,
        }

    check_or_call_ends_street = False
    i = 0
    while i < sz:
        if st >= NUM_STREETS:
            return {'error': 'Unexpected error'}
        c = action[i]
        i += 1
        if c == 'k':
            if last_bet_size > 0:
                return {'error': 'Illegal check'}
            if check_or_call_ends_street:
                if st < NUM_STREETS - 1 and i < sz:
                    if action[i] != '/':
                        return {'error': 'Missing slash'}
                    i += 1
                if st == NUM_STREETS - 1:
                    pos = -1
                else:
                    pos = 0
                    st += 1
                street_last_bet_to = 0
                check_or_call_ends_street = False
            else:
                pos = (pos + 1) % 2
                check_or_call_ends_street = True
        elif c == 'c':
            if last_bet_size == 0:
                return {'error': 'Illegal call'}
            if total_last_bet_to == STACK_SIZE:
                if i != sz:
                    for _st1 in range(st, NUM_STREETS - 1):
                        if i == sz:
                            return {'error': 'Missing slash (end of string)'}
                        else:
                            c = action[i]
                            i += 1
                            if c != '/':
                                return {'error': 'Missing slash'}
                if i != sz:
                    return {'error': 'Extra characters at end of action'}
                st = NUM_STREETS - 1
                pos = -1
                last_bet_size = 0
                return {
                    'st': st, 'pos': pos, 'street_last_bet_to': street_last_bet_to,
                    'total_last_bet_to': total_last_bet_to, 'last_bet_size': last_bet_size,
                    'last_bettor': last_bettor,
                }
            if check_or_call_ends_street:
                if st < NUM_STREETS - 1 and i < sz:
                    if action[i] != '/':
                        return {'error': 'Missing slash'}
                    i += 1
                if st == NUM_STREETS - 1:
                    pos = -1
                else:
                    pos = 0
                    st += 1
                street_last_bet_to = 0
                check_or_call_ends_street = False
            else:
                pos = (pos + 1) % 2
                check_or_call_ends_street = True
            last_bet_size = 0
            last_bettor = -1
        elif c == 'f':
            if last_bet_size == 0:
                return {'error': 'Illegal fold'}
            if i != sz:
                return {'error': 'Extra characters at end of action'}
            pos = -1
            return {
                'st': st, 'pos': pos, 'street_last_bet_to': street_last_bet_to,
                'total_last_bet_to': total_last_bet_to, 'last_bet_size': last_bet_size,
                'last_bettor': last_bettor,
            }
        elif c == 'b':
            j = i
            while i < sz and '0' <= action[i] <= '9':
                i += 1
            if i == j:
                return {'error': 'Missing bet size'}
            try:
                new_street_last_bet_to = int(action[j:i])
            except (TypeError, ValueError):
                return {'error': 'Bet size not an integer'}
            new_last_bet_size = new_street_last_bet_to - street_last_bet_to
            remaining = STACK_SIZE - total_last_bet_to
            if last_bet_size > 0:
                min_bet_size = last_bet_size
                if min_bet_size < BIG_BLIND:
                    min_bet_size = BIG_BLIND
            else:
                min_bet_size = BIG_BLIND
            if min_bet_size > remaining:
                min_bet_size = remaining
            if new_last_bet_size < min_bet_size:
                return {'error': 'Bet too small'}
            max_bet_size = remaining
            if new_last_bet_size > max_bet_size:
                return {'error': 'Bet too big'}
            last_bet_size = new_last_bet_size
            street_last_bet_to = new_street_last_bet_to
            total_last_bet_to += last_bet_size
            last_bettor = pos
            pos = (pos + 1) % 2
            check_or_call_ends_street = True
        else:
            return {'error': 'Unexpected character in action'}

    return {
        'st': st, 'pos': pos, 'street_last_bet_to': street_last_bet_to,
        'total_last_bet_to': total_last_bet_to, 'last_bet_size': last_bet_size,
        'last_bettor': last_bettor,
    }


# ===========================================================================
# Slumbot 网络层（真实端点；M6 用）
# ===========================================================================
class NetworkError(RuntimeError):
    """网络抖动（连接断/超时/SSL EOF），与 advisor/replay/Slumbot 拒单区分开。"""


def _post(endpoint, data, retries=4, backoff=2.0, timeout=30):
    """POST 到 Slumbot，对瞬时网络异常（SSL EOF / 超时 / 连接断）重试 `retries` 次
    （线性退避）。重试耗尽抛 NetworkError；HTTP 非 200 / error_msg 由 caller 处理。"""
    import requests
    url = f'https://{host}/slumbot/api/{endpoint}'
    last = None
    for attempt in range(retries):
        try:
            return requests.post(url, headers={}, json=data, timeout=timeout)
        except requests.exceptions.RequestException as e:
            last = e
            if attempt < retries - 1:
                time.sleep(backoff)
    raise NetworkError(f'{endpoint} 网络重试 {retries} 次仍失败: {last}')


def NewHand(token):
    data = {'token': token} if token else {}
    response = _post('new_hand', data)
    if response.status_code != 200:
        raise RuntimeError(f'new_hand status {response.status_code}: {response.text}')
    r = response.json()
    if 'error_msg' in r:
        raise RuntimeError(f'new_hand error: {r["error_msg"]}')
    return r


def Act(token, incr):
    response = _post('act', {'token': token, 'incr': incr})
    if response.status_code != 200:
        raise RuntimeError(f'act status {response.status_code}: {response.text}')
    r = response.json()
    if 'error_msg' in r:
        raise RuntimeError(f'act error: {r["error_msg"]}')
    return r


def Login(username, password):
    response = _post('login', {'username': username, 'password': password})
    if response.status_code != 200:
        raise RuntimeError(f'login status {response.status_code}: {response.text}')
    r = response.json()
    if 'error_msg' in r:
        raise RuntimeError(f'login error: {r["error_msg"]}')
    token = r.get('token')
    if not token:
        raise RuntimeError('login: no token in response')
    return token


# ===========================================================================
# 常驻 advisor 子进程
# ===========================================================================
class Advisor:
    def __init__(self, binary, checkpoint, bucket_table, fallback, seed):
        self.proc = subprocess.Popen(
            [binary, '--dense', '--checkpoint', checkpoint, '--bucket-table', bucket_table,
             '--fallback-policy', fallback, '--seed', str(seed)],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, bufsize=1,
        )
        line = self.proc.stdout.readline()
        if not line:
            raise RuntimeError('advisor 未输出 ready 行（提前退出？看 stderr）')
        ready = json.loads(line)
        if not ready.get('ready'):
            raise RuntimeError(f'advisor ready 行异常: {line!r}')
        self.ready = ready

    def act(self, hole_cards, board, client_pos, action):
        """返回 advisor 完整响应 dict：{'incr': ..., 'decision': {...}}。
        'decision' 是该决策点的明细（街/infoset/合法动作 + 分布/选了哪个/是否均匀兜底）。"""
        req = json.dumps({
            'hole_cards': hole_cards, 'board': board,
            'client_pos': client_pos, 'action': action,
        })
        self.proc.stdin.write(req + '\n')
        self.proc.stdin.flush()
        line = self.proc.stdout.readline()
        if not line:
            raise RuntimeError('advisor 无响应（退出？）')
        resp = json.loads(line)
        if 'error' in resp:
            raise RuntimeError(f'advisor error on {req}: {resp["error"]}')
        return resp

    def close(self):
        try:
            self.proc.stdin.close()
        except Exception:
            pass
        self.proc.wait(timeout=10)


# 当前街已翻开的公共牌数。
_BOARD_LEN = [0, 3, 4, 5]


def board_for_street(full_board, st):
    return full_board[: _BOARD_LEN[st]]


# ===========================================================================
# 真实对局（M6）
# ===========================================================================
def play_hand(token, advisor, repro_log=None):
    """打一手。返回 (token, winnings, transcript)。transcript 记录该手完整牌局：
    我方 hole / 最终 board / client_pos / 最终 action 串 / 我方逐步 incr / winnings（chips）/
    decisions（每个我方决策点的明细：街/infoset/合法动作 + blueprint 分布/选了哪个/兜底标记/
    决策前的 action 串与 board）。"""
    r = NewHand(token)
    new_token = r.get('token')
    if new_token:
        token = new_token
    our_incrs = []
    decisions = []
    hole_cards = r.get('hole_cards')
    client_pos = r.get('client_pos')
    board = r.get('board', [])
    while True:
        # 每个响应都刷新最新的 board / action / client_pos（hole 全程不变）。
        if r.get('hole_cards') is not None:
            hole_cards = r.get('hole_cards')
        if r.get('client_pos') is not None:
            client_pos = r.get('client_pos')
        board = r.get('board', board)
        action = r.get('action', '')
        winnings = r.get('winnings')
        if winnings is not None:
            transcript = {
                'type': 'hand',
                'client_pos': client_pos, 'hole_cards': hole_cards, 'board': board,
                'action': action, 'our_incrs': our_incrs, 'decisions': decisions,
                'winnings': winnings,
            }
            return token, winnings, transcript
        resp = advisor.act(hole_cards, board, client_pos, action)
        incr = resp['incr']
        our_incrs.append(incr)
        # 记录该决策点明细：advisor 给的 decision + 决策当时看到的 action 串与 board。
        dec = dict(resp.get('decision') or {})
        dec['action_before'] = action
        dec['board_at_decision'] = list(board)
        decisions.append(dec)
        try:
            r = Act(token, incr)
        except Exception as e:
            # Slumbot 拒了我们的 incr（如 "Illegal bet"）。落盘完整上下文供离线复现 +
            # 诊断（ParseAction(action) 可算出 Slumbot 合法区间，对比我们的 to）。
            ctx = {
                'hole_cards': hole_cards, 'board': board, 'client_pos': client_pos,
                'action': action, 'incr': incr, 'slumbot_error': str(e),
            }
            if repro_log:
                with open(repro_log, 'a') as f:
                    f.write(json.dumps(ctx) + '\n')
            raise RuntimeError(
                f'Slumbot 拒 incr={incr!r}（action={action!r} pos={client_pos} '
                f'board={board} hole={hole_cards}）: {e}'
            )


def run_real(advisor, num_hands, login_token, repro_log=None, hand_log=None):
    token = login_token
    mbb = []
    pos_mbb = {0: [], 1: []}  # 按 Slumbot client_pos 分组（1=SB/button，0=BB）
    errors = 0
    played = 0
    hand_log_f = open(hand_log, 'w') if hand_log else None
    try:
        while played < num_hands:
            try:
                token, w, transcript = play_hand(token, advisor, repro_log=repro_log)
            except Exception as e:
                # 不静默继续：打全上下文日志（含 offending request + 当前已成功手数），放弃
                # 该手（不计入统计），重置 token 起新会话续打。区分网络抖动 vs advisor/拒单。
                # 网络抖动已在 _post 内重试过，到这里说明重试也没救回。systematic → 上限后停。
                errors += 1
                kind = "网络" if isinstance(e, NetworkError) else "advisor/重放/拒单"
                print(f'  [drop #{errors} 类型={kind} @ played={played}] {e}', file=sys.stderr)
                token = login_token
                if errors > 20:
                    print('  错误累计 > 20，疑似 systematic 问题，停止联机', file=sys.stderr)
                    break
                continue
            w_mbb = w * 1000.0 / BIG_BLIND  # chips → mbb（1 BB = 100 chips）
            mbb.append(w_mbb)
            cp = transcript.get('client_pos')
            if cp in pos_mbb:
                pos_mbb[cp].append(w_mbb)
            played += 1
            if hand_log_f:
                transcript['hand'] = played
                transcript['mbb'] = w_mbb
                hand_log_f.write(json.dumps(transcript, ensure_ascii=False) + '\n')
                hand_log_f.flush()
            # 每 100 局做一次统计：打到 stderr + 作为 type=stats 行写进文件（累计口径）。
            if played % 100 == 0:
                stats = build_stats(mbb, pos_mbb, errors)
                print(f'  [{played}/{num_hands}] mbb/g = {stats["mbb_per_g"]:.1f}  '
                      f'total = {stats["total_bb"]:+.1f} BB', file=sys.stderr)
                if hand_log_f:
                    rec = {'type': 'stats'}
                    rec.update(stats)
                    hand_log_f.write(json.dumps(rec, ensure_ascii=False) + '\n')
                    hand_log_f.flush()
        # 文件末尾追加最终 summary 行（总输赢 + mbb/g + CI + 分位置拆分 + 放弃手数）。
        if hand_log_f:
            summary = {'type': 'summary'}
            summary.update(build_stats(mbb, pos_mbb, errors))
            hand_log_f.write(json.dumps(summary, ensure_ascii=False) + '\n')
            hand_log_f.flush()
    finally:
        if hand_log_f:
            hand_log_f.close()
    report(mbb)
    if errors:
        print(f'  ({errors} 手因 error 放弃，未计入统计；类型见上方 [drop #N] 行——'
              f'网络抖动已自动重试，仍失败才放弃)')
    if hand_log:
        print(f'  牌局明细已记录到 {hand_log}（{played} 手 JSONL + 每 100 局 type=stats '
              f'+ 末尾 type=summary 统计行）')


def compute_stats(mbb):
    """一组每手 mbb（= winnings_chips × 1000 / BB）→ 统计 dict。
    含总输赢（total_chips / total_bb）、mbb/g、SE、95% CI、bb/100。空列表只含 hands=0。"""
    n = len(mbb)
    if n == 0:
        return {'hands': 0}
    mean = sum(mbb) / n
    if n > 1:
        var = sum((x - mean) ** 2 for x in mbb) / (n - 1)
        se = math.sqrt(var / n)
    else:
        se = 0.0
    total_chips = sum(x * BIG_BLIND / 1000.0 for x in mbb)  # mbb → chips（1 BB = 100 chips）
    ci = 1.96 * se
    return {
        'hands': n,
        'total_chips': total_chips,      # 总净输赢（筹码）
        'total_bb': total_chips / BIG_BLIND,  # 总净输赢（BB）
        'mbb_per_g': mean,
        'se': se,
        'ci95': ci,
        # bb/100 = 每 100 手净 BB = (mbb/g) / 10（1 BB = 1000 mbb，再 ×100 手）。
        'bb_per_100': mean / 10.0,
        'bb_per_100_ci95': ci / 10.0,
    }


def build_stats(mbb, pos_mbb, errors):
    """整体统计 + 放弃手数 + 分位置拆分（SB/button vs BB）。周期 stats 行与末尾 summary
    行同形，便于下游统一解析、看 mbb/g 随手数的轨迹。"""
    s = dict(compute_stats(mbb))
    s['errors_dropped'] = errors
    s['by_position'] = {
        'sb_button': compute_stats(pos_mbb[1]),  # Slumbot pos 1 = SB/button
        'bb': compute_stats(pos_mbb[0]),         # pos 0 = BB
    }
    return s


def report(mbb):
    s = compute_stats(mbb)
    if s['hands'] == 0:
        print('no hands played')
        return
    mean, ci = s['mbb_per_g'], s['ci95']
    print(f"hands={s['hands']}  total={s['total_chips']:.0f} chips ({s['total_bb']:+.1f} BB)  "
          f"mbb/g={mean:.2f} ± {ci:.2f} (95% CI)  SE={s['se']:.2f}")
    print(f"  bb/100 = {s['bb_per_100']:.1f} ± {s['bb_per_100_ci95']:.1f}  "
          f"(95% CI [{(mean - ci) / 10:.0f}, {(mean + ci) / 10:.0f}] BB/100；"
          f"即每 100 手净 {s['bb_per_100']:.1f} BB)")


# ===========================================================================
# 本地 mock（M5 验收：不连网，跑通完整手局）
# ===========================================================================
def append_incr(action, incr):
    """把我方/对手 incr 接到 action 串尾，街切换时按 Slumbot 格式补 '/'."""
    before = ParseAction(action)
    new_action = action + incr
    after = ParseAction(new_action)
    if 'error' in after:
        raise RuntimeError(f'append_incr 产出非法串 {new_action!r}: {after["error"]}')
    if after.get('pos', -1) != -1 and after['st'] > before['st']:
        new_action += '/'
    return new_action


def opponent_incr(parsed):
    """mock 对手 = call station：能 check 就 check，面对下注就 call（从不弃牌/加注，
    保证手局打满到摊牌，最大化 advisor 路径覆盖）。"""
    return 'k' if parsed['last_bet_size'] == 0 else 'c'


def mock_play_hand(advisor, rng):
    """本地模拟一手：随机发牌 + call-station 对手 + ParseAction 推进，在我方决策点调
    advisor。返回 (client_pos, final_action, our_incrs)。winnings 不算（无评估器；真实
    强度在 M6 联机测）——本 mock 只验证驱动循环 + advisor 接通 + 出合法 incr。"""
    deck = [r + s for r in '23456789TJQKA' for s in 'cdhs']
    rng.shuffle(deck)
    our_hole = deck[0:2]
    board = deck[4:9]
    client_pos = rng.choice([0, 1])
    action = ''
    our_incrs = []
    guard = 0
    while True:
        guard += 1
        if guard > 300:
            raise RuntimeError(f'mock 手局循环失控 action={action!r}')
        a = ParseAction(action)
        if 'error' in a:
            raise RuntimeError(f'ParseAction({action!r}) error: {a["error"]}')
        pos = a['pos']
        if pos == -1:
            return client_pos, action, our_incrs
        bd = board_for_street(board, a['st'])
        if pos == client_pos:
            incr = advisor.act(our_hole, bd, client_pos, action)['incr']
            our_incrs.append(incr)
            _validate_incr(incr)
        else:
            incr = opponent_incr(a)
        action = append_incr(action, incr)


def _validate_incr(incr):
    ok = incr in ('f', 'k', 'c') or (incr.startswith('b') and incr[1:].isdigit())
    if not ok:
        raise RuntimeError(f'advisor 出非法 incr {incr!r}')


def run_selftest(advisor, num_hands, seed):
    rng = random.Random(seed)
    print(f'advisor ready: update_count={advisor.ready.get("update_count")} '
          f'strategy_blake3={advisor.ready.get("strategy_blake3")}', file=sys.stderr)
    for h in range(num_hands):
        client_pos, action, our_incrs = mock_play_hand(advisor, rng)
        print(f'[selftest hand {h + 1}] client_pos={client_pos} '
              f'decisions={len(our_incrs)} final_action={action!r} incrs={our_incrs}')
    print(f'OK: {num_hands} mock hands 跑通，advisor 全程出合法 incr、ParseAction 无报错。')


# ===========================================================================
# main
# ===========================================================================
def main():
    parser = argparse.ArgumentParser(description='Slumbot API driver + Rust advisor')
    parser.add_argument('--advisor-bin', default='target/release/slumbot_advisor')
    parser.add_argument('--checkpoint', required=True)
    parser.add_argument('--bucket-table', required=True)
    parser.add_argument('--fallback-policy', default='hybrid')
    parser.add_argument('--seed', type=int, default=1)
    parser.add_argument('--num-hands', type=int, default=100)
    parser.add_argument('--username', type=str)
    parser.add_argument('--password', type=str)
    parser.add_argument('--selftest', action='store_true',
                        help='本地 mock，不连 slumbot.com（M5 验收）')
    parser.add_argument('--repro-log', type=str, default=None,
                        help='Slumbot 拒我方 incr 时，落盘完整上下文 JSONL 供离线诊断')
    parser.add_argument('--hand-log', type=str, default='slumbot_hands.jsonl',
                        help='每手牌局明细写此 JSONL（type=hand：hole/board/action/our_incrs/'
                             'decisions/winnings/mbb），每 100 局追加 type=stats 统计行、'
                             '末尾追加 type=summary 统计行（总输赢/mbb-g/CI/分位置拆分）；空串关闭')
    args = parser.parse_args()

    advisor = Advisor(args.advisor_bin, args.checkpoint, args.bucket_table,
                      args.fallback_policy, args.seed)
    try:
        if args.selftest:
            run_selftest(advisor, args.num_hands, args.seed)
        else:
            token = None
            if args.username and args.password:
                token = Login(args.username, args.password)
            hand_log = args.hand_log if args.hand_log else None
            run_real(advisor, args.num_hands, token,
                     repro_log=args.repro_log, hand_log=hand_log)
    finally:
        advisor.close()


if __name__ == '__main__':
    main()
