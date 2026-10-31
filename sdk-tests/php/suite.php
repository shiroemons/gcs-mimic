<?php
declare(strict_types=1);

require __DIR__ . '/harness.php';
require __DIR__ . '/migration_scenario.php';
require __DIR__ . '/signed.php';
require __DIR__ . '/migration_adapters.php';

use Google\Cloud\Core\Exception\BadRequestException;
use Google\Cloud\Core\Exception\ServiceException;
use Google\Cloud\Storage\Bucket;
use GuzzleHttp\Client;
use GuzzleHttp\Handler\MockHandler;
use GuzzleHttp\HandlerStack;
use GuzzleHttp\Psr7\Response;

$mode = $argv[1] ?? '';
$runner = new Runner();

if ($mode === '--self-check') {
    $runner->run('php-lifecycle-clear-adapter-wire', function () {
        $history = [];
        $stack = HandlerStack::create(new MockHandler([new Response(200, [], '{"name":"sdk-php-fixture","lifecycle":{"rule":[]}}')]));
        $stack->push(\GuzzleHttp\Middleware::history($history));
        $http = new Client(['handler' => $stack]);
        $storage = new \Google\Cloud\Storage\StorageClient(['projectId' => 'local-project',
            'apiEndpoint' => 'http://127.0.0.1:4443', 'retries' => 0,
            'credentialsFetcher' => new \Google\Cloud\Core\AnonymousCredentials(),
            'httpHandler' => \Google\Auth\HttpHandler\HttpHandlerFactory::build($http)]);
        \GcsMimic\Migration\clearLifecycleRules($storage->bucket('sdk-php-fixture'));
        check(count($history) === 1, 'lifecycle adapter did not use one request');
        $request = $history[0]['request'];
        check($request->getMethod() === 'PATCH', 'lifecycle adapter did not PATCH');
        check(json_decode((string)$request->getBody(), true, flags: JSON_THROW_ON_ERROR) === ['lifecycle' => ['rule' => []]],
            'lifecycle adapter did not send the JSON object form');
        check($request->getUri()->getHost() === '127.0.0.1', 'lifecycle adapter left the mock endpoint');
        return 'official SDK sends lifecycle:{rule:[]} without a server';
    });
    $runner->run('php-origin-self-check', function () {
        check(emulatorOrigin('https://127.0.0.1:4443')[2] === 4443, 'loopback rejected');
        check(emulatorOrigin('https://localhost:4443')[1] === 'localhost', 'localhost rejected');
        check(emulatorOrigin('http://127.0.0.1:4443')[0] === 'http', 'HTTP loopback rejected');
        check(emulatorOrigin('http://[::1]:4443')[1] === '[::1]', 'IPv6 loopback rejected');
        foreach (['https://storage.googleapis.com', 'http://storage.googleapis.com', 'https://169.254.169.254',
            'http://metadata.google.internal', 'ftp://127.0.0.1', 'https://127.0.0.1.example.invalid',
            'https://user@127.0.0.1', 'https://127.0.0.1/storage/v1', 'https://127.0.0.1?cloud=1',
            'https://127.0.0.1#x', '127.0.0.1:4443', ''] as $raw) {
            try { emulatorOrigin($raw); } catch (InvalidArgumentException) { continue; }
            throw new RuntimeException("unsafe origin accepted: $raw");
        }
        foreach (['http', 'https'] as $scheme) {
            $origin = emulatorOrigin("$scheme://127.0.0.1:4443");
            $observed = new ArrayObject(['chunks' => 0, 'locations' => []]);
            $stack = HandlerStack::create(new MockHandler([new Response(200, ['Location' => "$scheme://127.0.0.1:4443/x"])]));
            $stack->push(originGuard($origin, $observed));
            $client = new Client(['handler' => $stack]);
            $client->put("$scheme://127.0.0.1:4443/upload", ['headers' => ['Content-Range' => 'bytes 0-0/1']]);
            check($observed['chunks'] === 1 && count($observed['locations']) === 1, 'guard did not observe the chunk');
            $other = $scheme === 'http' ? 'https' : 'http';
            foreach (['https://storage.googleapis.com/', 'http://169.254.169.254/', 'http://metadata.google.internal/',
                "$scheme://127.0.0.1:4444/", "$other://127.0.0.1:4443/"] as $raw) {
                try { $client->get($raw); } catch (OriginError) { continue; }
                throw new RuntimeException("outbound origin accepted: $raw");
            }
        }
        return 'HTTP/HTTPS/IPv6 loopback accepted; external, metadata, scheme and port changes rejected without a server';
    });
    $runner->finish('PHP origin self-check');
}

