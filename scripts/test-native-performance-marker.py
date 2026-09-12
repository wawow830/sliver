#!/usr/bin/env python3
"""Offline tests at the planned complete-frame pixel identity seam."""
import hashlib
import importlib.util
from pathlib import Path
import unittest
import zlib

spec = importlib.util.spec_from_file_location(
    "marker", Path(__file__).with_name("native-performance-marker.py"))
marker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(marker)
reader_spec = importlib.util.spec_from_file_location(
    "reader", Path(__file__).with_name("analyze-native-performance.py"))
reader = importlib.util.module_from_spec(reader_spec)
reader_spec.loader.exec_module(reader)


# Worked vector: run=r, generation=g, frame=1, no input. CRC independently
# evaluated bit-by-bit with polynomial 0xedb88320, not by the codec.
GOLDEN_PACKET = (bytes.fromhex("534c564d524b3030 0000000000000001 01010000")
                 + b"r" + bytes(127) + b"g" + bytes(127) + bytes(128)
                 + bytes.fromhex("aa94d712"))


def packet_frame(packet):
    """Reference rasterizer from the documented row layout, not codec internals."""
    bits = "".join(format(value, "08b") for value in packet)
    black, white = bytes.fromhex("000000ff"), bytes.fromhex("ffffffff")
    frame = bytearray(black * 2008 * 60)
    for row in range(4):
        scanline = b"".join((white if bit == "1" else black) * 2
                            for bit in bits[row*816:(row+1)*816])
        for y in (4+row*2, 5+row*2):
            frame[y*8032+752:y*8032+7280] = scanline
    return bytes(frame)


