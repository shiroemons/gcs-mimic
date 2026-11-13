import assert from 'node:assert/strict';
import {subscribe} from 'node:diagnostics_channel';
import {readFileSync} from 'node:fs';
import {request as httpRequest} from 'node:http';
import {request as httpsRequest} from 'node:https';
import {isIP, Socket} from 'node:net';
import {TLSSocket} from 'node:tls';
import {OAuth2Client, PassThroughClient} from 'google-auth-library';

// GCSM_NODE_STORAGE=8 runs the suite with the previous major line pinned in storage-8/.
const storageLine = process.env.GCSM_NODE_STORAGE ?? 'current';
if (!['current', '8'].includes(storageLine)) {
  throw new TypeError('GCSM_NODE_STORAGE must be unset, current or 8');
}
const {Storage} = await import(storageLine === '8' ? './storage-8/index.mjs' : '@google-cloud/storage');
export const storageVersion = JSON.parse(readFileSync(new URL(
  `${storageLine === '8' ? 'storage-8/' : ''}node_modules/@google-cloud/storage/package.json`,
  import.meta.url))).version;
assert.equal(storageVersion.split('.')[0], storageLine === '8' ? '8' : '9',
  `GCSM_NODE_STORAGE=${storageLine} loaded @google-cloud/storage ${storageVersion}`);

export const projectId = 'local-project';
export const signerEmail = 'local-admin@example.invalid';
export const signingKey = () =>
  readFileSync(new URL('../../fixtures/signing/test-only-private.pem', import.meta.url), 'utf8');
export const signingPublicKey = () =>
  readFileSync(new URL('../../fixtures/signing/test-only-public.pem', import.meta.url), 'utf8');

const bareHost = host => host.replace(/^\[|\]$/g, '');
const isLoopback = host => {
  const bare = bareHost(host);
  if (bare === 'localhost') return true;
  if (isIP(bare) === 4) return bare.startsWith('127.');
  return isIP(bare) === 6 && bare === '::1';
};
const defaultPort = protocol => (protocol === 'https:' ? 443 : 80);

// Accepts only a bare loopback HTTP(S) origin, so a typo cannot aim the suite at Google.
export function emulatorOrigin(raw) {
  let url;
  try {
    url = new URL(raw);
  } catch {
    throw new TypeError('invalid local HTTP or HTTPS emulator origin');
  }
  if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password ||
      !['', '/'].includes(url.pathname) || url.search || url.hash || raw.includes('?') ||
      !isLoopback(url.hostname)) {
    throw new TypeError('GCSM_ENDPOINT must be a loopback HTTP or HTTPS origin');
  }
  return url;
}

// The SDK transports (teeny-request and gaxios in 8.x, gaxios alone in 9.x) and the signed-URL
// helper reach the network through net.Socket#connect, so this is the one boundary that cannot be
// bypassed by a redirect, a resumable session URL or credential discovery. TLSSocket identifies HTTPS, rejecting scheme changes too.
export function guardSockets(origin) {
  const host = bareHost(origin.hostname);
  const port = Number(origin.port || defaultPort(origin.protocol));
  const tls = origin.protocol === 'https:';
  const connect = Socket.prototype.connect;
  Socket.prototype.connect = function guardedConnect(...args) {
    const first = Array.isArray(args[0]) ? args[0][0] : args[0];
    const options = typeof first === 'object' && first !== null ? first : {port: first, host: args[1]};
    if (options.path) throw new Error('request escaped emulator: Unix socket');
    const target = bareHost(String(options.host || options.hostname || 'localhost'));
    if (target !== host || Number(options.port) !== port || (this instanceof TLSSocket) !== tls) {
      throw new Error(`request escaped emulator: ${tls === this instanceof TLSSocket ? '' : 'scheme '}${target}:${options.port}`);
    }
    return connect.apply(this, args);
  };
}

// Records every HTTP request the process sends, at the HTTP layer, for origin assertions and
// resumable-chunk observation. Enforcement stays in guardSockets.
export function observeHttp(origin) {
  const observed = {requests: [], locations: [], violations: []};
  subscribe('http.client.request.start', ({request}) => {
    const authority = request.getHeader('host') ?? request.host;
    const entry = {method: request.method, path: request.path, protocol: request.protocol,
      authority, contentRange: request.getHeader('content-range')};
    observed.requests.push(entry);
    if (entry.protocol !== origin.protocol) observed.violations.push(entry);
  });
  subscribe('http.client.response.finish', ({response}) => {
    if (response.headers.location) observed.locations.push(response.headers.location);
  });
  return observed;
}

// Removes ambient configuration that would redirect the SDK or enable credential discovery.
export function scrubEnvironment() {
  for (const key of ['http_proxy', 'https_proxy', 'HTTP_PROXY', 'HTTPS_PROXY', 'all_proxy', 'ALL_PROXY',
    'no_proxy', 'NO_PROXY', 'STORAGE_EMULATOR_HOST', 'GOOGLE_APPLICATION_CREDENTIALS',
    'GOOGLE_CLOUD_PROJECT', 'GCLOUD_PROJECT', 'NODE_TLS_REJECT_UNAUTHORIZED']) delete process.env[key];
  process.env.METADATA_SERVER_DETECTION = 'none';
}

// Without a token the SDK skips google-auth-library entirely for a custom endpoint (anonymous).
// With a token, an OAuth2Client holding only a static non-expiring access token authorizes both the
// JSON transport and the resumable-upload transport; it has no refresh path to contact Google.
export function createStorage(origin, {token, retries = false} = {}) {
  const options = {projectId, apiEndpoint: origin.origin, timeout: 10000,
    retryOptions: retries ? {} : {autoRetry: false}};
  if (token) {
    const authClient = new OAuth2Client();
    authClient.setCredentials({access_token: token});
    Object.assign(options, {authClient, useAuthWithCustomEndpoint: true});
  } else {
    // Since 9.0.0 every JSON request goes through the auth client, even for a custom endpoint, so
    // without one the SDK looks for default credentials. PassThroughClient sends no credentials.
    options.authClient = new PassThroughClient();
  }
  return new Storage(options);
}

// The signer client is used only for offline URL signing with the clearly public fixture key.
export function createSigner(origin) {
  return new Storage({projectId, apiEndpoint: origin.origin,
    credentials: {client_email: signerEmail, private_key: signingKey()}});
}

// Sends a signed URL unchanged (unless a test deliberately tampers with it) without SDK auth.
export function rawRequest(origin, url, {method = 'GET', headers = {}, body, tlsOptions = {}} = {}) {
  const target = new URL(url);
  assert.equal(target.origin, origin.origin, 'refusing request outside the emulator origin');
  const send = target.protocol === 'https:' ? httpsRequest : httpRequest;
  return new Promise((resolve, reject) => {
    const req = send(target, {method, headers, timeout: 10000, agent: false, ...tlsOptions}, response => {
      const chunks = [];
      let size = 0;
      response.on('data', chunk => {
        size += chunk.length;
        if (size > 64 * 1024) req.destroy(new Error('response exceeded test limit'));
        else chunks.push(chunk);
      });
      response.on('end', () => resolve({status: response.statusCode, headers: response.headers,
        body: Buffer.concat(chunks)}));
      response.on('error', reject);
    });
    req.on('timeout', () => req.destroy(new Error('request timed out')));
    req.on('error', reject);
    req.end(body);
  });
}
