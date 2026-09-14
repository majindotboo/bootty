"""Behavior checks for the live switching benchmark's acceptance boundary."""
import importlib.util
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    'switching', Path(__file__).resolve().parents[1] / 'scripts/benchmark-switching.py')
BENCH = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BENCH)


class SwitchingBenchmarkTest(unittest.TestCase):
    def test_failed_warmup_cannot_be_reported_as_a_passed_empty_run(self):
        result = BENCH.summary([{'case': 'tab', 'warmup': True, 'status': 'invalidated'}])
        self.assertEqual(result['tab']['status'], 'invalidated')
        self.assertEqual(result['tab']['failures'], 1)

    def test_only_destination_paint_with_focus_and_content_completes_sample(self):
        with tempfile.TemporaryDirectory() as directory:
            trace = Path(directory) / 'trace.jsonl'
            trace.touch()
            with patch.object(BENCH.Runner, 'command', return_value={
                'instance': {'instance_id': 'bootty-dev-0123456789abcdef', 'pid': 123}
            }):
                runner = BENCH.Runner(SimpleNamespace(
                    namespace='bootty-dev-0123456789abcdef', trace=trace, timeout=1))
            destination = {'command': ['command', 'select_session', '2'], 'target': 'second'}

            def dispatch(command):
                self.assertEqual(command, destination['command'])
                records = [
                    {'target': 'first', 'focused': True, 'has_text': True},
                    {'target': 'second', 'focused': False, 'has_text': True},
                    {'target': 'second', 'focused': True, 'has_text': False},
                    {'target': 'second', 'focused': True, 'has_text': True},
                ]
                with trace.open('a') as output:
                    output.write(json.dumps({'event': 'command_received', 'pid': 123,
                                             'command': 'select_session', 'unix_ns': 1_000_000}) + '\n')
                    for index, record in enumerate(records):
                        output.write(json.dumps(dict(record, event='terminal_painted', pid=123,
                                                     unix_ns=1_000_000 + index)) + '\n')

            with patch.object(runner, 'command', side_effect=dispatch), \
                 patch.object(BENCH.time, 'time_ns', return_value=1_000_000), \
                 patch.object(BENCH.time, 'monotonic_ns', return_value=1_000_000):
                result = runner.switch(destination)
            runner.trace.close()
            self.assertEqual(result['paint']['unix_ns'], 1_000_003)
            self.assertEqual(result['command_to_paint_ms'], .000003)

    def test_failed_samples_invalidate_summary_and_warmups_do_not_affect_tails(self):
        def row(value, **extra):
            return dict(case='session', warmup=False, status='passed',
                        command_ms=value, command_to_paint_ms=value, owner_to_paint_ms=value, **extra)
        rows = [row(1), row(2), row(100)]
        rows.append(dict(row(999), warmup=True))
        rows.append({'case': 'session', 'warmup': False, 'status': 'invalidated'})
        result = BENCH.summary(rows)['session']
        self.assertEqual(result['samples'], 4)
        self.assertEqual(result['failures'], 1)
        self.assertEqual(result['status'], 'invalidated')
        self.assertEqual(result['metrics']['command_to_paint_ms']['p50'], 2)
        self.assertEqual(result['metrics']['command_to_paint_ms']['p99'], 100)


if __name__ == '__main__':
    unittest.main()
