import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]


class CombinedFaultEvidenceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        suffix = '.exe' if os.name == 'nt' else ''
        cls.binary = Path(os.environ.get('VV_LAB_BINARY', ROOT / f'target/release/vv-lab{suffix}')).resolve()
        if not cls.binary.is_file():
            raise unittest.SkipTest('compile vv-lab antes destes testes')

    def artifact(self, name):
        temporary = tempfile.TemporaryDirectory(dir=ROOT)
        self.addCleanup(temporary.cleanup)
        output = Path(temporary.name) / 'run'
        result = subprocess.run(
            [str(self.binary), 'run', str(ROOT / f'scenarios/m2/{name}.json'),
             '--seed', '42', '--output', str(output)],
            capture_output=True, text=True, timeout=10,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads((output / 'events.json').read_text(encoding='utf-8'))

    def test_pending_packets_keep_sample_age_through_dropout(self):
        artifact = self.artifact('a-dropout-pending-delay')
        records = artifact['events']
        deliveries = [row for row in records if row['event']['kind'] == 'sensor_delivery']
        pending = next(row for row in deliveries if row['tick'] == 6 and row['event']['sample_tick'] == 3)
        self.assertTrue(pending['event']['accepted'])
        ticks = {row['tick']: row['event'] for row in records if row['event']['kind'] == 'tick_result'}
        self.assertEqual(ticks[6]['observed']['age_ticks'], 3)
        self.assertEqual(ticks[6]['observed']['confidence_permille'], 250)
        self.assertFalse(ticks[7]['observed']['fresh'])
        self.assertEqual(ticks[7]['safety_state'], 'fallback')
        self.assertEqual(ticks[6]['truth_from'], ticks[6]['truth_position'])
        self.assertTrue(any(row['event']['sample_tick'] == 1 and not row['event']['accepted'] for row in deliveries))
        outcomes = {row['event']['packet_sequence']: row['event'] for row in records if row['event']['kind'] == 'packet_outcome'}
        ordering = [(outcomes[row['event']['packet_sequence']]['due_tick'], row['event']['packet_sequence']) for row in deliveries]
        self.assertEqual(ordering, sorted(ordering))
        self.assertTrue(all(row['tick'] == outcomes[row['event']['packet_sequence']]['due_tick'] for row in deliveries))

    def test_repeated_recovery_never_moves_during_low_confidence(self):
        artifact = self.artifact('c-intermittent')
        recoveries = [row for row in artifact['events'] if row['event']['kind'] == 'transition'
                      and row['event'].get('reason') == 'confidence_recovered_for_configured_interval']
        self.assertEqual(len(recoveries), 3)
        held = []
        for row in artifact['events']:
            event = row['event']
            if event['kind'] != 'tick_result':
                continue
            observed = event['observed']
            low = observed is None or not observed['fresh'] or observed['confidence_permille'] < 500
            if low or event['safety_state'] == 'fallback':
                held.append(row['tick'])
                self.assertEqual(event['truth_from'], event['truth_position'], row['tick'])
                self.assertEqual(event['action']['distance_mm'], 0, row['tick'])
        self.assertGreater(len(held), 10)


if __name__ == '__main__':
    unittest.main()
