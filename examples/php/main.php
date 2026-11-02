<?php

require __DIR__ . '/vendor/autoload.php';

use Google\Cloud\Core\AnonymousCredentials;
use Google\Cloud\Storage\StorageClient;

$endpoint = getenv('GCS_MIMIC_ENDPOINT') ?: 'http://localhost:4443';

// Anonymous credentials for the emulator endpoint; buckets need a project.
$storage = new StorageClient([
    'projectId' => 'local-project',
    'apiEndpoint' => $endpoint,
    'credentialsFetcher' => new AnonymousCredentials(),
]);

$bucket = $storage->createBucket('example-php-' . bin2hex(random_bytes(4)));
echo "Created bucket {$bucket->name()}\n";

$object = $bucket->upload('Hello from PHP!', ['name' => 'hello.txt']);
echo "Uploaded {$object->name()}\n";

foreach ($bucket->objects() as $listed) {
    echo "Listed {$listed->name()}\n";
}

$content = $object->downloadAsString();
echo "Downloaded {$object->name()}: {$content}\n";

$object->delete();
$bucket->delete();
echo "Deleted bucket {$bucket->name()}\n";
