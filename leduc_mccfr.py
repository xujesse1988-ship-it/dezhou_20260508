#!/usr/bin/env python3
"""双人限注莱杜克扑克的 external-sampling MCCFR 示例。

莱杜克扑克规则：
1. 牌堆
   使用 J、Q、K 三种点数，每种点数两张牌，一共 6 张牌。
   本程序用 0、1、2 分别表示 J、Q、K。

2. 底注与发牌
   每位玩家先投入 1 个筹码作为 ante。
   然后每位玩家各拿 1 张私牌；这张私牌只有自己可见。

3. 第一轮下注
   公共牌发出前进行第一轮下注。
   玩家 0 先行动，可以 check 或 bet。
   本程序默认第一轮 bet/raise 的固定大小为 2 个筹码。

4. 公共牌与第二轮下注
   第一轮若无人弃牌，则发出 1 张公共牌。
   公共牌对双方都可见，然后进入第二轮下注。
   第二轮仍由玩家 0 先行动，固定 bet/raise 大小为 4 个筹码。

5. 加注上限
   本程序默认每轮最多允许两次进攻动作，也就是一次 bet 加一次 raise。
   达到上限后，面对下注的一方只能 call 或 fold。

6. 终局
   如果有人 fold，另一名玩家立刻赢得底池。
   如果第二轮下注结束仍无人弃牌，则进入摊牌。

7. 摊牌大小
   私牌与公共牌点数相同即为对子。
   对子胜过所有非对子。
   双方都没有对子时，私牌点数大者胜：K > Q > J。
   如果双方牌力完全相同，则平分底池。

直接运行：

    python leduc_mccfr.py --iterations 200000

也可以作为小库导入：

    from leduc_mccfr import LeducMCCFRTrainer
"""

from __future__ import annotations

import argparse
import random
from dataclasses import dataclass, field
from typing import Dict, Iterable, Mapping, Sequence, Tuple


ANTE = 1
BET_SIZES = (2, 4)
DECK = (0, 0, 1, 1, 2, 2)
CARD_NAMES = {0: "J", 1: "Q", 2: "K"}


@dataclass(frozen=True)
class State:
    """下注状态。

    round0 记录第一轮下注历史。
    round1 记录第二轮下注历史；公共牌还没发出时为 None。

    历史字符串里的动作编码：
    - c: check 或 call，具体含义取决于当前是否面对下注。
    - b: bet 或 raise，具体含义取决于当前是否已经有人下注。
    - f: fold。

    例子：
    - State("", None): 第一轮刚开始。
    - State("cc", ""): 第一轮双方过牌，公共牌已发，第二轮刚开始。
    - State("b", None): 第一轮玩家 0 已下注，玩家 1 正在面对下注。
    - State("bc", ""): 第一轮玩家 0 下注、玩家 1 跟注，进入第二轮。
    """

    round0: str = ""
    round1: str | None = None

    @property
    def round_index(self) -> int:
        """当前下注轮：0 表示公共牌前，1 表示公共牌后。"""
        return 0 if self.round1 is None else 1

    @property
    def betting_history(self) -> str:
        """返回当前这一轮的下注历史，便于通用处理两轮下注。"""
        return self.round0 if self.round1 is None else self.round1


