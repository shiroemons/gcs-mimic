// Official @google-cloud/storage journeys against one isolated gcs-mimic origin. The permissive run
// is anonymous; the strict run sets GCS_MIMIC_TOKEN (and GCS_MIMIC_SIGNED_TESTS=1 for signed URLs).
import assert from 'node:assert/strict';
import {createHash, randomBytes, verify} from 'node:crypto';
import {createReadStream, existsSync, mkdtempSync, rmSync, writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {rootCertificates} from 'node:tls';
import {after, before, test} from 'node:test';
import {fileURLToPath} from 'node:url';
import {createSigner, createStorage, emulatorOrigin, guardSockets, observeHttp, projectId, rawRequest,
  scrubEnvironment, signerEmail, signingPublicKey} from './harness.mjs';
import {replaceLifecycleRules, signV4WithCli, uploadWithLocalAuth} from './migration-adapters.mjs';

const origin = emulatorOrigin(process.env.GCS_MIMIC_ENDPOINT);
if (origin.protocol === 'https:') {
  const ca = process.env.GCS_MIMIC_CA_CERT;
  assert(ca && existsSync(ca), 'HTTPS requires a readable GCS_MIMIC_CA_CERT');
  assert.equal(process.env.NODE_EXTRA_CA_CERTS, ca, 'run.sh trusts the CA through NODE_EXTRA_CA_CERTS');
}
scrubEnvironment();
guardSockets(origin);
const observed = observeHttp(origin);
const token = process.env.GCS_MIMIC_TOKEN;
const strict = Boolean(token);
const storage = createStorage(origin, {token});
const unique = purpose => `sdk-node-${purpose}-${randomBytes(6).toString('hex')}`;
const metadata = async resource => (await resource.getMetadata())[0];
const bytes = async (file, options) => (await file.download(options))[0];
const rejectsWith = (code, call, reason) => assert.rejects(call, error => {
  assert.equal(error.code, code, error.message);
  if (reason) assert.equal(error.errors?.[0]?.reason, reason, error.message);
  return true;
});

// Best-effort cleanup that must not hide the original failure.
async function removeBucket(bucket) {
  if (!bucket) return;
  try {
    const [files] = await bucket.getFiles({versions: true});
    for (const file of files) {
      const meta = file.metadata;
      if (meta.temporaryHold || meta.eventBasedHold) await file.setMetadata({temporaryHold: false, eventBasedHold: false});
      await file.delete({ignoreNotFound: true});
    }
    await bucket.delete();
  } catch (error) {
    console.error(`cleanup of ${bucket.name} failed: ${error.message}`);
  }
}
async function withBucket(purpose, options, run) {
  const [bucket] = await storage.createBucket(unique(purpose), options);
  try {
    await run(bucket);
  } finally {
    await removeBucket(bucket);
  }
}

// Shared fixture: a 576 KiB disk file of repeated 0..255 bytes, streamed in three resumable chunks.
const chunk = 256 * 1024;
const work = mkdtempSync(join(tmpdir(), 'gcs-mimic-node-'));
const source = join(work, 'source.bin');
const sourceBytes = Buffer.alloc(9 * 256 * 256);
for (let i = 0; i < sourceBytes.length; i++) sourceBytes[i] = i % 256;
writeFileSync(source, sourceBytes);
const sha256File = path => new Promise((resolve, reject) => {
  const hash = createHash('sha256');
  createReadStream(path, {highWaterMark: 64 * 1024}).on('data', data => hash.update(data))
    .on('end', () => resolve(hash.digest('hex'))).on('error', reject);
});
const expectedDigest = createHash('sha256').update(sourceBytes).digest('hex');
let bucket;
let object;

before(async () => {
  [bucket] = await storage.createBucket(unique('basic'));
});
after(async () => {
  await removeBucket(bucket);
  rmSync(work, {recursive: true, force: true});
  assert.deepEqual(observed.violations, [], 'every SDK request used the emulator scheme');
});

test('node-bucket-patch-preserves-unrelated-settings', async () => {
  await withBucket('patch', {versioning: {enabled: true}, labels: {keep: 'yes'}}, async b => {
    await b.setMetadata({iamConfiguration: {publicAccessPrevention: 'enforced'}});
    const meta = await metadata(b);
    assert.equal(meta.versioning?.enabled, true, 'PATCH lost versioning');
    assert.deepEqual(meta.labels, {keep: 'yes'}, 'PATCH lost labels');
    assert.equal(meta.iamConfiguration.publicAccessPrevention, 'enforced');
  });
});

test('node-default-create-bucket', async () => {
  const meta = await metadata(bucket);
  assert.equal(meta.name, bucket.name);
  assert.equal(meta.storageClass, 'STANDARD');
});

test('node-regional-location', async () => {
  await withBucket('region', {location: 'ASIA-NORTHEAST1'}, async b => {
    assert.equal(b.metadata.location, 'ASIA-NORTHEAST1');
    assert.equal(b.metadata.locationType, 'region');
    const meta = await metadata(b);
    assert.equal(meta.location, 'ASIA-NORTHEAST1');
    assert.equal(meta.locationType, 'region');
  });
});

test('node-unsupported-fails-fast', async () => {
  // The SDK's default retry policy must not turn an unsupported query into a slow failure.
  const retrying = createStorage(origin, {token, retries: true});
  const started = performance.now();
  await rejectsWith(400, () => retrying.bucket(bucket.name).getFiles({delimiter: '/', includeFoldersAsPrefixes: true}),
    'badRequest');
  assert(performance.now() - started < 3000, 'unsupported call waited for retries');
  await assert.rejects(() => retrying.bucket(bucket.name).getFiles({delimiter: '/', includeFoldersAsPrefixes: true}),
    /gcs-mimic/, 'unsupported call lacks actionable diagnostics');
});

test('node-bucket-notifications', async () => {
  const [notification] = await bucket.createNotification(`projects/${projectId}/topics/uploads`, {
    eventTypes: ['OBJECT_FINALIZE', 'OBJECT_DELETE'], objectNamePrefix: 'logs/',
    customAttributes: {team: 'storage'}, payloadFormat: 'NONE'});
  const meta = await metadata(bucket.notification(notification.id));
  assert.equal(meta.topic, `//pubsub.googleapis.com/projects/${projectId}/topics/uploads`);
  assert.deepEqual(meta.event_types, ['OBJECT_FINALIZE', 'OBJECT_DELETE']);
  assert.equal(meta.object_name_prefix, 'logs/');
  assert.deepEqual(meta.custom_attributes, {team: 'storage'});
  assert.equal(meta.payload_format, 'NONE');
  assert.deepEqual((await bucket.getNotifications())[0].map(n => n.id), [notification.id]);
  await notification.delete();
  assert.deepEqual((await bucket.getNotifications())[0], []);
});

test('node-json-acl', async () => {
  // ACL entries are stored and returned; this journey does not claim they grant access.
  const has = (entries, entity) => entries.some(e => e.entity === entity && e.role === 'READER');
  await bucket.acl.add({entity: 'allAuthenticatedUsers', role: 'READER'});
  assert(has((await bucket.acl.get())[0], 'allAuthenticatedUsers'), 'bucket ACL reader missing');
  await bucket.acl.default.add({entity: 'allAuthenticatedUsers', role: 'READER'});
  assert(has((await bucket.acl.default.get())[0], 'allAuthenticatedUsers'), 'default ACL reader missing');
  const file = bucket.file('acl.txt');
  await file.save('x', {resumable: false});
  assert(has((await file.acl.get())[0], 'allAuthenticatedUsers'), 'default ACL not inherited');
  await file.acl.add({entity: 'allUsers', role: 'READER'});
  assert(has((await file.acl.get())[0], 'allUsers'), 'object ACL reader missing');
  await file.acl.delete({entity: 'allUsers'});
  assert(!has((await file.acl.get())[0], 'allUsers'), 'object ACL entry not deleted');
  await file.delete();
  await bucket.acl.default.delete({entity: 'allAuthenticatedUsers'});
  await bucket.acl.delete({entity: 'allAuthenticatedUsers'});
});

test('node-bucket-autoclass', async () => {
  await withBucket('autoclass', {autoclass: {enabled: true}}, async b => {
    const meta = await metadata(b);
    assert.equal(meta.autoclass?.enabled, true);
    assert(meta.autoclass.toggleTime, 'autoclass toggle time missing');
    assert.equal(meta.autoclass.terminalStorageClass, 'NEARLINE');
    const file = b.file('cold');
    await file.save('x', {resumable: false, metadata: {storageClass: 'COLDLINE'}});
    assert.equal((await metadata(file)).storageClass, 'STANDARD');
  });
});

test('node-project-service-account', async () => {
  const [account] = await storage.getServiceAccount();
  assert.match(account.emailAddress, /@gs-project-accounts\.iam\.gserviceaccount\.com$/);
});

test('node-project-hmac-keys', async () => {
  const email = `sdk-node-${randomBytes(8).toString('hex')}@${projectId}.iam.gserviceaccount.com`;
  if (strict) {
    // roles/storage.admin does not include storage.hmacKeys.*; strict mode must enforce that.
    await rejectsWith(403, () => storage.createHmacKey(email));
    return;
  }
  const [key, secret] = await storage.createHmacKey(email);
  assert.equal(secret.length, 40);
  assert.equal(key.metadata.accessId.length, 61);
  assert.equal(key.metadata.state, 'ACTIVE');
  const listed = async options => (await storage.getHmacKeys({serviceAccountEmail: email, ...options}))[0]
    .map(k => k.metadata.accessId);
  assert.deepEqual(await listed(), [key.metadata.accessId]);
  await rejectsWith(400, () => key.delete());
  assert.equal((await metadata(key)).state, 'ACTIVE', 'rejected delete changed the key');
  await key.setMetadata({state: 'INACTIVE'});
  assert.equal((await metadata(key)).state, 'INACTIVE');
  await key.delete();
  assert.equal((await metadata(key)).state, 'DELETED');
  assert.deepEqual(await listed(), [], 'deleted key listed by default');
  assert.deepEqual(await listed({showDeletedKeys: true}), [key.metadata.accessId]);
});

test('node-bucket-uniform-access', async () => {
  await withBucket('ubla', {iamConfiguration: {uniformBucketLevelAccess: {enabled: true}}}, async b => {
    let ubla = (await metadata(b)).iamConfiguration.uniformBucketLevelAccess;
    assert.equal(ubla.enabled, true);
    assert(ubla.lockedTime, 'uniform access deadline missing');
    await b.setMetadata({iamConfiguration: {uniformBucketLevelAccess: {enabled: false}}});
    ubla = (await metadata(b)).iamConfiguration.uniformBucketLevelAccess;
    assert.equal(ubla.enabled, false);
  });
});

test('node-bucket-lifecycle-rules', async () => {
  await withBucket('lifecycle', {lifecycle: {rule: [{action: {type: 'Delete'}, condition: {age: 30}}]}}, async b => {
    let rules = (await metadata(b)).lifecycle.rule;
    assert.deepEqual(rules.map(r => [r.action.type, r.condition.age]), [['Delete', 30]]);
    await replaceLifecycleRules(b, [{action: {type: 'Delete'}, condition: {age: 60}}]);
    await b.addLifecycleRule({action: {type: 'SetStorageClass', storageClass: 'COLDLINE'}, condition: {age: 60}});
    rules = (await metadata(b)).lifecycle.rule;
    assert.deepEqual(rules.map(r => [r.action.type, r.action.storageClass, r.condition.age]),
      [['Delete', undefined, 60], ['SetStorageClass', 'COLDLINE', 60]]);
    await replaceLifecycleRules(b, []);
    assert.equal((await metadata(b)).lifecycle?.rule?.length ?? 0, 0, 'lifecycle not cleared');
  });
});

test('node-bucket-test-permissions', async () => {
  const permissions = ['storage.objects.get', 'storage.objects.create', 'storage.buckets.getIamPolicy'];
  const granted = async client => Object.entries((await client.bucket(bucket.name).iam.testPermissions(permissions))[0])
    .filter(([, allowed]) => allowed).map(([name]) => name).sort();
  assert.deepEqual(await granted(storage), [...permissions].sort());
  if (strict) {
    assert.deepEqual(await granted(createStorage(origin, {token: 'local-viewer-token'})), ['storage.objects.get']);
    assert.deepEqual(await granted(createStorage(origin, {token: 'local-bucket-reader-token'})), []);
  }
});

test('node-strict-authorization', {skip: !strict && 'strict profile only'}, async () => {
  const file = bucket.file('authz.txt');
  await file.save('authz bytes', {resumable: false});
  try {
    const anonymous = createStorage(origin);
    await rejectsWith(401, () => anonymous.bucket(bucket.name).getMetadata());
    await rejectsWith(401, () => anonymous.bucket(bucket.name).file('authz.txt').download());
    const viewer = createStorage(origin, {token: 'local-viewer-token'}).bucket(bucket.name);
    assert.equal((await bytes(viewer.file('authz.txt'))).toString(), 'authz bytes');
    await rejectsWith(403, () => viewer.file('viewer.txt').save('denied', {resumable: false}));
    await rejectsWith(403, () => viewer.file('authz.txt').delete());
    const reader = createStorage(origin, {token: 'local-bucket-reader-token'}).bucket(bucket.name);
    await rejectsWith(403, () => reader.file('authz.txt').download());
    await rejectsWith(401, () => createStorage(origin, {token: 'not-a-registered-token'})
      .bucket(bucket.name).getMetadata());
    assert.equal((await bucket.file('viewer.txt').exists())[0], false, 'denied upload created an object');
  } finally {
    await file.delete({ignoreNotFound: true});
  }
});

test('node-object-holds-custom-time', async () => {
  const customTime = '2026-10-20T01:02:03.000Z';
  const nextDay = '2026-10-21T01:02:03.000Z';
  const file = bucket.file('holds-custom-time');
  await file.save('held bytes', {resumable: false, metadata: {customTime}});
  try {
    assert.equal(new Date((await metadata(file)).customTime).toISOString(), customTime);
    await file.setMetadata({temporaryHold: true});
    assert.equal((await metadata(file)).temporaryHold, true);
    // A hold protects bytes while metadata remains editable.
    await rejectsWith(403, () => file.delete());
    await file.setMetadata({temporaryHold: false});
    await file.setMetadata({customTime: nextDay});
    const meta = await metadata(file);
    assert(!meta.temporaryHold, 'temporary hold not released');
    assert.equal(new Date(meta.customTime).toISOString(), nextDay);
  } finally {
    await file.setMetadata({temporaryHold: false}).catch(() => {});
    await file.delete({ignoreNotFound: true});
  }
});

test('node-bucket-get-list-update', async () => {
  assert.equal((await metadata(storage.bucket(bucket.name))).name, bucket.name);
  const [buckets] = await storage.getBuckets({prefix: bucket.name});
  assert(buckets.some(b => b.name === bucket.name), 'bucket absent from list');
  const scheme = origin.protocol.slice(0, -1);
  await bucket.setMetadata({labels: {transport: scheme}});
  assert.deepEqual((await metadata(bucket)).labels, {transport: scheme});
  // The Node SDK exposes no bucket PUT; a conditional PATCH replaces the same fields instead.
  const {metageneration} = await metadata(bucket);
  await rejectsWith(412, () => bucket.setMetadata({storageClass: 'COLDLINE'},
    {ifMetagenerationMatch: Number(metageneration) + 1}));
  await bucket.setMetadata({labels: {transport: null, replacement: 'yes'}, storageClass: 'NEARLINE'},
    {ifMetagenerationMatch: metageneration});
  const meta = await metadata(bucket);
  assert.deepEqual(meta.labels, {replacement: 'yes'});
  assert.equal(meta.storageClass, 'NEARLINE');
});

test('node-default-save-content-type', async () => {
  const file = bucket.file('extensionless');
  await file.save(Buffer.from('Node default bytes'), {resumable: false});
  try {
    assert.equal((await metadata(file)).contentType, 'application/octet-stream');
    assert.equal((await bytes(file)).toString(), 'Node default bytes');
  } finally {
    await file.delete();
  }
});

test('node-resumable-upload-three-chunks', async () => {
  const name = 'dir/日本語 +%?.bin';
  const start = observed.requests.length;
  observed.locations.length = 0;
  const preconditionOpts = {ifGenerationMatch: 0};
  const options = {destination: name, resumable: true, chunkSize: chunk, preconditionOpts};
  [object] = strict ? await uploadWithLocalAuth(bucket, source, {origin, ...options}) : await bucket.upload(source, options);
  // A supplied session URI first gets a `bytes */*` status query; count only data chunks.
  const chunks = observed.requests.slice(start)
    .filter(r => r.method === 'PUT' && /^bytes \d+-\d+\//.test(r.contentRange ?? ''));
  assert.equal(chunks.length, 3, 'resumable chunks were not sent');
  assert.equal(observed.locations.length, 1, 'resumable Location missing');
  assert.equal(new URL(observed.locations[0]).origin, origin.origin, 'resumable Location left the origin');
  assert.equal(object.name, name);
  assert.equal(Number((await metadata(object)).size), sourceBytes.length);
  await rejectsWith(412, () => bucket.upload(source, {destination: name, resumable: false,
    preconditionOpts: {ifGenerationMatch: 0}}));
});

if (process.env.GCS_MIMIC_SIGNED_TESTS === '1') {
  const signer = createSigner(origin);
  const signed = (file, options) => signer.bucket(bucket.name).file(file)
    .getSignedUrl({expires: Date.now() + 300000, ...options}).then(([url]) => url);
  const send = async (url, {method = 'GET', contentType, host, body} = {}) => {
    const headers = {};
    if (contentType) headers['content-type'] = contentType;
    if (host) headers.host = host;
    // Verify the original TLS endpoint while tampering only with the signed HTTP Host.
    const tlsOptions = host ? {servername: origin.hostname === 'localhost' ? 'localhost' : ''} : {};
    return rawRequest(origin, url, {method, headers, body, tlsOptions});
  };
  const denied = async (url, options) => {
    const response = await send(url, options);
    assert.equal(response.status, 403, response.body.toString());
    assert.match(response.body.toString(), /<Code>SignatureDoesNotMatch<\/Code>/);
  };

  test('node-signed-v2-get-put', async () => {
    try {
      for (const version of [undefined, 'v2']) {
        const v = version ? {version} : {};
        const putUrl = await signed('signed.txt', {...v, action: 'write', contentType: 'text/plain'});
        const getUrl = await signed('signed.txt', {...v, action: 'read'});
        assert.equal(new URL(putUrl).host, origin.host, 'V2 signer changed the endpoint');
        assert.equal(new URL(putUrl).pathname, `/${bucket.name}/signed.txt`);
        await denied(putUrl, {method: 'PUT', contentType: 'changed', body: 'Node signed private bytes'});
        assert.equal((await send(putUrl, {method: 'PUT', contentType: 'text/plain',
          body: 'Node signed private bytes'})).status, 200);
        const got = await send(getUrl);
        assert.equal(got.status, 200);
        assert.equal(got.body.toString(), 'Node signed private bytes');
        await denied(putUrl, {contentType: 'text/plain'});
        await denied(getUrl, {method: 'PUT', contentType: 'text/plain', body: 'x'});
        await denied(getUrl.replace('/signed.txt?', '/changed.txt?'));
      }
      assert.equal((await metadata(bucket.file('signed.txt'))).contentType, 'text/plain');
      const overrides = await signed('signed.txt', {action: 'read',
        responseDisposition: 'attachment; filename="Node + file.csv"', responseType: 'text/csv'});
      const response = await send(overrides);
      assert.equal(response.status, 200);
      assert.equal(response.headers['content-disposition'], 'attachment; filename="Node + file.csv"');
      assert.equal(response.headers['content-type'], 'text/csv');
      assert.equal(response.body.toString(), 'Node signed private bytes');
      assert.equal((await metadata(bucket.file('signed.txt'))).contentType, 'text/plain',
        'response override mutated metadata');
      const anonymous = await rawRequest(origin, `${origin.origin}/${bucket.name}/signed.txt`);
      assert.equal(anonymous.status, 401);
    } finally {
      await bucket.file('signed.txt').delete({ignoreNotFound: true});
    }
  });

  test('node-signed-v4-canonical-host-drops-port', async () => {
    // @google-cloud/storage 8.4.0 signs `host:<hostname>` without the port while emitting the URL
    // with host:port (signer.js uses URL#hostname). No public option binds host:port, so a strict
    // server at a nondefault port must reject the URL. This proves the limitation and keeps it
    // visible; it is not a successful V4 journey.
    const file = bucket.file('v4.txt');
    await file.save('Node V4 bytes', {resumable: false, contentType: 'text/plain'});
    try {
      const url = new URL(await signed('v4.txt', {version: 'v4', action: 'read'}));
      assert.equal(url.host, origin.host, 'V4 URL lost the endpoint port');
      assert.equal(url.pathname, `/${bucket.name}/v4.txt`);
      const params = Object.fromEntries(url.searchParams);
      assert.equal(params['X-Goog-SignedHeaders'], 'host');
      const encode = value => encodeURIComponent(value).replace(/[!'()*]/g, c => `%${c.charCodeAt(0).toString(16).toUpperCase()}`);
      const query = Object.entries(params).filter(([key]) => key !== 'X-Goog-Signature')
        .map(([key, value]) => [encode(key), encode(value)]).sort(([a], [b]) => (a < b ? -1 : 1))
        .map(([key, value]) => `${key}=${value}`).join('&');
      const signatureFor = host => {
        const canonical = ['GET', url.pathname, query, `host:${host}\n`, 'host', 'UNSIGNED-PAYLOAD'].join('\n');
        const scope = params['X-Goog-Credential'].split('/').slice(1).join('/');
        assert(params['X-Goog-Credential'].startsWith(`${signerEmail}/`));
        return ['GOOG4-RSA-SHA256', params['X-Goog-Date'], scope,
          createHash('sha256').update(canonical).digest('hex')].join('\n');
      };
      const signature = Buffer.from(params['X-Goog-Signature'], 'hex');
      const key = signingPublicKey();
      assert(verify('sha256', Buffer.from(signatureFor(url.hostname.replace(/^\[|\]$/g, ''))), key, signature),
        'SDK canonical host assumption differs');
      assert(!verify('sha256', Buffer.from(signatureFor(url.host)), key, signature),
        'SDK unexpectedly bound host:port');
      await denied(url.href);
    } finally {
      await file.delete({ignoreNotFound: true});
    }
  });

  test('node-signed-v4-cli-adapter-keeps-final-host-port', async () => {
    const options = {bucket: bucket.name, object: 'v4-adapter.txt', signer: signerEmail,
      privateKey: fileURLToPath(new URL('../../fixtures/signing/test-only-private.pem', import.meta.url))};
    try {
      const put = await signV4WithCli(origin, {...options, method: 'PUT', headers: {'content-type': 'text/plain'}});
      assert.equal((await send(put, {method: 'PUT', contentType: 'text/plain', body: 'Node strict V4 adapter bytes'})).status, 200);
      const get = await signV4WithCli(origin, options);
      const response = await send(get);
      assert.equal(response.status, 200);
      assert.equal(response.body.toString(), 'Node strict V4 adapter bytes');
      await denied(get, {method: 'PUT', body: 'tampered'});
      await denied(get, {host: 'wrong.example.invalid'});
    } finally {
      await bucket.file(options.object).delete({ignoreNotFound: true});
    }
  });
}

test('node-object-get-list-match-glob', async () => {
  assert.equal((await metadata(bucket.file(object.name))).generation, object.metadata.generation);
  const [all] = await bucket.getFiles();
  assert(all.some(f => f.name === object.name), 'object absent from list');
  assert.deepEqual((await bucket.getFiles({matchGlob: '**/*.bin'}))[0].map(f => f.name), [object.name]);
  assert.deepEqual((await bucket.getFiles({matchGlob: 'missing-*'}))[0], []);
});

test('node-object-metadata-patch', async () => {
  await object.setMetadata({metadata: {operation: 'patch'}});
  assert.deepEqual((await metadata(object)).metadata, {operation: 'patch'});
  // The Node SDK exposes no object PUT; a conditional PATCH replaces the same fields instead.
  const {metageneration} = await metadata(object);
  await rejectsWith(412, () => object.setMetadata({metadata: {operation: 'stale'}},
    {ifMetagenerationMatch: Number(metageneration) + 1}));
  await object.setMetadata({metadata: {operation: 'put'}, contentType: 'application/octet-stream'},
    {ifMetagenerationMatch: metageneration});
  const meta = await metadata(object);
  assert.deepEqual(meta.metadata, {operation: 'put'});
  assert.equal(meta.contentType, 'application/octet-stream');
});

test('node-rewrite-compose-download-bytes', async () => {
  // File#copy is the SDK's rewrite call (objects.rewrite); it has no objects.copy method.
  const [rewritten] = await object.copy(bucket.file('rewrite.bin'), {preconditionOpts: {ifGenerationMatch: 0}});
  const [nearline] = await object.copy(bucket.file('nearline.bin'), {storageClass: 'NEARLINE'});
  assert.equal((await metadata(nearline)).storageClass, 'NEARLINE');
  const [composed] = await bucket.combine([object, rewritten], 'compose.bin');
  for (const file of [rewritten, nearline]) assert.equal(Number((await metadata(file)).size), sourceBytes.length);
  const composedMeta = await metadata(composed);
  assert.equal(Number(composedMeta.size), 2 * sourceBytes.length);
  assert.equal(composedMeta.componentCount, 2);
  const doubled = createHash('sha256');
  for (let i = 0; i < 2; i++) {
    for (let offset = 0; offset < sourceBytes.length; offset += 64 * 1024) {
      doubled.update(sourceBytes.subarray(offset, offset + 64 * 1024));
    }
  }
  const doubledDigest = doubled.digest('hex');
  const destination = join(work, 'download.bin');
  // download() verifies the server-reported CRC32C/MD5 before resolving.
  for (const [file, digest] of [[object, expectedDigest], [rewritten, expectedDigest], [nearline, expectedDigest],
    [composed, doubledDigest]]) {
    await file.download({destination});
    assert.equal(await sha256File(destination), digest, `download bytes mismatch: ${file.name}`);
  }
  // A range spanning the component boundary and an open-ended suffix range.
  const boundary = sourceBytes.length;
  assert.deepEqual(await bytes(composed, {start: boundary - 4, end: boundary + 3}),
    Buffer.concat([sourceBytes.subarray(boundary - 4), sourceBytes.subarray(0, 4)]));
  assert.deepEqual(await bytes(composed, {start: 2 * boundary - 5}), sourceBytes.subarray(boundary - 5));
  for (const file of [rewritten, nearline, composed]) await file.delete();
});

test('node-soft-delete-restore', async () => {
  await withBucket('soft-delete', {}, async b => {
    await b.setMetadata({softDeletePolicy: {retentionDurationSeconds: 604800}});
    assert.equal(Number((await metadata(b)).softDeletePolicy.retentionDurationSeconds), 604800);
    const original = b.file('recoverable');
    await original.save('recoverable Node bytes', {resumable: false});
    const {generation} = await metadata(original);
    await original.delete();
    assert.equal((await b.file('recoverable').exists())[0], false, 'deleted file is live');
    const [deleted] = await b.getFiles({softDeleted: true});
    assert.deepEqual(deleted.map(f => [f.name, f.metadata.generation]), [['recoverable', generation]]);
    const [soft] = await b.file('recoverable', {generation}).getMetadata({softDeleted: true});
    assert.equal(soft.generation, generation);
    const restored = await b.file('recoverable').restore({generation});
    assert(BigInt(restored.generation) > BigInt(generation), 'restore generation differs');
    assert.equal(Number(restored.metageneration), 1);
    assert.equal((await bytes(b.file('recoverable'))).toString(), 'recoverable Node bytes');
  });
});

test('node-bucket-retention-policy', async () => {
  // Retention locks are exercised only on an empty emulator bucket.
  await withBucket('retention', {}, async b => {
    await b.setRetentionPeriod(3600);
    assert.equal(Number((await metadata(b)).retentionPolicy.retentionPeriod), 3600);
    const retained = b.file('retained');
    await retained.save('retained bytes', {resumable: false});
    await rejectsWith(403, () => retained.delete());
    await b.removeRetentionPeriod();
    await retained.delete();
    await b.setRetentionPeriod(7200);
    await b.lock((await metadata(b)).metageneration);
    assert.equal((await metadata(b)).retentionPolicy.isLocked, true);
    await assert.rejects(() => b.setRetentionPeriod(3600), error => {
      assert.equal(error.code, 400, error.message);
      assert.match(error.message, /locked/);
      assert.match(error.message, /extend/);
      return true;
    });
  });
});

test('node-object-move', async () => {
  await withBucket('move', {}, async b => {
    await b.file('source').save('Node move bytes', {resumable: false});
    const [moved] = await b.file('source').moveFileAtomic('destination');
    assert.equal(moved.name, 'destination');
    assert.equal((await b.file('source').exists())[0], false, 'move source still exists');
    assert.equal((await bytes(b.file('destination'))).toString(), 'Node move bytes');
  });
});

test('node-https-trust-and-hostname', {skip: origin.protocol !== 'https:' && 'HTTPS only'}, async () => {
  // Public roots alone must not trust the emulator; a hostname outside the SAN must fail as well.
  await assert.rejects(() => rawRequest(origin, `${origin.origin}/_internal/ready`, {tlsOptions: {ca: rootCertificates}}),
    error => /^(UNABLE_TO_|SELF_SIGNED_|DEPTH_ZERO_)/.test(error.code));
  await assert.rejects(() => rawRequest(origin, `${origin.origin}/_internal/ready`,
    {tlsOptions: {servername: 'outside-san.invalid'}}), error => error.code === 'ERR_TLS_CERT_ALTNAME_INVALID');
  assert.equal((await rawRequest(origin, `${origin.origin}/_internal/ready`)).status, 200);
});
