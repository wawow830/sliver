#!/usr/bin/env python3
"""Offline, experimental observation accounting; never grants release acceptance.

No devices, subprocesses or clocks are accessed. See
 docs/research/native-performance-observation-format.md for the input contract.
"""
import argparse
import hashlib
import json
import sys


class EvidenceError(ValueError):
    """The supplied observations cannot support internally consistent accounting."""


def require(condition, message):
    if not condition:
        raise EvidenceError(message)


def fields(value, names):
    require(type(value) is dict and set(value) == set(names.split()),
            f"expected fields: {names}")


def integer(value):
    require(type(value) is int and 0 <= value <= 2**63 - 1,
            "expected a nonnegative signed-64-bit integer")
    return value


def identity(value):
    require(type(value) is str and 0 < len(value) <= 128 and value.isascii()
            and all(c.isalnum() or c in "-_." for c in value), "invalid identity")
    return value


def schedule_rows(schedule, start, end):
    fields(schedule, "stop_ns period_numerator_ns period_denominator")
    stop = integer(schedule["stop_ns"])
    numerator = integer(schedule["period_numerator_ns"])
    denominator = integer(schedule["period_denominator"])
    require(start < stop <= end, "invalid schedule stop")
    require(numerator >= denominator > 0, "schedule period must be at least one nanosecond")
    count = ((stop - start) * denominator + numerator - 1) // numerator
    require(count <= 100_000, "too many schedule opportunities")
    return [{"due_ns": start + index * numerator // denominator,
             "decision_ns": None, "key": None, "reason": None} for index in range(count)]


def resolve_opportunity(rows, event, key=None):
    index = integer(event["opportunity_id"])
    require(1 <= index <= len(rows), "unknown schedule opportunity")
    row = rows[index - 1]
    require(row["decision_ns"] is None, "reused schedule opportunity")
    require(event["time_ns"] >= row["due_ns"], "opportunity resolved before due time")
    row.update(decision_ns=event["time_ns"], key=key,
               reason=identity(event["reason"]) if key is None else None)