@dataclass
class InfoSet:
    """一个信息集的遗憾值与平均策略累加器。

    信息集是“不完美信息博弈”里的核心概念：玩家只知道自己的私牌、
    公共牌（如果已发出）和公开下注历史，不知道对手私牌。因此所有在
    这些可见信息上相同的真实局面，都属于同一个信息集。
    """

    key: str
    actions: Tuple[str, ...]
    regret_sum: Dict[str, float] = field(default_factory=dict)
    strategy_sum: Dict[str, float] = field(default_factory=dict)

    def __post_init__(self) -> None:
        # 每个合法动作各维护一个累计遗憾值和一个平均策略累计值。
        self.regret_sum = {action: 0.0 for action in self.actions}
        self.strategy_sum = {action: 0.0 for action in self.actions}

    def strategy(self) -> Dict[str, float]:
        """用 regret matching 从累计遗憾值计算当前策略。"""
        # 只使用正遗憾：某个动作过去“后悔没多选”的量越大，当前越倾向选它。
        positive_regrets = {
            action: max(self.regret_sum[action], 0.0) for action in self.actions
        }
        normalizer = sum(positive_regrets.values())
        if normalizer > 0.0:
            return {
                action: positive_regrets[action] / normalizer
                for action in self.actions
            }

        # 如果所有动作都没有正遗憾，就先均匀随机。
        probability = 1.0 / len(self.actions)
        return {action: probability for action in self.actions}

    def accumulate_strategy(self, weight: float) -> None:
        """把当前策略累加到平均策略里。

        weight 是到达当前信息集的概率权重。external-sampling MCCFR 中，
        平均策略要按玩家自己策略导致的到达概率加权。
        """
        strategy = self.strategy()
        for action, probability in strategy.items():
            self.strategy_sum[action] += weight * probability

    def average_strategy(self) -> Dict[str, float]:
        """返回训练过程中的平均策略。"""
        normalizer = sum(self.strategy_sum.values())
        if normalizer > 0.0:
            return {
                action: self.strategy_sum[action] / normalizer
                for action in self.actions
            }

        probability = 1.0 / len(self.actions)
        return {action: probability for action in self.actions}


def betting_round_complete(history: str) -> bool:
    """判断一轮下注是否已经结束。"""
    if history.endswith("f"):
        return True
    if history == "cc":
        return True
    # 只要这一轮出现过下注，并且最后一个动作是 call，这轮就结束。
    return "b" in history and history.endswith("c")


def has_outstanding_bet(history: str) -> bool:
    """判断当前行动者是否正面对尚未跟注的 bet/raise。"""
    return "b" in history and not history.endswith(("c", "f"))


def legal_actions(state: State, max_bets_per_round: int = 2) -> Tuple[str, ...]:
    """返回当前状态下的合法动作编码。"""
    if is_terminal(state):
        return ()

    history = state.betting_history
    if betting_round_complete(history):
        return ()

    if not has_outstanding_bet(history):
        return ("c", "b")  # check, bet

    if history.count("b") < max_bets_per_round:
        return ("c", "b", "f")  # call, raise, fold
    return ("c", "f")  # 达到加注上限后，只能 call 或 fold


def action_name(state: State, action: str) -> str:
    """把动作编码转成人类可读名称。"""
    if action == "c":
        return "call" if has_outstanding_bet(state.betting_history) else "check"
    if action == "b":
        return "raise" if has_outstanding_bet(state.betting_history) else "bet"
    if action == "f":
        return "fold"
    raise ValueError(f"unknown action: {action}")


def current_player(state: State) -> int:
    """根据当前轮下注历史长度判断该谁行动。"""
    if is_terminal(state):
        raise ValueError(f"terminal state has no current player: {state}")
    # 每轮都由玩家 0 先行动，所以偶数长度轮到玩家 0，奇数长度轮到玩家 1。
    return len(state.betting_history) % 2


def apply_action(
    state: State,
    action: str,
    max_bets_per_round: int = 2,
) -> State:
    """执行一个下注动作；第一轮结束且无人弃牌时，自动进入公共牌轮。"""
    actions = legal_actions(state, max_bets_per_round)
    if action not in actions:
        raise ValueError(f"illegal action {action!r} in state {state}")

    history = state.betting_history + action
    if state.round1 is None:
        # 第一轮以 call 或 check/check 结束时，公共牌已经在 deal[2] 中固定，
        # 这里把 round1 置成空字符串，表示进入第二轮下注。
        if betting_round_complete(history) and not history.endswith("f"):
            return State(round0=history, round1="")
        return State(round0=history, round1=None)
    return State(round0=state.round0, round1=history)


def is_terminal(state: State) -> bool:
    """判断牌局是否结束。"""
    history = state.betting_history
    if history.endswith("f"):
        return True
    # 只有第二轮结束才会摊牌；第一轮正常结束只是进入公共牌轮。
    return state.round1 is not None and betting_round_complete(state.round1)


