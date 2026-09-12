#!/usr/bin/env python3
"""Evaluate approved P1 limits on supplied observations, never release provenance.

This uninstalled evaluator has no devices, clocks, subprocesses or threshold
switches. A policy pass is not native release acceptance. The trusted collector
and release provenance/transaction verifier are separate responsibilities.
"""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import sys

_spec = importlib.util.spec_from_file_location(
    'sliver_observation', Path(__file__).with_name('analyze-native-performance.py'))
_observation = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_observation)
EvidenceError = _observation.EvidenceError
require = _observation.require
fields = _observation.fields
integer = _observation.integer
identity = _observation.identity

POLICY_REVISION = 'cbe2b5da809eed7cc4b3406b222baf43e1e34a2d'
POLICY_SHA256 = 'fa565559057f855726e5bc728911617b76b1003f4116bfc21fbd3e05060f5a99'
SECOND = 1_000_000_000
AGE_LIMIT = 100_000_000
INPUT_LIMIT = 150_000_000
WINDOW_INCREASE_LIMIT = 33_333_334


def percentile95(values):
    return sorted(values)[(95 * len(values) + 99) // 100 - 1] if values else None


def input_accounting(case, observation):
    """Reconcile every raw down through delivery, mutation and observed pixels."""
    start, stop = case['start_ns'], case['stop_ns']
    causal = case['name'] == 'causal-response'
    contacts, tokens, delivered, mutated, answered = set(), {}, set(), set(), set()
    current_token, pending_down = None, None
    warmup_closed = False
    frames = {(frame['generation'], frame['frame_id']): frame for frame in observation['frames']}
    calls = {call['call_id']: call for call in observation['broker_calls']}
    for row in case['events']:
        kind, now = row['kind'], row['time_ns']
        if kind == 'touch':
            phase, contact = row['phase'], identity(row['contact_id'])
            require(phase in ('down', 'move', 'up', 'cancel'), 'unknown touch phase')
            require(phase != 'cancel', 'contact cancellation')
            require(now < start or causal, 'unexpected touch outside causal workload')
            if phase == 'down':
                require(now < stop, 'new down during drain')
                require(not contacts and pending_down is None and set(tokens) <= answered,
                        'concurrent, duplicate or unanswered previous down')
                contacts.add(contact)
                pending_down = (contact, now)
            else:
                require(contact in contacts, 'broken contact ordering')
                if phase == 'up':
                    contacts.remove(contact)
        elif kind == 'input':
            token = row['input_id']
            require(pending_down is not None and pending_down[1] == now,
                    'input receipt lacks matching physical down')
            tokens[token] = pending_down
            pending_down = None
        elif kind == 'input_delivered':
            token = identity(row['input_id'])
            require(token in tokens and token not in delivered and
                    identity(row['contact_id']) == tokens[token][0], 'ambiguous input delivery')
            delivered.add(token)
        elif kind == 'input_mutation':
            token = identity(row['input_id'])
            require(token in delivered and token not in mutated, 'mutation without delivered down')
            # Only the newest delivered down can mutate the isolated fixture.
            require(token == next(reversed(tokens)), 'out-of-order input mutation')
            mutated.add(token)
            current_token = token
        elif kind == 'render':
            require(row['input_id'] == current_token, 'render token differs from callback mutation')
            if now >= stop:
                require(causal and current_token is not None and current_token not in answered
                        and tokens[current_token][1] < stop,
                        'drain render is not answering a pre-stop input')
        elif kind == 'broker_end' and row['ok']:
            call = calls[row['call_id']]
            token = frames[(call['generation'], call['frame_id'])]['input_id']
            if token is not None and not call['replay']:
                answered.add(token)
        elif kind == 'key':
            identity(row['key_id'])
            require(type(row['pressed']) is bool, 'invalid key state')
            require(now < start, 'unexpected key transition')
        elif kind == 'warmup_closed':
            require(not contacts and pending_down is None and set(tokens) <= answered,
                    'warmup input not quiesced')
            warmup_closed = True
        if warmup_closed and now < start and kind in ('touch', 'input', 'input_delivered', 'input_mutation'):
            raise EvidenceError('new warmup input after closure')
    require(not contacts and pending_down is None, 'unreleased or unaccounted contact')
    require(set(tokens) == delivered == mutated, 'missing input callback or mutation')
    inputs = [row for row in observation['inputs'] if start <= row['receipt_ns'] < stop]
    latencies = [row['latency_ns'] for row in inputs if row['latency_ns'] is not None]
    counts, maxima = [0] * 6, [None] * 6
    for row in inputs:
        window = (row['receipt_ns'] - start) // (10 * SECOND)
        counts[window] += 1
        if row['latency_ns'] is not None:
            maxima[window] = max(maxima[window] or 0, row['latency_ns'])
    increase = max([0] + [maxima[j] - maxima[i] for j in range(6) for i in range(j)
                           if maxima[j] is not None and maxima[i] is not None])
    receipts = [row['receipt_ns'] for row in inputs]
    max_gap = max(b - a for a, b in zip([start] + receipts, receipts + [stop]))
    failures = []
    if causal:
        if len(inputs) < 60 or min(counts) < 8 or max_gap > 2 * SECOND:
            failures.append('input_sampling')
        if len(latencies) != len(inputs) or not latencies or max(latencies) > INPUT_LIMIT:
            failures.append('input_latency')
        if any(value is None for value in maxima) or increase > WINDOW_INCREASE_LIMIT:
            failures.append('input_window_worsening')
    ordered = sorted(latencies)
    median_twice = (ordered[(len(ordered) - 1) // 2] + ordered[len(ordered) // 2]
                    if ordered else None)
    return dict(sample_count=len(inputs), latencies_ns=latencies, window_counts=counts,
                window_maxima_ns=maxima, peak_window_increase_ns=increase,
                maximum_receipt_gap_ns=max_gap, p95_ns=percentile95(latencies),
                maximum_ns=max(latencies) if latencies else None, median_twice_ns=median_twice), failures


def evaluate_case(case, *, source):
    fields(case, 'name run_id arm_ns start_ns stop_ns end_ns capture_dropped cleanup_ok events')
    require(source in ('synthetic', 'native-broker'), 'unsupported source')
    require(case['name'] in ('cadence', 'causal-response', 'requested-overproduction'),
            'unsupported case')
    native_miss_allowance = case['name'] != 'requested-overproduction'
    rate = 30 if native_miss_allowance else 60
    count = rate * 60
    identity(case['run_id'])
    arm, start, stop, end = (integer(case[key]) for key in
                             ('arm_ns', 'start_ns', 'stop_ns', 'end_ns'))
    require(arm < start and stop == start + 60 * SECOND and end == stop + 2 * SECOND,
            'P1 requires a fixed 60 second interval and 2 second drain')
    require(type(case['cleanup_ok']) is bool, 'invalid cleanup result')
    require(integer(case['capture_dropped']) == 0, 'capture loss')
    require(type(case['events']) is list and 0 < len(case['events']) <= 100_000,
            'invalid event count')
    extra = {'input_idle': '', 'timer_registered':
             ' timer_id period_numerator_ns period_denominator',
             'timer_dispatch': ' timer_id skipped', 'warmup_closed': '', 'capture_closed': '',
             'touch': ' phase contact_id', 'key': ' key_id pressed',
             'input_delivered': ' input_id contact_id', 'input_mutation': ' input_id'}
    regular, metadata = [], {kind: [] for kind in extra}
    previous = arm
    for event in case['events']:
        require(type(event) is dict and type(event.get('kind')) is str, 'invalid event')
        now = integer(event.get('time_ns'))
        kind = event['kind']
        require(previous <= now and (now <= end or kind == 'capture_closed'),
                'events outside interval or out of order')
        previous = now
        if kind in extra:
            fields(event, 'kind time_ns' + extra[kind])
            metadata[kind].append(event)
        else:
            regular.append(event)
    observation = _observation.analyze(dict(
        schema='sliver-native-observation-v0', source=source, run_id=case['run_id'],
        clock='CLOCK_MONOTONIC', start_ns=arm, end_ns=end, window_ns=10 * SECOND,
        capture_dropped=case['capture_dropped'], events=regular))
    require(len(metadata['input_idle']) == 1 and metadata['input_idle'][0]['time_ns'] == arm
            and case['events'][0] == metadata['input_idle'][0],
            'missing initial no-contact preflight')
    require(len(metadata['capture_closed']) == 1 and case['events'][-1] ==
            metadata['capture_closed'][0] and metadata['capture_closed'][0]['time_ns'] >= end,
            'missing final capture closure')
    require(len(metadata['timer_registered']) == 1, 'missing or replaced animation timer')
    timer = metadata['timer_registered'][0]
    identity(timer['timer_id'])
    require(integer(timer['period_numerator_ns']) == SECOND and
            integer(timer['period_denominator']) == rate and timer['time_ns'] < start,
            'wrong or late timer registration')
    require(metadata['timer_dispatch'], 'missing timer dispatch observations')
    for dispatch in metadata['timer_dispatch']:
        require(dispatch['timer_id'] == timer['timer_id'] and
                timer['time_ns'] <= dispatch['time_ns'] < stop, 'invalid timer dispatch')
        integer(dispatch['skipped'])
    require(len(metadata['warmup_closed']) == 1, 'missing warmup closure')
    closed = metadata['warmup_closed'][0]['time_ns']
    frames = observation['frames']
    warmup = [frame for frame in frames if frame['render_ns'] < start]
    require(warmup and timer['time_ns'] <= warmup[0]['render_ns']
            and warmup[0]['render_ns'] + 5 * SECOND <= closed < start,
            'missing five second warmup')
    # Equal clock readings are ordered by the stream, not by inventing a 1 ns gap.
    closure_index = next(index for index, row in enumerate(case['events'])
                         if row['kind'] == 'warmup_closed')
    prefix = case['events'][:closure_index]
    terminal_kinds = ('supersede', 'invalidate', 'render_failed', 'not_submitted')
    disposed_before_close = {(row['generation'], row['frame_id']) for row in prefix
                             if row['kind'] in terminal_kinds}
    returned_before_close = {row['call_id'] for row in prefix if row['kind'] == 'broker_end'}
    disposed_before_close.update((call['generation'], call['frame_id'])
        for call in observation['broker_calls']
        if not call['replay'] and call['call_id'] in returned_before_close)
    require(all((frame['generation'], frame['frame_id']) in disposed_before_close for frame in warmup),
            'warmup cohort not quiesced before closure')
    measured = [frame for frame in frames if frame['render_ns'] >= start]
    due = [start + index * SECOND // rate for index in range(count)]
    opportunities = [None] * count
    next_opportunity = 0
    for frame in measured:
        now = frame['render_ns']
        if now >= stop:
            continue  # Causal-only drain allocation is checked against the actual input chain.
        latest = next_opportunity
        while latest < count and due[latest] <= now:
            latest += 1
        if latest > next_opportunity:
            opportunities[latest - 1] = frame
            next_opportunity = latest
    successful = [frame['return_ns'] for frame in opportunities if frame is not None
                  and frame['return_ns'] is not None and start <= frame['return_ns'] < stop]
    successful.sort()
    misses = sum(frame is None or frame['return_ns'] is None or
                 frame['return_ns'] > due[index] + AGE_LIMIT
                 for index, frame in enumerate(opportunities))
    gaps = [b - a for a, b in zip([start] + successful, successful + [stop])]
    input_report, failures = input_accounting(case, observation)
    if (any(frame['disposition'] not in ('completed', 'superseded') for frame in frames)
            or observation['broker_errors'] or observation['input_timeout_count']
            or any(call['replay'] for call in observation['broker_calls'])
            or len({frame['generation'] for frame in frames}) != 1):
        failures.append('disqualifying_event')
    if len(successful) < 1785:
        failures.append('scheduled_rate')
    if native_miss_allowance and misses > 15:
        failures.append('software_deadlines')
    if not successful or max(gaps) > AGE_LIMIT:
        failures.append('update_gap')
    if any(frame['age_at_disposition_ns'] > AGE_LIMIT or
           (frame['published_residence_ns'] is not None and
            frame['published_residence_ns'] > AGE_LIMIT) for frame in measured):
        failures.append('frame_age_or_residence')
    if not case['cleanup_ok']:
        failures.append('cleanup')
    return dict(policy_revision=POLICY_REVISION, source=source,
                policy_result='fail' if failures else 'pass', failures=failures,
                acceptance='not_evaluated', source_authentication='not_verified',
                scheduled_in_window_updates=len(successful), software_deadline_misses=misses,
                maximum_update_gap_ns=max(gaps), closed_ns=metadata['capture_closed'][0]['time_ns'],
                native_miss_allowance_applies=native_miss_allowance,
                skipped_generation=sum(frame is None for frame in opportunities),
                unscheduled_render_count=len(measured) - sum(frame is not None for frame in opportunities),
                timer_dispatch_count=len(metadata['timer_dispatch']),
                declared_timer_skips=sum(row['skipped'] for row in metadata['timer_dispatch']),
                opportunities=[dict(opportunity_id=index + 1, due_ns=due[index],
                                    generation=frame['generation'] if frame else None,
                                    frame_id=frame['frame_id'] if frame else None,
                                    disposition=frame['disposition'] if frame else 'skipped',
                                    return_ns=frame['return_ns'] if frame else None)
                               for index, frame in enumerate(opportunities)],
                input=input_report, observation=observation)


def overhead_accounting(runs):
    require(type(runs) is list and len(runs) == 6, 'six overhead runs required')
    reports = []
    for index, run in enumerate(runs):
        fields(run, 'mode run_id arm_ns start_ns stop_ns end_ns closed_ns warmup_start_ns '
               'warmup_closed_ns capture_dropped cleanup_ok samples')
        require(run['mode'] == 'ABCCBA'[index], 'wrong overhead run order')
        identity(run['run_id'])
        arm, start, stop, end, warmup, closed = (integer(run[key]) for key in
            ('arm_ns', 'start_ns', 'stop_ns', 'end_ns', 'warmup_start_ns', 'warmup_closed_ns'))
        require(arm <= warmup and warmup + 5 * SECOND <= closed < start and
                stop == start + 60 * SECOND and end == stop + 2 * SECOND
                and integer(run['closed_ns']) >= end, 'invalid overhead intervals or warmup')
        require(integer(run['capture_dropped']) == 0, 'overhead capture loss')
        require(type(run['cleanup_ok']) is bool, 'invalid overhead cleanup result')
        require(type(run['samples']) is list and 0 < len(run['samples']) <= 100_000,
                'invalid overhead samples')
        renders, drives, calls, successes, errors, boundary = [], [], [], 0, 0, 0
        previous = start
        for row in run['samples']:
            fields(row, 'drive_start_ns render_start_ns render_end_ns present_start_ns '
                   'present_end_ns drive_end_ns ok')
            ds, rs, re, ps, pe, de = (integer(row[key]) for key in
                ('drive_start_ns', 'render_start_ns', 'render_end_ns', 'present_start_ns',
                 'present_end_ns', 'drive_end_ns'))
            require(previous <= ds <= rs <= re <= ps <= pe <= de <= end and
                    ds < stop and rs < stop, 'out-of-order or unmeasured overhead sample')
            require(type(row['ok']) is bool, 'invalid overhead call result')
            previous = de
            renders.append(re - rs)
            drives.append(de - ds)
            calls.append(pe - ps)
            errors += not row['ok']
            successes += row['ok'] and pe < stop
            boundary += pe >= stop
        require(successes > 0, 'no successful in-window overhead calls')
        reports.append(dict(mode=run['mode'], run_id=run['run_id'], closed_ns=run['closed_ns'],
                            successful_calls=successes,
                            boundary_calls=boundary, errors=errors, cleanup_ok=run['cleanup_ok'],
                            render_p95_ns=percentile95(renders), drive_p95_ns=percentile95(drives),
                            call_p95_ns=percentile95(calls)))
    failures, comparisons = [], []
    for index, run in enumerate(reports):
        if run['errors'] or not run['cleanup_ok']:
            failures.append(f'overhead_run_{index}_error_or_cleanup')
    for triplet in ((0, 1, 2), (5, 4, 3)):
        a, b, c = triplet
        for reference, candidate in ((a, b), (b, c), (a, c)):
            ref, test = reports[reference], reports[candidate]
            loss_ok = test['successful_calls'] * 200 >= ref['successful_calls'] * 199
            render_increase = test['render_p95_ns'] - ref['render_p95_ns']
            drive_increase = test['drive_p95_ns'] - ref['drive_p95_ns']
            passed = loss_ok and render_increase <= 1_000_000 and drive_increase <= 1_000_000
            comparisons.append(dict(reference_run=ref['run_id'], candidate_run=test['run_id'],
                                    rate_loss_within_limit=loss_ok, render_increase_ns=render_increase,
                                    drive_increase_ns=drive_increase,
                                    policy_result='pass' if passed else 'fail'))
            if not passed:
                failures.append(f'overhead_comparison_{reference}_{candidate}')
    return dict(runs=reports, comparisons=comparisons), failures


def evaluate(document):
    fields(document, 'schema policy_revision policy_sha256 source cases overhead')
    require(document['schema'] == 'sliver-native-policy-evidence-v1', 'unsupported policy schema')
    require(document['policy_revision'] == POLICY_REVISION and
            document['policy_sha256'] == POLICY_SHA256, 'unapproved policy identity')
    require(document['source'] in ('synthetic', 'native-broker'), 'unsupported source')
    cases = document['cases']
    require(type(cases) is list and len(cases) == 3, 'three native cases required')
    reports = [evaluate_case(case, source=document['source']) for case in cases]
    require([case['name'] for case in cases] ==
            ['cadence', 'causal-response', 'requested-overproduction'], 'wrong native case order')
    overhead, failures = overhead_accounting(document['overhead'])
    runs = document['overhead'] + cases
    require(len({run['run_id'] for run in runs}) == 9, 'reused run identity')
    closures = [run['closed_ns'] for run in overhead['runs'] + reports]
    require(all(closed <= following['arm_ns'] for closed, following in zip(closures, runs[1:])),
            'overlapping or reordered battery runs')
    for case, report in zip(cases, reports):
        failures.extend(f"{case['name']}:{failure}" for failure in report['failures'])
    return dict(schema='sliver-native-policy-report-v1', policy_revision=POLICY_REVISION,
                policy_sha256=POLICY_SHA256, source=document['source'],
                policy_result='fail' if failures else 'pass', failures=failures,
                acceptance='not_evaluated', source_authentication='not_verified',
                cases=reports, overhead=overhead,
                limitations=['Caller-supplied observations; no collector/build provenance verification.',
                             'No release acceptance, optical FPS, or DMA retirement claim.',
                             'Run declarations and decoded-marker labels require trusted live collection.',
                             'No production slot-capacity proof or legacy TSV promotion.'])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('capture', help='P1 policy-observation battery, not a release ledger')
    args = parser.parse_args()
    try:
        with open(args.capture, 'rb') as capture:
            raw = capture.read(16 * 1024 * 1024 + 1)
        require(len(raw) <= 16 * 1024 * 1024, 'capture exceeds 16 MiB')
        document = json.loads(raw, object_pairs_hook=_observation.unique_object,
                              parse_constant=_observation.reject_constant)
        report = evaluate(document)
        report['capture_sha256'] = hashlib.sha256(raw).hexdigest()
        print(json.dumps(report, indent=2, allow_nan=False))
        return 0 if report['policy_result'] == 'pass' else 1
    except (OSError, ValueError, RecursionError) as error:
        print(f'invalid P1 evidence: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