$h = new Harness();
$storage = $h->storage;
$strict = $h->token !== null;
$profile = ($strict ? 'strict' : 'permissive') . ' ' . strtoupper($h->origin[0]);

if ($mode === '--migration-scenario') {
    check(!$strict, 'migration scenario requires an anonymous client');
    $runner->run('php-migration-scenario', fn() => migrationScenario($storage));
    $runner->finish("PHP migration scenario ($profile)");
}
check($mode === '', 'usage: php suite.php [--self-check|--migration-scenario]');

$runner->run('php-bucket-patch-preserves-unrelated-settings', function () use ($storage) {
    $bucket = $storage->createBucket(uniqueName('sdk-php-put'), ['versioning' => ['enabled' => true], 'labels' => ['keep' => 'yes']]);
    try {
        $bucket->update(['iamConfiguration' => ['publicAccessPrevention' => 'enforced']]);
        $info = $bucket->reload();
        check(($info['versioning']['enabled'] ?? false) === true, 'PATCH lost versioning');
        check(($info['labels'] ?? null) === ['keep' => 'yes'], 'PATCH lost labels');
        check($info['iamConfiguration']['publicAccessPrevention'] === 'enforced', 'PATCH did not change PAP');
    } finally {
        dropBucket($bucket);
    }
});

