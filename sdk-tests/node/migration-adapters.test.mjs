import assert from 'node:assert/strict';
import {test} from 'node:test';
import {replaceLifecycleRules, signingArguments, uploadWithLocalAuth} from './migration-adapters.mjs';

test('lifecycle replacement and clearing send an object without SDK append options', async () => {
  const calls = [];
  const bucket = {async setMetadata(...args) { calls.push(args); }};
  const rules = [{action: {type: 'Delete'}, condition: {age: 30}}];
  await replaceLifecycleRules(bucket, rules);
  await replaceLifecycleRules(bucket, []);
  assert.deepEqual(calls, [[{lifecycle: {rule: rules}}], [{lifecycle: {rule: []}}]]);
  assert.throws(() => replaceLifecycleRules(bucket, {}), /array/);
});

test('authenticated upload initiates the same destination before streaming to its capability', async () => {
  const origin = new URL('http://127.0.0.1:4443');
  const calls = [];
  const bucket = {file(name) {
    calls.push(name);
    return {async createResumableUpload(options) { calls.push(options); return [`${origin.origin}/upload?upload_id=local`]; }};
  }, async upload(source, options) { calls.push([source, options]); return ['object']; }};
  const preconditionOpts = {ifGenerationMatch: 0};
  assert.deepEqual(await uploadWithLocalAuth(bucket, 'fixture.bin', {origin, destination: 'dir/file.bin',
    chunkSize: 256 * 1024, preconditionOpts}), ['object']);
  assert.equal(calls[0], 'dir/file.bin');
  assert.deepEqual(calls[1].preconditionOpts, preconditionOpts);
  assert.equal(calls[2][1].uri, `${origin.origin}/upload?upload_id=local`);
  assert.equal(calls[2][1].chunkSize, 256 * 1024);
});

test('authenticated upload rejects a cross-origin session before reading the source', async () => {
  const bucket = {file() { return {async createResumableUpload() { return ['https://storage.googleapis.com/upload']; }}; },
    async upload() { assert.fail('source was sent after a cross-origin session'); }};
  await assert.rejects(uploadWithLocalAuth(bucket, 'fixture', {origin: new URL('http://127.0.0.1:4443'), destination: 'file'}), /escaped/);
});

test('signer passes the exact final origin and literal object/header/query arguments without a shell', () => {
  const args = signingArguments(new URL('http://127.0.0.1:4443'), {bucket: 'bucket', object: 'dir/日本語 +%?.txt',
    signer: 'local@example.invalid', privateKey: 'fixture.pem', method: 'PUT', headers: {'content-type': 'text/plain'},
    query: {'response-content-disposition': 'attachment; filename="file.txt"'}});
  assert.equal(args[args.indexOf('--base-url') + 1], 'http://127.0.0.1:4443');
  assert.equal(args[args.indexOf('--object') + 1], 'dir/日本語 +%?.txt');
  assert(args.includes('content-type:text/plain'));
  assert(args.includes('response-content-disposition=attachment; filename="file.txt"'));
});
