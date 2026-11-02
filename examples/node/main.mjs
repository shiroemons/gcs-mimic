// Creates a bucket, uploads, lists, downloads and deletes an object in a local gcs-mimic.
import {randomBytes} from 'node:crypto';
import {Storage} from '@google-cloud/storage';

const endpoint = process.env.GCS_MIMIC_ENDPOINT ?? 'http://localhost:4443';

// With a custom apiEndpoint and no credentials, the SDK sends anonymous requests.
// Leave STORAGE_EMULATOR_HOST unset: this SDK reads it as a path.
const storage = new Storage({projectId: 'local-project', apiEndpoint: endpoint});

const [bucket] = await storage.createBucket(`example-node-${randomBytes(4).toString('hex')}`);
console.log(`Created bucket ${bucket.name}`);
try {
  const file = bucket.file('hello.txt');
  await file.save('Hello from Node.js!', {contentType: 'text/plain'});
  console.log('Uploaded hello.txt');

  const [files] = await bucket.getFiles();
  console.log(`Objects: ${files.map((f) => f.name).join(', ')}`);

  const [content] = await file.download();
  console.log(`Downloaded hello.txt: ${content.toString()}`);
} finally {
  await bucket.deleteFiles();
  await bucket.delete();
  console.log(`Deleted bucket ${bucket.name}`);
}
