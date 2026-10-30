// Verifies the hermetic guards without a server, mirroring the Ruby runner's --self-check.
import assert from 'node:assert/strict';
import {connect} from 'node:net';
import {connect as tlsConnect} from 'node:tls';
import {test} from 'node:test';
import {createStorage, emulatorOrigin, guardSockets, scrubEnvironment} from './harness.mjs';

test('origin parser accepts loopback HTTP and HTTPS origins', () => {
  assert.equal(emulatorOrigin('https://127.0.0.1:4443').port, '4443');
  assert.equal(emulatorOrigin('https://localhost:4443').hostname, 'localhost');
  assert.equal(emulatorOrigin('http://127.0.0.1:4443').protocol, 'http:');
  assert.equal(emulatorOrigin('http://[::1]:4443').hostname, '[::1]');
});

test('origin parser rejects external, metadata, non-HTTP and non-origin inputs', () => {
  for (const raw of ['https://storage.googleapis.com', 'http://storage.googleapis.com',
    'https://169.254.169.254', 'http://metadata.google.internal', 'ftp://127.0.0.1',
    'https://127.0.0.1.example.invalid', 'https://user@127.0.0.1', 'https://127.0.0.1/storage/v1',
    'https://127.0.0.1?cloud=1', 'not a url']) {
    assert.throws(() => emulatorOrigin(raw), TypeError, raw);
  }
});

test('socket guard rejects other hosts, ports, schemes and the metadata server', async () => {
  const origin = emulatorOrigin('http://127.0.0.1:4443');
  guardSockets(origin);
  scrubEnvironment();
  for (const [port, host] of [[443, 'storage.googleapis.com'], [80, '169.254.169.254'],
    [80, 'metadata.google.internal'], [4444, '127.0.0.1']]) {
    assert.throws(() => connect(port, host), /request escaped emulator/, `${host}:${port}`);
  }
  assert.throws(() => tlsConnect(4443, '127.0.0.1'), /request escaped emulator: scheme/);
  assert.throws(() => connect({path: '/var/run/socket'}), /Unix socket/);
  // The JSON transport (teeny-request) and the resumable transport (gaxios) both stop at the guard.
  // 192.0.2.1 is TEST-NET-1, so even an unguarded attempt could not reach a real service.
  for (const token of [undefined, 'local-admin-token']) {
    const elsewhere = createStorage(new URL('http://192.0.2.1:4443'), {token});
    await assert.rejects(() => elsewhere.getBuckets(), /request escaped emulator/);
    await assert.rejects(() => elsewhere.bucket('b').file('f').createResumableUpload(),
      /request escaped emulator/);
  }
});
