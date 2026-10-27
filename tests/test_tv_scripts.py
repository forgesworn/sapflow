"""TV channel scripts: concat-list quoting, credential redaction, the restart
loop and, when ffmpeg is installed, a prepare-and-verify round trip.

The ffmpeg-free tests run with a fake ffmpeg on PATH, so they also run where
ffmpeg is absent.
"""

from pathlib import Path
import os
import json
import shutil
import signal
import subprocess
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[1]
PLAYOUT = ROOT / "scripts/tv-playout.sh"
PREPARE = ROOT / "scripts/tv-prepare.sh"

SECRET_URL = (
    "srt://media.example.net:8890?streamid=publish:live/event-tv:alice:hunter2pass"
    "&latency=2000&passphrase=correct%20horse%20staple"
)
SECRETS = ["alice", "hunter2pass", "correct%20horse%20staple", "correct horse staple"]

FAKE_FFMPEG = """#!/bin/sh
case "$*" in
  *-protocols*) echo "Output:"; echo "  srt"; exit 0 ;;
esac
echo "fake ffmpeg failing for: $*" >&2
exit 3
"""


def call(script, fn, *args, stdin=None):
    """Source SCRIPT (which does not run main when sourced) and call FN."""
    return subprocess.run(
        ["bash", "-c", 'source "$1"; shift; "$@"', "bash", str(script), fn, *args],
        input=stdin, capture_output=True, text=True, timeout=30,
    )


def have_ffmpeg(*encoders):
    if not (shutil.which("ffmpeg") and shutil.which("ffprobe")):
        return False
    out = subprocess.run(["ffmpeg", "-hide_banner", "-encoders"], capture_output=True, text=True).stdout
    return all(e in out for e in encoders)


class ConcatList(unittest.TestCase):
    def test_quotes_each_path_literally(self):
        cases = {
            "/media/plain.mp4": "file '/media/plain.mp4'",
            "/media/a b.mp4": "file '/media/a b.mp4'",
            "/media/it's.mp4": "file '/media/it'\\''s.mp4'",
            "/media/$HOME `x` \"q\" \\n.mp4": "file '/media/$HOME `x` \"q\" \\n.mp4'",
            "/media/''.mp4": "file '/media/'\\'''\\''.mp4'",
        }
        for path, want in cases.items():
            with self.subTest(path=path):
                got = call(PLAYOUT, "tv_concat_quote", path)
                self.assertEqual(got.returncode, 0, got.stderr)
                self.assertEqual(got.stdout, want + "\n")

    def test_list_has_one_line_per_path_in_order(self):
        got = call(PLAYOUT, "tv_concat_list", "/m/b.mp4", "/m/a.mp4")
        self.assertEqual(got.stdout, "file '/m/b.mp4'\nfile '/m/a.mp4'\n")

    def test_shuffle_keeps_every_line(self):
        lines = [f"/m/{n}.mp4" for n in range(40)]
        got = call(PLAYOUT, "shuffle_lines", stdin="\n".join(lines) + "\n")
        self.assertEqual(sorted(got.stdout.split("\n")[:-1]), sorted(lines))

    @unittest.skipUnless(have_ffmpeg("mpeg4"), "ffmpeg is not installed")
    def test_ffmpeg_reads_awkward_names_back(self):
        with tempfile.TemporaryDirectory() as tmp:
            names = ["a b.mp4", "it's.mp4", '$x `y` "z" \\w.mp4']
            paths = []
            for name in names:
                path = Path(tmp) / name
                subprocess.run(
                    ["ffmpeg", "-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i",
                     "color=c=red:s=64x64:r=10:d=0.5", "-c:v", "mpeg4", str(path)],
                    check=True, timeout=60,
                )
                paths.append(str(path))
            listing = call(PLAYOUT, "tv_concat_list", *paths).stdout
            (Path(tmp) / "list.txt").write_text(listing)
            probe = subprocess.run(
                ["ffprobe", "-v", "error", "-f", "concat", "-safe", "0", "-i", str(Path(tmp) / "list.txt"),
                 "-count_packets", "-show_entries", "stream=nb_read_packets", "-of", "json"],
                capture_output=True, text=True, timeout=60,
            )
            self.assertEqual(probe.returncode, 0, probe.stderr)
            self.assertEqual(int(json.loads(probe.stdout)["streams"][0]["nb_read_packets"]), 15)


