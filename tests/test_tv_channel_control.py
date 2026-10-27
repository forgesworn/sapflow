"""Synthetic lifecycle credentials only; no real network, tokens or media."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock
sys.path.insert(0,str(Path(__file__).resolve().parents[1]/'scripts'))
from tv_channel_control import ChannelControl

class Lifecycle(unittest.TestCase):
 def setUp(self):
  self.directory=tempfile.TemporaryDirectory();self.addCleanup(self.directory.cleanup)
  root=Path(self.directory.name);self.token=root/'token';self.token.write_text('a'*64);self.token.chmod(0o600)
  self.config=root/'control.json';self.config.write_text(json.dumps({'origin':'https://receiver.example','token_file':str(self.token)}));self.config.chmod(0o600)
 def test_no_configuration_means_no_network(self):
  c=ChannelControl()
  with mock.patch('tv_channel_control.subprocess.run') as run:c.start();self.assertFalse(c.end());run.assert_not_called()
 def test_notices_require_tor_remote_dns_https_and_secret_stdin(self):
  c=ChannelControl(self.config,9150)
  with mock.patch('tv_channel_control.subprocess.run',return_value=subprocess.CompletedProcess([],0,'200','')) as run:
   c.start();self.assertTrue(c.end());self.assertEqual(run.call_count,2)
  first,last=run.call_args_list
  for call in [first,last]:
   args=call.args[0];self.assertIn('socks5h://127.0.0.1:9150',args);self.assertEqual(args[args.index('--noproxy')+1],'');self.assertEqual(args[args.index('--proto')+1],'=https');self.assertNotIn('--location',args);self.assertNotIn('a'*64,' '.join(args));self.assertIn('Authorization: Bearer '+'a'*64,call.kwargs['input'])
  messages=[json.loads(call.args[0][call.args[0].index('--data-binary')+1]) for call in [first,last]]
  self.assertEqual([m['action'] for m in messages],['start','end']);self.assertEqual(messages[0]['session'],messages[1]['session']);self.assertEqual(messages[0]['startedAt'],messages[1]['startedAt'])
 def test_failed_start_response_still_attempts_scoped_end_without_direct_retry(self):
  c=ChannelControl(self.config)
  with mock.patch('tv_channel_control.subprocess.run',side_effect=[subprocess.TimeoutExpired('curl',12),subprocess.CompletedProcess([],0,'200','')]) as run:
   c.start();self.assertFalse(c.registered);self.assertTrue(c.end());self.assertEqual(run.call_count,2)
 def test_private_files_and_exact_https_origin_are_required(self):
  self.token.chmod(0o644)
  with self.assertRaises(ValueError):ChannelControl(self.config)
  self.token.chmod(0o600)
  for origin in ['http://receiver.example','https://user:pass@receiver.example','https://receiver.example/path','https://receiver.example?x=1']:
   self.config.write_text(json.dumps({'origin':origin,'token_file':str(self.token)}))
   with self.assertRaises(ValueError):ChannelControl(self.config)

if __name__=='__main__':unittest.main()
