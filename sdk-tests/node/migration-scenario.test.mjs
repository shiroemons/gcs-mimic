// The fake-gcs-server migration path: anonymous client, a bucket initialized by the buckets file,
// and ordinary save/exists/getMetadata/download/delete calls.
import assert from 'node:assert/strict';
import {test} from 'node:test';
import {createStorage, emulatorOrigin, guardSockets, scrubEnvironment} from './harness.mjs';

const origin = emulatorOrigin(process.env.GCS_MIMIC_ENDPOINT);
assert(!process.env.GCS_MIMIC_TOKEN, 'migration scenario requires an anonymous client');
scrubEnvironment();
guardSockets(origin);
const bucket = createStorage(origin).bucket('assets');

async function roundTrip(name, saveOptions) {
  const payload = 'Node migration bytes\n';
  const file = bucket.file(name);
  assert.equal((await file.exists())[0], false, 'migration object already exists');
  try {
    await file.save(payload, saveOptions);
    const [found] = await bucket.file(name).getMetadata();
    assert.equal(found.generation, file.metadata.generation);
    assert.equal(Number(found.size), Buffer.byteLength(payload));
    assert.equal(found.contentType, 'text/plain');
    assert.equal((await bucket.file(name).download())[0].toString(), payload);
    await file.delete();
    assert.equal((await bucket.file(name).exists())[0], false, 'deleted migration object still exists');
  } finally {
    await file.delete({ignoreNotFound: true});
  }
}

test('node-migration-scenario', async () => {
  assert.equal((await bucket.exists())[0], true, 'buckets file did not initialize assets');
  // resumable: false selects the SDK's multipart upload; the next case covers the default.
  await roundTrip('uploads/node-migration.txt', {resumable: false});
});

// File#save defaults to a single-request resumable upload whose PUT streams the whole body with
// `Content-Range: bytes 0-*/*` (unknown end and total).
test('node-migration-default-save', async () => {
  await roundTrip('uploads/node-migration-default.txt');
});
