#!/usr/bin/env python3
"""Synthetic capture documents exercise the offline reader, never hardware."""
import copy
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

spec = importlib.util.spec_from_file_location(
    "native_performance", Path(__file__).with_name("analyze-native-performance.py"))
reader = importlib.util.module_from_spec(spec)
spec.loader.exec_module(reader)


def capture():
    return {
        "schema": "sliver-native-observation-v0", "source": "synthetic",
        "run_id": "test-run", "clock": "CLOCK_MONOTONIC", "start_ns": 0,
        "end_ns": 1_000_000_000, "window_ns": 250_000_000,
        "capture_dropped": 0, "events": [],
    }


def frame_events(frame_id, start, end, generation="worker-a", input_id=None):
    key = {"generation": generation, "frame_id": frame_id}
    call = f"{generation}-{frame_id}"
    return [
        {"kind": "render", "time_ns": start, **key, "input_id": input_id},
        {"kind": "publish", "time_ns": start + 1, **key},
        {"kind": "select", "time_ns": start + 2, **key},
        {"kind": "broker_start", "time_ns": start + 3, **key,
         "run_id": "test-run", "input_id": input_id, "call_id": call, "replay": False},
        {"kind": "broker_end", "time_ns": end, "call_id": call, "ok": True},
    ]


