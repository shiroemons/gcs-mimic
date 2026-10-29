import assert from 'node:assert/strict';
import {Socket} from 'node:net';

export function guardLocalConnections(origin) {
  const connect = Socket.prototype.connect;
  // The SDK uses both teeny-request and gaxios; guard their common socket boundary.
  Socket.prototype.connect = function (...args) {
    const first = Array.isArray(args[0]) ? args[0][0] : args[0];
    const options = typeof first === 'object' ? first : {port: first, host: args[1]};
    const host = (options.host || options.hostname || 'localhost').replace(/^\[|\]$/g, '');
    assert.equal(host, origin.hostname.replace(/^\[|\]$/g, ''), 'request escaped emulator');
    assert.equal(Number(options.port), Number(origin.port || (origin.protocol === 'https:' ? 443 : 80)),
      'request escaped emulator');
    assert(!options.path, 'Unix sockets are outside the emulator origin');
    return connect.apply(this, args);
  };
}