$bucket = null;
$source = tempnam(sys_get_temp_dir(), 'gcs-mimic-php-upload-');
$download = tempnam(sys_get_temp_dir(), 'gcs-mimic-php-download-');
try {
    // A bounded 576 KiB disk fixture: three 256 KiB resumable chunks, the last one partial.
    $block = '';
    for ($i = 0; $i < 256; $i++) $block .= chr($i);
    $handle = fopen($source, 'wb');
    for ($i = 0; $i < 9 * 256; $i++) fwrite($handle, $block);
    fclose($handle);
    $expected = hash_file('sha256', $source);
    $size = filesize($source);

    $runner->run('php-default-create-bucket', function () use ($storage, &$bucket) {
        $bucket = $storage->createBucket(uniqueName('sdk-php-basic'));
        check($bucket->exists(), 'created bucket missing');
    });
    if ($bucket === null) $runner->finish("PHP SDK suite ($profile)");

    $runner->run('php-regional-location', function () use ($storage) {
        $regional = $storage->createBucket(uniqueName('sdk-php-region'), ['location' => 'ASIA-NORTHEAST1']);
        try {
            $info = $regional->info();
            check($info['location'] === 'ASIA-NORTHEAST1' && $info['locationType'] === 'region', 'regional create location mismatch');
            $info = $regional->reload();
            check($info['location'] === 'ASIA-NORTHEAST1' && $info['locationType'] === 'region', 'regional get location mismatch');
        } finally {
            dropBucket($regional);
        }
    });

    $runner->run('php-unsupported-fails-fast', function () use ($h, $bucket) {
        // The SDK's default retry policy, independent of the no-retry transfer client.
        $defaultRetries = $h->client($h->token, null)->bucket($bucket->name());
        $started = hrtime(true);
        try {
            iterator_to_array($defaultRetries->objects(['delimiter' => '/', 'includeFoldersAsPrefixes' => true]));
        } catch (BadRequestException $e) {
            $elapsed = (hrtime(true) - $started) / 1e9;
            check($elapsed < 3, "includeFoldersAsPrefixes waited for retries: $elapsed");
            $body = json_decode($e->getMessage(), true);
            check(($body['error']['errors'][0]['reason'] ?? null) === 'badRequest', 'differs from 400 badRequest: ' . detail($e));
            check(str_contains($e->getMessage(), 'gcs-mimic'), 'lacks actionable diagnostics: ' . detail($e));
            return 'includeFoldersAsPrefixes: 400 badRequest without retries';
        }
        throw new RuntimeException('includeFoldersAsPrefixes unexpectedly succeeded');
    });

    $runner->run('php-bucket-notifications', function () use ($bucket) {
        // The SDK expands the relative topic; gcs-mimic stores it and never publishes.
        $notification = $bucket->createNotification('projects/local-project/topics/uploads', [
            'event_types' => ['OBJECT_FINALIZE', 'OBJECT_DELETE'], 'object_name_prefix' => 'logs/',
            'custom_attributes' => ['team' => 'storage'], 'payload_format' => 'NONE']);
        $listed = $bucket->notification($notification->id())->reload();
        check($listed['topic'] === '//pubsub.googleapis.com/projects/local-project/topics/uploads'
            && $listed['event_types'] === ['OBJECT_FINALIZE', 'OBJECT_DELETE'] && $listed['object_name_prefix'] === 'logs/'
            && $listed['custom_attributes'] === ['team' => 'storage'] && $listed['payload_format'] === 'NONE',
            'notification read-back mismatch: ' . json_encode($listed));
        $ids = [];
        foreach ($bucket->notifications() as $item) $ids[] = $item->id();
        check($ids === [$notification->id()], 'notification not listed');
        $notification->delete();
        check(iterator_to_array($bucket->notifications()) === [], 'notification still listed');
    });

    $runner->run('php-json-acl', function () use ($bucket) {
        // ACL entries are stored and returned; they do not yet grant or deny access.
        $readers = fn(array $entries) => array_column(array_filter($entries, fn($e) => $e['role'] === 'READER'), 'entity');
        $bucket->acl()->add('allAuthenticatedUsers', 'READER');
        check(in_array('allAuthenticatedUsers', $readers($bucket->acl()->get()), true), 'bucket ACL reader missing');
        $bucket->defaultAcl()->add('allAuthenticatedUsers', 'READER');
        check(in_array('allAuthenticatedUsers', $readers($bucket->defaultAcl()->get()), true), 'default ACL reader missing');
        $file = $bucket->upload('x', ['name' => 'acl.txt']);
        check(in_array('allAuthenticatedUsers', $readers($file->acl()->get()), true), 'default ACL not inherited');
        $file->acl()->add('allUsers', 'READER');
        check(in_array('allUsers', $readers($file->acl()->get()), true), 'object ACL reader missing');
        $file->acl()->delete('allUsers');
        check(!in_array('allUsers', $readers($file->acl()->get()), true), 'object ACL entry not deleted');
        $file->delete();
        $bucket->defaultAcl()->delete('allAuthenticatedUsers');
        $bucket->acl()->delete('allAuthenticatedUsers');
    });

    $runner->run('php-bucket-autoclass', function () use ($storage) {
        $autoclass = $storage->createBucket(uniqueName('sdk-php-autoclass'), ['autoclass' => ['enabled' => true]]);
        try {
            $info = $autoclass->reload()['autoclass'];
            check($info['enabled'] === true, 'autoclass not enabled');
            check(isset($info['toggleTime']), 'autoclass toggle time missing');
            check($info['terminalStorageClass'] === 'NEARLINE', 'autoclass terminal class differs');
            $file = $autoclass->upload('x', ['name' => 'cold', 'metadata' => ['storageClass' => 'COLDLINE']]);
            check($file->info()['storageClass'] === 'STANDARD', 'autoclass object class differs');
        } finally {
            dropBucket($autoclass);
        }
    });

    $runner->run('php-project-service-account', function () use ($storage) {
        $account = $storage->getServiceAccount();
        check(str_ends_with($account, '@gs-project-accounts.iam.gserviceaccount.com'), "service account mismatch: $account");
    });

    $hmacEmail = uniqueName('sdk-php') . '@local-project.iam.gserviceaccount.com';
    if ($strict) {
        $runner->run('php-project-hmac-keys-strict-denied', function () use ($storage, $hmacEmail) {
            // roles/storage.admin does not include storage.hmacKeys.*; strict mode must enforce that.
            expectStatus(403, fn() => $storage->createHmacKey($hmacEmail));
        });
    } else {
        $runner->run('php-project-hmac-keys', function () use ($storage, $hmacEmail) {
            $created = $storage->createHmacKey($hmacEmail);
            $key = $created->hmacKey();
            check(strlen($created->secret()) === 40, 'HMAC secret mismatch');
            check(strlen($key->accessId()) === 61 && $key->info()['state'] === 'ACTIVE', 'HMAC access ID mismatch');
            $ids = fn(array $options = []) => array_map(fn($k) => $k->accessId(),
                iterator_to_array($storage->hmacKeys(['serviceAccountEmail' => $hmacEmail] + $options), false));
            check($ids() === [$key->accessId()], 'HMAC key listing mismatch');
            expectStatus(400, fn() => $key->delete());
            check($key->reload()['state'] === 'ACTIVE', 'rejected delete changed the key');
            check($key->update('INACTIVE')['state'] === 'INACTIVE', 'HMAC key not inactive');
            $key->delete();
            check($key->reload()['state'] === 'DELETED', 'HMAC key not deleted');
            check($ids() === [], 'deleted key listed by default');
            check($ids(['showDeletedKeys' => true]) === [$key->accessId()], 'showDeletedKeys mismatch');
        });
    }

    $runner->run('php-bucket-uniform-access', function () use ($storage) {
        $uniform = $storage->createBucket(uniqueName('sdk-php-ubla'),
            ['iamConfiguration' => ['uniformBucketLevelAccess' => ['enabled' => true]]]);
        try {
            $ubla = $uniform->reload()['iamConfiguration']['uniformBucketLevelAccess'];
            check($ubla['enabled'] === true, 'uniform access missing');
            check(isset($ubla['lockedTime']), 'uniform access deadline missing');
            $uniform->update(['iamConfiguration' => ['uniformBucketLevelAccess' => ['enabled' => false]]]);
            check($uniform->reload()['iamConfiguration']['uniformBucketLevelAccess']['enabled'] === false, 'uniform access still enabled');
        } finally {
            dropBucket($uniform);
        }
    });

    $runner->run('php-bucket-lifecycle-rules', function () use ($storage) {
        $lifecycle = $storage->createBucket(uniqueName('sdk-php-lifecycle'),
            ['lifecycle' => Bucket::lifecycle()->addDeleteRule(['age' => 30])]);
        try {
            $rules = $lifecycle->reload()['lifecycle']['rule'];
            check(count($rules) === 1 && $rules[0]['action']['type'] === 'Delete' && $rules[0]['condition']['age'] === 30,
                'created lifecycle differs');
            $lifecycle->update(['lifecycle' => Bucket::lifecycle()->addDeleteRule(['age' => 60])
                ->addSetStorageClassRule('COLDLINE', ['age' => 60])]);
            $rules = $lifecycle->reload()['lifecycle']['rule'];
            check(count($rules) === 2 && $rules[1]['action']['storageClass'] === 'COLDLINE', 'updated lifecycle differs');
            \GcsMimic\Migration\clearLifecycleRules($lifecycle);
            check(($lifecycle->reload()['lifecycle']['rule'] ?? []) === [], 'lifecycle not cleared');
        } finally {
            dropBucket($lifecycle);
        }
    });

    $runner->run('php-bucket-test-permissions', function () use ($h, $bucket, $strict) {
        $permissions = ['storage.objects.get', 'storage.objects.create', 'storage.buckets.getIamPolicy'];
        $granted = $bucket->iam()->testPermissions($permissions);
        sort($granted);
        $want = $permissions;
        sort($want);
        check($granted === $want, 'bucket test permissions mismatch');
        if ($strict) {
            $as = fn(string $token) => $h->client($token, 0)->bucket($bucket->name())->iam()->testPermissions($permissions);
            check($as('local-viewer-token') === ['storage.objects.get'], 'project permissions mismatch');
            check($as('local-bucket-reader-token') === [], 'ungranted permissions mismatch');
        }
    });

    if ($strict) {
        $runner->run('php-strict-authorization-denied', function () use ($h, $bucket) {
            expectStatus(401, fn() => $h->client(null, 0)->bucket($bucket->name())->info());
            expectStatus(403, fn() => $h->client('local-viewer-token', 0)->createBucket(uniqueName('sdk-php-denied')));
            expectStatus(403, fn() => $h->client('local-viewer-token', 0)->bucket($bucket->name())
                ->upload('x', ['name' => 'denied.txt']));
            return 'anonymous 401; objectViewer cannot create buckets or objects';
        });
    }

    $runner->run('php-object-holds-custom-time', function () use ($bucket) {
        $customTime = '2026-10-20T01:02:03Z';
        $held = $bucket->upload('held bytes', ['name' => 'holds-custom-time', 'metadata' => ['customTime' => $customTime]]);
        try {
            check(strtotime($held->info()['customTime']) === strtotime($customTime), 'upload custom time differs');
            $held->update(['temporaryHold' => true]);
            check($held->reload()['temporaryHold'] === true, 'temporary hold missing');
            // A hold protects bytes while metadata remains editable.
            expectStatus(403, fn() => $held->delete());
            $held->update(['temporaryHold' => false, 'customTime' => '2026-10-20T01:02:04Z']);
            $info = $held->reload();
            check(($info['temporaryHold'] ?? false) === false && strtotime($info['customTime']) === strtotime($customTime) + 1,
                'hold release/custom time differs');
        } finally {
            try { $held->update(['temporaryHold' => false]); } catch (ServiceException) {}
            $held->delete();
        }
    });

    $runner->run('php-bucket-get-list-patch', function () use ($storage, $bucket, $h) {
        check($storage->bucket($bucket->name())->info()['name'] === $bucket->name(), 'bucket get mismatch');
        $names = array_map(fn($b) => $b->name(), iterator_to_array($storage->buckets(), false));
        check(in_array($bucket->name(), $names, true), 'bucket absent from list');
        $bucket->update(['labels' => ['transport' => $h->origin[0]]]);
        check($bucket->reload()['labels'] === ['transport' => $h->origin[0]], 'bucket label update mismatch');
        // The PHP SDK exposes buckets.patch only; null removes a label key.
        $bucket->update(['labels' => ['transport' => null, 'replacement' => 'yes'], 'storageClass' => 'NEARLINE',
            'ifMetagenerationMatch' => $bucket->info()['metageneration']]);
        $info = $bucket->reload();
        check($info['labels'] === ['replacement' => 'yes'] && $info['storageClass'] === 'NEARLINE', 'bucket conditional PATCH mismatch');
        expectStatus(412, fn() => $bucket->update(['storageClass' => 'STANDARD', 'ifMetagenerationMatch' => 999]));
    });

    $runner->run('php-default-create-file-content-type', function () use ($bucket) {
        $file = $bucket->upload('PHP default bytes', ['name' => 'extensionless']);
        try {
            check($file->info()['contentType'] === 'application/octet-stream', 'default content type differs');
            check($file->downloadAsString() === 'PHP default bytes', 'default upload bytes differ');
        } finally {
            $file->delete();
        }
    });

    $object = null;
    $objects = [];
    $runner->run('php-resumable-upload', function () use ($h, $bucket, $source, $size, &$object, &$objects) {
        $h->observed['chunks'] = 0;
        $h->observed['locations'] = [];
        $stream = fopen($source, 'rb');
        $object = $bucket->upload($stream, ['name' => 'dir/日本語 +%?.bin', 'resumable' => true,
            'chunkSize' => 256 * 1024, 'ifGenerationMatch' => 0]);
        $objects[] = $object;
        check((int)$object->info()['size'] === $size, 'object size mismatch');
        check($h->observed['chunks'] === 3, "resumable chunks: {$h->observed['chunks']}");
        check(count($h->observed['locations']) === 1, 'resumable Location missing');
        $location = parse_url($h->observed['locations'][0]);
        check(sameOrigin($h->origin, $location['scheme'], $location['host'], $location['port'] ?? null),
            'resumable Location was not the emulator origin');
        expectStatus(412, fn() => $bucket->upload('second', ['name' => $object->name(), 'ifGenerationMatch' => 0]));
        return '3 chunks of 256 KiB; Location keeps the emulator origin';
    });

    if ($strict && getenv('GCS_MIMIC_SIGNED_TESTS') === '1') {
        $signedCleanup = [];
        try {
            foreach (signedUrlJourney($h, $bucket, $signedCleanup) as $id => $case) $runner->run($id, $case);
        } finally {
            foreach ($signedCleanup as $item) {
                try { $item->delete(); } catch (ServiceException) {}
            }
        }
    } elseif ($strict) {
        $runner->skip('php-signed-urls', 'GCS_MIMIC_SIGNED_TESTS is not 1');
    }

    if ($object !== null) {
        $runner->run('php-object-get-list-match-glob', function () use ($bucket, $object) {
            check($bucket->object($object->name())->info()['generation'] === $object->info()['generation'], 'object metadata mismatch');
            $names = array_map(fn($o) => $o->name(), iterator_to_array($bucket->objects(), false));
            check(in_array($object->name(), $names, true), 'object absent from list');
            $globbed = array_map(fn($o) => $o->name(), iterator_to_array($bucket->objects(['matchGlob' => '**/*.bin']), false));
            check($globbed === [$object->name()], 'glob listing mismatch: ' . json_encode($globbed, JSON_UNESCAPED_UNICODE));
            check(iterator_to_array($bucket->objects(['matchGlob' => 'missing-*']), false) === [], 'glob empty listing mismatch');
        });

        $runner->run('php-object-metadata-patch', function () use ($object) {
            $object->update(['metadata' => ['operation' => 'patch']]);
            check($object->reload()['metadata'] === ['operation' => 'patch'], 'object PATCH mismatch');
            // The PHP SDK exposes objects.patch only; a conditional PATCH stands in for Ruby's PUT.
            $object->update(['metadata' => ['operation' => 'put'], 'contentType' => 'application/octet-stream'],
                ['ifMetagenerationMatch' => $object->info()['metageneration']]);
            check($object->reload()['metadata'] === ['operation' => 'put'], 'object conditional PATCH mismatch');
            expectStatus(412, fn() => $object->update(['contentType' => 'text/plain'], ['ifMetagenerationMatch' => 999]));
        });

        $composed = null;
        $runner->run('php-copy-rewrite-compose', function () use ($bucket, $object, $size, &$objects, &$composed) {
            $copied = $object->copy($bucket, ['name' => 'copy.bin', 'ifGenerationMatch' => 0]);
            $objects[] = $copied;
            $rewritten = $object->rewrite($bucket, ['name' => 'rewrite.bin', 'ifGenerationMatch' => 0]);
            $objects[] = $rewritten;
            // No optional arguments: the SDK derives contentType from the .bin name.
            $composed = $bucket->compose([$object, $rewritten], 'compose.bin');
            $objects[] = $composed;
            check((int)$copied->info()['size'] === $size && (int)$rewritten->info()['size'] === $size
                && (int)$composed->info()['size'] === 2 * $size, 'derived object size mismatch');
            return 'objects.copy, objects.rewrite and default objects.compose';
        });

        $runner->run('php-download-bytes', function () use (&$objects, &$composed, $download, $source, $expected) {
            $twice = hash_init('sha256');
            for ($i = 0; $i < 2; $i++) {
                $in = fopen($source, 'rb');
                while (!feof($in)) hash_update($twice, fread($in, 64 * 1024));
                fclose($in);
            }
            $twice = hash_final($twice);
            foreach ($objects as $item) {
                // downloadToFile streams to disk with the SDK's checksum validation.
                $item->downloadToFile($download);
                $want = $item === $composed ? $twice : $expected;
                check(hash_file('sha256', $download) === $want, 'download bytes mismatch: ' . $item->name());
            }
            $range = $objects[0]->downloadAsString(['restOptions' => ['headers' => ['Range' => 'bytes=256-511']]]);
            check($range === file_get_contents($source, false, null, 256, 256), 'range bytes differ');
            return count($objects) . ' objects streamed to disk with matching SHA-256; ranged read';
        });
    }

    foreach ($objects as $item) $item->delete();
    $objects = [];
    $bucket->delete();
    $bucket = null;
} finally {
    dropBucket($bucket);
    unlink($source);
    unlink($download);
}

