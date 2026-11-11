import {randomBytes} from 'node:crypto';
import {Storage} from '@google-cloud/storage';

if (process.env.GCSM_ENDPOINT === undefined && process.env.GCS_MIMIC_ENDPOINT !== undefined) {
  throw new Error('GCS_MIMIC_ENDPOINT was renamed to GCSM_ENDPOINT');
}
const endpoint = process.env.GCSM_ENDPOINT ?? 'http://localhost:4443';

// A custom apiEndpoint without credentials sends anonymous requests.
const storage = new Storage({projectId: 'local-project', apiEndpoint: endpoint});

const [bucket] = await storage.createBucket(`example-node-${randomBytes(4).toString('hex')}`);
console.log(`created bucket ${bucket.name}`);

const file = bucket.file('hello.txt');
await file.save('Hello from gcs-mimic!');
console.log(`uploaded ${file.name}`);

const [files] = await bucket.getFiles();
for (const f of files) console.log(`listed ${f.name} (${f.metadata.size} bytes)`);

const [content] = await file.download();
console.log(`downloaded: ${content}`);

await file.delete();
console.log(`deleted ${file.name}`);
await bucket.delete();
console.log(`deleted bucket ${bucket.name}`);
