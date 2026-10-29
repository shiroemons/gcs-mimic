import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {connect} from 'node:net';
import {test} from 'node:test';
import {Storage} from '@google-cloud/storage';
import {Gaxios} from 'gaxios';
import {guardLocalConnections} from './local-http.mjs';

test('SDK transports accept loopback and reject external sockets', {timeout: 10000}, async () => {
  const server = createServer((request, response) => {
    request.resume();
    response.setHeader('Content-Type', 'application/json');
    response.end('{"items":[]}');
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const origin = new URL(`http://127.0.0.1:${server.address().port}`);
  guardLocalConnections(origin);
  for (const key of ['HTTP_PROXY', 'HTTPS_PROXY', 'http_proxy', 'https_proxy']) delete process.env[key];
  try {
    assert.throws(() => connect(443, 'storage.googleapis.com'), /request escaped emulator/);
    assert.throws(() => connect(Number(origin.port) + 1, '127.0.0.1'), /request escaped emulator/);
    const storage = new Storage({projectId: 'local-project', apiEndpoint: origin.origin, retryOptions: {autoRetry: false}});
    assert.deepEqual((await storage.getBuckets())[0], []);
    await assert.rejects(() => new Gaxios().request({url: 'http://storage.googleapis.com/upload', retry: false}),
      /request escaped emulator/);
  } finally {
    server.closeAllConnections();
    await new Promise(resolve => server.close(resolve));
  }
});