class ObservationTests(unittest.TestCase):
    def test_unique_successful_updates_are_not_release_or_optical_acceptance(self):
        doc = capture()
        doc["events"] = frame_events(1, 0, 100_000_000) + frame_events(2, 200_000_000, 300_000_000)
        result = reader.analyze(doc)
        self.assertEqual(result["source"], "synthetic")
        self.assertEqual(result["acceptance"], "not_evaluated")
        self.assertEqual(result["successful_unique_updates"], 2)
        self.assertEqual(result["between_returns_per_second"], 5.0)
        self.assertEqual(result["whole_interval_updates_per_second"], 2.0)
        self.assertEqual(result["max_frame_age_ns"], 100_000_000)
        self.assertIsNone(result["optical_fps"])
        self.assertIsNone(result["missed_physical_refreshes"])

    def test_recovery_replay_does_not_create_a_new_update(self):
        doc = capture()
        doc["events"] = frame_events(1, 0, 100)
        doc["events"] += [
            {"kind": "broker_start", "time_ns": 200, "generation": "worker-a",
             "frame_id": 1, "run_id": "test-run", "input_id": None,
             "call_id": "recovery", "replay": True},
            {"kind": "broker_end", "time_ns": 300, "call_id": "recovery", "ok": True},
        ]
        result = reader.analyze(doc)
        self.assertEqual(result["successful_unique_updates"], 1)
        self.assertEqual(result["successful_replays"], 1)
        self.assertEqual(result["max_frame_age_ns"], 100)
        self.assertIsNone(result["between_returns_per_second"])
        doc["events"][-2]["replay"] = False
        with self.assertRaises(reader.EvidenceError):
            reader.analyze(doc)

    def test_closed_cohort_accounts_for_supersession_and_failures_separately(self):
        doc = capture()
        doc["events"] = frame_events(1, 0, 100)[:2] + [
            {"kind": "supersede", "time_ns": 2, "generation": "worker-a", "frame_id": 1},
        ]
        doc["events"] += frame_events(2, 10, 100)[:3] + [
            {"kind": "not_submitted", "time_ns": 13, "generation": "worker-a",
             "frame_id": 2, "reason": "backlight-failed"},
        ]
        doc["events"] += frame_events(3, 20, 100)[:1] + [
            {"kind": "render_failed", "time_ns": 21, "generation": "worker-a",
             "frame_id": 3, "reason": "worker-error"},
        ]
        doc["events"] += frame_events(4, 30, 100)[:2] + [
            {"kind": "invalidate", "time_ns": 32, "generation": "worker-a",
             "frame_id": 4, "reason": "worker-replaced"},
        ]
        result = reader.analyze(doc)
        self.assertEqual(result["dispositions"], {
            "superseded": 1, "not_submitted": 1, "render_failed": 1, "invalidated": 1,
        })
        self.assertEqual(result["successful_unique_updates"], 0)
        self.assertEqual(result["broker_call_count"], 0)
        self.assertEqual(result["frames"][1]["reason"], "backlight-failed")
        self.assertIsNone(result["missed_physical_refreshes"])
        doc["events"].pop()
        with self.assertRaisesRegex(reader.EvidenceError, "unresolved frame"):
            reader.analyze(doc)

    def test_causal_latency_exposes_an_interior_spike_between_equal_endpoints(self):
        doc = capture()
        for index, receipt, latency in ((1, 0, 10_000_000), (2, 300_000_000, 200_000_000),
                                        (3, 600_000_000, 10_000_000)):
            token = f"touch-{index}"
            doc["events"].append({"kind": "input", "time_ns": receipt, "input_id": token})
            doc["events"] += frame_events(index, receipt + 1, receipt + latency, input_id=token)
        result = reader.analyze(doc)
        self.assertEqual([row["latency_ns"] for row in result["inputs"]],
                         [10_000_000, 200_000_000, 10_000_000])
        self.assertEqual(result["input_window_max_latency_ns"],
                         [10_000_000, 200_000_000, 10_000_000, None])
        self.assertEqual(result["peak_window_increase_ns"], 190_000_000)
        self.assertEqual(result["max_input_latency_ns"], 200_000_000)
        # Proximity cannot replace a marker that disagrees with the render.
        doc["events"][-2]["input_id"] = "touch-1"
        with self.assertRaisesRegex(reader.EvidenceError, "marker"):
            reader.analyze(doc)

    def test_timeout_and_late_response_are_both_retained(self):
        doc = capture()
        doc["events"] = [{"kind": "input", "time_ns": 0, "input_id": "touch"}]
        doc["events"] += frame_events(1, 1, 100, input_id="touch")[:-1]
        doc["events"] += [
            {"kind": "input_timeout", "time_ns": 50, "input_id": "touch"},
            {"kind": "broker_end", "time_ns": 100, "call_id": "worker-a-1", "ok": True},
        ]
        result = reader.analyze(doc)
        self.assertEqual(result["inputs"][0]["timeout_ns"], 50)
        self.assertEqual(result["inputs"][0]["latency_ns"], 100)
        self.assertEqual(result["input_timeout_count"], 1)
        doc["events"][-1]["ok"] = False
        result = reader.analyze(doc)
        self.assertIsNone(result["inputs"][0]["response_ns"])
        self.assertEqual(result["broker_errors"], 1)
        self.assertEqual(result["successful_unique_updates"], 0)
        doc["events"].pop(-2)
        with self.assertRaisesRegex(reader.EvidenceError, "unresolved input"):
            reader.analyze(doc)

    def test_generation_change_allows_restart_but_wrap_and_missing_attempts_do_not(self):
        doc = capture()
        doc["events"] = frame_events(1, 0, 100) + frame_events(1, 200, 300, generation="worker-b")
        self.assertEqual(reader.analyze(doc)["successful_unique_updates"], 2)
        for change in ("reuse", "gap", "wrap"):
            bad = copy.deepcopy(doc)
            for event in bad["events"][5:]:
                if "generation" in event:
                    event["generation"] = "worker-a"
                    event["frame_id"] = {"reuse": 1, "gap": 3, "wrap": 0}[change]
            with self.subTest(change=change), self.assertRaises(reader.EvidenceError):
                reader.analyze(bad)

    def test_cli_is_diagnostic_only_and_rejects_ambiguous_json_even_with_optimization(self):
        doc = capture()
        doc["events"] = frame_events(1, 0, 100)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "capture.json"
            command = [sys.executable, "-O", "-B", reader.__file__, str(path)]
            path.write_text(json.dumps(doc))
            result = subprocess.run(command, capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(result.stdout)["acceptance"], "not_evaluated")
            for text in ('{"schema":"x","schema":"y"}', '{"capture_dropped":NaN}',
                         '{"capture_dropped":Infinity}', '[]', '{'):
                path.write_text(text)
                with self.subTest(text=text):
                    result = subprocess.run(command, capture_output=True, text=True, check=False)
                    self.assertEqual(result.returncode, 1)
                    self.assertEqual(result.stdout, "")
                    self.assertNotIn("Traceback", result.stderr)

    def test_declared_native_source_preserves_call_errors_without_authenticating_it(self):
        doc = capture()
        doc["source"] = "native-broker"
        doc["events"] = frame_events(1, 0, 100)
        doc["events"][-1]["ok"] = False
        result = reader.analyze(doc)
        self.assertEqual(result["source_authentication"], "not_verified")
        self.assertEqual(result["acceptance"], "not_evaluated")
        self.assertIsNone(result["native_deadline_misses"])
        self.assertEqual(result["broker_calls"][0], {
            "call_id": "worker-a-1", "generation": "worker-a", "frame_id": 1,
            "start_ns": 3, "end_ns": 100, "ok": False, "replay": False,
        })
        self.assertEqual(result["dispositions"], {"broker_error": 1})

    def test_loss_wrong_clocks_partial_calls_and_unrecognized_evidence_are_rejected(self):
        base = capture()
        base["events"] = frame_events(1, 0, 100)
        for field, value in (("capture_dropped", 1), ("capture_dropped", False),
                             ("clock", "CLOCK_REALTIME"), ("source", "optical"),
                             ("schema", "sliver-performance-v1"), ("start_ns", True),
                             ("end_ns", 0), ("window_ns", 0), ("window_ns", 1),
                             ("release_accepted", True), ("events", [])):
            bad = copy.deepcopy(base)
            bad[field] = value
            with self.subTest(field=field, value=value), self.assertRaises(reader.EvidenceError):
                reader.analyze(bad)
        for label, events in (
            ("missing result", base["events"][:-1]),
            ("missing publication", [base["events"][0]] + base["events"][2:]),
            ("duplicate result", base["events"] + [base["events"][-1]]),
            ("backwards time", list(reversed(base["events"]))),
        ):
            bad = copy.deepcopy(base)
            bad["events"] = events
            with self.subTest(label=label), self.assertRaises(reader.EvidenceError):
                reader.analyze(bad)

    def test_old_run_pixels_cannot_match_a_reused_generation_and_frame_number(self):
        doc = capture()
        doc["events"] = frame_events(1, 0, 100)
        doc["events"][-2]["run_id"] = "previous-capture"
        with self.assertRaisesRegex(reader.EvidenceError, "run marker"):
            reader.analyze(doc)
        doc["events"][-2]["run_id"] = doc["run_id"]
        self.assertEqual(reader.analyze(doc)["successful_unique_updates"], 1)


if __name__ == "__main__":
    unittest.main()