def round_contributions(history: str, bet_size: int) -> Tuple[int, int]:
    """计算某一轮两名玩家各自投入了多少筹码，不包含 ante。"""
    contributions = [0, 0]
    current_bet = 0

    for index, action in enumerate(history):
        player = index % 2
        if action == "c":
            # 没有面对下注时，current_bet 为 0，所以 check 不会增加投入；
            # 面对下注时，补齐到当前下注额，即 call。
            contributions[player] += current_bet - contributions[player]
        elif action == "b":
            # bet/raise 都表示把当前轮下注额再提高一个固定 bet_size。
            call_amount = current_bet - contributions[player]
            current_bet += bet_size
            contributions[player] += call_amount + bet_size
        elif action == "f":
            # fold 不再投入额外筹码。
            pass
        else:
            raise ValueError(f"unknown action in history: {action}")

    return contributions[0], contributions[1]


def total_contributions(state: State) -> Tuple[int, int]:
    """计算整手牌到终局时两名玩家的总投入，包含 ante。"""
    contributions = [ANTE, ANTE]

    round0 = round_contributions(state.round0, BET_SIZES[0])
    contributions[0] += round0[0]
    contributions[1] += round0[1]

    if state.round1 is not None:
        round1 = round_contributions(state.round1, BET_SIZES[1])
        contributions[0] += round1[0]
        contributions[1] += round1[1]

    return contributions[0], contributions[1]


def showdown_winner(private_cards: Sequence[int], public_card: int) -> int | None:
    """摊牌比较大小；返回赢家编号，平分底池则返回 None。"""
    scores = []
    for card in private_cards:
        # 分数先比较是否成对，再比较私牌点数。
        has_pair = 1 if card == public_card else 0
        scores.append((has_pair, card))

    if scores[0] == scores[1]:
        return None
    return 0 if scores[0] > scores[1] else 1


def payoff(state: State, deal: Sequence[int], player: int) -> float:
    """计算某位玩家的终局收益，已经扣除自己的 ante 和下注投入。"""
    if not is_terminal(state):
        raise ValueError(f"state is not terminal: {state}")

    contributions = total_contributions(state)
    pot = sum(contributions)

    if state.betting_history.endswith("f"):
        # 最后一个动作是 fold；执行 fold 的玩家输，另一方拿下底池。
        folder = (len(state.betting_history) - 1) % 2
        winner = 1 - folder
    else:
        winner = showdown_winner(deal[:2], deal[2])

    if winner is None:
        # 平分底池时，收益等于拿回半个底池减去自己的总投入。
        return pot / 2.0 - contributions[player]
    if winner == player:
        return float(pot - contributions[player])
    return float(-contributions[player])


def payoff_player0(state: State, deal: Sequence[int]) -> float:
    return payoff(state, deal, player=0)