def analyze(document):
    scheduled = type(document) is dict and "schedule" in document
    fields(document, "schema source run_id clock start_ns end_ns window_ns capture_dropped events"
           + (" schedule" if scheduled else ""))
    require(document["schema"] == "sliver-native-observation-v0", "unsupported schema")
    require(document["source"] in ("synthetic", "native-broker"), "unsupported observation source")
    require(document["clock"] == "CLOCK_MONOTONIC", "unsupported clock")
    identity(document["run_id"])
    start, end = integer(document["start_ns"]), integer(document["end_ns"])
    require(start < end, "empty or reversed interval")
    opportunities = schedule_rows(document["schedule"], start, end) if scheduled else []
    window = integer(document["window_ns"])
    require(window > 0, "empty window")
    window_count = (end - start + window - 1) // window
    require(window_count <= 100_000, "too many observation windows")
    require(integer(document["capture_dropped"]) == 0, "capture loss")
    events = document["events"]
    require(type(events) is list and 0 < len(events) <= 100_000, "invalid event count")
    frames, calls, latest, inputs = {}, {}, {}, {}
    active_input = None
    active_call = None
    previous = start
    for event in events:
        require(type(event) is dict and "kind" in event and "time_ns" in event, "invalid event")
        now = integer(event["time_ns"])
        require(previous <= now <= end, "events outside interval or out of order")
        previous = now
        kind = event["kind"]
        if kind in ("input", "input_timeout"):
            fields(event, "kind time_ns input_id")
            token = identity(event["input_id"])
            if kind == "input_timeout":
                require(token == active_input, "timeout without outstanding input")
                inputs[token]["timeout_ns"] = now
                active_input = None
                continue
            require(token not in inputs, "reused input identity")
            require(active_input is None, "isolated-transition capture has overlapping inputs")
            require(now < end, "input at closed interval end")
            inputs[token] = {"input_id": token, "receipt_ns": now, "response_ns": None,
                             "latency_ns": None, "timeout_ns": None}
            active_input = token
            continue
        if kind == "broker_end":
            fields(event, "kind time_ns call_id ok")
            call_id = identity(event["call_id"])
            require(call_id == active_call, "unmatched broker result")
            active_call = None
            require(type(event["ok"]) is bool, "invalid broker result")
            call = calls[call_id]
            call.update(end=now, ok=event["ok"])
            frame = frames[call["key"]]
            if not call["replay"]:
                frame["state"] = "completed" if event["ok"] else "broker_error"
                frame["disposed"] = now
                if event["ok"]:
                    frame["returned"] = now
            token = frame["input_id"]
            if event["ok"] and token is not None and inputs[token]["response_ns"] is None:
                inputs[token].update(response_ns=now, latency_ns=now - inputs[token]["receipt_ns"])
                if active_input == token:
                    active_input = None
            continue
        if kind == "opportunity_skipped":
            require(scheduled, "opportunity without declared schedule")
            fields(event, "kind time_ns opportunity_id reason")
            resolve_opportunity(opportunities, event)
            continue
        extra = {"render": " input_id" + (" opportunity_id" if scheduled else ""),
                 "publish": "", "select": "", "supersede": "",
                 "invalidate": " reason", "render_failed": " reason", "not_submitted": " reason",
                 "broker_start": " run_id input_id call_id replay"}
        require(type(kind) is str and kind in extra, "unknown event kind")
        fields(event, "kind time_ns generation frame_id" + extra[kind])
        generation = identity(event["generation"])
        frame_id = integer(event["frame_id"])
        require(frame_id > 0, "frame identity starts at one")
        key = (generation, frame_id)
        if kind == "render":
            require(frame_id == latest.get(generation, 0) + 1,
                    "reused, decreasing or missing render identity in closed cohort")
            token = event["input_id"]
            if token is not None:
                identity(token)
                require(token in inputs, "render marker precedes input receipt")
            opportunity_id = event.get("opportunity_id")
            if opportunity_id is not None:
                resolve_opportunity(opportunities, event, key)
            latest[generation] = frame_id
            frames[key] = {"state": "rendered", "rendered": now, "returned": None,
                           "reason": None, "input_id": token, "opportunity_id": opportunity_id,
                           "published": None, "selected": None, "submitted": None, "disposed": None}
            continue
        require(key in frames, "unknown frame")
        frame = frames[key]
        if kind in ("supersede", "invalidate", "render_failed", "not_submitted"):
            expected, terminal = {
                "supersede": ("published", "superseded"),
                "invalidate": ("published", "invalidated"),
                "render_failed": ("rendered", "render_failed"),
                "not_submitted": ("selected", "not_submitted"),
            }[kind]
            require(frame["state"] == expected, "disposition out of order")
            frame["state"] = terminal
            frame["disposed"] = now
            if "reason" in event:
                frame["reason"] = identity(event["reason"])
        elif kind == "publish":
            require(frame["state"] == "rendered", "publication out of order")
            frame["state"] = "published"
            frame["published"] = now
        elif kind == "select":
            require(frame["state"] == "published", "selection out of order")
            frame["state"] = "selected"
            frame["selected"] = now
        else:
            require(event["run_id"] == document["run_id"], "broker run marker differs from capture")
            require(type(event["replay"]) is bool, "invalid replay flag")
            require(frame["state"] == ("completed" if event["replay"] else "selected"),
                    "broker call without selected frame or completed replay source")
            require(event["input_id"] == frame["input_id"], "broker marker differs from render marker")
            call_id = identity(event["call_id"])
            require(call_id not in calls, "reused call identity")
            require(active_call is None, "overlapping broker calls")
            active_call = call_id
            calls[call_id] = {"key": key, "start": now, "end": None, "replay": event["replay"]}
            if not event["replay"]:
                frame["state"] = "in_flight"
                frame["submitted"] = now
    require(all(call["end"] is not None for call in calls.values()), "unresolved broker call")
    require(all(frame["state"] in ("completed", "broker_error", "superseded", "invalidated",
                                   "render_failed", "not_submitted") for frame in frames.values()),
            "unresolved frame disposition")
    require(all(row["response_ns"] is not None or row["timeout_ns"] is not None
                for row in inputs.values()), "unresolved input")
    require(all(row["decision_ns"] is not None for row in opportunities),
            "unresolved schedule opportunity")
    schedule = None
    if scheduled:
        attempted = sum(row["key"] is not None for row in opportunities)
        schedule = {
            **document["schedule"], "epoch_ns": start, "opportunity_count": len(opportunities),
            "attempted_count": attempted, "skipped_count": len(opportunities) - attempted,
            "unscheduled_render_count": sum(f["opportunity_id"] is None for f in frames.values()),
            "opportunities": [
                {"opportunity_id": index + 1, "due_ns": row["due_ns"],
                 "decision_ns": row["decision_ns"],
                 "decision_delay_ns": row["decision_ns"] - row["due_ns"],
                 "generation": row["key"][0] if row["key"] else None,
                 "frame_id": row["key"][1] if row["key"] else None,
                 "disposition": frames[row["key"]]["state"] if row["key"] else "skipped",
                 "return_ns": frames[row["key"]]["returned"] if row["key"] else None,
                 "reason": row["reason"]} for index, row in enumerate(opportunities)],
        }
    windows = [None] * window_count
    for row in inputs.values():
        if row["latency_ns"] is not None:
            index = (row["receipt_ns"] - start) // window
            windows[index] = max(windows[index] or 0, row["latency_ns"])
    # Compare every observed window with the best earlier observed window.
    # Equal endpoints cannot conceal a worse interior window. Empty windows
    # remain visible and do not establish a no-growth claim.
    best, peak_increase = None, None
    for value in windows:
        if value is not None:
            if best is not None:
                peak_increase = max(peak_increase or 0, value - best)
            best = value if best is None else min(best, value)
    returns = sorted(frame["returned"] for frame in frames.values() if frame["returned"] is not None)
    duration = returns[-1] - returns[0] if returns else 0
    dispositions = {}
    for frame in frames.values():
        state = frame["state"]
        dispositions[state] = dispositions.get(state, 0) + 1
        residence_end = frame["selected"] if frame["selected"] is not None else frame["disposed"]
        frame["residence"] = (residence_end - frame["published"]
                              if frame["published"] is not None else None)
    return {
        "schema": "sliver-native-analysis-v0",
        "source": document["source"], "source_authentication": "not_verified",
        "run_id": document["run_id"], "clock": document["clock"],
        "start_ns": start, "end_ns": end, "window_ns": window,
        "acceptance": "not_evaluated", "successful_unique_updates": len(returns),
        "inputs": list(inputs.values()),
        "input_timeout_count": sum(row["timeout_ns"] is not None for row in inputs.values()),
        "input_window_max_latency_ns": windows,
        "peak_window_increase_ns": peak_increase,
        "max_input_latency_ns": max((row["latency_ns"] for row in inputs.values()
                                     if row["latency_ns"] is not None), default=None),
        "dispositions": dispositions,
        "frames": [{"generation": key[0], "frame_id": key[1],
                    "disposition": frame["state"], "render_ns": frame["rendered"],
                    "return_ns": frame["returned"], "reason": frame["reason"],
                    "publish_ns": frame["published"], "select_ns": frame["selected"],
                    "broker_start_ns": frame["submitted"], "disposition_ns": frame["disposed"],
                    "published_residence_ns": frame["residence"],
                    "age_at_disposition_ns": frame["disposed"] - frame["rendered"],
                    "input_id": frame["input_id"], "opportunity_id": frame["opportunity_id"]}
                   for key, frame in frames.items()],
        "broker_calls": [{"call_id": call_id, "generation": call["key"][0],
                          "frame_id": call["key"][1], "start_ns": call["start"],
                          "end_ns": call["end"], "ok": call["ok"], "replay": call["replay"]}
                         for call_id, call in calls.items()],
        "broker_call_count": len(calls),
        "broker_errors": sum(not call["ok"] for call in calls.values()),
        "successful_replays": sum(call["replay"] and call["ok"] for call in calls.values()),
        "between_returns_per_second": (len(returns) - 1) * 1e9 / duration if duration else None,
        "whole_interval_updates_per_second": len(returns) * 1e9 / (end - start),
        "max_frame_age_ns": max((f["returned"] - f["rendered"] for f in frames.values()
                                 if f["returned"] is not None), default=None),
        "max_published_residence_ns": max((f["residence"] for f in frames.values()
                                           if f["residence"] is not None), default=None),
        "max_disposition_age_ns": max((f["disposed"] - f["rendered"] for f in frames.values()), default=None),
        "optical_fps": None, "missed_physical_refreshes": None,
        "native_deadline_misses": None, "schedule": schedule,
        "skipped_generation": schedule["skipped_count"] if scheduled else None,
        "limitations": [
            "Caller-declared JSON; no pixel decoding, collector or source authentication in this reader.",
            "Closed cohort only; no implicit warmup, discarded edges or v1 ledger conversion.",
            "Isolated broker-input receipt to successful return; not physical or optical latency.",
            "No approved rate/deadline/latency budget or release verdict; declared schedule only.",
        ],
    }


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, f"duplicate JSON key: {key}")
        result[key] = value
    return result


def reject_constant(value):
    raise EvidenceError(f"non-finite JSON number: {value}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("capture", help="closed-cohort observation JSON; not a v1 release ledger")
    args = parser.parse_args()
    try:
        with open(args.capture, "rb") as source:
            raw = source.read(16 * 1024 * 1024 + 1)
        require(len(raw) <= 16 * 1024 * 1024, "capture exceeds 16 MiB")
        document = json.loads(raw, object_pairs_hook=unique_object, parse_constant=reject_constant)
        result = analyze(document)
        result["capture_sha256"] = hashlib.sha256(raw).hexdigest()
        print(json.dumps(result, indent=2, allow_nan=False))
    except (OSError, ValueError, RecursionError) as error:
        print(f"invalid observation evidence: {error}", file=sys.stderr)
        return 1
    return 0  # Parse/accounting success only, explicitly not release acceptance.


if __name__ == "__main__":
    raise SystemExit(main())
