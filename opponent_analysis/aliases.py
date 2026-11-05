"""Opponent aliases: several names of one person (account switching, server-truncated names) share one profile.

The table is the JSON the wopo driver uses (tools/poker_live/wopo_aliases.json; passed with --aliases):
  exact    {raw name: alias}, looked up first;
  patterns [{regex, alias}], re.search on the raw name in order, first hit wins;
otherwise the raw name. Applied when hands enter the analysis (the database keeps raw names), so changing the
table needs no re-ingest. Same rules as tools/poker_live/aliases.py (this package does not import tools/).
"""

from __future__ import annotations

import dataclasses
import json
import re
from typing import Optional

from .model import Hand


class AliasError(ValueError):
    pass


class Aliases:
    def __init__(self, exact: Optional[dict] = None, patterns=()):
        self.exact = dict(exact or {})
        self.patterns = [(re.compile(rx), alias) for rx, alias in patterns]
        for raw, alias in self.exact.items():
            if not isinstance(raw, str) or not isinstance(alias, str) or not raw or not alias:
                raise AliasError(f"empty or non-string name in exact: {raw!r} -> {alias!r}")
        for rx, alias in self.patterns:
            if not isinstance(alias, str) or not alias:
                raise AliasError(f"empty alias for pattern {rx.pattern!r}")
        self.seen: dict[str, str] = {}  # raw -> alias actually applied so far (for the page's name lookup)

    def __bool__(self) -> bool:
        return bool(self.exact or self.patterns)

    def canon(self, name):
        if not isinstance(name, str):
            return name
        if name in self.exact:
            return self.exact[name]
        for rx, alias in self.patterns:
            if rx.search(name):
                return alias
        return name

    def names(self, names: dict, hero_seat=None) -> dict:
        """One hand's {seat: name} -> aliased copy. The hero seat is never renamed. Seats whose alias would
        clash with another seat's name in the same hand keep their raw names (repeated until no clash), so
        two seats' actions never land on one person."""
        raw = dict(names)
        out = {s: (n if s == hero_seat else self.canon(n)) for s, n in raw.items()}
        while True:
            count: dict = {}
            for n in out.values():
                count[n] = count.get(n, 0) + 1
            clash = [s for s, n in out.items() if n != raw[s] and count[n] > 1]
            if not clash:
                break
            for s in clash:
                out[s] = raw[s]
        for s, n in out.items():
            if n != raw[s]:
                self.seen[raw[s]] = n
        return out

    def hand(self, h: Hand) -> Hand:
        if not self:
            return h
        new = self.names(h.names, h.hero_seat)
        return h if new == h.names else dataclasses.replace(h, names=new)


def load(path: Optional[str]) -> Aliases:
    """Read the alias table; None / 'off' -> empty (names unchanged). Bad format raises, never ignored."""
    if path in (None, "off"):
        return Aliases()
    with open(path, encoding="utf-8") as f:
        d = json.load(f)
    if not isinstance(d, dict):
        raise AliasError(f"{path}: top level must be an object")
    exact = d.get("exact", {})
    pats = d.get("patterns", [])
    if not isinstance(exact, dict) or not isinstance(pats, list):
        raise AliasError(f"{path}: exact must be an object, patterns a list")
    try:
        return Aliases(exact, [(p["regex"], p["alias"]) for p in pats])
    except (KeyError, TypeError, re.error) as e:
        raise AliasError(f"{path}: bad pattern: {e!r}") from e
