import {randomBytes} from 'node:crypto';
import {Storage} from '@google-cloud/storage';

const endpoint = process.env.GCS_MIMIC_ENDPOINT ?? 'http://localhost:4443';

// A custom apiEndpoint without credentials sends anonymous requests.
const storage = new Storage({projectId: 'local-project', apiEndpoint: endpoint});

const [bucket] = await storage.createBucket(`example-node-${randomBytes(4).toString('hex')}`);
console.log(`Created bucket ${bucket.name}`);

const file = bucket.file('hello.txt');
await file.save('Hello from Node.js!');
console.log(`Uploaded ${file.name}`);

const [files] = await bucket.getFiles();
for (const f of files) console.log(`Listed ${f.name}`);

const [content] = await file.download();
console.log(`Downloaded ${file.name}: ${content}`);

await file.delete();
await bucket.delete();
console.log(`Deleted bucket ${bucket.name}`);