class LeducMCCFRTrainer:
    """双人限注莱杜克扑克的 external-sampling MCCFR 训练器。"""

    def __init__(self, seed: int | None = None, max_bets_per_round: int = 2) -> None:
        self.rng = random.Random(seed)
        self.max_bets_per_round = max_bets_per_round
        # key 是信息集字符串，value 保存该信息集的累计遗憾和平均策略。
        self.info_sets: Dict[str, InfoSet] = {}

    def train(self, iterations: int) -> None:
        """训练指定轮数。

        每次迭代先采样一副完整发牌，然后分别让玩家 0 和玩家 1 各做一次
        traverser。这样两个玩家的信息集遗憾值都会被更新。
        """
        for _ in range(iterations):
            cards = list(DECK)
            self.rng.shuffle(cards)
            # deal = (玩家0私牌, 玩家1私牌, 公共牌)。
            deal = (cards[0], cards[1], cards[2])
            self._external_sampling(State(), deal, traverser=0, reach=1.0)
            self._external_sampling(State(), deal, traverser=1, reach=1.0)

    def average_strategy(self) -> Dict[str, Dict[str, float]]:
        """导出所有信息集的平均策略。"""
        return {
            key: info_set.average_strategy()
            for key, info_set in sorted(self.info_sets.items())
        }

    def expected_value(self, strategy_profile: Mapping[str, Mapping[str, float]]) -> float:
        """枚举所有发牌，计算给定策略下玩家 0 的期望收益。"""
        total = 0.0
        deals = 0

        # 这里按物理牌索引枚举，而不是只枚举点数。
        # 例如两张 J 是两张不同的实体牌，抽走其中一张后会影响公共牌分布。
        for private0_index, private0 in enumerate(DECK):
            for private1_index, private1 in enumerate(DECK):
                if private1_index == private0_index:
                    continue
                for public_index, public in enumerate(DECK):
                    if public_index in (private0_index, private1_index):
                        continue
                    total += self._expected_value_recursive(
                        State(),
                        (private0, private1, public),
                        strategy_profile,
                    )
                    deals += 1

        return total / deals

    def _external_sampling(
        self,
        state: State,
        deal: Sequence[int],
        traverser: int,
        reach: float,
    ) -> float:
        """external-sampling MCCFR 的递归核心。

        traverser 是本次要更新遗憾值的玩家。
        - 如果当前行动者是 traverser，就枚举所有合法动作并更新遗憾。
        - 如果当前行动者是对手，就只按对手当前策略采样一个动作。

        这就是 external sampling：只采样对手和机会节点，遍历自己的动作。
        """
        if is_terminal(state):
            return payoff(state, deal, traverser)

        player = current_player(state)
        info_set = self._info_set(state, deal, player)
        strategy = info_set.strategy()

        if player == traverser:
            # 当前玩家是被训练的一方：此处要把当前策略计入平均策略。
            info_set.accumulate_strategy(weight=reach)
            action_values: Dict[str, float] = {}
            node_value = 0.0

            # 枚举 traverser 的所有动作，计算每个动作的反事实价值。
            for action, probability in strategy.items():
                child = apply_action(state, action, self.max_bets_per_round)
                action_value = self._external_sampling(
                    child,
                    deal,
                    traverser,
                    reach * probability,
                )
                action_values[action] = action_value
                node_value += probability * action_value

            # 遗憾值 = 选某个动作的价值 - 按当前混合策略行动的价值。
            # 如果某个动作长期更好，它的正遗憾会推动策略更多选择它。
            for action in info_set.actions:
                info_set.regret_sum[action] += action_values[action] - node_value
            return node_value

        # 当前玩家不是 traverser：不枚举所有对手动作，只随机采样一个。
        sampled_action = self._sample_action(strategy)
        child = apply_action(state, sampled_action, self.max_bets_per_round)
        return self._external_sampling(child, deal, traverser, reach)

    def _expected_value_recursive(
        self,
        state: State,
        deal: Sequence[int],
        strategy_profile: Mapping[str, Mapping[str, float]],
    ) -> float:
        """用完整博弈树递归计算某个策略 profile 的玩家 0 EV。"""
        if is_terminal(state):
            return payoff_player0(state, deal)

        player = current_player(state)
        key = info_set_key(state, deal, player)
        actions = legal_actions(state, self.max_bets_per_round)
        strategy = complete_strategy(actions, strategy_profile.get(key))

        value = 0.0
        for action in actions:
            child = apply_action(state, action, self.max_bets_per_round)
            # 评估 EV 时不采样，而是按策略概率完整加权求和。
            value += strategy[action] * self._expected_value_recursive(
                child,
                deal,
                strategy_profile,
            )
        return value

    def _info_set(self, state: State, deal: Sequence[int], player: int) -> InfoSet:
        """获取或创建当前玩家在当前可见信息下的信息集。"""
        key = info_set_key(state, deal, player)
        if key not in self.info_sets:
            self.info_sets[key] = InfoSet(
                key,
                legal_actions(state, self.max_bets_per_round),
            )
        return self.info_sets[key]

    def _sample_action(self, strategy: Mapping[str, float]) -> str:
        """按给定概率分布采样一个动作。"""
        threshold = self.rng.random()
        cumulative = 0.0
        last_action = ""
        for action, probability in strategy.items():
            cumulative += probability
            last_action = action
            if threshold <= cumulative:
                return action
        return last_action


def complete_strategy(
    actions: Iterable[str],
    strategy: Mapping[str, float] | None,
) -> Dict[str, float]:
    """补全并归一化一个策略。

    评估 EV 时，有些信息集可能没有被训练访问过，或者传入策略缺少某些动作。
    这里会把缺失/非法概率修正成一个完整的合法动作分布。
    """
    actions = tuple(actions)
    if strategy is None:
        probability = 1.0 / len(actions)
        return {action: probability for action in actions}

    total = sum(max(strategy.get(action, 0.0), 0.0) for action in actions)
    if total <= 0.0:
        probability = 1.0 / len(actions)
        return {action: probability for action in actions}

    return {
        action: max(strategy.get(action, 0.0), 0.0) / total
        for action in actions
    }


