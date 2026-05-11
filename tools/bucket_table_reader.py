#!/usr/bin/env python3
"""
跨语言 bucket table reader（D-249 / D-244-rev1）。

`pluribus_stage2_decisions.md` D-249 字面：
    `tools/bucket_table_reader.py` 用纯 Python（无 protoc / mmap C 扩展依赖）
    读 D-244 文件格式，至少能解码 magic / schema_version / feature_set_id /
    BucketConfig / preflop lookup / 任意 1k 个 postflop canonical_id → bucket_id。
    继承 stage 1 `tools/history_reader.py` 同型（minimal proto3 decoder 风格）。

`src/abstraction/bucket_table.rs::BucketTable` doc 字面 80-byte 定长 header +
变长 body + 32-byte BLAKE3 trailer。本脚本镜像该格式：

```text
// ===== header (80 bytes, 8-byte aligned) =====
offset 0x00: magic: [u8; 8] = b"PLBKT\\0\\0\\0"
offset 0x08: schema_version:                u32 LE = 1
offset 0x0C: feature_set_id:                u32 LE = 1
offset 0x10: bucket_count_flop:             u32 LE
offset 0x14: bucket_count_turn:             u32 LE
offset 0x18: bucket_count_river:            u32 LE
offset 0x1C: n_canonical_observation_flop:  u32 LE
offset 0x20: n_canonical_observation_turn:  u32 LE
offset 0x24: n_canonical_observation_river: u32 LE
offset 0x28: n_dims:                        u8 = 9
offset 0x29: pad:                           [u8; 7] = 0
offset 0x30: training_seed:                 u64 LE
offset 0x38: centroid_metadata_offset:      u64 LE
offset 0x40: centroid_data_offset:          u64 LE
offset 0x48: lookup_table_offset:           u64 LE
// ===== body (变长, 按 header §⑨ 偏移定位) =====
// centroid_metadata: 3 streets × n_dims × (min:f32 LE, max:f32 LE)
// centroid_data:     3 streets × bucket_count(street) × n_dims × u8
// lookup_table:
//   preflop:  [u32 LE; 1326]
//   flop:     [u32 LE; n_canonical_observation_flop]
//   turn:     [u32 LE; n_canonical_observation_turn]
//   river:    [u32 LE; n_canonical_observation_river]
// ===== trailer (32 bytes) =====
// blake3: [u8; 32] = BLAKE3(file_body[..len-32])
```

校验路径与 Rust `BucketTable::from_bytes` 同构（D-247 5 类错误中除
`FileNotFound` 外全部覆盖；本脚本通过 raise 表达错误，不区分 enum）：

- `magic != b"PLBKT\\0\\0\\0"` → `BucketTableReaderError("magic bytes mismatch")`
- `schema_version != 1`         → `BucketTableReaderError("schema mismatch ...")`
- `feature_set_id != 1`         → `BucketTableReaderError("feature_set_id mismatch ...")`
- `n_dims != 9`                 → `BucketTableReaderError("n_dims mismatch ...")`
- header pad != 0               → `BucketTableReaderError("header pad ...")`
- 偏移不递增 / 不 8-byte 对齐  → `BucketTableReaderError("section offset ...")`
- 段大小不匹配                  → `BucketTableReaderError("size mismatch ...")`
- BLAKE3 trailer 不匹配         → `BucketTableReaderError("blake3 trailer mismatch")`

依赖：仅 stdlib（`hashlib` 不含 BLAKE3，trailer 校验需要 `blake3`；如未装则跳
过校验并 stderr 提示——与 stage 1 `history_reader.py` 仅依赖 stdlib 同型；
production CI 节点应 `pip install blake3` 让校验生效）。

用法::

    # 1) summary 摘要（默认）
    python3 tools/bucket_table_reader.py artifacts/bucket_table_default_500_500_500_seed_cafebabe.bin

    # 2) 抽样 1k postflop canonical_id → bucket_id 转 JSON 跨语言比对
    python3 tools/bucket_table_reader.py <path> --sample 1000 --json > sample.json

    # 3) 打印 preflop 169 lookup table（1326 行 hole_id → hand_class_169）
    python3 tools/bucket_table_reader.py <path> --preflop

退出码：
    0 = 正常 (含 schema/feature_set/blake3 校验通过)
    2 = 解析失败 / 校验失败
    3 = 文件 IO 失败
"""
from __future__ import annotations

