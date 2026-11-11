<?php

if (getenv('GCSM_ENDPOINT') === false && getenv('GCS_MIMIC_ENDPOINT') !== false) {
    throw new RuntimeException('GCS_MIMIC_ENDPOINT was renamed to GCSM_ENDPOINT');
}

require __DIR__ . '/vendor/autoload.php';

use Google\Cloud\Core\AnonymousCredentials;
use Google\Cloud\Storage\StorageClient;

$endpoint = getenv('GCSM_ENDPOINT') ?: 'http://localhost:4443';

// Anonymous credentials for the emulator endpoint; buckets need a project.
$storage = new StorageClient([
    'projectId' => 'local-project',
    'apiEndpoint' => $endpoint,
    'credentialsFetcher' => new AnonymousCredentials(),
]);

$bucket = $storage->createBucket('example-php-' . bin2hex(random_bytes(4)));
echo "created bucket {$bucket->name()}\n";

$object = $bucket->upload('Hello from gcs-mimic!', ['name' => 'hello.txt']);
echo "uploaded {$object->name()}\n";

foreach ($bucket->objects() as $listed) {
    echo "listed {$listed->name()} ({$listed->info()['size']} bytes)\n";
}

echo "downloaded: {$object->downloadAsString()}\n";

$object->delete();
echo "deleted {$object->name()}\n";
$bucket->delete();
echo "deleted bucket {$bucket->name()}\n";