$runner->run('php-soft-delete-restore', function () use ($storage) {
    $soft = $storage->createBucket(uniqueName('php-soft-delete'));
    try {
        $soft->update(['softDeletePolicy' => ['retentionDurationSeconds' => 604800]]);
        check((int)$soft->reload()['softDeletePolicy']['retentionDurationSeconds'] === 604800, 'soft delete policy differs');
        $original = $soft->upload('recoverable PHP bytes', ['name' => 'recoverable']);
        $generation = $original->info()['generation'];
        $original->delete();
        check(!$soft->object('recoverable')->exists(), 'deleted file is live');
        $deleted = iterator_to_array($soft->objects(['softDeleted' => true]), false);
        check(count($deleted) === 1 && $deleted[0]->info()['generation'] === $generation, 'soft deleted listing differs');
        // Bucket::object() documents softDeleted but drops it in 2.6.1; info() forwards it to objects.get.
        check($soft->object('recoverable', ['generation' => $generation])->info(['softDeleted' => true])['name'] === 'recoverable',
            'soft deleted metadata missing');
        $restored = $soft->restore('recoverable', $generation);
        $info = $restored->info();
        check((int)$info['generation'] > (int)$generation && (int)$info['metageneration'] === 1, 'restore generation differs');
        check($restored->downloadAsString() === 'recoverable PHP bytes', 'restored bytes differ');
    } finally {
        dropBucket($soft);
    }
});

