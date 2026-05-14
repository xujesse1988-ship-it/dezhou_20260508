#!/usr/bin/env python3
"""
跨语言 checkpoint reader（D-357 / API-350）。

`pluribus_stage3_decisions.md` D-357 字面：

    `tools/checkpoint_reader.py`（继承 stage 1 `tools/history_reader.py` + stage 2
    `tools/bucket_table_reader.py` 同型）：minimal bincode + struct 解码，输出
    `(schema_version, trainer_variant, game_variant, update_count, regret_table,
    strategy_sum)`。F3 [报告] 落地，stage 3 主线工作（A1..F2）不依赖。该路径用于
    stage 7 评测脚本 / blueprint visualization。

`src/training/checkpoint.rs` doc 字面 108-byte 定长 header + 变长 bincode body
（regret_table + strategy_sum）+ 32-byte BLAKE3 trailer。本脚本镜像该格式：

```text
// ===== header (108 bytes, 8-byte aligned) =====
offset 0x00: magic:                  [u8; 8] = b"PLCKPT\\0\\0"
offset 0x08: schema_version:         u32 LE = 1
offset 0x0C: trainer_variant:        u8 (0 = VanillaCfr, 1 = EsMccfr)
offset 0x0D: game_variant:           u8 (0 = Kuhn, 1 = Leduc, 2 = SimplifiedNlhe)
offset 0x0E: pad:                    [u8; 6] = 0
offset 0x14: update_count:           u64 LE
offset 0x1C: rng_state:              [u8; 32]
offset 0x3C: bucket_table_blake3:    [u8; 32]（Kuhn / Leduc 全零）
offset 0x5C: regret_table_offset:    u64 LE（≥ 108）
offset 0x64: strategy_sum_offset:    u64 LE
// ===== body (变长, 按 header §regret/strategy offset 定位) =====
// regret_table_body:    bincode 1.x serialized Vec<(InfoSet, Vec<f64>)>
// strategy_sum_body:    bincode 1.x serialized Vec<(InfoSet, Vec<f64>)>
// ===== trailer (32 bytes) =====
// blake3: [u8; 32] = BLAKE3(file_body[..len-32])
```

校验路径与 Rust `Checkpoint::parse_bytes` 同构（D-352 trailer eager + 5 类
`CheckpointError` 中除 `FileNotFound` 外全部覆盖；本脚本通过 raise 表达错误，
不区分 enum）：

- `magic != b"PLCKPT\\0\\0"` → `CheckpointReaderError("magic bytes mismatch")`
- `schema_version != 1`       → `CheckpointReaderError("schema mismatch ...")`
- `trainer_variant` 未知 tag  → `CheckpointReaderError("unknown trainer_variant tag ...")`
- `game_variant` 未知 tag     → `CheckpointReaderError("unknown game_variant tag ...")`
- header pad != 0             → `CheckpointReaderError("pad byte non-zero ...")`
- 偏移不递增 / 段大小负       → `CheckpointReaderError("offset table out of range ...")`
- BLAKE3 trailer 不匹配       → `CheckpointReaderError("blake3 trailer mismatch")`

bincode 1.x decode（D-354 锁定 1.x default config = LE + fixint）：

- `Vec<T>` 序列化 = u64 LE len + N × `T` 元素
- `(A, B)` tuple = `A` 序列化 + `B` 序列化（无 prefix）
- `struct { a, b }` = `a` 序列化 + `b` 序列化（field order = 声明顺序）
- enum variant tag = u32 LE
- `Option<T>` tag = u8（0=None / 1=Some）+ Some 跟 `T`
- `f64` = 8 bytes LE / `u8` = 1 byte / `u16/u32/u64` 固定 2/4/8 bytes LE

各 game variant 的 InfoSet 字段顺序见
`src/training/{kuhn,leduc,nlhe}.rs`：

- Kuhn: `KuhnInfoSet { actor: u8, private_card: u8, history: KuhnHistory }`
  - u8 + u8 + u32_tag = 6 bytes
  - KuhnHistory tag: 0=Empty / 1=Check / 2=Bet / 3=CheckBet
- Leduc: `LeducInfoSet { actor: u8, private_card: u8, public_card: Option<u8>,
  street: LeducStreet, history: Vec<LeducAction> }`
  - u8 + u8 + (1 or 2 bytes Option<u8>) + u32_tag + (u64 len + N × u32_tag)
  - LeducStreet: 0=Preflop / 1=Postflop
  - LeducAction tag: 0=Check / 1=Bet / 2=Call / 3=Fold / 4=Raise / 5..10=Deal0..Deal5
- SimplifiedNlhe: `pub type SimplifiedNlheInfoSet = InfoSetId;` /
  `pub struct InfoSetId(u64);` newtype → 8 bytes u64 LE

依赖：仅 stdlib（trailer BLAKE3 校验需要 `pip install blake3`；缺失时 stderr
警告并跳过——与 stage 1 `history_reader.py` 同型）。

用法::

    # 1) summary 摘要（默认）
    python3 tools/checkpoint_reader.py <path>

    # 2) JSON 输出（regret/strategy 表前 N 条 + 统计）
    python3 tools/checkpoint_reader.py <path> --json [--sample N]

    # 3) 仅 header 解码（最快路径，跳过 bincode body）
    python3 tools/checkpoint_reader.py <path> --header-only

退出码：
    0 = 正常 (含 schema/variant/blake3 校验通过)
    2 = 解析失败 / 校验失败
    3 = 文件 IO 失败
"""
from __future__ import annotations

