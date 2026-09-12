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

    def test_schedule_opportunities_are_not_inferred_from_render_or_callback_counts(self):
        doc = capture()
        doc.update(end_ns=100, window_ns=100, schedule={
            "stop_ns": 30, "period_numerator_ns": 10, "period_denominator": 1,
        })
        first, second = frame_events(1, 0, 5), frame_events(2, 22, 27)
        first[0]["opportunity_id"], second[0]["opportunity_id"] = 1, 3
        doc["events"] = first + [{"kind": "opportunity_skipped", "time_ns": 10,
                                    "opportunity_id": 2, "reason": "callback-coalesced"}] + second
        result = reader.analyze(doc)
        self.assertEqual(result["schedule"]["opportunity_count"], 3)
        self.assertEqual(result["schedule"]["attempted_count"], 2)
        self.assertEqual(result["skipped_generation"], 1)
        self.assertEqual([row["due_ns"] for row in result["schedule"]["opportunities"]], [0, 10, 20])
        self.assertEqual([row["decision_delay_ns"] for row in result["schedule"]["opportunities"]],
                         [0, 0, 2])
        self.assertEqual([row["return_ns"] for row in result["schedule"]["opportunities"]],
                         [5, None, 27])
        self.assertEqual(result["schedule"]["opportunities"][1]["reason"], "callback-coalesced")
        self.assertEqual(result["successful_unique_updates"], 2)
        self.assertEqual(result["acceptance"], "not_evaluated")
        self.assertIsNone(result["native_deadline_misses"])
        self.assertIsNone(result["missed_physical_refreshes"])

    def test_rational_schedule_keeps_phase_and_excludes_stop_and_drain_opportunities(self):
        doc = capture()
        doc.update(start_ns=100, end_ns=150, window_ns=50, schedule={
            "stop_ns": 110, "period_numerator_ns": 10, "period_denominator": 3,
        })
        doc["events"] = [
            {"kind": "opportunity_skipped", "time_ns": due, "opportunity_id": index,
             "reason": "callback-coalesced"} for index, due in ((1, 100), (2, 103), (3, 106))
        ]
        empty = reader.analyze(doc)
        self.assertEqual(empty["frames"], [])
        self.assertIsNone(empty["max_published_residence_ns"])
        self.assertIsNone(empty["max_disposition_age_ns"])
        # Unscheduled work must be explicit, and cannot fill a missing opportunity.
        tail = frame_events(1, 120, 140)
        tail[0]["opportunity_id"] = None
        doc["events"] += tail
        before = copy.deepcopy(doc)
        result = reader.analyze(doc)
        self.assertEqual(doc, before)
        self.assertEqual(result["schedule"]["epoch_ns"], 100)
        self.assertEqual(result["schedule"]["opportunity_count"], 3)
        self.assertEqual(result["schedule"]["unscheduled_render_count"], 1)
        self.assertEqual(result["skipped_generation"], 3)
        self.assertEqual(result["successful_unique_updates"], 1)
        # Extending stop by one ns includes the phase-exact 110ns opportunity.
        doc["schedule"]["stop_ns"] = 111
        with self.assertRaisesRegex(reader.EvidenceError, "unresolved schedule"):
            reader.analyze(doc)
        doc["events"].append({"kind": "opportunity_skipped", "time_ns": 150,
                              "opportunity_id": 4, "reason": "callback-coalesced"})
        self.assertEqual([row["due_ns"] for row in reader.analyze(doc)["schedule"]["opportunities"]],
                         [100, 103, 106, 110])

    def test_schedule_cannot_hide_missing_duplicated_early_or_invalid_opportunities(self):
        base = capture()
        base.update(end_ns=100, window_ns=100, schedule={
            "stop_ns": 20, "period_numerator_ns": 10, "period_denominator": 1,
        })
        base["events"] = frame_events(1, 0, 5) + [
            {"kind": "opportunity_skipped", "time_ns": 10, "opportunity_id": 2, "reason": "coalesced"},
        ]
        base["events"][0]["opportunity_id"] = 1
        mutations = []
        for field, value in (("stop_ns", 0), ("stop_ns", 101), ("stop_ns", True),
                             ("period_numerator_ns", 0), ("period_numerator_ns", 2**63),
                             ("period_numerator_ns", 1.5), ("period_denominator", 0),
                             ("period_denominator", 11), ("period_denominator", False),
                             ("period_denominator", "1"), ("approved", True)):
            bad = copy.deepcopy(base)
            bad["schedule"][field] = value
            mutations.append(bad)
        for value in (None, [], {}, "30fps"):
            bad = copy.deepcopy(base)
            bad["schedule"] = value
            mutations.append(bad)
        for field, value in (("opportunity_id", 0), ("opportunity_id", 3), ("opportunity_id", True),
                             ("opportunity_id", 1.0), ("opportunity_id", None),
                             ("opportunity_id", 1), ("time_ns", 9), ("reason", "")):
            bad = copy.deepcopy(base)
            bad["events"][-1][field] = value
            mutations.append(bad)
        bad = copy.deepcopy(base)
        bad["events"].pop()  # Missing skip is not inferred from the frame sequence.
        mutations.append(bad)
        bad = copy.deepcopy(base)
        bad["events"][0]["opportunity_id"] = None  # Unscheduled render cannot fill slot 1.
        mutations.append(bad)
        bad = copy.deepcopy(base)
        del bad["events"][0]["opportunity_id"]
        mutations.append(bad)
        bad = copy.deepcopy(base)
        bad["events"][0]["opportunity_id"] = 2  # Attempt before due, not just early skip.
        mutations.append(bad)
        bad = copy.deepcopy(base)
        bad["events"].append(copy.deepcopy(bad["events"][-1]))
        mutations.append(bad)
        bad = copy.deepcopy(base)
        del bad["schedule"]
        mutations.append(bad)
        bad = copy.deepcopy(base)
        bad.update(end_ns=100_001, window_ns=100_001)
        bad["schedule"].update(stop_ns=100_001, period_numerator_ns=1)
        mutations.append(bad)
        for index, bad in enumerate(mutations):
            with self.subTest(index=index), self.assertRaises(reader.EvidenceError):
                reader.analyze(bad)
        legacy = capture()
        legacy["events"] = frame_events(1, 0, 100)
        result = reader.analyze(legacy)
        self.assertIsNone(result["schedule"])
        self.assertIsNone(result["skipped_generation"])
        legacy["events"].append({"kind": "opportunity_skipped", "time_ns": 200,
                                 "opportunity_id": 1, "reason": "coalesced"})
        with self.assertRaises(reader.EvidenceError):
            reader.analyze(legacy)

    def test_scheduled_failures_and_supersession_are_attempts_not_forgiven_deadlines(self):
        doc = capture()
        doc.update(end_ns=100, window_ns=100, schedule={
            "stop_ns": 50, "period_numerator_ns": 10, "period_denominator": 1,
        })
        for frame_id, start, terminal, prefix in (
            (1, 0, "supersede", 2), (2, 10, "render_failed", 1), (3, 20, "not_submitted", 3),
        ):
            attempt = frame_events(frame_id, start, start + 5)[:prefix]
            attempt[0]["opportunity_id"] = frame_id
            event = {"kind": terminal, "time_ns": start + 5, "generation": "worker-a", "frame_id": frame_id}
            if terminal != "supersede":
                event["reason"] = "diagnostic-failure"
            doc["events"] += attempt + [event]
        for frame_id, start, ok in ((4, 30, False), (5, 40, True)):
            attempt = frame_events(frame_id, start, start + 5)
            attempt[0]["opportunity_id"] = frame_id
            attempt[-1]["ok"] = ok
            doc["events"] += attempt
        doc["events"] += [
            {"kind": "broker_start", "time_ns": 60, "generation": "worker-a",
             "frame_id": 5, "run_id": "test-run", "input_id": None, "call_id": "recovery", "replay": True},
            {"kind": "broker_end", "time_ns": 70, "call_id": "recovery", "ok": True},
        ]
        result = reader.analyze(doc)
        self.assertEqual(result["schedule"]["attempted_count"], 5)
        self.assertEqual(result["skipped_generation"], 0)
        self.assertEqual([row["disposition"] for row in result["schedule"]["opportunities"]],
                         ["superseded", "render_failed", "not_submitted", "broker_error", "completed"])
        self.assertEqual([row["return_ns"] for row in result["schedule"]["opportunities"]],
                         [None, None, None, None, 45])
        self.assertEqual(result["successful_unique_updates"], 1)
        self.assertEqual(result["acceptance"], "not_evaluated")
        self.assertIsNone(result["native_deadline_misses"])
        self.assertIsNone(result["missed_physical_refreshes"])

    def test_zero_duration_phases_remain_zero_not_missing(self):
        doc = capture()
        doc["events"] = frame_events(1, 0, 0)
        for event in doc["events"]:
            event["time_ns"] = 0
        result = reader.analyze(doc)
        self.assertEqual(result["max_published_residence_ns"], 0)
        self.assertEqual(result["max_disposition_age_ns"], 0)
        for field in ("publish_ns", "select_ns", "broker_start_ns", "disposition_ns", "published_residence_ns"):
            self.assertEqual(result["frames"][0][field], 0)

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

    def test_published_residence_includes_drops_and_preserves_original_age_across_replay(self):
        doc = capture()
        doc["events"] = frame_events(1, 0, 100)
        for frame_id, start, terminal, time in (
            (2, 110, "supersede", 160), (3, 170, "not_submitted", 190),
            (4, 200, "render_failed", 210), (5, 220, "invalidate", 260),
        ):
            prefix = {"supersede": 2, "not_submitted": 3, "render_failed": 1, "invalidate": 2}[terminal]
            doc["events"] += frame_events(frame_id, start, time)[:prefix]
            event = {"kind": terminal, "time_ns": time, "generation": "worker-a", "frame_id": frame_id}
            if terminal != "supersede":
                event["reason"] = "diagnostic-failure"
            doc["events"].append(event)
        failed = frame_events(6, 270, 300)
        failed[-1]["ok"] = False
        doc["events"] += failed + [
            {"kind": "broker_start", "time_ns": 310, "generation": "worker-a",
             "frame_id": 1, "run_id": "test-run", "input_id": None, "call_id": "recovery", "replay": True},
            {"kind": "broker_end", "time_ns": 400, "call_id": "recovery", "ok": True},
        ]
        result = reader.analyze(doc)
        self.assertEqual([f["published_residence_ns"] for f in result["frames"]], [1, 49, 1, None, 39, 1])
        self.assertEqual([f["disposition_ns"] for f in result["frames"]], [100, 160, 190, 210, 260, 300])
        self.assertEqual([f["age_at_disposition_ns"] for f in result["frames"]], [100, 50, 20, 10, 40, 30])
        self.assertEqual([f["broker_start_ns"] for f in result["frames"]], [3, None, None, None, None, 273])
        self.assertEqual(result["max_published_residence_ns"], 49)
        self.assertEqual(result["max_disposition_age_ns"], 100)
        self.assertEqual(result["frames"][0]["publish_ns"], 1)
        self.assertEqual(result["frames"][0]["select_ns"], 2)
        self.assertIsNone(result["frames"][3]["publish_ns"])

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
            doc["schedule"] = {"stop_ns": 1, "period_numerator_ns": 1, "period_denominator": 1}
            doc["events"][0]["opportunity_id"] = 1
            original = json.dumps(doc)
            path.write_text(original)
            result = subprocess.run(command, capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)
            report = json.loads(result.stdout)
            self.assertEqual(report["schedule"]["attempted_count"], 1)
            self.assertEqual(report["acceptance"], "not_evaluated")
            self.assertEqual(path.read_text(), original)
            for text in ('{"schema":"x","schema":"y"}', '{"capture_dropped":NaN}',
                         '{"capture_dropped":Infinity}', '[]', '{',
                         '{"schedule":{"stop_ns":1,"stop_ns":2}}'):
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
