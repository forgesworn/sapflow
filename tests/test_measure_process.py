"""Process accounting keeps one deadline even when sampling takes time."""
import importlib.util
import io
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

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

    def test_macos_sampling_still_asks_ps_for_time_and_rss(self):
        with patch.object(sampler.subprocess, "check_output", return_value="  1:02.50  20480\n") as ps:
            self.assertEqual(sampler.sample_process(77), (62.5, 20))
        arguments, options = ps.call_args
        self.assertEqual(arguments[0], ["ps", "-o", "time=,rss=", "-p", "77"])
        self.assertEqual(options["env"]["LC_ALL"], "C")
        with patch.object(sampler.subprocess, "check_output", return_value=""):
            with self.assertRaises(RuntimeError):
                sampler.sample_process(77)

    def test_platform_picks_the_proc_sampler_only_on_linux(self):
        self.assertIs(sampler.platform_sampler("linux"), sampler.sample_proc)
        for name in ("darwin", "freebsd14"):
            self.assertIs(sampler.platform_sampler(name), sampler.sample_process)


# A real /proc/<pid>/stat line: utime 250 and stime 50 ticks, then children's
# cutime 7 and cstime 8, which a process's own CPU does not include. The
# command name holds a space and parentheses, as a thread or script name may.
PROC_STAT = (
    "4321 (sap (flow) x) S 1 4321 4321 0 -1 4194560 5000 0 12 0 250 50 7 8 20 0 9 0 "
    "12345 123456789 3000 18446744073709551615 1 1 0 0 0 0 0 0 0 0 0 0 17 3 0 0 0 0 0\n"
)
PROC_STATUS = (
    "Name:\tsap (flow) x\nUmask:\t0022\nState:\tS (sleeping)\nTgid:\t4321\n"
    "VmPeak:\t  200000 kB\nVmSize:\t  190000 kB\nVmHWM:\t   30000 kB\n"
    "VmRSS:\t   27852 kB\nThreads:\t9\n"
)


class ProcSamplerTests(unittest.TestCase):
    def test_cpu_is_utime_plus_stime_in_seconds_and_ignores_children(self):
        self.assertEqual(sampler.parse_proc_stat(PROC_STAT, 100), 3.0)
        self.assertEqual(sampler.parse_proc_stat(PROC_STAT, 250), 1.2)
        # The name can hold anything up to the last closing parenthesis.
        plain = PROC_STAT.replace("(sap (flow) x)", "(ffmpeg)")
        self.assertEqual(sampler.parse_proc_stat(plain, 100), 3.0)

    def test_malformed_stat_text_is_an_error_not_a_guess(self):
        for text in ("", "4321 sapflow S 1 2 3", "4321 (x) S 1 2", "4321 (x) " + "S " * 13):
            with self.subTest(text=text), self.assertRaises(RuntimeError):
                sampler.parse_proc_stat(text, 100)

    def test_rss_is_vmrss_in_mib(self):
        self.assertEqual(sampler.parse_vm_rss(PROC_STATUS), 27852 / 1024)
        # Same unit conversion as the ps path: KiB / 1024.
        self.assertEqual(sampler.parse_vm_rss("VmRSS:\t1024 kB\n"), 1)

    def test_status_without_a_usable_vmrss_is_an_error(self):
        # A zombie or kernel thread has no VmRSS line; a changed unit is not guessed.
        for text in ("Name:\tx\nState:\tZ (zombie)\n", "VmRSS:\t5 MB\n", "VmRSS:\tmany kB\n", "VmRSS:\n"):
            with self.subTest(text=text), self.assertRaises(RuntimeError):
                sampler.parse_vm_rss(text)

    def test_sample_reads_both_files_for_the_pid(self):
        with tempfile.TemporaryDirectory() as directory:
            process = Path(directory) / "4321"
            process.mkdir()
            (process / "stat").write_text(PROC_STAT)
            (process / "status").write_text(PROC_STATUS)
            self.assertEqual(sampler.sample_proc(4321, root=directory, clock_ticks=100), (3.0, 27852 / 1024))
            with self.assertRaises(RuntimeError):
                sampler.sample_proc(4322, root=directory, clock_ticks=100)

    def test_the_clock_tick_rate_comes_from_the_system_by_default(self):
        with tempfile.TemporaryDirectory() as directory:
            process = Path(directory) / "9"
            process.mkdir()
            (process / "stat").write_text(PROC_STAT)
            (process / "status").write_text(PROC_STATUS)
            with patch.object(sampler.os, "sysconf", create=True, return_value=200) as sysconf:
                self.assertEqual(sampler.sample_proc(9, root=directory)[0], 1.5)
            sysconf.assert_called_once_with("SC_CLK_TCK")

    def test_proc_samples_give_the_same_measurement_schema_as_ps(self):
        # 40 ticks of CPU a second at 100 Hz is 40% of one core, whichever sampler ran.
        now = [0.0]
        ticks = [0]
        with tempfile.TemporaryDirectory() as directory:
            process = Path(directory) / "5"
            process.mkdir()
            (process / "status").write_text(PROC_STATUS)

            def sleep(seconds):
                now[0] += seconds
                ticks[0] += int(40 * seconds)

            def sample(pid):
                (process / "stat").write_text(
                    f"5 (x) S 1 5 5 0 -1 4194560 5000 0 12 0 {ticks[0]} 0 0 0 20 0 9 0 1 2 3 4\n")
                return sampler.sample_proc(5, root=directory, clock_ticks=100)

            cpu, peak, elapsed = sampler.measure(5, 10, clock=lambda: now[0], sleep=sleep, sample=sample)
        self.assertAlmostEqual(cpu, 40)
        self.assertEqual(peak, 27852 / 1024)
        self.assertEqual(elapsed, 10)

    def test_main_prints_one_line_of_cpu_peak_and_elapsed(self):
        with patch.object(sampler, "measure", return_value=(12.34, 56.78, 20.0004)) as measure, \
                patch.object(sampler, "platform_sampler", return_value="the sampler"), \
                patch.object(sampler.sys, "argv", ["measure-process.py", "99", "20"]), \
                patch("sys.stdout", new_callable=io.StringIO) as out:
            sampler.main()
        self.assertEqual(out.getvalue(), "12.3 56.8 20.000\n")
        measure.assert_called_once_with(99, 20.0, sample="the sampler")


if __name__ == "__main__":
    unittest.main()