// Retention locks are tested only on an empty emulator bucket.
$runner->run('php-bucket-retention-policy', function () use ($storage) {
    $retention = $storage->createBucket(uniqueName('php-retention'));
    try {
        $retention->update(['retentionPolicy' => ['retentionPeriod' => 3600]]);
        check((int)$retention->reload()['retentionPolicy']['retentionPeriod'] === 3600, 'retention period differs');
        $retained = $retention->upload('retained bytes', ['name' => 'retained']);
        expectStatus(403, fn() => $retained->delete());
        // The policy must be removed before cleaning up this object.
        $retention->update(['retentionPolicy' => null]);
        $retained->delete();
        $retention->update(['retentionPolicy' => ['retentionPeriod' => 7200]]);
        $retention->reload();
        $retention->lockRetentionPolicy();
        check($retention->reload()['retentionPolicy']['isLocked'] === true, 'retention policy is not locked');
        $error = expectStatus(400, fn() => $retention->update(['retentionPolicy' => ['retentionPeriod' => 3600]]));
        check(str_contains($error->getMessage(), 'locked') && str_contains($error->getMessage(), 'extend'),
            'locked policy error lacks guidance: ' . detail($error));
    } finally {
        dropBucket($retention);
    }
});

// Keep move independent from the transfer journey so its default API stays visible.
$runner->run('php-object-move', function () use ($storage) {
    $move = $storage->createBucket(uniqueName('php-move'));
    try {
        $moved = $move->upload('PHP move bytes', ['name' => 'source'])->move('destination');
        check($moved->name() === 'destination', 'move returned wrong name');
        check(!$move->object('source')->exists(), 'move source still exists');
        check($move->object('destination')->downloadAsString() === 'PHP move bytes', 'move bytes differ');
    } finally {
        dropBucket($move);
    }
});

$runner->finish(sprintf('PHP %s; google/cloud-storage %s; %s', PHP_VERSION, \Composer\InstalledVersions::getPrettyVersion('google/cloud-storage'), $profile));
