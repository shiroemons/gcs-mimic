<?php
declare(strict_types=1);

require __DIR__ . '/vendor/autoload.php';

ini_set('display_errors', 'stderr');
set_error_handler(function ($severity, $message, $file, $line) {
    throw new ErrorException($message, 0, $severity, $file, $line);
});

use Google\Auth\Credentials\ServiceAccountCredentials;
use Google\Auth\HttpHandler\HttpHandlerFactory;
use Google\Cloud\Core\AnonymousCredentials;
use Google\Cloud\Core\Exception\ServiceException;
use Google\Cloud\Storage\StorageClient;
use GuzzleHttp\Client;
use GuzzleHttp\HandlerStack;
use GuzzleHttp\Middleware;

function check(bool $condition, string $message = 'unexpected result'): void {
    if (!$condition) throw new RuntimeException($message);
}
function expectCode(int $code, callable $call): void {
    try { $call(); }
    catch (ServiceException $e) {
        check($e->getCode() === $code, "expected HTTP $code: " . $e->getMessage());
        return;
    }
    throw new RuntimeException("expected HTTP $code, got success");
}
function failureDetail(Throwable $error): string {
    $cause = $error;
    while ($cause->getPrevious()) $cause = $cause->getPrevious();
    $message = $cause->getMessage();
    if ($error instanceof ServiceException && $error->getMetadata()) {
        $message = json_encode($error->getMetadata(), JSON_THROW_ON_ERROR | JSON_UNESCAPED_SLASHES | JSON_INVALID_UTF8_SUBSTITUTE);
    }
    // SDK resumable errors replace the cause with a bearer-capability URI.
    $message = preg_replace('~https?://[^\s"<>]+~', '[redacted URL]', $message);
    return substr(get_class($error) . ' HTTP ' . $error->getCode() . ': ' . $message, 0, 300);
}
function uniqueName(): string { return 'sc-php-' . bin2hex(random_bytes(6)); }
function newBucket(array $options = []) {
    global $storage;
    return $storage->createBucket(uniqueName(), $options);
}
function upload($bucket, string $name, string $data, array $options = []) {
    return $bucket->upload($data, ['name' => $name] + $options);
}
function names(iterable $objects): array {
    $names = [];
    foreach ($objects as $object) $names[] = $object->name();
    sort($names);
    return $names;
}
function seed($bucket, array $names): void {
    foreach ($names as $name) upload($bucket, $name, $name);
}
function bytes($object, string $want, array $options = []): void {
    check($object->downloadAsString($options) === $want, 'bytes differ');
}
function v4MatchesRequest(string $url, string $method, string $publicKey): bool {
    $parts = parse_url($url);
    parse_str($parts['query'], $query);
    $signature = hex2bin($query['X-Goog-Signature']);
    unset($query['X-Goog-Signature']);
    check($query['X-Goog-SignedHeaders'] === 'host', 'unexpected signed headers');
    $encoded = [];
    foreach ($query as $name => $value) $encoded[rawurlencode($name)] = rawurlencode($value);
    ksort($encoded, SORT_STRING);
    $parameters = [];
    foreach ($encoded as $name => $value) $parameters[] = "$name=$value";
    $host = $parts['host'] . (isset($parts['port']) ? ':' . $parts['port'] : '');
    $canonical = "$method\n{$parts['path']}\n" . implode('&', $parameters) . "\nhost:$host\n\nhost\nUNSIGNED-PAYLOAD";
    $scope = explode('/', $query['X-Goog-Credential'], 2)[1];
    $stringToSign = "GOOG4-RSA-SHA256\n{$query['X-Goog-Date']}\n$scope\n" . hash('sha256', $canonical);
    return openssl_verify($stringToSign, $signature, $publicKey, OPENSSL_ALGO_SHA256) === 1;
}
function v2MatchesRequest(string $url, string $method, string $publicKey): bool {
    $parts = parse_url($url);
    parse_str($parts['query'], $query);
    $canonical = "$method\n\n\n{$query['Expires']}\n{$parts['path']}";
    return openssl_verify($canonical, base64_decode($query['Signature'], true), $publicKey, OPENSSL_ALGO_SHA256) === 1;
}

