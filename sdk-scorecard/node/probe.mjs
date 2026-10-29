import assert from 'node:assert/strict';
import {createHash, randomBytes} from 'node:crypto';
import {readFileSync} from 'node:fs';
import {gzipSync} from 'node:zlib';
import {Storage} from '@google-cloud/storage';
import {guardLocalConnections} from './local-http.mjs';

const origin = new URL(process.env.SCORECARD_ENDPOINT);
assert(['127.0.0.1', 'localhost', '[::1]'].includes(origin.hostname), 'local endpoint required');
assert(['http:', 'https:'].includes(origin.protocol) && origin.pathname === '/' &&
  !origin.username && !origin.password && !origin.search && !origin.hash, 'origin required');
guardLocalConnections(origin);
for (const key of ['http_proxy', 'https_proxy', 'HTTP_PROXY', 'HTTPS_PROXY', 'all_proxy', 'ALL_PROXY']) delete process.env[key];
const projectId = process.env.SCORECARD_PROJECT || 'local-project';
const private_key = readFileSync(process.env.SCORECARD_SIGNING_KEY ||
  new URL('../../fixtures/signing/test-only-private.pem', import.meta.url), 'utf8');
delete process.env.STORAGE_EMULATOR_HOST;
const storage = new Storage({projectId, apiEndpoint: origin.origin,
  credentials: {client_email: 'local-admin@example.invalid', private_key},
  retryOptions: {autoRetry: false}, timeout: 20000});
const unique = () => `sc-node-${randomBytes(6).toString('hex')}`;
const bucket = async (metadata = {}) => (await storage.createBucket(unique(), metadata))[0];
const metadata = async resource => (await resource.getMetadata())[0];
const upload = async (b, name, data, options = {}) => {
  const file = b.file(name);
  await file.save(data, {resumable: false, ...options});
  return file;
};
const read = async (file, options = {}) => (await file.download(options))[0];
const equalBytes = async (file, want, options = {}) => assert.deepEqual(await read(file, options), Buffer.from(want));
const expectCode = async (code, call) => assert.rejects(call, error => error.code === code);
const seed = async (b, names) => { for (const name of names) await upload(b, name, name); };
const names = files => files.map(f => f.name).sort();
const list = async (b, options = {}) => (await b.getFiles(options))[0];
const cases = [];
const probe = (id, run) => cases.push({id, run});
const http = async (url, options = {}) => {
  assert.equal(new URL(url).origin, origin.origin, 'request escaped emulator');
  return fetch(url, {...options, redirect: 'error', signal: AbortSignal.timeout(20000)});
};
if (process.argv.includes('--transport-check')) {
  await assert.rejects(() => http('https://storage.googleapis.com/'), /request escaped emulator/);
  assert.equal((await http(origin.href)).status, 200);
  // Both SDK transports use node:https, while signed requests use native fetch.
  const {get} = await import('node:https');
  await new Promise((resolve, reject) => get(origin, response => {
    response.resume();
    response.on('end', resolve);
  }).on('error', reject));
  console.log('PASS: Node transport CA/SAN and exact-origin guards');
  process.exit();
}

