#!/usr/bin/env python3
"""P1 behavior at the offline evaluator interface; no host/device access."""
import copy
import importlib.util
import hashlib
import json
import subprocess
import sys
import tempfile
from pathlib import Path
import unittest


def load(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(name + '.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


policy = load('evaluate-native-performance')
T = 10_000_000_000
S = 70_000_000_000
E = 72_000_000_000


def event(kind, time, **fields):
    return dict(kind=kind, time_ns=time, **fields)


def frame_events(frame_id, time, token=None, duration=1_000_000):
    key = dict(generation='worker-1', frame_id=frame_id)
    return [event('render', time, input_id=token, **key),
            event('publish', time + 100, **key),
            event('select', time + 200, **key),
            event('broker_start', time + 300, run_id='cadence-run', input_id=token,
                  call_id=f'call-{frame_id}', replay=False, **key),
            event('broker_end', time + duration, call_id=f'call-{frame_id}', ok=True)]


def cadence(skips=(), durations=None, rate=30):
    events = [event('input_idle', 1),
              event('timer_registered', 2, timer_id='animation',
                    period_numerator_ns=1_000_000_000, period_denominator=rate)]
    events += frame_events(1, 3)
    events += [event('warmup_closed', 6_000_000_000)]
    next_frame = 2
    for i in range(rate * 60):
        due = T + i * 1_000_000_000 // rate
        events += [event('timer_dispatch', due, timer_id='animation', skipped=0)]
        if i not in skips:
            events += frame_events(next_frame, due, duration=(durations or {}).get(i, 1_000_000))
            next_frame += 1
    events.sort(key=lambda row: row['time_ns'])
    events += [event('capture_closed', E)]
    return dict(name='cadence', run_id='cadence-run', arm_ns=1, start_ns=T,
                stop_ns=S, end_ns=E, capture_dropped=0, cleanup_ok=True, events=events)


def causal_case(receipts=None, delays=None):
    document = cadence()
    document['name'] = 'causal-response'
    if receipts is None:
        receipts = [T + i * 1_000_000_000 + 10_000_000 for i in range(60)]
    for i, receipt in enumerate(receipts):
        token, contact = f'input-{i+1}', f'contact-{i+1}'
        document['events'] += [
            event('touch', receipt, phase='down', contact_id=contact),
            event('input', receipt, input_id=token),
            event('input_delivered', receipt + (delays or {}).get(i, 0) + 1,
                  input_id=token, contact_id=contact),
            event('input_mutation', receipt + (delays or {}).get(i, 0) + 2, input_id=token),
            event('touch', receipt + 200_000_000, phase='up', contact_id=contact)]
    tokens = {}
    for row in document['events']:
        if row['kind'] == 'render':
            prior = [i for i, receipt in enumerate(receipts)
                     if receipt + (delays or {}).get(i, 0) + 2 <= row['time_ns']]
            tokens[row['frame_id']] = f'input-{prior[-1]+1}' if prior else None
    for row in document['events']:
        if row['kind'] in ('render', 'broker_start'):
            row['input_id'] = tokens[row['frame_id']]
    document['events'].sort(key=lambda row: row['time_ns'])
    return document


def overhead(mode, run_id, offset):
    samples = []
    for i in range(1800):
        due = T + offset + i * 1_000_000_000 // 30
        samples.append(dict(drive_start_ns=due, render_start_ns=due + 100,
                            render_end_ns=due + 1000, present_start_ns=due + 2000,
                            present_end_ns=due + 1_000_000, drive_end_ns=due + 1_000_100, ok=True))
    return dict(mode=mode, run_id=run_id, arm_ns=offset + 1,
                start_ns=T + offset, stop_ns=S + offset, end_ns=E + offset,
                closed_ns=E + offset, warmup_start_ns=offset + 2, warmup_closed_ns=offset + 6_000_000_000,
                capture_dropped=0, cleanup_ok=True, samples=samples)


def battery():
    cases = [cadence(), causal_case(), cadence(rate=60, skips=range(1, 3600, 2))]
    cases[2]['name'] = 'requested-overproduction'
    for i, case in enumerate(cases):
        case['run_id'] = f'case-{i}'
        offset = (i + 6) * 80_000_000_000
        for key in ('arm_ns', 'start_ns', 'stop_ns', 'end_ns'):
            case[key] += offset
        for row in case['events']:
            row['time_ns'] += offset
            if row['kind'] == 'broker_start':
                row['run_id'] = case['run_id']
    return dict(schema='sliver-native-policy-evidence-v1', policy_revision=policy.POLICY_REVISION,
                policy_sha256='fa565559057f855726e5bc728911617b76b1003f4116bfc21fbd3e05060f5a99',
                source='synthetic', cases=cases,
                overhead=[overhead(mode, f'overhead-{i}', i * 80_000_000_000)
                          for i, mode in enumerate('ABCCBA')])


class PolicyTests(unittest.TestCase):
    def test_cadence_meets_policy_but_is_not_release_evidence(self):
        report = policy.evaluate_case(cadence(), source='synthetic')
        self.assertEqual(report['policy_result'], 'pass')
        self.assertEqual(report['scheduled_in_window_updates'], 1800)
        self.assertEqual(report['software_deadline_misses'], 0)
        self.assertEqual(report['acceptance'], 'not_evaluated')
        self.assertEqual(report['source_authentication'], 'not_verified')

    def test_rate_and_misses_keep_fixed_denominators_at_threshold(self):
        at_limit = policy.evaluate_case(cadence(skips=range(0, 1500, 100)), source='synthetic')
        self.assertEqual(at_limit['policy_result'], 'pass')
        self.assertEqual(at_limit['scheduled_in_window_updates'], 1785)
        self.assertEqual(at_limit['software_deadline_misses'], 15)
        below = policy.evaluate_case(cadence(skips=range(0, 1600, 100)), source='synthetic')
        self.assertIn('scheduled_rate', below['failures'])
        self.assertIn('software_deadlines', below['failures'])

    def test_age_and_first_boundary_gap_are_inclusive(self):
        at_limit = policy.evaluate_case(cadence(skips=(1, 2, 3), durations={0: 100_000_000}),
                                        source='synthetic')
        self.assertEqual(at_limit['policy_result'], 'pass')
        above = policy.evaluate_case(cadence(skips=(1, 2, 3), durations={0: 100_000_001}),
                                     source='synthetic')
        self.assertIn('frame_age_or_residence', above['failures'])
        self.assertIn('update_gap', above['failures'])

    def test_every_isolated_input_has_a_callback_token_and_window_sample(self):
        report = policy.evaluate_case(causal_case(), source='synthetic')
        self.assertEqual(report['policy_result'], 'pass')
        self.assertEqual(report['input']['sample_count'], 60)
        self.assertEqual(report['input']['window_counts'], [10, 10, 10, 10, 10, 10])
        self.assertEqual(report['input']['latencies_ns'][0], 24_333_333)
        self.assertEqual(report['input']['peak_window_increase_ns'], 0)

    def test_input_latency_limit_equality_and_one_nanosecond_failure(self):
        for excess, expected in ((0, 'pass'), (1, 'fail')):
            with self.subTest(excess=excess):
                document = causal_case(delays={0: 120_000_000})
                row = next(row for row in document['events']
                           if row['kind'] == 'broker_end' and row['call_id'] == 'call-6')
                row['time_ns'] = T + 160_000_000 + excess
                document['events'].sort(key=lambda row: row['time_ns'])
                report = policy.evaluate_case(document, source='synthetic')
                self.assertEqual(report['policy_result'], expected)
                self.assertEqual(report['input']['maximum_ns'], 150_000_000 + excess)

    def test_middle_window_spike_cannot_hide_behind_equal_endpoints(self):
        for extra, expected in ((1, 'pass'), (2, 'fail')):
            with self.subTest(extra=extra):
                document = causal_case(delays={30: 33_333_334})
                frame = next(row for row in document['events']
                             if row['kind'] == 'render' and row['input_id'] == 'input-31')
                next(row for row in document['events'] if row['kind'] == 'broker_end'
                     and row['call_id'] == f"call-{frame['frame_id']}")['time_ns'] += extra
                report = policy.evaluate_case(document, source='synthetic')
                self.assertEqual(report['policy_result'], expected)
                self.assertEqual(report['input']['peak_window_increase_ns'], 33_333_333 + extra)
                self.assertEqual(report['input']['window_maxima_ns'][0],
                                 report['input']['window_maxima_ns'][-1])

    def test_late_input_can_allocate_causal_only_drain_without_padding_rate(self):
        receipts = [T + i * 1_000_000_000 + 10_000_000 for i in range(59)] + [S - 10_000_000]
        document = causal_case(receipts)
        document['events'] += frame_events(1802, S + 10_000_000, token='input-60')
        document['events'].sort(key=lambda row: row['time_ns'])
        report = policy.evaluate_case(document, source='synthetic')
        self.assertEqual(report['policy_result'], 'pass')
        self.assertEqual(report['scheduled_in_window_updates'], 1800)
        self.assertEqual(report['input']['latencies_ns'][-1], 21_000_000)
        self.assertEqual(report['input']['window_counts'][-1], 10)

    def test_missing_causal_links_or_raw_down_cannot_be_excluded(self):
        for kind in ('touch', 'input', 'input_delivered', 'input_mutation'):
            with self.subTest(kind=kind):
                document = causal_case()
                index = next(i for i, row in enumerate(document['events']) if row['kind'] == kind)
                del document['events'][index]
                with self.assertRaises(policy.EvidenceError):
                    policy.evaluate_case(document, source='synthetic')

    def test_sampling_counts_every_down_and_rejects_sparse_windows(self):
        receipts = [T + i * 1_000_000_000 + 10_000_000 for i in range(60) if not 20 <= i < 23]
        report = policy.evaluate_case(causal_case(receipts), source='synthetic')
        self.assertIn('input_sampling', report['failures'])
        self.assertEqual(report['input']['sample_count'], 57)
        self.assertEqual(report['input']['window_counts'][2], 7)

    def test_native_label_never_authenticates_supplied_observations(self):
        report = policy.evaluate_case(cadence(), source='native-broker')
        self.assertEqual(report['policy_result'], 'pass')
        self.assertEqual(report['acceptance'], 'not_evaluated')
        self.assertEqual(report['source_authentication'], 'not_verified')

    def test_requested_60hz_does_not_apply_native_miss_allowance(self):
        document = cadence(rate=60, skips=range(1, 3600, 2))
        document['name'] = 'requested-overproduction'
        report = policy.evaluate_case(document, source='synthetic')
        self.assertEqual(report['policy_result'], 'pass')
        self.assertEqual(report['scheduled_in_window_updates'], 1800)
        self.assertEqual(report['software_deadline_misses'], 1800)
        self.assertEqual(report['skipped_generation'], 1800)
        self.assertEqual(len(report['opportunities']), 3600)
        self.assertFalse(report['native_miss_allowance_applies'])

    def test_complete_battery_checks_each_case_and_both_overhead_triplets(self):
        report = policy.evaluate(battery())
        self.assertEqual(report['policy_result'], 'pass')
        self.assertEqual(report['acceptance'], 'not_evaluated')
        self.assertEqual(len(report['overhead']['comparisons']), 6)
        self.assertEqual([case['scheduled_in_window_updates'] for case in report['cases']],
                         [1800, 1800, 1800])

    def test_overhead_does_not_pool_away_one_bad_triplet(self):
        for loss, expected in ((9, 'pass'), (10, 'fail')):
            with self.subTest(loss=loss):
                document = battery()
                del document['overhead'][3]['samples'][:loss]
                report = policy.evaluate(document)
                self.assertEqual(report['policy_result'], expected)
                self.assertEqual(report['overhead']['comparisons'][0]['policy_result'], 'pass')
                self.assertEqual(report['overhead']['comparisons'][4]['policy_result'], expected)

    def test_complete_drive_overhead_has_its_own_inclusive_budget(self):
        for increase, expected in ((1_000_000, 'pass'), (1_000_001, 'fail')):
            with self.subTest(increase=increase):
                document = battery()
                for sample in document['overhead'][2]['samples']:
                    sample['drive_end_ns'] += increase
                report = policy.evaluate(document)
                self.assertEqual(report['policy_result'], expected)
                comparison = report['overhead']['comparisons'][1]
                self.assertTrue(comparison['rate_loss_within_limit'])
                self.assertEqual(comparison['render_increase_ns'], 0)
                self.assertEqual(comparison['drive_increase_ns'], increase)

    def test_wrong_policy_run_reuse_or_reordering_is_not_accepted(self):
        original = battery()
        for mutate in (
                lambda d: d.update(policy_revision='another-policy'),
                lambda d: d.update(policy_sha256='0' * 64),
                lambda d: d['overhead'][0].update(run_id='case-0'),
                lambda d: d['cases'].reverse(),
                lambda d: d['overhead'].reverse(),
                lambda d: d['cases'][0].update(arm_ns=1)):
            document = copy.deepcopy(original)
            mutate(document)
            with self.assertRaises(policy.EvidenceError):
                policy.evaluate(document)

    def test_open_cohorts_loss_malformed_fields_and_failed_cleanup(self):
        original = cadence()
        for mutate in (
                lambda d: d.update(capture_dropped=1),
                lambda d: d.update(capture_dropped=False),
                lambda d: d.update(end_ns=E + 1),
                lambda d: d.update(extra='not permitted'),
                lambda d: d['events'].pop(),
                lambda d: d['events'].reverse(),
                lambda d: d['events'].pop(7)):
            document = copy.deepcopy(original)
            mutate(document)
            with self.assertRaises(policy.EvidenceError):
                policy.evaluate_case(document, source='synthetic')
        document = cadence()
        document['cleanup_ok'] = False
        self.assertIn('cleanup', policy.evaluate_case(document, source='synthetic')['failures'])

    def test_replay_cannot_add_rate_or_reset_frame_age(self):
        document = cadence()
        document['events'] += [event('broker_start', 2_000_000, generation='worker-1',
                                    frame_id=1, run_id='cadence-run', input_id=None,
                                    call_id='replay-call', replay=True),
                               event('broker_end', 3_000_000, call_id='replay-call', ok=True)]
        document['events'].sort(key=lambda row: row['time_ns'])
        report = policy.evaluate_case(document, source='synthetic')
        self.assertEqual(report['scheduled_in_window_updates'], 1800)
        self.assertIn('disqualifying_event', report['failures'])

    def test_surplus_unscheduled_frames_cannot_pad_scheduled_rate(self):
        document = cadence(skips=range(0, 1600, 100))
        for i in range(16):
            document['events'] += frame_events(10_000 + i,
                T + (i * 100 + 1) * 1_000_000_000 // 30 + 5_000_000)
        document['events'].sort(key=lambda row: row['time_ns'])
        renders = [row for row in document['events'] if row['kind'] == 'render']
        mapping = {row['frame_id']: i + 1 for i, row in enumerate(renders)}
        for row in document['events']:
            if 'frame_id' in row:
                row['frame_id'] = mapping[row['frame_id']]
            if 'call_id' in row:
                row['call_id'] = f"call-{mapping[int(row['call_id'].removeprefix('call-'))]}"
        report = policy.evaluate_case(document, source='synthetic')
        self.assertEqual(report['scheduled_in_window_updates'], 1784)
        self.assertEqual(report['unscheduled_render_count'], 16)
        self.assertIn('scheduled_rate', report['failures'])

    def test_initial_preflight_and_registration_must_precede_work(self):
        document = cadence()
        render = next(row for row in document['events'] if row['kind'] == 'render')
        render['time_ns'] = 1
        document['events'].sort(key=lambda row: row['time_ns'])
        with self.assertRaises(policy.EvidenceError):
            policy.evaluate_case(document, source='synthetic')

    def test_raw_down_before_equal_time_response_is_still_overlapping_input(self):
        receipts = [T + 10_000_000, T + 34_333_333] + [
            T + i * 1_000_000_000 + 10_000_000 for i in range(2, 60)]
        document = causal_case(receipts)
        events = document['events']
        next(row for row in events if row['kind'] == 'touch' and row['phase'] == 'up'
             and row['contact_id'] == 'contact-1')['time_ns'] = T + 20_000_000
        events.sort(key=lambda row: row['time_ns'])
        down = next(row for row in events if row['kind'] == 'touch' and row['phase'] == 'down'
                    and row['contact_id'] == 'contact-2')
        events.remove(down)
        response_index = next(i for i, row in enumerate(events)
                              if row['kind'] == 'broker_end' and row['call_id'] == 'call-3')
        events.insert(response_index, down)
        with self.assertRaises(policy.EvidenceError):
            policy.evaluate_case(document, source='synthetic')

    def test_equal_time_warmup_closure_uses_record_order_for_frames_and_contacts(self):
        document = cadence()
        closed = 6_000_000_000
        for row in document['events']:
            if 'frame_id' in row and row['frame_id'] >= 2:
                row['frame_id'] += 1
                if row['kind'] in ('render', 'broker_start'):
                    row['input_id'] = 'warmup-input'
            if 'call_id' in row and int(row['call_id'][5:]) >= 2:
                row['call_id'] = f"call-{int(row['call_id'][5:]) + 1}"
        document['events'] += [
            event('touch', closed - 10_000_000, phase='down', contact_id='warmup-contact'),
            event('input', closed - 10_000_000, input_id='warmup-input'),
            event('input_delivered', closed - 9_999_999, input_id='warmup-input',
                  contact_id='warmup-contact'),
            event('input_mutation', closed - 9_999_998, input_id='warmup-input')]
        document['events'] += frame_events(2, closed - 1_000_000, token='warmup-input')
        document['events'] += [event('touch', closed, phase='up', contact_id='warmup-contact')]
        document['events'].sort(key=lambda row: (row['time_ns'], row['kind'] == 'warmup_closed'))
        self.assertEqual(policy.evaluate_case(document, source='synthetic')['policy_result'], 'pass')
        closure = next(row for row in document['events'] if row['kind'] == 'warmup_closed')
        document['events'].remove(closure)
        terminal = next(i for i, row in enumerate(document['events'])
                        if row['kind'] == 'broker_end' and row['call_id'] == 'call-2')
        document['events'].insert(terminal, closure)
        with self.assertRaises(policy.EvidenceError):
            policy.evaluate_case(document, source='synthetic')
        document['events'].remove(closure)
        lift = next(i for i, row in enumerate(document['events'])
                    if row['kind'] == 'touch' and row['contact_id'] == 'warmup-contact'
                    and row['phase'] == 'up')
        document['events'].insert(lift, closure)
        with self.assertRaisesRegex(policy.EvidenceError, 'warmup input not quiesced'):
            policy.evaluate_case(document, source='synthetic')

    def test_observed_capture_closure_may_follow_deadline_without_backdating(self):
        document = cadence()
        document['events'][-1]['time_ns'] = E + 1
        report = policy.evaluate_case(document, source='synthetic')
        self.assertEqual(report['policy_result'], 'pass')
        self.assertEqual(report['closed_ns'], E + 1)
        self.assertEqual(report['scheduled_in_window_updates'], 1800)
        document['events'][-1]['time_ns'] = E - 1
        with self.assertRaises(policy.EvidenceError):
            policy.evaluate_case(document, source='synthetic')

    def test_late_observer_closure_cannot_overlap_next_run_arming(self):
        document = battery()
        document['overhead'][0]['closed_ns'] += 1
        self.assertEqual(policy.evaluate(document)['policy_result'], 'pass')
        document['overhead'][0]['closed_ns'] = document['overhead'][1]['arm_ns'] + 1
        with self.assertRaises(policy.EvidenceError):
            policy.evaluate(document)

    def test_return_at_stop_is_timely_but_not_an_in_window_update(self):
        document = cadence()
        next(row for row in document['events'] if row['kind'] == 'broker_end'
             and row['call_id'] == 'call-1801')['time_ns'] = S
        report = policy.evaluate_case(document, source='synthetic')
        self.assertEqual(report['scheduled_in_window_updates'], 1799)
        self.assertEqual(report['software_deadline_misses'], 0)

    def test_cli_hashes_exact_input_and_never_grants_release_acceptance(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'capture.json'
            raw = json.dumps(battery()).encode() + b'\n'
            path.write_bytes(raw)
            command = [sys.executable] + (['-O'] if sys.flags.optimize else []) + [
                '-B', str(Path(__file__).with_name('evaluate-native-performance.py')), str(path)]
            result = subprocess.run(command, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            report = json.loads(result.stdout)
            self.assertEqual(report['capture_sha256'], hashlib.sha256(raw).hexdigest())
            self.assertEqual(report['acceptance'], 'not_evaluated')
            self.assertEqual(path.read_bytes(), raw)
            for invalid in (b'{"schema":1,"schema":2}', b'{"value":NaN}', b'[]'):
                path.write_bytes(invalid)
                result = subprocess.run(command, capture_output=True, text=True)
                self.assertEqual(result.returncode, 1)
                self.assertEqual(result.stdout, '')

    def test_global_error_is_not_forgiven_by_miss_allowance(self):
        document = cadence()
        next(row for row in document['events'] if row['kind'] == 'broker_end')['ok'] = False
        report = policy.evaluate_case(document, source='synthetic')
        self.assertEqual(report['policy_result'], 'fail')
        self.assertIn('disqualifying_event', report['failures'])


if __name__ == '__main__':
    unittest.main()