$origin = rtrim(getenv('SCORECARD_ENDPOINT') ?: '', '/');
$parts = parse_url($origin);
check(in_array($parts['host'] ?? '', ['127.0.0.1', 'localhost', '::1'], true), 'local endpoint required');
check(in_array($parts['scheme'] ?? '', ['http', 'https'], true) &&
    !isset($parts['path']) && !isset($parts['query']) && !isset($parts['fragment']) && !isset($parts['user']), 'origin required');
$project = getenv('SCORECARD_PROJECT') ?: 'local-project';
$key = file_get_contents(getenv('SCORECARD_SIGNING_KEY') ?: __DIR__ . '/../../fixtures/signing/test-only-private.pem');
$signer = new ServiceAccountCredentials(null, ['type' => 'service_account',
    'client_email' => 'local-admin@example.invalid', 'private_key' => $key]);
$publicKey = openssl_pkey_get_details(openssl_pkey_get_private($key))['key'];
$stack = HandlerStack::create();
$stack->push(Middleware::mapRequest(function ($request) use ($origin) {
    $uri = $request->getUri();
    check($uri->getScheme() . '://' . $uri->getAuthority() === $origin, 'request escaped emulator');
    return $request;
}));
$config = ['handler' => $stack, 'timeout' => 20, 'allow_redirects' => false, 'proxy' => ''];
if (getenv('SCORECARD_CA_CERT')) $config['verify'] = getenv('SCORECARD_CA_CERT');
if (getenv('SCORECARD_DOCKER_GATEWAY')) {
    // Route loopback through Docker's gateway without changing signed URLs or Host.
    $port = $parts['port'] ?? ($parts['scheme'] === 'https' ? 443 : 80);
    $config['curl'] = [CURLOPT_CONNECT_TO => ["{$parts['host']}:$port:host.docker.internal:$port"]];
}
$http = new Client($config);
$storage = new StorageClient(['projectId' => $project, 'apiEndpoint' => $origin,
    'credentialsFetcher' => new AnonymousCredentials(), 'retries' => 0, 'requestTimeout' => 20,
    'httpHandler' => HttpHandlerFactory::build($http)]);
if (($argv[1] ?? '') === '--transport-check') {
    $url = $storage->bucket('signing-check')->object('signing-check/object')->signedUrl(
        new DateTimeImmutable('+5 minutes'), ['version' => 'v4', 'credentialsFetcher' => $signer,
            'bucketBoundHostname' => $origin, 'scheme' => $parts['scheme']]);
    check(v4MatchesRequest($url, 'GET', $publicKey), 'V4 does not sign the actual host/path');
    check(!v4MatchesRequest(str_replace('/signing-check/object?', '/signing-check/changed?', $url), 'GET', $publicKey),
        'V4 accepted a changed object path');
    $url = $storage->bucket('signing-check')->object('object')->signedUrl(
        new DateTimeImmutable('+5 minutes'), ['version' => 'v2', 'credentialsFetcher' => $signer,
            'bucketBoundHostname' => $origin . '/signing-check']);
    check(v2MatchesRequest($url, 'GET', $publicKey), 'V2 does not sign the actual object path');
    check(!v2MatchesRequest($url, 'PUT', $publicKey), 'V2 accepted a changed method');
    $detail = failureDetail(new ServiceException('Resume at https://127.0.0.1/upload?upload_id=redact-me', 400, null,
        ['error' => ['code' => 400, 'message' => 'Final metadata requires a final request',
            'location' => 'https://127.0.0.1/upload?upload_id=redact-me']]));
    check(str_contains($detail, 'HTTP 400') && str_contains($detail, 'Final metadata'), 'lost root cause');
    check(!str_contains($detail, 'redact-me'), 'structured session capability leaked');
    check(!str_contains(failureDetail(new RuntimeException('Resume at https://127.0.0.1/upload?upload_id=redact-me')),
        'redact-me'), 'session capability leaked');
    try { $http->get('https://storage.googleapis.com/'); throw new LogicException('external origin accepted'); }
    catch (RuntimeException $e) { check($e->getMessage() === 'request escaped emulator', $e->getMessage()); }
    check($http->get($origin . '/')->getStatusCode() === 200, 'TLS fixture status');
    echo "PASS: PHP transport CA/SAN and exact-origin guard\n";
    exit;
}
$cases = [];
function probe(string $id, callable $run): void { global $cases; $cases[$id] = $run; }

