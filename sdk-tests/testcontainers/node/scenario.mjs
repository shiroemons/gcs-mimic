import assert from 'node:assert/strict';
import {Socket} from 'node:net';
import {request} from 'node:http';
import {Storage} from '@google-cloud/storage';
import {GenericContainer, Wait} from 'testcontainers';

function localOrigin(raw) {
  const origin = new URL(raw);
  assert.equal(origin.protocol, 'http:');
  assert(['127.0.0.1', 'localhost', '[::1]'].includes(origin.hostname));
  assert(origin.port && !origin.username && !origin.password && !origin.search && !origin.hash);
  assert.equal(origin.pathname, '/');
  return origin;
}

function guardSockets(origin) {
  const connect = Socket.prototype.connect;
  Socket.prototype.connect = function(...args) {
    const first = Array.isArray(args[0]) ? args[0][0] : args[0];
    const options = typeof first === 'object' ? first : {port: first, host: args[1]};
    assert(!options.path, 'SDK attempted a Unix socket');
    assert.equal(options.host || options.hostname || 'localhost', origin.hostname.replace(/^\[|\]$/g, ''));
    assert.equal(Number(options.port), Number(origin.port));
    return connect.apply(this, args);
  };
  return () => { Socket.prototype.connect = connect; };
}

async function advertise(origin) {
  await new Promise((resolve, reject) => {
    const req = request(new URL('/_internal/config', origin), {method: 'PUT', headers: {'Content-Type': 'application/json'}}, response => {
      response.resume();
      response.on('end', () => response.statusCode === 200 ? resolve() : reject(new Error('endpoint advertisement failed')));
    });
    req.on('error', reject);
    req.end(JSON.stringify({externalUrl: origin.origin, publicHost: origin.host}));
  });
}

if (process.argv.includes('--self-check')) {
  assert.throws(() => localOrigin('https://storage.googleapis.com'));
  assert.throws(() => localOrigin('http://127.0.0.1:4443/path'));
  const restore = guardSockets(localOrigin('http://127.0.0.1:4443'));
  try { assert.throws(() => new Socket().connect({host: 'metadata.google.internal', port: 80})); }
  finally { restore(); }
  console.log('node-testcontainers-origin-guard: PASS');
} else {
  assert(process.env.GCS_MIMIC_IMAGE, 'set GCS_MIMIC_IMAGE to the exact image under test');
  const container = await new GenericContainer(process.env.GCS_MIMIC_IMAGE)
    .withEnvironment({GCS_MIMIC_SCHEME: 'http', GCS_MIMIC_ADMIN_ENABLED: 'true'})
    .withExposedPorts(4443).withWaitStrategy(Wait.forHttp('/_internal/healthcheck', 4443)).start();
  try {
    const origin = localOrigin(`http://${container.getHost()}:${container.getMappedPort(4443)}`);
    await advertise(origin);
    for (const key of ['HTTP_PROXY', 'HTTPS_PROXY', 'ALL_PROXY', 'http_proxy', 'https_proxy', 'all_proxy',
      'STORAGE_EMULATOR_HOST', 'GOOGLE_APPLICATION_CREDENTIALS']) delete process.env[key];
    process.env.METADATA_SERVER_DETECTION = 'none';
    const restore = guardSockets(origin);
    try {
      const storage = new Storage({projectId: 'local-project', apiEndpoint: origin.origin, retryOptions: {autoRetry: false}});
      const [bucket] = await storage.createBucket(`sdk-tc-node-${crypto.randomUUID()}`);
      try {
        const payload = Buffer.alloc(576 * 1024, 0x5a);
        const file = bucket.file('resumable/日本語 +%?.bin');
        await file.save(payload, {resumable: true, chunkSize: 256 * 1024, preconditionOpts: {ifGenerationMatch: 0}});
        assert.deepEqual((await file.download())[0], payload);
        assert(Number((await file.getMetadata())[0].generation) > 0);
        console.log('node-testcontainers: start, health, mapped Location, resumable, bytes PASS');
      } finally {
        await bucket.deleteFiles();
        await bucket.delete();
      }
    } finally { restore(); }
  } finally { await container.stop(); }
}
