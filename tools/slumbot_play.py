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
def NewHand(token):
    import requests
    data = {}
    if token:
        data['token'] = token
    response = requests.post(f'https://{host}/slumbot/api/new_hand', headers={}, json=data)
    if response.status_code != 200:
        raise RuntimeError(f'new_hand status {response.status_code}: {response.text}')
    r = response.json()
    if 'error_msg' in r:
        raise RuntimeError(f'new_hand error: {r["error_msg"]}')
    return r


def Act(token, incr):
    import requests
    data = {'token': token, 'incr': incr}
    response = requests.post(f'https://{host}/slumbot/api/act', headers={}, json=data)
    if response.status_code != 200:
        raise RuntimeError(f'act status {response.status_code}: {response.text}')
    r = response.json()
    if 'error_msg' in r:
        raise RuntimeError(f'act error: {r["error_msg"]}')
    return r


def Login(username, password):
    import requests
    data = {'username': username, 'password': password}
    response = requests.post(f'https://{host}/slumbot/api/login', json=data)
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
        return resp['incr']

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
def play_hand(token, advisor):
    r = NewHand(token)
    new_token = r.get('token')
    if new_token:
        token = new_token
    while True:
        winnings = r.get('winnings')
        if winnings is not None:
            return token, winnings
        action = r.get('action', '')
        client_pos = r.get('client_pos')
        hole_cards = r.get('hole_cards')
        board = r.get('board', [])
        incr = advisor.act(hole_cards, board, client_pos, action)
        r = Act(token, incr)


def run_real(advisor, num_hands, login_token):
    token = login_token
    mbb = []
    errors = 0
    played = 0
    while played < num_hands:
        try:
            token, w = play_hand(token, advisor)
        except Exception as e:
            # 不静默继续：打全上下文日志（advisor error 含 offending request），放弃该手
            # （不计入统计），重置 token 起新会话续打。systematic 问题 → 累计上限后停。
            errors += 1
            print(f'  [hand error #{errors}] {e}', file=sys.stderr)
            token = login_token
            if errors > 20:
                print('  错误累计 > 20，疑似 systematic 问题，停止联机', file=sys.stderr)
                break
            continue
        mbb.append(w * 1000.0 / BIG_BLIND)  # chips → mbb（1 BB = 100 chips）
        played += 1
        if played % 50 == 0:
            print(f'  [{played}/{num_hands}] running mbb/g = {sum(mbb) / len(mbb):.1f}',
                  file=sys.stderr)
    report(mbb)
    if errors:
        print(f'  ({errors} 手因 advisor/replay error 放弃，未计入统计)')


def report(mbb):
    n = len(mbb)
    if n == 0:
        print('no hands played')
        return
    mean = sum(mbb) / n
    if n > 1:
        var = sum((x - mean) ** 2 for x in mbb) / (n - 1)
        se = math.sqrt(var / n)
    else:
        se = 0.0
    total_chips = sum(x * BIG_BLIND / 1000.0 for x in mbb)
    print(f'hands={n}  total_chips={total_chips:.0f}  '
          f'mbb/g={mean:.2f} ± {1.96 * se:.2f} (95% CI)  SE={se:.2f}')
    print(f'  (mbb/g = milli-big-blind per hand；bb/100 = mbb/g × 0.1 × 100 = {mean:.2f}/10 = '
          f'{mean / 10:.2f})')


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
            incr = advisor.act(our_hole, bd, client_pos, action)
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
            run_real(advisor, args.num_hands, token)
    finally:
        advisor.close()


if __name__ == '__main__':
    main()