import argparse
import json
import struct
import sys
from typing import Any, Optional


MAGIC = b"PLCKPT\x00\x00"
SCHEMA_VERSION = 1
HEADER_LEN = 108
TRAILER_LEN = 32

OFFSET_MAGIC = 0
OFFSET_SCHEMA_VERSION = 8
OFFSET_TRAINER_VARIANT = 12
OFFSET_GAME_VARIANT = 13
OFFSET_PAD = 14
OFFSET_UPDATE_COUNT = 20
OFFSET_RNG_STATE = 28
OFFSET_BUCKET_TABLE_BLAKE3 = 60
OFFSET_REGRET_TABLE_OFFSET = 92
OFFSET_STRATEGY_SUM_OFFSET = 100

TRAINER_VARIANTS = {0: "VanillaCfr", 1: "EsMccfr"}
GAME_VARIANTS = {0: "Kuhn", 1: "Leduc", 2: "SimplifiedNlhe"}

KUHN_HISTORY = {0: "Empty", 1: "Check", 2: "Bet", 3: "CheckBet"}
LEDUC_STREET = {0: "Preflop", 1: "Postflop"}
LEDUC_ACTION = {
    0: "Check",
    1: "Bet",
    2: "Call",
    3: "Fold",
    4: "Raise",
    5: "Deal0",
    6: "Deal1",
    7: "Deal2",
    8: "Deal3",
    9: "Deal4",
    10: "Deal5",
}


class CheckpointReaderError(Exception):
    pass


# --- 基础解码器 ---------------------------------------------------------


def _u32_le(buf: bytes, off: int) -> int:
    return struct.unpack_from("<I", buf, off)[0]


def _u64_le(buf: bytes, off: int) -> int:
    return struct.unpack_from("<Q", buf, off)[0]


def _f64_le(buf: bytes, off: int) -> float:
    return struct.unpack_from("<d", buf, off)[0]


class _Cursor:
    """Bincode body 流式 cursor（fixint LE 编码）。"""

    __slots__ = ("buf", "pos")

    def __init__(self, buf: bytes) -> None:
        self.buf = buf
        self.pos = 0

    def remaining(self) -> int:
        return len(self.buf) - self.pos

    def _ensure(self, n: int, what: str) -> None:
        if self.pos + n > len(self.buf):
            raise CheckpointReaderError(
                f"bincode 解码越界（{what}）：pos={self.pos} need={n} len={len(self.buf)}"
            )

    def read_u8(self) -> int:
        self._ensure(1, "u8")
        b = self.buf[self.pos]
        self.pos += 1
        return b

    def read_u32(self) -> int:
        self._ensure(4, "u32")
        v = _u32_le(self.buf, self.pos)
        self.pos += 4
        return v

    def read_u64(self) -> int:
        self._ensure(8, "u64")
        v = _u64_le(self.buf, self.pos)
        self.pos += 8
        return v

    def read_f64(self) -> float:
        self._ensure(8, "f64")
        v = _f64_le(self.buf, self.pos)
        self.pos += 8
        return v

    def read_option_u8(self) -> Optional[int]:
        tag = self.read_u8()
        if tag == 0:
            return None
        if tag == 1:
            return self.read_u8()
        raise CheckpointReaderError(f"Option<u8> tag 越界：{tag}（合法 0/1）")

    def read_vec_f64(self) -> list[float]:
        n = self.read_u64()
        if n > 1 << 20:
            raise CheckpointReaderError(
                f"Vec<f64> 长度 {n} > 1<<20，疑似格式错位"
            )
        out = []
        self._ensure(n * 8, "Vec<f64>")
        # 批量解码，避免逐元素 _ensure
        out = list(struct.unpack_from(f"<{n}d", self.buf, self.pos))
        self.pos += n * 8
        return out


