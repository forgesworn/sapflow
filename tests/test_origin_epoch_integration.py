"""Deterministic, offline harness tests. No network, publisher or device starts."""
import importlib.util
import contextlib
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("epoch_harness",Path(__file__).with_name("origin-epoch-integration.py"))
harness = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(harness)

BASE = '''#EXTM3U
#EXT-X-TARGETDURATION:2
#EXT-X-MEDIA-SEQUENCE:7
#EXT-X-DISCONTINUITY-SEQUENCE:0
#EXT-X-PART-INF:PART-TARGET=0.234
#EXT-X-MAP:URI="abc_init.mp4"
#EXT-X-PART:DURATION=0.200,URI="abc_part0.mp4",INDEPENDENT=YES
#EXTINF:1.000,
abc_seg7.mp4
#EXT-X-DISCONTINUITY
#EXT-X-MAP:URI="abc_init1.mp4"
#EXT-X-PART:DURATION=0.200,URI="abc_part1.mp4",INDEPENDENT=YES
#EXTINF:1.000,
abc_seg8.mp4
#EXT-X-PRELOAD-HINT:TYPE=PART,URI="abc_part2.mp4"
'''


def parse(text=BASE):
    return harness.parse_playlist(text.encode())


def box(kind,body):
    return (8+len(body)).to_bytes(4,"big")+kind+body


def file_prefix(version,duration,scale=1000):
    if version == 0:
        body=bytes(12)+scale.to_bytes(4,"big")+duration.to_bytes(4,"big")
    else:
        body=b"\x01"+bytes(19)+scale.to_bytes(4,"big")+duration.to_bytes(8,"big")
    return box(b"ftyp",b"isom0000")+box(b"moov",box(b"mvhd",body))


class PlaylistTests(unittest.TestCase):
    def test_segment_uses_its_own_map_not_first_map(self):
        result=parse()
        self.assertEqual([(s["msn"],s["map"],s["dseq"]) for s in result["segments"]],
                         [(7,"abc_init.mp4",0),(8,"abc_init1.mp4",1)])
        self.assertEqual(result["parts"][-1]["map"],"abc_init1.mp4")

    def test_gap_and_open_parts_and_nonbinding_map_hint(self):
        text=BASE.replace('#EXTINF:1.000,\nabc_seg7.mp4','#EXTINF:1.000,\nabc_seg7.mp4')
        text=text.replace('#EXT-X-PRELOAD-HINT:TYPE=PART', '#EXT-X-PRELOAD-HINT:TYPE=MAP,URI="abc_init2.mp4"\n#EXT-X-PRELOAD-HINT:TYPE=PART')
        result=parse(text)
        self.assertEqual(result["segments"][-1]["map"],"abc_init1.mp4")
        gap=BASE.replace('#EXT-X-PART:DURATION=0.200,URI="abc_part0.mp4",INDEPENDENT=YES\n#EXTINF:1.000,\nabc_seg7.mp4',
                         '#EXT-X-GAP\n#EXTINF:1.000,\ngap.mp4')
        self.assertTrue(parse(gap)["segments"][0]["gap"])

    def test_refuses_external_or_traversal_resources_and_wrong_resource_roles(self):
        for value in ('https://other.invalid/abc_seg7.mp4','../abc_seg7.mp4','/abc_seg7.mp4',
                      'abc_seg7.mp4?token=secret','abc_seg'+('1'*30)+'.mp4','abc_init1.mp4'):
            with self.subTest(value=value),self.assertRaises(harness.Failure):
                parse(BASE.replace('abc_seg7.mp4',value))
        with self.assertRaises(harness.Failure):
            parse(BASE.replace('abc_part0.mp4','abc_init.mp4'))

    def test_refuses_bounds_bad_numbers_missing_map_and_mid_part_discontinuity(self):
        invalid=[b'x'*(harness.MAX_PLAYLIST+1),BASE.replace('0.234','NaN').encode(),
                 BASE.replace('TARGETDURATION:2','TARGETDURATION:0').encode(),
                 BASE.replace('DURATION=0.200','DURATION=0.300').encode(),
                 BASE.replace('#EXT-X-MAP:URI="abc_init.mp4"\n','').encode(),
                 BASE.replace('#EXTINF:1.000,','#EXT-X-DISCONTINUITY\n#EXTINF:1.000,',1).encode()]
        for value in invalid:
            with self.assertRaises(harness.Failure):harness.parse_playlist(value)

    def test_history_detects_exact_observed_sequence_uri_map_and_dseq_failures(self):
        for modified in (BASE.replace('abc_seg7.mp4','def_seg7.mp4'),
                         BASE.replace('abc_init.mp4','def_init.mp4'),
                         BASE.replace('DISCONTINUITY-SEQUENCE:0','DISCONTINUITY-SEQUENCE:1'),
                         BASE.replace('MEDIA-SEQUENCE:7','MEDIA-SEQUENCE:0'),
                         BASE.replace('TARGETDURATION:2','TARGETDURATION:3')):
            history=harness.History();history.accept(parse())
            with self.assertRaises(harness.Failure):history.accept(parse(modified))

    def test_published_part_identity_is_immutable_and_new_map_needs_discontinuity(self):
        history=harness.History();history.accept(parse())
        with self.assertRaises(harness.Failure):
            history.accept(parse(BASE.replace('abc_part1.mp4','abc_part0.mp4')))
        with self.assertRaises(harness.Failure):
            harness.History().accept(parse(BASE.replace('#EXT-X-DISCONTINUITY\n','')))