class Redaction(unittest.TestCase):
    def redact(self, url):
        got = call(PLAYOUT, "tv_redact_url", url)
        self.assertEqual(got.returncode, 0, got.stderr)
        return got.stdout.strip()

    def test_srt_url_keeps_host_and_transport_settings_only(self):
        out = self.redact(SECRET_URL)
        self.assertEqual(out, "srt://media.example.net:8890?streamid=***&latency=2000&passphrase=***")
        for secret in SECRETS:
            self.assertNotIn(secret, out)

    def test_rtmp_query_and_key_are_hidden(self):
        out = self.redact("rtmp://host.example:1935/live/event-tv?user=alice&pass=hunter2pass")
        self.assertEqual(out, "rtmp://host.example:1935/live/***?user=***&pass=***")

    def test_userinfo_and_upper_case_keys(self):
        out = self.redact("rtmps://alice:hunter2pass@host.example/app/STREAMKEY123?Token=abc&PBKEYLEN=16")
        self.assertEqual(out, "rtmps://***@host.example/app/***?Token=***&PBKEYLEN=16")

    def test_url_without_a_query_is_unchanged(self):
        self.assertEqual(self.redact("srt://host.example:8890"), "srt://host.example:8890")

    def test_secret_list_has_raw_and_decoded_values(self):
        got = call(PLAYOUT, "tv_secrets", SECRET_URL).stdout.split("\n")
        for want in ("publish:live/event-tv:alice:hunter2pass", "correct%20horse%20staple", "correct horse staple"):
            self.assertIn(want, got)
        self.assertNotIn("2000", got)

    def test_individually_logged_stream_credentials_and_short_keys_are_hidden(self):
        secrets = call(PLAYOUT, "tv_secrets", SECRET_URL).stdout.splitlines()
        got = call(PLAYOUT, "tv_redact_stream", *secrets, stdin="username alice password hunter2pass\n")
        self.assertEqual(got.stdout, "username *** password ***\n")
        secrets = call(PLAYOUT, "tv_secrets", "rtmp://h.example/live/x").stdout.splitlines()
        self.assertIn("x", secrets)

    def test_userinfo_components_are_hidden_when_logged_separately(self):
        secrets = call(PLAYOUT, "tv_secrets", "rtmp://alice:smallpass@h.example/live/short").stdout.splitlines()
        got = call(PLAYOUT, "tv_redact_stream", *secrets, stdin="user alice password smallpass key short\n")
        self.assertEqual(got.stdout, "user *** password *** key ***\n")

    def test_stream_filter_replaces_every_occurrence(self):
        text = (
            "Connecting to srt://h:1?streamid=publish:live/event-tv:alice:hunter2pass&x=1\n"
            "wrong passphrase 'correct horse staple' (correct%20horse%20staple)\n"
            "nothing to hide here\n"
        )
        secrets = call(PLAYOUT, "tv_secrets", SECRET_URL).stdout.split("\n")
        got = call(PLAYOUT, "tv_redact_stream", *[s for s in secrets if s], stdin=text)
        self.assertEqual(got.returncode, 0, got.stderr)
        for secret in SECRETS:
            self.assertNotIn(secret, got.stdout)
        self.assertIn("nothing to hide here", got.stdout)
        self.assertEqual(got.stdout.count("***"), 3)