import argparse
import json
import struct
import sys
from typing import Optional


MAGIC = b"PLBKT\x00\x00\x00"
SCHEMA_VERSION = 1
FEATURE_SET_ID = 1
N_DIMS = 9
HEADER_LEN = 80
TRAILER_LEN = 32
PREFLOP_LOOKUP_LEN = 1326


class BucketTableReaderError(Exception):
    pass


def _u32_le(buf: bytes, offset: int) -> int:
    return struct.unpack_from("<I", buf, offset)[0]


def _u64_le(buf: bytes, offset: int) -> int:
    return struct.unpack_from("<Q", buf, offset)[0]


def _f32_le(buf: bytes, offset: int) -> float:
    return struct.unpack_from("<f", buf, offset)[0]


def parse(buf: bytes) -> dict:
    """解析 buf 为 BucketTable 字典视图。raise BucketTableReaderError on any 校验失败."""
    n = len(buf)
    if n < HEADER_LEN + TRAILER_LEN:
        raise BucketTableReaderError(
            f"size mismatch: file too small ({n} < header {HEADER_LEN} + trailer {TRAILER_LEN})"
        )
    if buf[0:8] != MAGIC:
        raise BucketTableReaderError(
            f"magic bytes mismatch: expected {MAGIC!r}, got {bytes(buf[0:8])!r}"
        )
    schema_version = _u32_le(buf, 0x08)
    if schema_version != SCHEMA_VERSION:
        raise BucketTableReaderError(
            f"schema mismatch: expected {SCHEMA_VERSION}, got {schema_version}"
        )
    feature_set_id = _u32_le(buf, 0x0C)
    if feature_set_id != FEATURE_SET_ID:
        raise BucketTableReaderError(
            f"feature_set_id mismatch: expected {FEATURE_SET_ID}, got {feature_set_id}"
        )
    bucket_count_flop = _u32_le(buf, 0x10)
    bucket_count_turn = _u32_le(buf, 0x14)
    bucket_count_river = _u32_le(buf, 0x18)
    n_canonical_flop = _u32_le(buf, 0x1C)
    n_canonical_turn = _u32_le(buf, 0x20)
    n_canonical_river = _u32_le(buf, 0x24)
    n_dims = buf[0x28]
    if n_dims != N_DIMS:
        raise BucketTableReaderError(
            f"n_dims mismatch: expected {N_DIMS}, got {n_dims}"
        )
    for off in range(0x29, 0x30):
        if buf[off] != 0:
            raise BucketTableReaderError(
                f"header pad bytes must be zero (offset 0x{off:02x} = 0x{buf[off]:02x})"
            )
    training_seed = _u64_le(buf, 0x30)
    centroid_metadata_offset = _u64_le(buf, 0x38)
    centroid_data_offset = _u64_le(buf, 0x40)
    lookup_table_offset = _u64_le(buf, 0x48)

    body_start = HEADER_LEN
    body_end = n - TRAILER_LEN
    if not (
        centroid_metadata_offset >= body_start
        and centroid_metadata_offset < centroid_data_offset
        and centroid_data_offset < lookup_table_offset
        and lookup_table_offset <= body_end
    ):
        raise BucketTableReaderError(
            f"section offset invariant violated: "
            f"meta={centroid_metadata_offset} data={centroid_data_offset} "
            f"lookup={lookup_table_offset} body=[{body_start}, {body_end}]"
        )
    for name, off in (
        ("centroid_metadata", centroid_metadata_offset),
        ("centroid_data", centroid_data_offset),
        ("lookup_table", lookup_table_offset),
    ):
        if off % 8 != 0:
            raise BucketTableReaderError(
                f"{name} offset {off} not 8-byte aligned"
            )

    centroid_metadata_size = 3 * n_dims * 8  # 3 streets × n_dims × (min:f32, max:f32)
    centroid_data_size = (
        bucket_count_flop + bucket_count_turn + bucket_count_river
    ) * n_dims  # u8 quantized
    lookup_table_entries = (
        PREFLOP_LOOKUP_LEN + n_canonical_flop + n_canonical_turn + n_canonical_river
    )
    lookup_table_size_bytes = lookup_table_entries * 4
    if centroid_data_offset - centroid_metadata_offset < centroid_metadata_size:
        raise BucketTableReaderError(
            f"size mismatch: centroid_metadata segment too small "
            f"({centroid_data_offset - centroid_metadata_offset} < {centroid_metadata_size})"
        )
    if lookup_table_offset - centroid_data_offset < centroid_data_size:
        raise BucketTableReaderError(
            f"size mismatch: centroid_data segment too small "
            f"({lookup_table_offset - centroid_data_offset} < {centroid_data_size})"
        )
    if body_end - lookup_table_offset != lookup_table_size_bytes:
        raise BucketTableReaderError(
            f"size mismatch: lookup_table segment expected "
            f"{lookup_table_size_bytes} bytes, got {body_end - lookup_table_offset}"
        )

    # Trailer BLAKE3 校验（依赖 blake3 包；缺失时仅警告）。
    trailer = buf[body_end:n]
    blake3_hex_stored = trailer.hex()
    blake3_check_status = "skipped (no blake3 module)"
    try:
        import blake3 as _blake3  # type: ignore
    except ImportError:
        sys.stderr.write(
            "[bucket_table_reader] WARN: 'blake3' Python package not installed; "
            "skipping trailer integrity check (pip install blake3 to enable)\n"
        )
    else:
        body_hash = _blake3.blake3(buf[:body_end]).digest()
        if body_hash != trailer:
            raise BucketTableReaderError(
                f"blake3 trailer mismatch: computed {body_hash.hex()}, stored {blake3_hex_stored}"
            )
        blake3_check_status = "ok (matches body BLAKE3)"

    # Parse centroid metadata: 3 streets × n_dims × (min, max)
    streets = ["flop", "turn", "river"]
    centroid_metadata: dict[str, list[dict[str, float]]] = {}
    off = centroid_metadata_offset
    for street in streets:
        per_dim: list[dict[str, float]] = []
        for _ in range(n_dims):
            mn = _f32_le(buf, off)
            off += 4
            mx = _f32_le(buf, off)
            off += 4
            per_dim.append({"min": mn, "max": mx})
        centroid_metadata[street] = per_dim

    # Parse centroid data: 3 streets × bucket_count(street) × n_dims × u8
    centroid_data: dict[str, list[list[int]]] = {}
    off = centroid_data_offset
    for street, k in zip(
        streets, [bucket_count_flop, bucket_count_turn, bucket_count_river]
    ):
        centroids: list[list[int]] = []
        for _ in range(k):
            row = list(buf[off : off + n_dims])
            off += n_dims
            centroids.append(row)
        centroid_data[street] = centroids

    # Parse lookup table.
    lookup_offsets: dict[str, int] = {
        "preflop": lookup_table_offset,
        "flop": lookup_table_offset + PREFLOP_LOOKUP_LEN * 4,
        "turn": (
            lookup_table_offset + (PREFLOP_LOOKUP_LEN + n_canonical_flop) * 4
        ),
        "river": (
            lookup_table_offset
            + (PREFLOP_LOOKUP_LEN + n_canonical_flop + n_canonical_turn) * 4
        ),
    }
    lookup_lengths = {
        "preflop": PREFLOP_LOOKUP_LEN,
        "flop": n_canonical_flop,
        "turn": n_canonical_turn,
        "river": n_canonical_river,
    }

    # bucket id 范围 sanity 校验：每条街 bucket_id < bucket_count(street)。
    bucket_count_by_street = {
        "preflop": 169,
        "flop": bucket_count_flop,
        "turn": bucket_count_turn,
        "river": bucket_count_river,
    }
    for street in ("preflop", "flop", "turn", "river"):
        cap = bucket_count_by_street[street]
        n_entries = lookup_lengths[street]
        seg_off = lookup_offsets[street]
        # 抽 16 个均匀分布的 sample 校验 + 头/尾 各 16 entry。
        sample_idxs = list(range(min(16, n_entries)))
        sample_idxs += list(range(max(0, n_entries - 16), n_entries))
        if n_entries > 32:
            stride = n_entries // 16
            sample_idxs += list(range(0, n_entries, max(stride, 1)))
        for i in sorted(set(sample_idxs)):
            entry = _u32_le(buf, seg_off + i * 4)
            if entry >= cap:
                raise BucketTableReaderError(
                    f"lookup table {street} entry {i} = {entry} >= bucket_count {cap}"
                )

    return {
        "file_size_bytes": n,
        "header": {
            "magic": MAGIC.decode("latin1"),
            "schema_version": schema_version,
            "feature_set_id": feature_set_id,
            "bucket_count": {
                "flop": bucket_count_flop,
                "turn": bucket_count_turn,
                "river": bucket_count_river,
            },
            "n_canonical_observation": {
                "flop": n_canonical_flop,
                "turn": n_canonical_turn,
                "river": n_canonical_river,
            },
            "n_dims": n_dims,
            "training_seed_hex": f"0x{training_seed:016x}",
            "training_seed_dec": training_seed,
            "section_offsets": {
                "centroid_metadata": centroid_metadata_offset,
                "centroid_data": centroid_data_offset,
                "lookup_table": lookup_table_offset,
            },
        },
        "centroid_metadata": centroid_metadata,
        # `centroid_data` 大数据块；不默认全量返回，仅 summary 提供 shape。
        "centroid_data_shape": {
            "flop": [bucket_count_flop, n_dims],
            "turn": [bucket_count_turn, n_dims],
            "river": [bucket_count_river, n_dims],
        },
        "_centroid_data_raw": centroid_data,  # 内部，summary mode 不输出
        "lookup_table_offsets": lookup_offsets,
        "lookup_table_lengths": lookup_lengths,
        "_buf": buf,  # 内部供 lookup() 使用
        "trailer": {
            "blake3_hex": blake3_hex_stored,
            "check_status": blake3_check_status,
        },
    }


