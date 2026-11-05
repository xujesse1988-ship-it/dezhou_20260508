"""对手别名：同一个人的多个名字（换号、服务器截断名字）在画像里合成一个。

别名表是 JSON（缺省 tools/poker_live/wopo_aliases.json）：
  exact    {原名: 别名}，先查；
  patterns [{regex, alias}]，按顺序对原名做 re.search，第一个命中的生效；
都不中 = 原名。只换发给 advisor 的名字（observe / 决策请求 / 画像暖启动）；play log、hh、事件流照旧记原名。

opponent_analysis/aliases.py 是同一套规则的独立实现（那边不引用 tools/ 的代码），读同一份 JSON。
"""
import json
import os
import re

DEFAULT_PATH = os.path.join(os.path.dirname(os.path.abspath(__file__)), "wopo_aliases.json")


class AliasError(ValueError):
    pass


class NameAliases:
    def __init__(self, exact=None, patterns=()):
        self.exact = dict(exact or {})
        self.patterns = [(re.compile(rx), alias) for rx, alias in patterns]
        for raw, alias in self.exact.items():
            if not isinstance(raw, str) or not isinstance(alias, str) or not raw or not alias:
                raise AliasError(f"exact 里有空的或不是字符串的名字: {raw!r} → {alias!r}")
        for rx, alias in self.patterns:
            if not isinstance(alias, str) or not alias:
                raise AliasError(f"pattern {rx.pattern!r} 的 alias 为空")

    def __bool__(self):
        return bool(self.exact or self.patterns)

    def canon(self, name):
        """单个名字 → 别名（不认识 = 原样）。"""
        if not isinstance(name, str):
            return name
        if name in self.exact:
            return self.exact[name]
        for rx, alias in self.patterns:
            if rx.search(name):
                return alias
        return name

    def apply(self, names, hero_seat=None):
        """一手的 {座位: 名字} → 换成别名的新 dict（键原样，hero 座不换）。同一手里换完会和别的座重名的，
        这些被换的座退回原名（反复检查到没有重名为止），避免两个座位的动作记到同一个人头上。"""
        raw = dict(names)
        out = {s: (n if s == hero_seat else self.canon(n)) for s, n in raw.items()}
        while True:
            count = {}
            for n in out.values():
                count[n] = count.get(n, 0) + 1
            clash = [s for s, n in out.items() if n != raw[s] and count[n] > 1]
            if not clash:
                return out
            for s in clash:
                out[s] = raw[s]


def load_aliases(path):
    """读别名表；path 为 None / 'off' → 空表（名字原样）。格式错直接报错，不静默忽略。"""
    if path in (None, "off"):
        return NameAliases()
    with open(path, encoding="utf-8") as f:
        d = json.load(f)
    if not isinstance(d, dict):
        raise AliasError(f"{path}: 顶层应是对象")
    exact = d.get("exact", {})
    pats = d.get("patterns", [])
    if not isinstance(exact, dict) or not isinstance(pats, list):
        raise AliasError(f"{path}: exact 应是对象、patterns 应是列表")
    try:
        return NameAliases(exact, [(p["regex"], p["alias"]) for p in pats])
    except (KeyError, TypeError, re.error) as e:
        raise AliasError(f"{path}: patterns 格式错: {e!r}") from e