probe('bucket.create_get_delete', function () use ($storage) {
    $b = newBucket();
    check($storage->bucket($b->name())->exists(), 'bucket lookup');
    $b->delete();
    check(!$b->exists(), 'deleted bucket visible');
});
probe('bucket.list_prefix_paging', function () use ($storage) {
    $prefix = uniqueName();
    for ($i = 0; $i < 3; $i++) $storage->createBucket("$prefix-$i");
    $found = [];
    foreach ($storage->buckets(['prefix' => $prefix, 'maxResults' => 2])->iterateByPage() as $page) {
        check(count($page) <= 2, 'bucket page exceeds 2');
        $found = array_merge($found, names($page));
    }
    sort($found);
    check($found === ["$prefix-0", "$prefix-1", "$prefix-2"], 'bucket names');
});
probe('bucket.update_labels', function () {
    $b = newBucket(['labels' => ['a' => '1', 'b' => '2']]);
    $b->update(['labels' => ['a' => null, 'b' => '2', 'c' => '3']]);
    check($b->reload()['labels'] == ['b' => '2', 'c' => '3'], 'labels differ');
});
probe('bucket.metageneration_precondition', function () {
    $b = newBucket();
    expectCode(412, fn() => $b->update(['storageClass' => 'NEARLINE', 'ifMetagenerationMatch' => 999]));
});
probe('bucket.versioning_history', function () {
    $b = newBucket(['versioning' => ['enabled' => true]]);
    $first = upload($b, 'v.txt', 'one');
    $generation = $first->info()['generation'];
    upload($b, 'v.txt', 'two');
    check(count(iterator_to_array($b->objects(['versions' => true]))) === 2, 'version count');
    bytes($b->object('v.txt', ['generation' => $generation]), 'one');
});
foreach ([
    ['lifecycle', 'lifecycle', ['rule' => [['action' => ['type' => 'Delete'], 'condition' => ['age' => 30]]]]],
    ['cors', 'cors', [['origin' => ['https://app.example'], 'method' => ['GET'], 'maxAgeSeconds' => 3600]]],
    ['soft_delete_policy', 'softDeletePolicy', ['retentionDurationSeconds' => '864000']],
    ['autoclass', 'autoclass', ['enabled' => true]],
] as [$id, $field, $value]) probe("bucket.$id", function () use ($field, $value) {
    $b = newBucket();
    $b->update([$field => $value]);
    $got = $b->reload()[$field];
    foreach ($value as $key => $want) check(($got[$key] ?? null) == $want, "$field differs");
});
probe('bucket.retention_policy', function () {
    $b = newBucket();
    $b->update(['retentionPolicy' => ['retentionPeriod' => '3600']]);
    $file = upload($b, 'r', 'r');
    expectCode(403, fn() => $file->delete());
});
probe('bucket.retention_lock', function () {
    $b = newBucket();
    $b->update(['retentionPolicy' => ['retentionPeriod' => '3600']]);
    $b->reload();
    $b->lockRetentionPolicy();
    check($b->reload()['retentionPolicy']['isLocked'] === true, 'policy not locked');
});
probe('bucket.uniform_bucket_level_access', function () {
    $b = newBucket();
    $b->update(['iamConfiguration' => ['uniformBucketLevelAccess' => ['enabled' => true]]]);
    check($b->reload()['iamConfiguration']['uniformBucketLevelAccess']['enabled'] === true, 'UBLA not enabled');
});
probe('bucket.public_access_prevention', function () {
    $b = newBucket();
    $b->update(['iamConfiguration' => ['publicAccessPrevention' => 'enforced']]);
    check($b->reload()['iamConfiguration']['publicAccessPrevention'] === 'enforced', 'PAP not enforced');
});
probe('bucket.iam_policy', function () {
    $b = newBucket();
    $policy = $b->iam()->policy(['requestedPolicyVersion' => 3]);
    $policy['bindings'][] = ['role' => 'roles/storage.objectViewer', 'members' => ['user:someone@example.invalid']];
    $b->iam()->setPolicy($policy);
    $found = false;
    foreach ($b->iam()->reload(['requestedPolicyVersion' => 3])['bindings'] as $binding) {
        if ($binding['role'] === 'roles/storage.objectViewer' && in_array('user:someone@example.invalid', $binding['members'])) $found = true;
    }
    check($found, 'missing binding');
});
probe('bucket.iam_test_permissions', function () {
    check(count(newBucket()->iam()->testPermissions(['storage.buckets.get', 'storage.objects.list'])) > 0, 'no permissions');
});
foreach (['acl' => 'acl', 'default_object_acl' => 'defaultAcl'] as $id => $method) probe("bucket.$id", function () use ($method) {
    $b = newBucket();
    $b->$method()->add('allAuthenticatedUsers', 'READER');
    $entries = $b->$method()->get();
    check(count(array_filter($entries, fn($e) => $e['entity'] === 'allAuthenticatedUsers' && $e['role'] === 'READER')) > 0, 'ACL missing');
});
probe('bucket.notifications', function () use ($project) {
    $b = newBucket();
    $notification = $b->createNotification("projects/$project/topics/scorecard", ['payloadFormat' => 'JSON_API_V1']);
    $ids = [];
    foreach ($b->notifications() as $n) $ids[] = $n->id();
    check(in_array($notification->id(), $ids), 'notification missing');
    $notification->delete();
});
probe('project.hmac_keys', function () use ($storage) {
    $created = $storage->createHmacKey('sa@example.invalid');
    $key = $created->hmacKey();
    check($created->secret() !== '' && $key->accessId() !== '', 'empty HMAC key');
    $key->update('INACTIVE');
    $key->delete();
});
probe('project.service_account', function () use ($storage) {
    check(str_contains($storage->getServiceAccount(), '@'), 'service account missing');
});
probe('object.write_read_small', function () { bytes(upload(newBucket(), 'dir/日本語 +%?.txt', 'hello'), 'hello'); });
probe('object.resumable_large', function () {
    $b = newBucket();
    $data = str_repeat('a', 3 * 1024 * 1024 + 123);
    $file = upload($b, 'big.bin', $data, ['resumable' => true, 'chunkSize' => 256 * 1024]);
    check((int)$file->info()['size'] === strlen($data), 'size differs');
    bytes($file, $data);
});
probe('object.metadata_on_write', function () {
    $want = ['contentType' => 'text/plain', 'cacheControl' => 'no-cache', 'contentDisposition' => 'attachment',
        'contentLanguage' => 'ja', 'metadata' => ['k' => 'v']];
    $file = upload(newBucket(), 'meta.txt', 'x', ['metadata' => $want]);
    $got = $file->reload();
    foreach ($want as $key => $value) check(($got[$key] ?? null) == $value, "metadata $key");
});
probe('object.checksum_rejected', function () {
    $b = newBucket();
    try { upload($b, 'crc.txt', 'payload', ['metadata' => ['crc32c' => 'AAAAAA==']]); }
    catch (ServiceException $e) {
        check($e->getCode() === 400 && preg_match('/checksum|crc32c/i', $e->getMessage()) === 1, $e->getMessage());
        check(!$b->object('crc.txt')->exists(), 'rejected upload left an object');
        return;
    }
    throw new RuntimeException('wrong CRC32C accepted');
});
probe('object.md5_accepted', function () {
    $md5 = base64_encode(md5('md5 payload', true));
    $file = upload(newBucket(), 'md5.txt', 'md5 payload', ['metadata' => ['md5Hash' => $md5]]);
    check($file->reload()['md5Hash'] === $md5, 'MD5 differs');
});
probe('object.range_reads', function () {
    $file = upload(newBucket(), 'range.txt', '0123456789');
    foreach (['bytes=2-5' => '2345', 'bytes=7-' => '789', 'bytes=-3' => '789'] as $range => $want) {
        bytes($file, $want, ['restOptions' => ['headers' => ['Range' => $range]]]);
    }
});
probe('object.update_metadata', function () {
    $file = upload(newBucket(), 'u.txt', 'x', ['metadata' => ['metadata' => ['a' => '1', 'b' => '2']]]);
    $file->update(['contentType' => 'text/csv', 'metadata' => ['a' => null, 'c' => '3']]);
    $got = $file->reload();
    check($got['contentType'] === 'text/csv' && $got['metadata'] == ['b' => '2', 'c' => '3'], 'metadata differs');
});
probe('object.delete_not_found', function () {
    $file = upload(newBucket(), 'd.txt', 'x');
    $file->delete();
    check(!$file->exists(), 'deleted object visible');
    expectCode(404, fn() => $file->delete());
});
probe('object.does_not_exist_precondition', function () {
    $b = newBucket();
    upload($b, 'once.txt', 'first', ['ifGenerationMatch' => 0]);
    expectCode(412, fn() => upload($b, 'once.txt', 'second', ['ifGenerationMatch' => 0]));
    bytes($b->object('once.txt'), 'first');
});
probe('object.generation_match_precondition', function () {
    $file = upload(newBucket(), 'g.txt', 'x');
    $generation = $file->info()['generation'];
    expectCode(412, fn() => $file->delete(['ifGenerationMatch' => (string)((int)$generation + 1)]));
    $file->delete(['ifGenerationMatch' => $generation]);
});
probe('object.metageneration_match_precondition', function () {
    $file = upload(newBucket(), 'm.txt', 'x');
    expectCode(412, fn() => $file->update(['contentType' => 'text/plain'], ['ifMetagenerationMatch' => 999]));
});
probe('object.list_prefix_delimiter', function () {
    $b = newBucket();
    seed($b, ['a/1', 'a/2', 'b/1', 'c']);
    $list = $b->objects(['delimiter' => '/']);
    check(names($list) === ['c'], 'names differ');
    $prefixes = $list->prefixes();
    sort($prefixes);
    check($prefixes === ['a/', 'b/'], 'prefixes differ');
    check(names($b->objects(['prefix' => 'a/'])) === ['a/1', 'a/2'], 'prefix listing');
});
probe('object.list_paging', function () {
    $b = newBucket();
    seed($b, ['p1', 'p2', 'p3', 'p4', 'p5']);
    $found = []; $count = 0;
    foreach ($b->objects(['maxResults' => 2])->iterateByPage() as $page) {
        check(count($page) <= 2, 'object page exceeds 2');
        $count++;
        $found = array_merge($found, names($page));
    }
    sort($found);
    check($count >= 3 && $found === ['p1', 'p2', 'p3', 'p4', 'p5'], 'paged listing');
});
foreach ([
    ['list_offsets', ['a', 'b', 'c', 'd'], ['startOffset' => 'b', 'endOffset' => 'd'], ['b', 'c']],
    ['list_match_glob', ['x/a.txt', 'x/b.csv', 'y/c.txt'], ['matchGlob' => '**/*.txt'], ['x/a.txt', 'y/c.txt']],
] as [$id, $objects, $options, $want]) probe("object.$id", function () use ($objects, $options, $want) {
    $b = newBucket();
    seed($b, $objects);
    check(names($b->objects($options)) === $want, 'listing differs');
});
probe('object.copy_with_metadata', function () {
    $source = upload(newBucket(), 'src', 'copy me');
    $dest = newBucket();
    $file = $source->copy($dest, ['name' => 'dst', 'contentType' => 'text/plain', 'metadata' => ['copied' => 'yes']]);
    $got = $file->reload();
    check($got['contentType'] === 'text/plain' && $got['metadata'] == ['copied' => 'yes'], 'copy metadata');
    bytes($file, 'copy me');
});
probe('object.rewrite_storage_class', function () {
    $b = newBucket();
    $data = str_repeat('a', 2 * 1024 * 1024 + 7);
    $file = upload($b, 'src', $data)->rewrite($b, ['name' => 'dst', 'storageClass' => 'NEARLINE']);
    check($file->reload()['storageClass'] === 'NEARLINE', 'storage class differs');
    bytes($file, $data);
});
probe('object.compose', function () {
    $b = newBucket();
    seed($b, ['p1', 'p2', 'p3']);
    $file = $b->compose(['p1', 'p2', 'p3'], 'all', ['metadata' => ['contentType' => 'text/plain']]);
    check((int)$file->info()['componentCount'] === 3, 'component count');
    bytes($file, 'p1p2p3');
});
probe('object.move', function () {
    $b = newBucket();
    upload($b, 'old', 'moved')->move('new');
    check(!$b->object('old')->exists(), 'source remains');
    bytes($b->object('new'), 'moved');
});
probe('object.acl', function () {
    $file = upload(newBucket(), 'acl.txt', 'x');
    $file->acl()->add('allUsers', 'READER');
    check(count(array_filter($file->acl()->get(), fn($e) => $e['entity'] === 'allUsers' && $e['role'] === 'READER')) > 0, 'ACL missing');
});
probe('object.holds', function () {
    $file = upload(newBucket(), 'hold.txt', 'x');
    $file->update(['temporaryHold' => true]);
    expectCode(403, fn() => $file->delete());
    $file->update(['temporaryHold' => false]);
    $file->delete();
});
probe('object.retention', function () {
    $file = upload(newBucket(['enableObjectRetention' => true]), 'ret.txt', 'x');
    $file->update(['retention' => ['mode' => 'Unlocked', 'retainUntilTime' => gmdate('Y-m-d\TH:i:s\Z', time() + 3600)]]);
    check($file->reload()['retention']['mode'] === 'Unlocked', 'retention not stored');
    expectCode(403, fn() => $file->delete());
});
probe('object.custom_time', function () {
    $file = upload(newBucket(), 'ct.txt', 'x');
    $file->update(['customTime' => '2026-01-02T03:04:05Z']);
    check(strtotime($file->reload()['customTime']) === strtotime('2026-01-02T03:04:05Z'), 'customTime differs');
});
probe('object.storage_class_on_write', function () {
    $file = upload(newBucket(), 'cold', 'x', ['metadata' => ['storageClass' => 'COLDLINE']]);
    check($file->reload()['storageClass'] === 'COLDLINE', 'storage class differs');
});
probe('object.gzip_transcoding', function () {
    $plain = str_repeat('compress me ', 50);
    $zipped = gzencode($plain);
    $file = upload(newBucket(), 'gz.txt', $zipped, ['metadata' => ['contentType' => 'text/plain', 'contentEncoding' => 'gzip']]);
    bytes($file, $plain);
    bytes($file, $zipped, ['restOptions' => ['decode_content' => false, 'headers' => ['Accept-Encoding' => 'gzip']]]);
});
probe('object.customer_supplied_key', function () {
    $b = newBucket();
    $key = base64_encode(random_bytes(32));
    $file = upload($b, 'csek.txt', 'secret', ['encryptionKey' => $key]);
    try { $b->object('csek.txt')->downloadAsString(); }
    catch (ServiceException $e) {
        check(in_array($e->getCode(), [400, 403], true), $e->getMessage());
        bytes($file, 'secret');
        return;
    }
    throw new RuntimeException('read without key succeeded');
});
probe('object.soft_delete_restore', function () {
    $b = newBucket(['softDeletePolicy' => ['retentionDurationSeconds' => '604800']]);
    $file = upload($b, 'sd.txt', 'restore me');
    $generation = $file->info()['generation'];
    $file->delete();
    check(names($b->objects(['softDeleted' => true])) === ['sd.txt'], 'soft-deleted listing');
    $b->restore('sd.txt', $generation);
    bytes($b->object('sd.txt'), 'restore me');
});
foreach ([['v4_get', 'v4', 'GET'], ['v4_put', 'v4', 'PUT'], ['v2_get', 'v2', 'GET']] as [$id, $version, $method]) {
    probe("signed.$id", function () use ($http, $origin, $signer, $publicKey, $version, $method) {
        $b = newBucket();
        if ($method === 'GET') upload($b, 'signed.txt', 'signed');
        $file = $b->object('signed.txt');
        $signingObject = $version === 'v4' ? $b->object($b->name() . '/signed.txt') : $file;
        $signingHost = $version === 'v4' ? $origin : "$origin/{$b->name()}";
        $url = $signingObject->signedUrl(new DateTimeImmutable('+5 minutes'), ['version' => $version, 'method' => $method,
            'credentialsFetcher' => $signer, 'bucketBoundHostname' => $signingHost,
            'scheme' => parse_url($origin, PHP_URL_SCHEME)]);
        if ($version === 'v4') check(v4MatchesRequest($url, $method, $publicKey), 'V4 does not sign the actual host/path');
        else check(v2MatchesRequest($url, $method, $publicKey), 'V2 does not sign the actual object path');
        $response = $http->request($method, $url, ['body' => $method === 'PUT' ? 'uploaded' : '']);
        check($response->getStatusCode() === 200, 'signed status');
        if ($method === 'GET') check((string)$response->getBody() === 'signed', 'signed bytes');
        else bytes($file, 'uploaded');
    });
}
probe('signed.post_policy_v4', function () use ($http, $origin, $signer) {
    $b = newBucket();
    $policy = $b->generateSignedPostPolicyV4('post.txt', new DateTimeImmutable('+5 minutes'), ['credentialsFetcher' => $signer]);
    $form = [];
    foreach ($policy['fields'] as $name => $value) $form[] = ['name' => $name, 'contents' => $value];
    $form[] = ['name' => 'file', 'contents' => 'posted', 'filename' => 'post.txt'];
    $response = $http->post("$origin/{$b->name()}", ['multipart' => $form]);
    check($response->getStatusCode() >= 200 && $response->getStatusCode() < 300, 'POST status');
    bytes($b->object('post.txt'), 'posted');
});

foreach ($cases as $id => $run) {
    if (!str_contains($id, getenv('SCORECARD_RUN') ?: '')) continue;
    $result = ['sdk' => 'php', 'id' => $id, 'area' => str_starts_with($id, 'project.') ? 'bucket' : explode('.', $id)[0], 'pass' => true];
    try { set_time_limit(60); $run(); }
    catch (Throwable $e) {
        $result['pass'] = false;
        $result['detail'] = failureDetail($e);
    }
    echo json_encode($result, JSON_THROW_ON_ERROR | JSON_INVALID_UTF8_SUBSTITUTE | JSON_UNESCAPED_UNICODE) . "\n";
    flush();
}