def lookup(parsed: dict, street: str, observation_id: int) -> int:
    """`(street, observation_id) → bucket_id`，与 Rust `BucketTable::lookup` 同语义."""
    if street not in ("preflop", "flop", "turn", "river"):
        raise ValueError(f"unknown street: {street}")
    cap = parsed["lookup_table_lengths"][street]
    if observation_id < 0 or observation_id >= cap:
        raise IndexError(
            f"observation_id {observation_id} out of range [0, {cap}) for street {street}"
        )
    seg_off = parsed["lookup_table_offsets"][street]
    return _u32_le(parsed["_buf"], seg_off + observation_id * 4)


def summary_dict(parsed: dict) -> dict:
    """summary 视图：去掉大体积内部字段。"""
    out = {k: v for k, v in parsed.items() if not k.startswith("_")}
    return out


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Cross-language bucket table reader (D-249 / D-244-rev1)"
    )
    parser.add_argument("path", help="path to bucket table .bin artifact")
    parser.add_argument(
        "--sample",
        type=int,
        default=0,
        help="抽样 N 个 (street, observation_id) → bucket_id 转 JSON 输出 (跨语言比对用)",
    )
    parser.add_argument(
        "--sample-seed",
        type=int,
        default=0xCAFE_BABE,
        help="抽样 RNG seed (默认 0xCAFEBABE)",
    )
    parser.add_argument(
        "--json", action="store_true", help="输出 JSON 视图（默认人类可读 markdown）"
    )
    parser.add_argument(
        "--preflop", action="store_true", help="打印 preflop 1326 hole_id → hand_class_169 全量"
    )
    args = parser.parse_args()

    try:
        with open(args.path, "rb") as f:
            buf = f.read()
    except OSError as e:
        sys.stderr.write(f"[bucket_table_reader] file not found / IO error: {e}\n")
        return 3

    try:
        parsed = parse(buf)
    except BucketTableReaderError as e:
        sys.stderr.write(f"[bucket_table_reader] parse error: {e}\n")
        return 2

    sample_pairs: list[dict] = []
    if args.sample > 0:
        import random

        rng = random.Random(args.sample_seed)
        per_street = max(args.sample // 4, 1)
        for street in ("preflop", "flop", "turn", "river"):
            cap = parsed["lookup_table_lengths"][street]
            for _ in range(per_street):
                obs_id = rng.randrange(cap)
                bucket_id = lookup(parsed, street, obs_id)
                sample_pairs.append(
                    {"street": street, "observation_id": obs_id, "bucket_id": bucket_id}
                )

    if args.json:
        out = summary_dict(parsed)
        if sample_pairs:
            out["sample_lookups"] = sample_pairs
        if args.preflop:
            out["preflop_lookup"] = [
                lookup(parsed, "preflop", i) for i in range(PREFLOP_LOOKUP_LEN)
            ]
        json.dump(out, sys.stdout, indent=2, sort_keys=True)
        sys.stdout.write("\n")
        return 0

    # markdown 默认视图
    h = parsed["header"]
    print(f"# Bucket table summary — {args.path}")
    print()
    print(f"- file size: {parsed['file_size_bytes']} bytes")
    print(f"- magic: `{h['magic']!r}`")
    print(f"- schema_version: `{h['schema_version']}`")
    print(f"- feature_set_id: `{h['feature_set_id']}` (1 = EHS² + OCHS(N=8), 9 dims)")
    print(
        f"- bucket_count: flop={h['bucket_count']['flop']} / turn={h['bucket_count']['turn']} / river={h['bucket_count']['river']}"
    )
    print(
        f"- n_canonical_observation: flop={h['n_canonical_observation']['flop']} / "
        f"turn={h['n_canonical_observation']['turn']} / river={h['n_canonical_observation']['river']}"
    )
    print(f"- n_dims: {h['n_dims']}")
    print(f"- training_seed: {h['training_seed_hex']} (dec={h['training_seed_dec']})")
    print(f"- section offsets: {h['section_offsets']}")
    print(f"- BLAKE3 trailer: `{parsed['trailer']['blake3_hex']}`")
    print(f"- BLAKE3 check: {parsed['trailer']['check_status']}")
    print()
    if sample_pairs:
        print(f"## Sample lookups ({len(sample_pairs)})")
        print()
        print("| street | observation_id | bucket_id |")
        print("|---|---:|---:|")
        for p in sample_pairs[:32]:
            print(f"| {p['street']} | {p['observation_id']} | {p['bucket_id']} |")
        if len(sample_pairs) > 32:
            print(f"| ... | ... | ... |")
        print()
    if args.preflop:
        print("## Preflop lookup (1326 hole_id → hand_class_169)")
        print()
        print("```")
        for hole_id in range(PREFLOP_LOOKUP_LEN):
            print(f"hole_id={hole_id:4d} → hand_class_169={lookup(parsed, 'preflop', hole_id):3d}")
        print("```")
    return 0


if __name__ == "__main__":
    sys.exit(main())
