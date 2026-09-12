#!/usr/bin/env python3
"""Experimental offline complete-frame markers; not optical or release evidence.

See docs/research/native-performance-marker-format.md. No hardware or file IO.
"""
import struct
import zlib


_MAGIC = b"SLVMRK00"
_HEADER = struct.Struct(">8sQ4B128s128s128s")
_PACKET_BYTES = 408
_COLUMNS = 816


def _validate_identity(record):
    if type(record) is not dict or set(record) != {"run_id", "generation", "frame_id", "input_id"}:
        raise ValueError("expected run_id, generation, frame_id and input_id")
    if type(record["frame_id"]) is not int or not 0 < record["frame_id"] < 2**63:
        raise ValueError("frame_id must be a positive signed-64-bit integer")
    for field in ("run_id", "generation", "input_id"):
        value = record[field]
        if field == "input_id" and value is None:
            continue
        if not (type(value) is str and 0 < len(value) <= 128 and value.isascii()
                and all(c.isalnum() or c in "-_." for c in value)):
            raise ValueError(f"invalid {field}")
    return record


def _validate_frame(pixels, stride, layout="logical"):
    if layout not in ("logical", "panel"):
        raise ValueError("unsupported frame layout")
    width, height = (2008, 60) if layout == "logical" else (60, 2008)
    if stride is None:
        stride = width * 4
    if (type(stride) is not int or stride < width * 4 or stride % 4 != 0
            or stride * height > 16 * 1024 * 1024):
        raise ValueError("invalid or oversized frame stride")
    if type(pixels) is not bytes or len(pixels) != stride * height:
        raise ValueError("expected an immutable complete frame of exactly stride * height bytes")
    return stride


def _offset(bit, dx, dy, stride, layout="logical"):
    x = 188 + (bit % _COLUMNS) * 2 + dx
    y = 4 + (bit // _COLUMNS) * 2 + dy
    if layout == "panel":
        x, y = 59 - y, x
    return y * stride + x * 4


def stamp(pixels, identity, *, stride=8032):
    """Return a new logical ARGB32 frame with the reserved marker band replaced."""
    stride = _validate_frame(pixels, stride)
    _validate_identity(identity)
    run = identity["run_id"].encode("ascii")
    generation = identity["generation"].encode("ascii")
    token = (identity["input_id"] or "").encode("ascii")
    header = _HEADER.pack(_MAGIC, identity["frame_id"], len(run), len(generation), len(token),
                          0, run, generation, token)
    packet = header + struct.pack(">I", zlib.crc32(header))
    result = bytearray(pixels)
    for bit in range(_PACKET_BYTES * 8):
        level = 255 if packet[bit // 8] & (128 >> (bit % 8)) else 0
        pixel = bytes((level, level, level, 255))
        for dy in range(2):
            for dx in range(2):
                offset = _offset(bit, dx, dy, stride)
                result[offset:offset+4] = pixel
    return bytes(result)


def decode(pixels, *, stride=None, layout="logical"):
    """Recover identity from pixels, without access to the producer's identity record."""
    stride = _validate_frame(pixels, stride, layout)
    packet = bytearray(_PACKET_BYTES)
    for bit in range(_PACKET_BYTES * 8):
        first = _offset(bit, 0, 0, stride, layout)
        pixel = pixels[first:first+4]
        if pixel not in (b"\x00\x00\x00\xff", b"\xff\xff\xff\xff"):
            raise ValueError("marker pixel is not opaque black or white")
        for dy in range(2):
            for dx in range(2):
                offset = _offset(bit, dx, dy, stride, layout)
                if pixels[offset:offset+4] != pixel:
                    raise ValueError("nonuniform marker cell")
        if pixel[0] == 255:
            packet[bit // 8] |= 128 >> (bit % 8)
    if zlib.crc32(packet[:-4]) != int.from_bytes(packet[-4:], "big"):
        raise ValueError("marker CRC mismatch")
    magic, frame_id, run_len, generation_len, token_len, reserved, run, generation, token = (
        _HEADER.unpack(packet[:-4]))
    if magic != _MAGIC or reserved != 0:
        raise ValueError("unsupported marker version or flags")
    for length, slot in ((run_len, run), (generation_len, generation), (token_len, token)):
        if length > 128 or any(slot[length:]):
            raise ValueError("invalid marker length or nonzero padding")
    return _validate_identity({"run_id": run[:run_len].decode("ascii"),
                               "generation": generation[:generation_len].decode("ascii"),
                               "frame_id": frame_id,
                               "input_id": token[:token_len].decode("ascii") if token_len else None})