class MarkerTests(unittest.TestCase):
    def test_complete_frame_carries_exact_identity_without_changing_other_pixels(self):
        identity = {"run_id": "run-A", "generation": "worker-2", "frame_id": 19,
                    "input_id": "touch-7"}
        background = bytes.fromhex("123456ff") * (2008 * 60)
        stamped = marker.stamp(background, identity)
        self.assertEqual(marker.decode(stamped), identity)
        self.assertEqual(len(stamped), len(background))
        self.assertEqual(background, bytes.fromhex("123456ff") * (2008 * 60))
        for y in range(60):
            if 4 <= y < 12:
                self.assertEqual(stamped[y*8032:y*8032+188*4], background[y*8032:y*8032+188*4])
                self.assertEqual(stamped[y*8032+1820*4:(y+1)*8032],
                                 background[y*8032+1820*4:(y+1)*8032])
            else:
                self.assertEqual(stamped[y*8032:(y+1)*8032], background[y*8032:(y+1)*8032])

    def test_missing_partial_or_corrupted_markers_fail_instead_of_inventing_identity(self):
        background = bytes.fromhex("000000ff") * (2008 * 60)
        stamped = marker.stamp(background, {"run_id": "r", "generation": "g",
                                            "frame_id": 1, "input_id": None})
        first_cell = 4*8032 + 188*4
        damaged = bytearray(stamped)
        damaged[first_cell+4] ^= 255  # not the sampled corner: every pixel must be checked
        bad_alpha = bytearray(stamped)
        bad_alpha[first_cell+3] = 0
        flipped = bytearray(stamped)
        for row in (0, 8032):
            for column in (0, 4):
                for channel in range(3):
                    flipped[first_cell+row+column+channel] ^= 255
        for frame in (background, bytes(damaged), bytes(bad_alpha), bytes(flipped)):
            with self.subTest(frame_prefix=frame[first_cell:first_cell+8]):
                with self.assertRaises(ValueError):
                    marker.decode(frame)

    def test_capacity_limits_are_exact_and_never_truncate_or_alias_identities(self):
        background = bytes(8032 * 60)
        largest = {"run_id": "r" * 128, "generation": "g" * 128,
                   "frame_id": 2**63 - 1, "input_id": "i" * 128}
        self.assertEqual(marker.decode(marker.stamp(background, largest)), largest)
        capacity_packet = (bytes.fromhex("534c564d524b3030 7fffffffffffffff 80808000")
                           + b"r" * 128 + b"g" * 128 + b"i" * 128
                           + bytes.fromhex("474654e2"))
        self.assertEqual(marker.decode(packet_frame(capacity_packet)), largest)
        self.assertEqual(marker.stamp(bytes.fromhex("000000ff") * 2008 * 60, largest),
                         packet_frame(capacity_packet))
        invalid = [None, [], {}, dict(largest, extra=0)]
        for field, values in {
            "run_id": ("", "r" * 129, "é", "has space", 1),
            "generation": ("", None, "a/b"),
            "frame_id": (0, -1, 2**63, True, 1.0, "1"),
            "input_id": ("", False, 0, "i" * 129),
        }.items():
            invalid.extend(dict(largest, **{field: value}) for value in values)
        for record in invalid:
            with self.subTest(record=record):
                with self.assertRaises(ValueError):
                    marker.stamp(background, record)

    def test_row_padding_is_preserved_and_incomplete_or_invalid_frame_layouts_fail(self):
        identity = {"run_id": "r", "generation": "g", "frame_id": 1, "input_id": None}
        stride = 8192
        background = bytes.fromhex("123456ff") * (stride * 60 // 4)
        stamped = marker.stamp(background, identity, stride=stride)
        self.assertEqual(marker.decode(stamped, stride=stride), identity)
        for y in range(60):
            self.assertEqual(stamped[y*stride+8032:(y+1)*stride],
                             background[y*stride+8032:(y+1)*stride])
        for pixels, row_bytes in ((stamped[:-1], stride), (stamped+b"x", stride),
                                  (stamped, 8031), (stamped, 0), (stamped, -1),
                                  (stamped, True), (stamped, 8192.0),
                                  (stamped, 2**64), (bytearray(stamped), stride),
                                  (None, stride)):
            with self.subTest(row_bytes=row_bytes, pixel_type=type(pixels)):
                with self.assertRaises(ValueError):
                    marker.decode(pixels, stride=row_bytes)
                with self.assertRaises(ValueError):
                    marker.stamp(pixels, identity, stride=row_bytes)

    def test_worked_wire_vector_and_rechecksummed_noncanonical_packets(self):
        expected = {"run_id": "r", "generation": "g", "frame_id": 1, "input_id": None}
        golden = packet_frame(GOLDEN_PACKET)
        self.assertEqual(hashlib.sha256(golden).hexdigest(),
                         "9e1dabfda3ae5c7cf56d4ffffc9794983124b2f81ff879275b4a64aa4fbff444")
        self.assertEqual(marker.decode(golden), expected)
        self.assertEqual(marker.stamp(bytes.fromhex("000000ff") * 2008 * 60, expected), golden)
        mutations = [(0, b"X"), (19, b"\x01"), (16, b"\x00"),
                     (21, b"z"), (149, b"z"), (276, b"z"),
                     (8, bytes(8)), (8, bytes.fromhex("8000000000000000")),
                     (20, b"/"), (148, b"\xff")]
        for offset, replacement in mutations:
            packet = bytearray(GOLDEN_PACKET)
            packet[offset:offset+len(replacement)] = replacement
            packet[-4:] = zlib.crc32(packet[:-4]).to_bytes(4, "big")
            with self.subTest(offset=offset, replacement=replacement):
                with self.assertRaises(ValueError):
                    marker.decode(packet_frame(packet))
        packet = bytearray(GOLDEN_PACKET)
        packet[16] = 129
        packet[20:148] = b"r" * 128
        packet[-4:] = zlib.crc32(packet[:-4]).to_bytes(4, "big")
        with self.assertRaises(ValueError):
            marker.decode(packet_frame(packet))

    def test_clockwise_panel_layout_and_padding_are_explicit_not_autodetected(self):
        logical = packet_frame(GOLDEN_PACKET)
        rows = [[logical[y*8032+x*4:y*8032+x*4+4] for x in range(2008)]
                for y in range(60)]
        # Standard matrix rotation, independent of decoder pixel addressing.
        panel_rows = [b"".join(row) for row in zip(*reversed(rows))]
        expected = {"run_id": "r", "generation": "g", "frame_id": 1, "input_id": None}
        tight = b"".join(panel_rows)
        self.assertEqual(marker.decode(tight, layout="panel"), expected)
        padded = b"".join(row + bytes.fromhex("abcd1234") * 4 for row in panel_rows)
        self.assertEqual(marker.decode(padded, layout="panel", stride=256), expected)
        for pixels, kwargs in ((tight, {}), (logical, {"layout": "panel"}),
                               (logical, {"layout": "auto"}),
                               (padded[:-1], {"layout": "panel", "stride": 256}),
                               (padded, {"layout": "panel", "stride": 239})):
            with self.subTest(kwargs=kwargs):
                with self.assertRaises(ValueError):
                    marker.decode(pixels, **kwargs)

    def test_pixel_observations_do_not_turn_animation_or_replay_into_input_responses(self):
        frame = bytes(8032 * 60)

        def events(number, token, start, end):
            rendered = {"generation": "g", "frame_id": number, "input_id": token}
            observed = marker.decode(marker.stamp(frame, dict(rendered, run_id="r")))
            key = {"generation": "g", "frame_id": number}
            return [dict(kind="render", time_ns=start, **rendered),
                    dict(kind="publish", time_ns=start+1, **key),
                    dict(kind="select", time_ns=start+2, **key),
                    dict(kind="broker_start", time_ns=start+3, call_id=str(number),
                         replay=False, **observed),
                    dict(kind="broker_end", time_ns=end, call_id=str(number), ok=True)]

        observation = {"schema": "sliver-native-observation-v0", "source": "synthetic",
                       "run_id": "r", "clock": "CLOCK_MONOTONIC", "start_ns": 0,
                       "end_ns": 100, "window_ns": 50, "capture_dropped": 0,
                       "events": [{"kind": "input", "time_ns": 0, "input_id": "touch-1"}]}
        observation["events"] += events(1, None, 1, 5)
        observation["events"].append({"kind": "input_timeout", "time_ns": 10, "input_id": "touch-1"})
        observation["events"] += events(2, "touch-1", 20, 30)
        old_pixels = marker.stamp(frame, {"run_id": "r", "generation": "g",
                                          "frame_id": 1, "input_id": None})
        observation["events"] += [dict(kind="broker_start", time_ns=31, call_id="replay",
                                       replay=True, **marker.decode(old_pixels)),
                                   dict(kind="broker_end", time_ns=32, call_id="replay", ok=True)]
        report = reader.analyze(observation)
        self.assertEqual(report["successful_unique_updates"], 2)
        self.assertEqual(report["successful_replays"], 1)
        self.assertEqual(report["inputs"], [{"input_id": "touch-1", "receipt_ns": 0,
                                           "response_ns": 30, "latency_ns": 30, "timeout_ns": 10}])
        self.assertEqual(report["acceptance"], "not_evaluated")
        observation["run_id"] = "new-run"
        with self.assertRaisesRegex(ValueError, "run marker"):
            reader.analyze(observation)


if __name__ == "__main__":
    unittest.main()