class Supervisor(unittest.TestCase):
    """The playout loop against a fake ffmpeg that always fails."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        bin_dir = Path(self.tmp.name) / "bin"
        bin_dir.mkdir()
        fake = bin_dir / "ffmpeg"
        fake.write_text(FAKE_FFMPEG)
        fake.chmod(0o755)
        self.media = Path(self.tmp.name) / "media"
        self.media.mkdir()
        (self.media / "one.mp4").write_bytes(b"")
        self.env = dict(os.environ, PATH=f"{bin_dir}{os.pathsep}{os.environ['PATH']}",
                        TV_BACKOFF_MIN="1", TV_BACKOFF_MAX="2", TMPDIR=self.tmp.name)

    def test_once_logs_why_and_exits_non_zero_without_credentials(self):
        for url in (SECRET_URL, "rtmp://h.example/live/event-tv?user=alice&pass=hunter2pass"):
            with self.subTest(url=url.split("?")[0]):
                got = subprocess.run(
                    ["bash", str(PLAYOUT), "--once", "--no-verify", str(self.media), url],
                    capture_output=True, text=True, env=self.env, timeout=30,
                )
                out = got.stdout + got.stderr
                self.assertEqual(got.returncode, 1, out)
                self.assertIn("fake ffmpeg failing for", out)
                self.assertIn("attempt 1", out)
                self.assertIn("***", out)
                for secret in SECRETS:
                    self.assertNotIn(secret, out)

    def test_url_can_come_from_the_environment(self):
        env = dict(self.env, TV_PUBLISH_URL=SECRET_URL)
        got = subprocess.run(["bash", str(PLAYOUT), "--once", "--no-verify", str(self.media)],
                             capture_output=True, text=True, env=env, timeout=30)
        self.assertEqual(got.returncode, 1, got.stderr)
        self.assertIn("publishing to srt://media.example.net:8890", got.stderr)

    def test_restarts_with_growing_delay_and_stops_on_sigterm(self):
        proc = subprocess.Popen(
            ["bash", str(PLAYOUT), "--no-verify", str(self.media), SECRET_URL],
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, env=self.env,
        )
        time.sleep(4.5)
        proc.send_signal(signal.SIGTERM)
        out, _ = proc.communicate(timeout=15)
        self.assertEqual(proc.returncode, 0, out)
        self.assertGreaterEqual(out.count("attempt "), 3, out)
        self.assertIn("restarting in 1s", out)
        self.assertIn("restarting in 2s", out)
        self.assertIn("stopping on signal", out)
        for secret in SECRETS:
            self.assertNotIn(secret, out)

    def test_refuses_an_empty_folder_and_a_bad_url(self):
        empty = Path(self.tmp.name) / "empty"
        empty.mkdir()
        got = subprocess.run(["bash", str(PLAYOUT), "--once", "--no-verify", str(empty), SECRET_URL],
                             capture_output=True, text=True, env=self.env, timeout=30)
        self.assertEqual(got.returncode, 1)
        self.assertIn("no .mp4 files", got.stderr)
        got = subprocess.run(["bash", str(PLAYOUT), "--once", str(self.media), "http://h.example/x"],
                             capture_output=True, text=True, env=self.env, timeout=30)
        self.assertEqual(got.returncode, 1)
        self.assertIn("must start with srt://", got.stderr)

    def test_invalid_backoff_is_refused_before_starting_ffmpeg(self):
        for value in ("0", "-1", "abc", "1+1", "999999"):
            got = subprocess.run(["bash", str(PLAYOUT), "--once", "--no-verify", str(self.media), SECRET_URL],
                                 capture_output=True, text=True, env=dict(self.env, TV_BACKOFF_MIN=value), timeout=10)
            self.assertNotEqual(got.returncode, 0)
            self.assertNotIn("attempt 1", got.stderr)

    def test_sigterm_stops_an_active_encoder_and_cleans_its_concat_list(self):
        fake = Path(self.tmp.name) / "bin/ffmpeg"
        marker = Path(self.tmp.name) / "encoder.pid"
        fake.write_text('#!/bin/sh\ncase "$*" in *"-h full"*) exit 0 ;; esac\necho $$ > "$TV_TEST_PID"\ntrap "exit 0" TERM INT\nwhile :; do sleep 1; done\n')
        env = dict(self.env, TV_TEST_PID=str(marker))
        proc = subprocess.Popen(["bash", str(PLAYOUT), "--no-verify", str(self.media), "rtmp://h.example/live/key"],
                                stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, env=env)
        try:
            deadline = time.monotonic() + 5
            while not marker.exists() and time.monotonic() < deadline:
                time.sleep(.05)
            self.assertTrue(marker.exists(), "fake encoder started")
            pid = int(marker.read_text())
            proc.terminate()
            output, _ = proc.communicate(timeout=10)
            self.assertEqual(proc.returncode, 0, output)
            deadline = time.monotonic() + 3
            while time.monotonic() < deadline:
                try: os.kill(pid, 0)
                except ProcessLookupError: break
                time.sleep(.05)
            else: self.fail("encoder left running after SIGTERM")
            self.assertFalse(list(Path(self.tmp.name).glob("tv-playout.*")))
        finally:
            if proc.poll() is None:
                proc.kill(); proc.communicate(timeout=5)


class CatchupOptions(unittest.TestCase):
    def test_new_ffmpeg_catches_up_and_older_builds_keep_compatible_input(self):
        self.assertEqual(call(PLAYOUT, 'tv_catchup_options', ' -readrate_catchup <speed>').stdout.splitlines(), ['-readrate_catchup', '2'])
        self.assertEqual(call(PLAYOUT, 'tv_catchup_options', ' -readrate <speed>').stdout, '')


class PrepareHelpers(unittest.TestCase):
    def test_bitrate_and_gop(self):
        for fn, arg, want in (
            ("to_kbit", "4500k", "4500"), ("to_kbit", "4.5M", "4500"), ("to_kbit", "5000000", "5000"),
            ("fps_gop", "25", "25"), ("fps_gop", "30000/1001", "30"), ("fps_gop", "24000/1001", "24"),
        ):
            with self.subTest(fn=fn, arg=arg):
                got = call(PREPARE, fn, arg)
                self.assertEqual(got.stdout.strip(), want, got.stderr)
        for fn, arg in (("to_kbit", "fast"), ("to_kbit", "0"), ("fps_gop", "0"),
                        ("fps_gop", "25/0"), ("fps_gop", "25junk"), ("fps_gop", "25/1junk")):
            with self.subTest(fn=fn, arg=arg):
                self.assertNotEqual(call(PREPARE, fn, arg).returncode, 0)


@unittest.skipUnless(have_ffmpeg("libx264", "aac"), "ffmpeg with libx264 is not installed")
class PrepareRoundTrip(unittest.TestCase):
    def clip(self, path, size, rate, sample_rate):
        subprocess.run(
            ["ffmpeg", "-hide_banner", "-loglevel", "error", "-y",
             "-f", "lavfi", "-i", f"testsrc2=size={size}:rate={rate}:duration=1.2",
             "-f", "lavfi", "-i", f"sine=frequency=440:sample_rate={sample_rate}:duration=1.2",
             "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p", "-c:a", "aac", str(path)],
            check=True, timeout=120,
        )

    def test_short_audio_is_padded_to_video_end(self):
        with tempfile.TemporaryDirectory() as tmp:
            src,out=Path(tmp)/'src',Path(tmp)/'out';src.mkdir()
            subprocess.run(['ffmpeg','-v','error','-f','lavfi','-i','color=c=red:s=320x180:r=25:d=3',
                            '-f','lavfi','-i','sine=r=48000:d=0.5','-c:v','libx264','-preset','ultrafast',
                            '-pix_fmt','yuv420p','-c:a','aac',str(src/'generated.mp4')],check=True,timeout=60)
            env=dict(os.environ,TV_ENCODER='libx264',TV_PRESET='ultrafast',TV_WIDTH='320',TV_HEIGHT='180')
            prepared=subprocess.run(['bash',str(PREPARE),str(src),str(out)],capture_output=True,text=True,env=env,timeout=60)
            self.assertEqual(prepared.returncode,0,prepared.stderr)
            probe=subprocess.run(['ffprobe','-v','error','-show_entries','stream=codec_type,duration','-of','json',str(out/'generated.mp4')],capture_output=True,text=True,check=True)
            durations={row['codec_type']:float(row['duration']) for row in json.loads(probe.stdout)['streams']}
            self.assertGreaterEqual(durations['audio'],durations['video']-.022)
            self.assertLessEqual(durations['audio'],durations['video']+.05)

    def test_mixed_sources_come_out_identical_and_reruns_skip(self):
        with tempfile.TemporaryDirectory() as tmp:
            src, out = Path(tmp) / "src", Path(tmp) / "out"
            src.mkdir()
            self.clip(src / "a.mp4", "320x240", 24, 44100)
            self.clip(src / "b c.mp4", "640x360", 30, 48000)
            env = dict(os.environ, TV_PRESET="ultrafast")
            first = subprocess.run(["bash", str(PREPARE), str(src), str(out)],
                                   capture_output=True, text=True, env=env, timeout=300)
            self.assertEqual(first.returncode, 0, first.stdout + first.stderr)
            self.assertIn("prepared 2, skipped 0", first.stdout)
            self.assertIn("verified: 2 file(s) share one format and identical SPS/PPS", first.stdout)
            probe = subprocess.run(
                ["ffprobe", "-v", "error", "-show_entries", "stream=width,height,r_frame_rate,sample_rate,profile,level",
                 "-of", "json", str(out / "a.mp4")], capture_output=True, text=True, check=True)
            streams = json.loads(probe.stdout)["streams"]
            self.assertEqual({key: streams[0][key] for key in ("profile", "level", "width", "height", "r_frame_rate")},
                             dict(profile="High", level=41, width=1920, height=1080, r_frame_rate="25/1"))
            self.assertEqual(streams[1]["sample_rate"], "48000")
            second = subprocess.run(["bash", str(PREPARE), str(src), str(out)],
                                    capture_output=True, text=True, env=env, timeout=300)
            self.assertEqual(second.returncode, 0, second.stdout + second.stderr)
            self.assertIn("prepared 0, skipped 2", second.stdout)

            # A file in another format fails the check, loudly.
            mixed = Path(tmp) / "mixed"
            mixed.mkdir()
            shutil.copy(out / "a.mp4", mixed / "a.mp4")
            shutil.copy(src / "b c.mp4", mixed / "raw.mp4")
            bad = subprocess.run(["bash", str(PREPARE), "--verify", str(mixed)],
                                 capture_output=True, text=True, timeout=120)
            self.assertEqual(bad.returncode, 1)
            self.assertIn("MISMATCH", bad.stderr)
            self.assertIn("video.sps_pps", bad.stderr)


if __name__ == "__main__":
    unittest.main()