class CommandAndFileTests(unittest.TestCase):
    def test_publisher_uses_fresh_bounded_copy_command_without_shell_interpolation(self):
        command=harness.publisher_command('ffmpeg',Path('fixture.ts'),'rtmps://origin.invalid/live',
                                          'event-epoch-case','user value','secret&/? value')
        self.assertIsInstance(command,list)
        self.assertEqual(command[command.index('-t')+1],'12')
        self.assertEqual(command[command.index('-c')+1],'copy')
        self.assertEqual(command[command.index('-map')+1],'0:v:0')
        self.assertIn('0:a:0',command)
        self.assertIn('pass=secret%26%2F%3F+value',command[-1])
        self.assertNotIn('-y',command)
        self.assertNotIn('shell',command)

    def test_only_dedicated_events_and_explicit_safe_bases_are_admitted(self):
        for event in ('event-mobile-live','event-epoch-../oops','event-epoch-a?pass=x','event-epoch-'):
            with self.assertRaises(harness.Failure):
                harness.publisher_command('ffmpeg',Path('a'),'rtmp://origin.invalid/live',event,'u','p')
        for base in ('rtmp://u:p@origin.invalid/live','rtmp://origin.invalid/live?x=1',
                     'http://origin.invalid/live','rtmp://origin.invalid:bad/live','rtmp://origin.invalid/\nx'):
            with self.assertRaises(harness.Failure):harness.validate_base(base,('rtmp','rtmps'))

    def test_redaction_removes_raw_encoded_credentials_and_echoed_urls(self):
        secret='secret&/? value'
        text=f'rtmp://origin.invalid/live/event?pass={harness.urllib.parse.quote_plus(secret)}\n{secret}'
        clean=harness.redact(text,[secret])
        self.assertNotIn(secret,clean)
        self.assertNotIn('secret%26',clean)
        self.assertNotIn('origin.invalid',clean)

    def test_recording_duration_versions_zero_open_and_bad_bounds(self):
        for version in (0,1):
            self.assertEqual(harness.final_file_duration(file_prefix(version,12000)),12)
            self.assertEqual(harness.final_file_duration(file_prefix(version,0)),0)
        for data in (b'',b'bad',file_prefix(0,1,0),b'\x00\x00\x00\x01moov'):
            with self.assertRaises(harness.Failure):harness.final_file_duration(data)

    def test_recording_prefix_reads_only_init_and_tolerates_incomplete_writer(self):
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/'own.mp4'
            path.write_bytes(b'')
            self.assertEqual(harness.recording_prefix(path),b'')
            prefix=file_prefix(1,12000)
            path.write_bytes(prefix+b'not read mdat or a large payload')
            self.assertEqual(harness.recording_prefix(path),prefix)
            path.write_bytes(prefix[:-2])
            self.assertEqual(harness.recording_prefix(path),b'')