# --- InfoSet 解码（按 game variant 分支）-------------------------------


def _read_kuhn_info_set(cur: _Cursor) -> dict:
    actor = cur.read_u8()
    private_card = cur.read_u8()
    history_tag = cur.read_u32()
    history = KUHN_HISTORY.get(history_tag, f"Unknown({history_tag})")
    return {
        "actor": actor,
        "private_card": private_card,
        "history": history,
        "history_tag": history_tag,
    }


def _read_leduc_info_set(cur: _Cursor) -> dict:
    actor = cur.read_u8()
    private_card = cur.read_u8()
    public_card = cur.read_option_u8()
    street_tag = cur.read_u32()
    street = LEDUC_STREET.get(street_tag, f"Unknown({street_tag})")
    history_len = cur.read_u64()
    if history_len > 64:
        raise CheckpointReaderError(
            f"LeducHistory 长度 {history_len} > 64，疑似格式错位"
        )
    history: list[str] = []
    for _ in range(history_len):
        action_tag = cur.read_u32()
        history.append(LEDUC_ACTION.get(action_tag, f"Unknown({action_tag})"))
    return {
        "actor": actor,
        "private_card": private_card,
        "public_card": public_card,
        "street": street,
        "street_tag": street_tag,
        "history": history,
    }


def _read_nlhe_info_set(cur: _Cursor) -> dict:
    raw = cur.read_u64()
    return {"raw": raw, "raw_hex": f"0x{raw:016x}"}


_INFO_SET_DECODER = {
    "Kuhn": _read_kuhn_info_set,
    "Leduc": _read_leduc_info_set,
    "SimplifiedNlhe": _read_nlhe_info_set,
}


def decode_table(buf: bytes, game_variant: str) -> list[tuple[dict, list[float]]]:
    """解码 bincode body = Vec<(InfoSet, Vec<f64>)>。

    与 Rust `encode_table` 同型；entries 按 InfoSet `Debug` 排序输出（D-327），
    本 reader 不重排，按文件物理顺序返回。
    """
    decoder = _INFO_SET_DECODER.get(game_variant)
    if decoder is None:
        raise CheckpointReaderError(
            f"不支持的 game_variant: {game_variant}（仅支持 Kuhn / Leduc / SimplifiedNlhe）"
        )
    cur = _Cursor(buf)
    n = cur.read_u64()
    if n > 1 << 30:
        raise CheckpointReaderError(
            f"Vec<(I, Vec<f64>)> 外层长度 {n} > 1<<30，疑似格式错位"
        )
    entries: list[tuple[dict, list[float]]] = []
    for _ in range(n):
        info_set = decoder(cur)
        values = cur.read_vec_f64()
        entries.append((info_set, values))
    if cur.remaining() != 0:
        raise CheckpointReaderError(
            f"bincode body 解码后剩余 {cur.remaining()} 字节未读取（疑似 schema 漂移）"
        )
    return entries


# --- 顶层 parse ---------------------------------------------------------


