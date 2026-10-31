import {execFile} from 'node:child_process';
import {promisify} from 'node:util';

const execute = promisify(execFile);

// The JSON contract has a lifecycle object; no undeclared append query is needed.
export function replaceLifecycleRules(bucket, rules) {
  if (!Array.isArray(rules)) throw new TypeError('lifecycle rules must be an array');
  return bucket.setMetadata({lifecycle: {rule: rules}});
}

// The SDK's public initiation method retains the configured local bearer identity.
export async function uploadWithLocalAuth(bucket, source, {origin, destination, ...options}) {
  if (!destination) throw new TypeError('destination is required');
  const [uri] = await bucket.file(destination).createResumableUpload({
    metadata: options.metadata, preconditionOpts: options.preconditionOpts,
  });
  if (new URL(uri).origin !== origin.origin) throw new Error('resumable session escaped the emulator origin');
  return bucket.upload(source, {...options, destination, resumable: true, uri});
}

export function signingArguments(origin, {bucket, object, method = 'GET', expires = 300,
  signer, privateKey, headers = {}, query = {}}) {
  if (!['http:', 'https:'].includes(origin.protocol) || origin.username || origin.password ||
      origin.search || origin.hash || origin.pathname !== '/') throw new TypeError('use a bare final HTTP(S) origin');
  if (!bucket || !object || !signer || !privateKey) throw new TypeError('bucket, object, signer and privateKey are required');
  if (!['GET', 'HEAD', 'PUT', 'DELETE', 'POST'].includes(method) || !Number.isInteger(expires) || expires < 1 || expires > 604800) {
    throw new TypeError('use a supported HTTP method and 1–604800 expiry seconds');
  }
  const args = ['url', 'sign', '--base-url', origin.origin, '--bucket', bucket, '--object', object,
    '--method', method, '--expires', String(expires), '--signer', signer, '--private-key', privateKey];
  for (const [name, value] of Object.entries(headers)) args.push('--header', `${name}:${value}`);
  for (const [name, value] of Object.entries(query)) args.push('--query', `${name}=${value}`);
  return args;
}

// The Rust signer binds the final host:port; @google-cloud/storage 8.4.0 drops that port.
export async function signV4WithCli(origin, options) {
  const {stdout} = await execute(process.env.GCS_MIMIC_BINARY || 'gcs-mimic', signingArguments(origin, options),
    {timeout: 10000, maxBuffer: 64 * 1024, encoding: 'utf8', windowsHide: true});
  const result = new URL(stdout.trim());
  if (result.origin !== origin.origin) throw new Error('signer changed the final origin');
  return result.href;
}