class TimingControlTests(unittest.TestCase):
    def arguments(self):
        return ['--http-base','https://origin.invalid','--rtmp-base','rtmps://origin.invalid/live',
                '--event','event-epoch-timing','--publish-user-env','OWN_USER','--publish-pass-env','OWN_PASS',
                '--recording-root','unused-recordings','--origin-binary','unused-binary',
                '--expected-origin-sha256','a'*64,'--source-manifest','unused-manifest',
                '--expected-manifest-sha256','b'*64,'--output','unused-output']

    def test_defaults_and_bounded_integer_arguments(self):
        args=harness.parse_arguments(self.arguments())
        self.assertEqual((args.stage_seconds,args.reconnect_gap_seconds),(12,0))
        for option,values in (('--stage-seconds',['11','31','12.5','bad']),
                              ('--reconnect-gap-seconds',['-1','4','1.5','bad'])):
            for value in values:
                with self.subTest(option=option,value=value),contextlib.redirect_stderr(io.StringIO()),self.assertRaises(SystemExit):
                    harness.parse_arguments(self.arguments()+[option,value])

    def test_whole_schedule_counts_all_nine_stages_eight_gaps_and_reserve(self):
        self.assertEqual(harness.timing_plan(1,12,0,100),196)
        self.assertEqual(harness.timing_plan(3,12,0,100),268)
        self.assertEqual(harness.timing_plan(1,30,2,100),254)
        self.assertEqual(harness.timing_plan(2,20,2,100),290)
        self.assertEqual(harness.timing_plan(3,20,2,44),300)
        for values in ((3,20,2,45),(3,30,0,1),(3,12,3,120),(1,11,0,100),(1,12,4,100),(True,12,0,100)):
            with self.subTest(values=values),self.assertRaises(harness.Failure):
                harness.timing_plan(*values)
        with patch.object(harness,'file_sha') as hashed,self.assertRaises(harness.Failure):
            harness.main(self.arguments()+['--cycles','3','--stage-seconds','30'])
        hashed.assert_not_called() # Reject before reading pins, creating output or networking.

    def test_publisher_duration_and_process_deadline_follow_requested_stage_and_global_limit(self):
        with patch.object(harness.time,'monotonic',return_value=100),patch.object(harness,'Process') as process:
            harness.start_publisher(Path('fixture.ts'),'rtmp://origin.invalid/live','event-epoch-timing','u','p',30,200)
            command,seconds=process.call_args.args
            self.assertEqual(command[command.index('-t')+1],'30')
            self.assertEqual(seconds,38)
            process.reset_mock()
            harness.start_publisher(Path('fixture.ts'),'rtmp://origin.invalid/live','event-epoch-timing','u','p',12,110)
            self.assertEqual(process.call_args.args[1],10)
            process.reset_mock()
            with self.assertRaises(harness.Failure):
                harness.start_publisher(Path('fixture.ts'),'rtmp://origin.invalid/live','event-epoch-timing','u','p',12,100)
            process.assert_not_called()

    def test_gaps_only_between_completed_stages_and_never_consume_deadline_silently(self):
        with patch.object(harness.time,'sleep') as sleep,patch.object(harness.time,'monotonic',return_value=100) as now:
            harness.reconnect_pause(0,3,200)
            harness.reconnect_pause(1,0,200)
            sleep.assert_not_called();now.assert_not_called()
            for stage in range(9):harness.reconnect_pause(stage,2,200)
            self.assertEqual(sleep.call_count,8)
            self.assertTrue(all(call.args==(2,) for call in sleep.call_args_list))
        with patch.object(harness.time,'sleep') as sleep,patch.object(harness.time,'monotonic',return_value=100):
            with self.assertRaises(harness.Failure):harness.reconnect_pause(1,2,102)
            sleep.assert_not_called()
        with patch.object(harness.time,'sleep'),patch.object(harness.time,'monotonic',side_effect=[100,103]):
            with self.assertRaises(harness.Failure):harness.reconnect_pause(1,2,102.5)

    def test_receipt_retains_timing_and_utc_stage_events_without_network_or_publishers(self):
        from datetime import datetime, timedelta
        from types import SimpleNamespace
        created=[]
        class FakeProcess:
            def __init__(self,command,seconds):
                self.started=harness.time.monotonic()-21
                self.child=SimpleNamespace(pid=123,returncode=0,poll=lambda:0)
                self.buffers=[b'',b''];self.overflow=harness.threading.Event()
                self.command=command;self.seconds=seconds;self.closed=False;created.append(self)
            def check_deadline(self):pass
            def close(self):self.closed=True
        def fetch_stub(fetch,url,limit):
            if url.endswith('index.m3u8'):
                raise harness.urllib.error.HTTPError(url,404,'reserved event absent',None,None)
            data=BASE.encode() if url.endswith('stream.m3u8') else url.rsplit('/',1)[-1].encode()
            fetch.bytes+=len(data)
            return data
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory).resolve()
            fixtures=root/'crates/sapflow-hls/tests/fixtures';fixtures.mkdir(parents=True)
            manifest={}
            for pose in ('portrait','landscape'):
                path=fixtures/f'epoch-{pose}.ts';path.write_bytes(pose.encode())
                manifest[str(path.relative_to(root))]=harness.file_sha(path)
            source=root/'source.json';source.write_text(json.dumps(manifest))
            binary=root/'origin';binary.write_bytes(b'bounded offline test')
            output=root/'output'
            args=self.arguments()+['--recording-root',str(root/'recordings'),'--origin-binary',str(binary),
                 '--expected-origin-sha256',harness.file_sha(binary),'--source-manifest',str(source),
                 '--expected-manifest-sha256',harness.file_sha(source),'--output',str(output),
                 '--cycles','2','--stage-seconds','20','--reconnect-gap-seconds','2']
            with patch.object(harness,'ROOT',root),patch.dict(harness.os.environ,OWN_USER='offline-user',OWN_PASS='offline-password'), \
                 patch.object(harness.shutil,'which',return_value='/unused'), \
                 patch.object(harness.Fetcher,'get',new=fetch_stub),patch.object(harness.time,'sleep') as sleep, \
                 patch.object(harness,'Process',FakeProcess),patch.object(harness,'probe_and_decode') as decode, \
                 patch.object(harness.signal,'signal'), \
                 patch.object(harness.signal,'setitimer') as alarm,contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(harness.main(args),1)
            decode.assert_not_called() # Synthetic playlists deliberately expose only two epochs, then stop.
            self.assertEqual(len(created),6)
            self.assertTrue(all(p.closed and p.seconds==28 for p in created))
            self.assertEqual(sleep.call_count,5)
            self.assertGreater(alarm.call_args_list[0].args[1],299)
            self.assertLessEqual(alarm.call_args_list[0].args[1],300)
            receipt=json.loads((output/'receipt.local.json').read_text())
            self.assertEqual((receipt['stage_seconds'],receipt['reconnect_gap_seconds']),(20,2))
            self.assertEqual(receipt['planned_seconds'],290)
            self.assertEqual(receipt['decode_cleanup_reserve_seconds'],60)
            self.assertEqual(receipt['deadline_seconds'],300)
            self.assertEqual(receipt['source_hashes'],manifest)
            self.assertTrue(receipt['owned_scratch_removed'])
            self.assertNotIn('offline-password',json.dumps(receipt))
            self.assertEqual([p['gap_before_seconds'] for p in receipt['publishers']],[0,2,2,2,2,2])
            for item,process in zip(receipt['publishers'],created):
                self.assertEqual(process.command[process.command.index('-t')+1],'20')
                self.assertEqual(item['requested_seconds'],20)
                self.assertEqual(item['process_deadline_seconds'],28)
                self.assertGreaterEqual(item['elapsed_seconds'],21)
                started=datetime.fromisoformat(item['started_utc'])
                completed=datetime.fromisoformat(item['completed_utc'])
                self.assertEqual(started.utcoffset(),timedelta(0))
                self.assertEqual(completed.utcoffset(),timedelta(0))
                self.assertLessEqual(started,completed)


