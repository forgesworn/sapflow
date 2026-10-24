"""Process accounting keeps one deadline even when sampling takes time."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("measure_process", Path(__file__).resolve().parents[1] / "scripts/measure-process.py")
sampler = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sampler)


class SamplerTests(unittest.TestCase):
    def test_cpu_formats_include_days_and_fractional_seconds(self):
        for text, expected in [("00:03.25", 3.25), ("02:03:04", 7384), ("2-03:04:05.5", 183845.5)]:
            with self.subTest(text=text):
                self.assertEqual(sampler.cpu_seconds(text), expected)

    def test_slow_samples_do_not_extend_every_poll(self):
        now = [0.0]
        samples = []
        def clock():
            return now[0]
        def sleep(seconds):
            now[0] += seconds
        def sample(pid):
            self.assertEqual(pid, 123)
            now[0] += .2
            samples.append(now[0])
            return now[0] / 2, 10 + len(samples)
        cpu, peak, elapsed = sampler.measure(123, 10, clock=clock, sleep=sleep, sample=sample)
        self.assertAlmostEqual(cpu, 50)
        self.assertLess(elapsed, 10.3)
        self.assertGreaterEqual(elapsed, 10)
        self.assertEqual(peak, 10 + len(samples))


if __name__ == "__main__":
    unittest.main()