def parse_header(buf: bytes) -> dict:
    n = len(buf)
    if n < HEADER_LEN + TRAILER_LEN:
        raise CheckpointReaderError(
            f"size mismatch: file too small ({n} < header {HEADER_LEN} + trailer {TRAILER_LEN})"
        )
    if buf[OFFSET_MAGIC:OFFSET_SCHEMA_VERSION] != MAGIC:
        raise CheckpointReaderError(
            f"magic bytes mismatch: expected {MAGIC!r}, got {bytes(buf[OFFSET_MAGIC:OFFSET_SCHEMA_VERSION])!r}"
        )
    schema_version = _u32_le(buf, OFFSET_SCHEMA_VERSION)
    if schema_version != SCHEMA_VERSION:
        raise CheckpointReaderError(
            f"schema mismatch: expected {SCHEMA_VERSION}, got {schema_version}"
        )
    trainer_tag = buf[OFFSET_TRAINER_VARIANT]
    if trainer_tag not in TRAINER_VARIANTS:
        raise CheckpointReaderError(
            f"unknown trainer_variant tag {trainer_tag} at offset {OFFSET_TRAINER_VARIANT}"
        )
    game_tag = buf[OFFSET_GAME_VARIANT]
    if game_tag not in GAME_VARIANTS:
        raise CheckpointReaderError(
            f"unknown game_variant tag {game_tag} at offset {OFFSET_GAME_VARIANT}"
        )
    for off in range(OFFSET_PAD, OFFSET_UPDATE_COUNT):
        if buf[off] != 0:
            raise CheckpointReaderError(
                f"pad byte non-zero at offset {off}: 0x{buf[off]:02x}"
            )
    update_count = _u64_le(buf, OFFSET_UPDATE_COUNT)
    rng_state = bytes(buf[OFFSET_RNG_STATE:OFFSET_BUCKET_TABLE_BLAKE3])
    bucket_blake3 = bytes(buf[OFFSET_BUCKET_TABLE_BLAKE3:OFFSET_REGRET_TABLE_OFFSET])
    regret_offset = _u64_le(buf, OFFSET_REGRET_TABLE_OFFSET)
    strategy_offset = _u64_le(buf, OFFSET_STRATEGY_SUM_OFFSET)
    body_end = n - TRAILER_LEN
    if not (
        regret_offset >= HEADER_LEN
        and regret_offset <= strategy_offset
        and strategy_offset <= body_end
    ):
        raise CheckpointReaderError(
            f"offset table out of range: regret={regret_offset} "
            f"strategy={strategy_offset} trailer_start={body_end}"
        )
    return {
        "schema_version": schema_version,
        "trainer_variant": TRAINER_VARIANTS[trainer_tag],
        "trainer_variant_tag": trainer_tag,
        "game_variant": GAME_VARIANTS[game_tag],
        "game_variant_tag": game_tag,
        "update_count": update_count,
        "rng_state_hex": rng_state.hex(),
        "bucket_table_blake3_hex": bucket_blake3.hex(),
        "regret_table_offset": regret_offset,
        "strategy_sum_offset": strategy_offset,
        "body_end": body_end,
        "file_len": n,
    }


def verify_trailer(buf: bytes) -> tuple[str, str]:
    """返回 (status, trailer_hex)。状态 = "ok" / "mismatch" / "skipped"。"""
    body_end = len(buf) - TRAILER_LEN
    trailer_hex = bytes(buf[body_end:]).hex()
    try:
        import blake3 as _blake3  # type: ignore
    except ImportError:
        sys.stderr.write(
            "[checkpoint_reader] WARN: 'blake3' Python package not installed; "
            "skipping trailer integrity check (pip install blake3 to enable)\n"
        )
        return "skipped (no blake3 module)", trailer_hex
    body_hash = _blake3.blake3(buf[:body_end]).digest()
    if body_hash.hex() != trailer_hex:
        raise CheckpointReaderError(
            f"blake3 trailer mismatch: computed {body_hash.hex()}, stored {trailer_hex}"
        )
    return "ok (matches body BLAKE3)", trailer_hex


def parse(buf: bytes, decode_body: bool = True) -> dict:
    """解析 checkpoint binary 完整视图。

    Args:
        buf: 完整 checkpoint 字节
        decode_body: 是否对 regret_table_body / strategy_sum_body 做 bincode 解码
    """
    header = parse_header(buf)
    trailer_status, trailer_hex = verify_trailer(buf)

    regret_bytes = buf[header["regret_table_offset"]: header["strategy_sum_offset"]]
    strategy_bytes = buf[header["strategy_sum_offset"]: header["body_end"]]

    result: dict[str, Any] = {
        **header,
        "trailer_hex": trailer_hex,
        "trailer_blake3_status": trailer_status,
        "regret_table_bytes_len": len(regret_bytes),
        "strategy_sum_bytes_len": len(strategy_bytes),
    }

    if decode_body:
        regret_table = decode_table(regret_bytes, header["game_variant"])
        strategy_sum = decode_table(strategy_bytes, header["game_variant"])
        result["regret_table"] = regret_table
        result["strategy_sum"] = strategy_sum
        result["regret_table_entry_count"] = len(regret_table)
        result["strategy_sum_entry_count"] = len(strategy_sum)
    else:
        result["regret_table"] = None
        result["strategy_sum"] = None
        result["regret_table_entry_count"] = None
        result["strategy_sum_entry_count"] = None
    return result


# --- CLI ----------------------------------------------------------------