class ResourceBudgetTests(unittest.TestCase):
    def test_http_reader_asks_for_limit_plus_one_and_rejects_overflow_without_network(self):
        class Response:
            headers={}
            def __init__(self,data):self.data=data;self.requested=None
            def __enter__(self):return self
            def __exit__(self,*args):pass
            def read(self,limit):self.requested=limit;return self.data[:limit]
        class Opener:
            def __init__(self,response):self.response=response
            def open(self,*args,**kwargs):return self.response
        response=Response(b'x'*20)
        fetch=harness.Fetcher(harness.time.monotonic()+10);fetch.opener=Opener(response)
        with self.assertRaises(harness.Failure):fetch.get('https://origin.invalid/own',10)
        self.assertEqual(response.requested,11)
        self.assertEqual(fetch.bytes,0)
        response=Response(b'ok')
        fetch.opener=Opener(response)
        self.assertEqual(fetch.get('https://origin.invalid/own',10),b'ok')
        self.assertEqual(fetch.bytes,2)
        fetch.deadline=0
        with self.assertRaises(harness.Failure):fetch.get('https://origin.invalid/own',10)

    def test_record_copy_is_bounded_create_new_and_never_mutates_source(self):
        with tempfile.TemporaryDirectory() as directory:
            source=Path(directory)/'source.mp4';destination=Path(directory)/'copy.mp4'
            source.write_bytes(b'fixture bytes')
            harness.copy_recording(source,destination)
            self.assertEqual(destination.read_bytes(),source.read_bytes())
            with self.assertRaises(FileExistsError):harness.copy_recording(source,destination)
            original=harness.MAX_WIRE
            try:
                harness.MAX_WIRE=4
                with self.assertRaises(harness.Failure):harness.copy_recording(source,Path(directory)/'overflow.mp4')
            finally:harness.MAX_WIRE=original
            self.assertEqual(source.read_bytes(),b'fixture bytes')

    def test_recording_listing_is_count_bounded_numeric_and_refuses_symlinks(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            for name in ('9.mp4','10.mp4'):(root/name).write_bytes(b'own')
            self.assertEqual([p.name for p in harness.recording_files(root,2)],['9.mp4','10.mp4'])
            with self.assertRaises(harness.Failure):harness.recording_files(root,1)
            (root/'11.mp4').symlink_to(root/'10.mp4')
            with self.assertRaises(harness.Failure):harness.recording_files(root,3)

    def test_seeded_bounded_parser_fuzz_smoke(self):
        import random
        randomizer=random.Random(8216)
        for _ in range(500):
            data=bytes(randomizer.randrange(256) for _ in range(randomizer.randrange(512)))
            try:harness.parse_playlist(data)
            except harness.Failure:pass
        for line in ('#EXT-X-MAP:', '#EXT-X-PART:', '#EXT-X-PART-INF:', '#EXTINF:NaN,'):
            with self.assertRaises(harness.Failure):parse(BASE.replace('#EXT-X-MAP:URI="abc_init.mp4"',line))

    def test_empty_movie_header_and_duplicate_part_target_fail_closed(self):
        with self.assertRaises(harness.Failure):
            harness.final_file_duration(box(b'moov',box(b'mvhd',b'')))
        with self.assertRaises(harness.Failure):
            parse(BASE.replace('#EXT-X-PART-INF:PART-TARGET=0.234',
                               '#EXT-X-PART-INF:PART-TARGET=0.234\n#EXT-X-PART-INF:PART-TARGET=0.200'))

    def test_probe_requires_both_exact_dimensions_and_two_tracks(self):
        import json
        metadata={'streams':[{'codec_type':'video','codec_name':'h264','width':720,'height':720},
                             {'codec_type':'audio','codec_name':'aac','sample_rate':'48000'}]}
        with patch.object(harness,'capture',return_value=json.dumps(metadata).encode()) as capture:
            with self.assertRaises(harness.Failure):
                harness.probe_and_decode(Path('unused.mp4'),720,100,[])
            self.assertEqual(capture.call_count,1)
        metadata['streams'][0]['height']=1280
        with patch.object(harness,'capture',side_effect=[json.dumps(metadata).encode(),b'']) as capture:
            self.assertEqual(harness.probe_and_decode(Path('unused.mp4'),720,100,[]),metadata)
            self.assertEqual(capture.call_count,2)
        metadata['streams'].append(metadata['streams'][1])
        with patch.object(harness,'capture',return_value=json.dumps(metadata).encode()):
            with self.assertRaises(harness.Failure):
                harness.probe_and_decode(Path('unused.mp4'),720,100,[])


class RetentionTests(unittest.TestCase):
    def test_only_new_epoch_rereads_latest_previous_segment_not_first_playlist_map(self):
        order=harness.EpochRetention()
        old,new=parse()['segments']
        self.assertIsNone(order.observe(old))
        latest=dict(old,msn=12,uri='abc_seg12.mp4')
        self.assertIsNone(order.observe(latest))
        self.assertIsNone(order.observe(old))
        self.assertEqual(order.observe(new),latest)
        self.assertIsNone(order.observe(old))
        self.assertIsNone(order.observe(new))
        class Fetch:
            def __init__(self,resources):self.resources=resources;self.requests=[]
            def get(self,url,limit):
                self.requests.append((url,limit))
                return self.resources[url.rsplit('/',1)[-1]]
        resources={'abc_init.mp4':b'old init','abc_seg12.mp4':b'old segment'}
        fetch=Fetch(resources)
        result=harness.verify_retained(fetch,'https://origin.invalid/own/',resources,latest,new['map'])
        self.assertEqual([url.rsplit('/',1)[-1] for url,_ in fetch.requests],list(resources))
        self.assertTrue(result['unchanged_http_200'])
        self.assertEqual(result['old_segment'],'abc_seg12.mp4')
        bad=Fetch(dict(resources,**{'abc_init.mp4':b'changed'}))
        with self.assertRaises(harness.Failure):
            harness.verify_retained(bad,'https://origin.invalid/own/',resources,latest,new['map'])


if __name__=='__main__':unittest.main()