def info_set_key(state: State, deal: Sequence[int], player: int) -> str:
    """生成信息集 key。

    第一轮只包含私牌和第一轮下注历史，例如：Q|r0:b。
    第二轮包含私牌、公共牌、第一轮历史和第二轮历史，例如：K/Q|r1:bc:cb。
    注意 key 绝不包含对手私牌，这正是不完美信息的体现。
    """
    private = CARD_NAMES[deal[player]]
    if state.round1 is None:
        return f"{private}|r0:{state.round0 or '-'}"

    public = CARD_NAMES[deal[2]]
    return f"{private}/{public}|r1:{state.round0}:{state.round1 or '-'}"


def state_from_key(key: str) -> State:
    """从信息集 key 里还原下注状态，主要用于打印策略。"""
    _, state_part = key.split("|", maxsplit=1)
    if state_part.startswith("r0:"):
        history = state_part.removeprefix("r0:")
        return State(round0="" if history == "-" else history, round1=None)

    if state_part.startswith("r1:"):
        histories = state_part.removeprefix("r1:")
        round0, round1 = histories.split(":", maxsplit=1)
        return State(round0=round0, round1="" if round1 == "-" else round1)

    raise ValueError(f"invalid information-set key: {key}")


def sorted_info_set_keys(keys: Iterable[str]) -> list[str]:
    """按牌面和下注历史排序，让命令行输出更容易阅读。"""
    card_order = {"J": 0, "Q": 1, "K": 2}
    history_order = {
        "-": 0,
        "c": 1,
        "b": 2,
        "cb": 3,
        "bb": 4,
        "cbb": 5,
    }

    def sort_key(key: str) -> Tuple[int, int, int, int, int]:
        card_part, state_part = key.split("|", maxsplit=1)
        if "/" in card_part:
            private, public = card_part.split("/", maxsplit=1)
            public_index = card_order[public]
        else:
            private = card_part
            public_index = -1

        state = state_from_key(key)
        round0_history = state.round0 or "-"
        round1_history = state.round1 or "-"
        return (
            0 if state_part.startswith("r0:") else 1,
            card_order[private],
            public_index,
            history_order.get(round0_history, 99),
            history_order.get(round1_history, 99),
        )

    return sorted(keys, key=sort_key)


def print_strategy(
    strategy_profile: Mapping[str, Mapping[str, float]],
    only_nonzero: bool = False,
) -> None:
    """打印平均策略。"""
    print("Average strategy:")
    for key in sorted_info_set_keys(strategy_profile):
        state = state_from_key(key)
        actions = []
        for action, probability in strategy_profile[key].items():
            if only_nonzero and probability < 0.005:
                continue
            actions.append(f"{action_name(state, action)}={probability:6.3f}")
        print(f"  {key:12s}  " + "  ".join(actions))


def parse_args() -> argparse.Namespace:
    """解析命令行参数。"""
    parser = argparse.ArgumentParser(
        description="Train an external-sampling MCCFR agent for limit Leduc Poker."
    )
    parser.add_argument(
        "-n",
        "--iterations",
        type=int,
        default=200_000,
        help="number of MCCFR iterations, each traversing both players",
    )
    parser.add_argument("--seed", type=int, default=7, help="random seed")
    parser.add_argument(
        "--max-bets-per-round",
        type=int,
        default=2,
        help="maximum aggressive actions in each round, including the first bet",
    )
    parser.add_argument(
        "--compact",
        action="store_true",
        help="hide actions with average probability below 0.005",
    )
    return parser.parse_args()


def main() -> None:
    """命令行入口：训练、打印平均策略，并报告玩家 0 的 EV。"""
    args = parse_args()
    trainer = LeducMCCFRTrainer(
        seed=args.seed,
        max_bets_per_round=args.max_bets_per_round,
    )
    trainer.train(args.iterations)

    average_strategy = trainer.average_strategy()
    print_strategy(average_strategy, only_nonzero=args.compact)
    value = trainer.expected_value(average_strategy)
    print()
    print(f"Expected value for player 0: {value:.5f}")


if __name__ == "__main__":
    main()
