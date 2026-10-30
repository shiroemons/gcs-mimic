<?php
declare(strict_types=1);

use Google\Cloud\Storage\StorageClient;

/** The README's first-run path: anonymous endpoint, buckets-file bucket, upload, lookup, bytes, delete. */
function migrationScenario(StorageClient $storage): string
{
    $bucket = $storage->bucket('assets');
    check($bucket->exists(), 'buckets file did not initialize assets');
    $name = 'uploads/php-migration.txt';
    $payload = "PHP migration bytes\n";
    $object = $bucket->object($name);
    check(!$object->exists(), 'migration object already exists');
    $created = null;
    try {
        // No contentType: the SDK derives text/plain from the .txt name.
        $created = $bucket->upload($payload, ['name' => $name]);
        $found = $bucket->object($name)->reload();
        check($found['generation'] === $created->info()['generation'] && (int)$found['size'] === strlen($payload)
            && $found['contentType'] === 'text/plain', 'migration object metadata differs');
        check($bucket->object($name)->downloadAsString() === $payload, 'migration downloaded bytes differ');
        $created->delete();
        $created = null;
        check(!$bucket->object($name)->exists(), 'deleted migration object still exists');
    } finally {
        $created?->delete();
    }
    return 'anonymous endpoint, initialized bucket, upload, object lookup, bytes and delete';
}