def _format_info_set(info: dict) -> str:
    if "raw_hex" in info:
        return info["raw_hex"]
    if "history_tag" in info and "private_card" in info and "actor" in info and "public_card" not in info:
        return f"actor={info['actor']} card={info['private_card']} hist={info['history']}"
    return (
        f"actor={info['actor']} card={info['private_card']} "
        f"pub={info['public_card']} street={info['street']} "
        f"hist=[{','.join(info['history'])}]"
    )


def _print_summary(view: dict, sample: int) -> None:
    print(f"file_len               : {view['file_len']} bytes")
    print(f"schema_version         : {view['schema_version']}")
    print(f"trainer_variant        : {view['trainer_variant']} (tag={view['trainer_variant_tag']})")
    print(f"game_variant           : {view['game_variant']} (tag={view['game_variant_tag']})")
    print(f"update_count           : {view['update_count']}")
    print(f"rng_state              : {view['rng_state_hex']}")
    print(f"bucket_table_blake3    : {view['bucket_table_blake3_hex']}")
    print(f"regret_table_offset    : {view['regret_table_offset']}")
    print(f"strategy_sum_offset    : {view['strategy_sum_offset']}")
    print(f"body_end               : {view['body_end']}")
    print(f"regret_table_bytes_len : {view['regret_table_bytes_len']}")
    print(f"strategy_sum_bytes_len : {view['strategy_sum_bytes_len']}")
    print(f"trailer_blake3         : {view['trailer_blake3_status']}")
    print(f"trailer_hex            : {view['trailer_hex']}")

    if view["regret_table"] is None:
        return
    print()
    print(
        f"regret_table           : {view['regret_table_entry_count']} entries "
        f"(showing first {min(sample, view['regret_table_entry_count'])})"
    )
    for info, values in view["regret_table"][:sample]:
        vals = ", ".join(f"{v:+.4e}" for v in values[:6])
        more = "" if len(values) <= 6 else f", ... ({len(values)} actions)"
        print(f"  [{_format_info_set(info)}] -> [{vals}{more}]")
    print()
    print(
        f"strategy_sum           : {view['strategy_sum_entry_count']} entries "
        f"(showing first {min(sample, view['strategy_sum_entry_count'])})"
    )
    for info, values in view["strategy_sum"][:sample]:
        vals = ", ".join(f"{v:+.4e}" for v in values[:6])
        more = "" if len(values) <= 6 else f", ... ({len(values)} actions)"
        print(f"  [{_format_info_set(info)}] -> [{vals}{more}]")


def _to_json(view: dict, sample: int) -> dict:
    """JSON-friendly 视图：regret/strategy 各裁剪到前 `sample` 条避免膨胀。"""
    out = {
        k: v
        for k, v in view.items()
        if k not in ("regret_table", "strategy_sum")
    }
    if view["regret_table"] is not None:
        out["regret_table_sample"] = [
            {"info_set": info, "values": vals}
            for info, vals in view["regret_table"][:sample]
        ]
        out["strategy_sum_sample"] = [
            {"info_set": info, "values": vals}
            for info, vals in view["strategy_sum"][:sample]
        ]
    return out


def main() -> int:
    ap = argparse.ArgumentParser(
        description="Checkpoint reader (D-357 / API-350)",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="See `tools/checkpoint_reader.py --help` and module docstring for binary layout details.",
    )
    ap.add_argument("path", help="checkpoint binary file path")
    ap.add_argument(
        "--header-only",
        action="store_true",
        help="仅解码 header + trailer 校验，不解码 bincode body（最快路径）",
    )
    ap.add_argument(
        "--json",
        action="store_true",
        help="输出 JSON（regret/strategy 各前 N 条 entry，由 --sample 控制）",
    )
    ap.add_argument(
        "--sample",
        type=int,
        default=10,
        help="summary / JSON 显示的 regret/strategy 表头部 entry 数（默认 10）",
    )
    args = ap.parse_args()

    try:
        with open(args.path, "rb") as f:
            buf = f.read()
    except OSError as e:
        sys.stderr.write(f"[checkpoint_reader] IO error reading {args.path}: {e}\n")
        return 3

    try:
        view = parse(buf, decode_body=not args.header_only)
    except CheckpointReaderError as e:
        sys.stderr.write(f"[checkpoint_reader] parse failed: {e}\n")
        return 2

    if args.json:
        json.dump(_to_json(view, args.sample), sys.stdout, indent=2, ensure_ascii=False)
        sys.stdout.write("\n")
    else:
        _print_summary(view, args.sample)
    return 0


if __name__ == "__main__":
    sys.exit(main())