probe('bucket.create_get_delete', async () => {
  const b = await bucket();
  assert.equal((await metadata(b)).name, b.name);
  await b.delete();
  assert.equal((await b.exists())[0], false);
});
probe('bucket.list_prefix_paging', async () => {
  const prefix = unique();
  for (let i = 0; i < 3; i++) await storage.createBucket(`${prefix}-${i}`);
  let query = {prefix, maxResults: 2, autoPaginate: false};
  const found = [];
  do {
    const [page, next] = await storage.getBuckets(query);
    assert(page.length <= 2);
    found.push(...names(page));
    query = next;
  } while (query);
  assert.deepEqual(found.sort(), [0, 1, 2].map(i => `${prefix}-${i}`));
});
probe('bucket.update_labels', async () => {
  const b = await bucket({labels: {a: '1', b: '2'}});
  await b.setMetadata({labels: {a: null, b: '2', c: '3'}});
  assert.deepEqual((await metadata(b)).labels, {b: '2', c: '3'});
});
probe('bucket.metageneration_precondition', async () => {
  const b = await bucket();
  await expectCode(412, () => b.setMetadata({storageClass: 'NEARLINE'}, {ifMetagenerationMatch: 999}));
});
probe('bucket.versioning_history', async () => {
  const b = await bucket({versioning: {enabled: true}});
  const first = await upload(b, 'v.txt', 'one');
  const generation = (await metadata(first)).generation;
  await upload(b, 'v.txt', 'two');
  assert.equal((await list(b, {versions: true})).length, 2);
  await equalBytes(b.file('v.txt', {generation}), 'one');
});
for (const [id, field, value] of [
  ['lifecycle', 'lifecycle', {rule: [{action: {type: 'Delete'}, condition: {age: 30}}]}],
  ['cors', 'cors', [{origin: ['https://app.example'], method: ['GET'], maxAgeSeconds: 3600}]],
  ['uniform_bucket_level_access', 'iamConfiguration', {uniformBucketLevelAccess: {enabled: true}}],
  ['public_access_prevention', 'iamConfiguration', {publicAccessPrevention: 'enforced'}],
  ['soft_delete_policy', 'softDeletePolicy', {retentionDurationSeconds: '864000'}],
  ['autoclass', 'autoclass', {enabled: true}],
]) probe(`bucket.${id}`, async () => {
  const b = await bucket();
  await b.setMetadata({[field]: value});
  assert.partialDeepStrictEqual((await metadata(b))[field], value);
});
probe('bucket.retention_policy', async () => {
  const b = await bucket();
  await b.setRetentionPeriod(3600);
  const file = await upload(b, 'r', 'r');
  await expectCode(403, () => file.delete());
});
probe('bucket.retention_lock', async () => {
  const b = await bucket();
  await b.setRetentionPeriod(3600);
  await b.lock((await metadata(b)).metageneration);
  assert.equal((await metadata(b)).retentionPolicy.isLocked, true);
});
probe('bucket.iam_policy', async () => {
  const b = await bucket();
  const [policy] = await b.iam.getPolicy({requestedPolicyVersion: 3});
  policy.bindings.push({role: 'roles/storage.objectViewer', members: ['user:someone@example.invalid']});
  await b.iam.setPolicy(policy);
  const [got] = await b.iam.getPolicy({requestedPolicyVersion: 3});
  assert(got.bindings.some(x => x.role === 'roles/storage.objectViewer' && x.members.includes('user:someone@example.invalid')));
});
probe('bucket.iam_test_permissions', async () => {
  const b = await bucket();
  const [permissions] = await b.iam.testPermissions(['storage.buckets.get', 'storage.objects.list']);
  assert(Object.values(permissions).some(Boolean));
});
for (const id of ['acl', 'default_object_acl']) probe(`bucket.${id}`, async () => {
  const b = await bucket();
  const acl = id === 'acl' ? b.acl : b.acl.default;
  await acl.add({entity: 'allAuthenticatedUsers', role: 'READER'});
  const [entries] = await acl.get();
  assert(entries.some(e => e.entity === 'allAuthenticatedUsers' && e.role === 'READER'));
});
probe('bucket.notifications', async () => {
  const b = await bucket();
  const [notification] = await b.createNotification(`projects/${projectId}/topics/scorecard`, {payloadFormat: 'JSON_API_V1'});
  const [all] = await b.getNotifications();
  assert(all.some(n => n.id === notification.id));
  await notification.delete();
});
probe('project.hmac_keys', async () => {
  const [key, secret] = await storage.createHmacKey('sa@example.invalid');
  assert(secret && key.id);
  await key.setMetadata({state: 'INACTIVE'});
  await key.delete();
});
probe('project.service_account', async () => {
  const [account] = await storage.getServiceAccount();
  assert(account.emailAddress.includes('@'));
});
probe('object.write_read_small', async () => {
  await equalBytes(await upload(await bucket(), 'dir/日本語 +%?.txt', 'hello'), 'hello');
});
probe('object.resumable_large', async () => {
  const b = await bucket();
  const data = Buffer.alloc(3 * 1024 * 1024 + 123, 'a');
  const file = await upload(b, 'big.bin', data, {resumable: true, chunkSize: 256 * 1024});
  assert.equal(Number((await metadata(file)).size), data.length);
  await equalBytes(file, data);
});
probe('object.metadata_on_write', async () => {
  const want = {contentType: 'text/plain', cacheControl: 'no-cache', contentDisposition: 'attachment',
    contentLanguage: 'ja', metadata: {k: 'v'}};
  const file = await upload(await bucket(), 'meta.txt', 'x', {metadata: want});
  assert.partialDeepStrictEqual(await metadata(file), want);
});
probe('object.checksum_rejected', async () => {
  const b = await bucket();
  await assert.rejects(() => upload(b, 'crc.txt', 'payload', {metadata: {crc32c: 'AAAAAA=='}}),
    error => error.code === 400 && /checksum|crc32c/i.test(error.message));
  assert.equal((await b.file('crc.txt').exists())[0], false);
});
probe('object.md5_accepted', async () => {
  const md5Hash = createHash('md5').update('md5 payload').digest('base64');
  const file = await upload(await bucket(), 'md5.txt', 'md5 payload', {metadata: {md5Hash}});
  assert.equal((await metadata(file)).md5Hash, md5Hash);
});
probe('object.range_reads', async () => {
  const file = await upload(await bucket(), 'range.txt', '0123456789');
  await equalBytes(file, '2345', {start: 2, end: 5});
  await equalBytes(file, '789', {start: 7});
  await equalBytes(file, '789', {end: -3});
});
probe('object.update_metadata', async () => {
  const file = await upload(await bucket(), 'u.txt', 'x', {metadata: {metadata: {a: '1', b: '2'}}});
  await file.setMetadata({contentType: 'text/csv', metadata: {a: null, c: '3'}});
  const got = await metadata(file);
  assert.equal(got.contentType, 'text/csv');
  assert.deepEqual(got.metadata, {b: '2', c: '3'});
});
probe('object.delete_not_found', async () => {
  const file = await upload(await bucket(), 'd.txt', 'x');
  await file.delete();
  assert.equal((await file.exists())[0], false);
  await expectCode(404, () => file.delete());
});
probe('object.does_not_exist_precondition', async () => {
  const b = await bucket();
  const options = {preconditionOpts: {ifGenerationMatch: 0}};
  await upload(b, 'once.txt', 'first', options);
  await expectCode(412, () => upload(b, 'once.txt', 'second', options));
  await equalBytes(b.file('once.txt'), 'first');
});
probe('object.generation_match_precondition', async () => {
  const file = await upload(await bucket(), 'g.txt', 'x');
  const generation = (await metadata(file)).generation;
  await expectCode(412, () => file.delete({ifGenerationMatch: (BigInt(generation) + 1n).toString()}));
  await file.delete({ifGenerationMatch: generation});
});
probe('object.metageneration_match_precondition', async () => {
  const file = await upload(await bucket(), 'm.txt', 'x');
  const meta = await metadata(file);
  await expectCode(412, () => file.setMetadata({contentType: 'text/plain'}, {ifMetagenerationMatch: Number(meta.metageneration) + 5}));
});
probe('object.list_prefix_delimiter', async () => {
  const b = await bucket();
  await seed(b, ['a/1', 'a/2', 'b/1', 'c']);
  const [files, , response] = await b.getFiles({delimiter: '/', autoPaginate: false});
  assert.deepEqual(names(files), ['c']);
  assert.deepEqual(response.prefixes.sort(), ['a/', 'b/']);
  assert.deepEqual(names(await list(b, {prefix: 'a/'})), ['a/1', 'a/2']);
});
probe('object.list_paging', async () => {
  const b = await bucket();
  await seed(b, ['p1', 'p2', 'p3', 'p4', 'p5']);
  let query = {maxResults: 2, autoPaginate: false};
  const found = [];
  let pages = 0;
  do {
    const [files, next] = await b.getFiles(query);
    assert(files.length <= 2);
    pages++;
    found.push(...names(files));
    query = next;
  } while (query);
  assert(pages >= 3);
  assert.deepEqual(found.sort(), ['p1', 'p2', 'p3', 'p4', 'p5']);
});
for (const [id, objects, options, want] of [
  ['list_offsets', ['a', 'b', 'c', 'd'], {startOffset: 'b', endOffset: 'd'}, ['b', 'c']],
  ['list_match_glob', ['x/a.txt', 'x/b.csv', 'y/c.txt'], {matchGlob: '**/*.txt'}, ['x/a.txt', 'y/c.txt']],
]) probe(`object.${id}`, async () => {
  const b = await bucket();
  await seed(b, objects);
  assert.deepEqual(names(await list(b, options)), want);
});
probe('object.copy_with_metadata', async () => {
  const source = await upload(await bucket(), 'src', 'copy me');
  const dest = (await bucket()).file('dst');
  await source.copy(dest, {contentType: 'text/plain', metadata: {copied: 'yes'}});
  assert.partialDeepStrictEqual(await metadata(dest), {contentType: 'text/plain', metadata: {copied: 'yes'}});
  await equalBytes(dest, 'copy me');
});
probe('object.rewrite_storage_class', async () => {
  const b = await bucket();
  const data = Buffer.alloc(2 * 1024 * 1024 + 7, 'a');
  const source = await upload(b, 'src', data);
  await source.copy(b.file('dst'), {storageClass: 'NEARLINE'});
  assert.equal((await metadata(b.file('dst'))).storageClass, 'NEARLINE');
  await equalBytes(b.file('dst'), data);
});
probe('object.compose', async () => {
  const b = await bucket();
  await seed(b, ['p1', 'p2', 'p3']);
  await b.combine(['p1', 'p2', 'p3'], 'all');
  assert.equal(Number((await metadata(b.file('all'))).componentCount), 3);
  await equalBytes(b.file('all'), 'p1p2p3');
});
probe('object.move', async () => {
  const b = await bucket();
  const file = await upload(b, 'old', 'moved');
  await file.moveFileAtomic('new');
  assert.equal((await b.file('old').exists())[0], false);
  await equalBytes(b.file('new'), 'moved');
});
probe('object.acl', async () => {
  const file = await upload(await bucket(), 'acl.txt', 'x');
  await file.acl.add({entity: 'allUsers', role: 'READER'});
  const [acl] = await file.acl.get();
  assert(acl.some(e => e.entity === 'allUsers' && e.role === 'READER'));
});
probe('object.holds', async () => {
  const file = await upload(await bucket(), 'hold.txt', 'x');
  await file.setMetadata({temporaryHold: true});
  await expectCode(403, () => file.delete());
  await file.setMetadata({temporaryHold: false});
  await file.delete();
});
probe('object.retention', async () => {
  const b = await bucket({enableObjectRetention: true});
  const file = await upload(b, 'ret.txt', 'x');
  await file.setMetadata({retention: {mode: 'Unlocked', retainUntilTime: new Date(Date.now() + 3600000).toISOString()}});
  assert.equal((await metadata(file)).retention.mode, 'Unlocked');
  await expectCode(403, () => file.delete());
});
probe('object.custom_time', async () => {
  const file = await upload(await bucket(), 'ct.txt', 'x');
  await file.setMetadata({customTime: '2026-01-02T03:04:05Z'});
  assert.equal(Date.parse((await metadata(file)).customTime), Date.parse('2026-01-02T03:04:05Z'));
});
probe('object.storage_class_on_write', async () => {
  const file = await upload(await bucket(), 'cold', 'x', {metadata: {storageClass: 'COLDLINE'}});
  assert.equal((await metadata(file)).storageClass, 'COLDLINE');
});
probe('object.gzip_transcoding', async () => {
  const plain = Buffer.from('compress me '.repeat(50));
  const zipped = gzipSync(plain);
  const file = await upload(await bucket(), 'gz.txt', zipped, {metadata: {contentType: 'text/plain', contentEncoding: 'gzip'}});
  await equalBytes(file, plain);
  await equalBytes(file, zipped, {decompress: false});
});
probe('object.customer_supplied_key', async () => {
  const b = await bucket();
  const file = b.file('csek.txt').setEncryptionKey(randomBytes(32));
  await file.save('secret', {resumable: false});
  await assert.rejects(() => read(b.file('csek.txt')));
  await equalBytes(file, 'secret');
});
probe('object.soft_delete_restore', async () => {
  const b = await bucket({softDeletePolicy: {retentionDurationSeconds: '604800'}});
  const file = await upload(b, 'sd.txt', 'restore me');
  const generation = (await metadata(file)).generation;
  await file.delete();
  assert.deepEqual(names(await list(b, {softDeleted: true})), ['sd.txt']);
  await b.file('sd.txt').restore({generation});
  await equalBytes(b.file('sd.txt'), 'restore me');
});
for (const [id, version, action] of [['v4_get', 'v4', 'read'], ['v4_put', 'v4', 'write'], ['v2_get', 'v2', 'read']]) {
  probe(`signed.${id}`, async () => {
    const b = await bucket();
    const file = b.file('signed.txt');
    if (action === 'read') await file.save('signed', {resumable: false});
    const [url] = await file.getSignedUrl({version, action, expires: Date.now() + 300000});
    const response = await http(url, action === 'write' ? {method: 'PUT', body: 'uploaded'} : {});
    assert.equal(response.status, 200);
    if (action === 'read') assert.equal(await response.text(), 'signed');
    else await equalBytes(file, 'uploaded');
  });
}
probe('signed.post_policy_v4', async () => {
  const b = await bucket();
  const [policy] = await b.file('post.txt').generateSignedPostPolicyV4({expires: Date.now() + 300000});
  const form = new FormData();
  for (const [key, value] of Object.entries(policy.fields)) form.append(key, value);
  form.append('file', new Blob(['posted']), 'post.txt');
  const response = await http(`${origin.origin}/${b.name}`, {method: 'POST', body: form});
  assert(response.ok, `POST status ${response.status}: ${await response.text()}`);
  await equalBytes(b.file('post.txt'), 'posted');
});

for (const {id, run} of cases) {
  if (process.env.SCORECARD_RUN && !id.includes(process.env.SCORECARD_RUN)) continue;
  const result = {sdk: 'node', id, area: id.startsWith('project.') ? 'bucket' : id.split('.')[0], pass: true};
  const timer = setTimeout(() => { console.error(`case timed out: ${id}`); process.exit(2); }, 60000);
  try { await run(); }
  catch (error) { result.pass = false; result.detail = `${error.name}: ${error.message}`.slice(0, 300); }
  finally { clearTimeout(timer); }
  console.log(JSON.stringify(result));
}
