<?php
declare(strict_types=1);

use Google\Auth\Credentials\ServiceAccountCredentials;

/**
 * Sends one signed request unchanged through the guarded client and returns its bounded body.
 * A 403 must be the XML SignatureDoesNotMatch error, not some unrelated denial.
 */
function signedCall(Harness $h, string $method, string $url, ?string $contentType, int $expected,
                    ?string $changedHost = null, ?callable $inspect = null): string
{
    $parts = parse_url($url);
    check(sameOrigin($h->origin, $parts['scheme'], $parts['host'], $parts['port'] ?? null),
        'refusing signed request outside the emulator origin');
    $headers = [];
    if ($contentType !== null) $headers['Content-Type'] = $contentType;
    if ($changedHost !== null) $headers['Host'] = $changedHost;
    // Guzzle's streaming handler ignores cURL options (CONNECT_TO), so the bounded check
    // reads the cURL handler's temporary body stream instead.
    $options = ['http_errors' => false, 'headers' => $headers];
    if ($method === 'PUT') $options['body'] = 'PHP signed private bytes';
    $response = $h->http->request($method, $url, $options);
    $body = $response->getBody();
    $bytes = '';
    while (!$body->eof()) {
        $bytes .= $body->read(8192);
        check(strlen($bytes) <= 64 * 1024, 'signed response exceeded test limit');
    }
    check($response->getStatusCode() === $expected,
        "signed $method expected $expected, got {$response->getStatusCode()}: " . substr($bytes, 0, 300));
    if ($expected === 403) {
        check(str_contains($bytes, '<Code>SignatureDoesNotMatch</Code>'), 'unexpected signature error: ' . substr($bytes, 0, 300));
    }
    if ($inspect) $inspect($response);
    return $bytes;
}

/** Recomputes the V4 string to sign for a given canonical Host and checks the URL's signature. */
function v4SignatureCovers(string $url, string $method, string $host, string $contentType, string $publicKey): bool
{
    $parts = parse_url($url);
    $pairs = explode('&', $parts['query']);
    $signature = null;
    $canonical = [];
    foreach ($pairs as $pair) {
        if (str_starts_with($pair, 'X-Goog-Signature=')) $signature = hex2bin(substr($pair, 17));
        else $canonical[] = $pair;
    }
    parse_str($parts['query'], $query);
    $request = implode("\n", [$method, $parts['path'], implode('&', $canonical),
        "content-type:$contentType\nhost:$host\n", 'content-type;host', 'UNSIGNED-PAYLOAD']);
    $scope = explode('/', $query['X-Goog-Credential'], 2)[1];
    $toSign = implode("\n", ['GOOG4-RSA-SHA256', $query['X-Goog-Date'], $scope, hash('sha256', $request)]);
    return openssl_verify($toSign, $signature, $publicKey, OPENSSL_ALGO_SHA256) === 1;
}

/** Strict-profile V4 and V2 signed GET/PUT through the official signer with the public fixture key. */
function signedUrlJourney(Harness $h, $bucket, array &$cleanup): array
{
    $key = file_get_contents(__DIR__ . '/../../fixtures/signing/test-only-private.pem');
    check(is_string($key) && $key !== '', 'signing fixture key is unreadable');
    $issuer = 'local-admin@example.invalid';
    // A local signer: signBlob uses OpenSSL in-process and never requests a token.
    $signer = new ServiceAccountCredentials(null, ['type' => 'service_account', 'client_email' => $issuer,
        'private_key' => $key]);
    $publicKey = openssl_pkey_get_details(openssl_pkey_get_private($key))['key'];
    [$scheme, $host, $port] = $h->origin;
    $authority = "$host:$port";
    $expires = fn() => new DateTimeImmutable('+300 seconds');
    $notes = [];

    // bucketBoundHostname binds host:port but drops the bucket path, so the signing object
    // name carries "<bucket>/signed.txt" to produce /<bucket>/signed.txt on the final endpoint.
    $signingObject = $bucket->object($bucket->name() . '/signed.txt');
    $signed = fn(string $method, array $extra = []) => $signingObject->signedUrl($expires(), [
        'version' => 'v4', 'method' => $method, 'credentialsFetcher' => $signer,
        'bucketBoundHostname' => $authority, 'scheme' => $scheme, 'contentType' => 'text/plain'] + $extra);
    $putUrl = $signed('PUT');
    $getUrl = $signed('GET');
    // The later cases read the object this case writes; a failure here fails them visibly too.
    $notes['php-signed-v4-put-get'] = function () use ($h, $bucket, $putUrl, $getUrl, $scheme, $authority, $publicKey, &$cleanup) {
        $putParts = parse_url($putUrl);
        check($putParts['scheme'] === $scheme && "{$putParts['host']}:{$putParts['port']}" === $authority, 'PHP V4 signer lost the origin');
        check($putParts['path'] === "/{$bucket->name()}/signed.txt", 'PHP V4 signer changed the path');
        check(v4SignatureCovers($putUrl, 'PUT', $authority, 'text/plain', $publicKey), 'PHP V4 signature does not cover the actual host');
        signedCall($h, 'PUT', $putUrl, 'text/plain', 200);
        $cleanup[] = $bucket->object('signed.txt');
        check(signedCall($h, 'GET', $getUrl, 'text/plain', 200) === 'PHP signed private bytes', 'signed bytes differ');
        return 'bucket-bound host:port with an explicit bucket/name path, sent unchanged';
    };

    $normal = $bucket->object('signed.txt');
    foreach ([null, 'v2'] as $version) {
        $label = $version === null ? 'default' : 'explicit';
        $notes["php-signed-v2-$label"] = function () use ($h, $normal, $version, $signer, $authority, $bucket, $expires, $scheme) {
            // The V2 signer binds only the path; bucketBoundHostname "<host:port>/<bucket>" places it on the endpoint.
            $options = ['method' => 'GET', 'credentialsFetcher' => $signer,
                'bucketBoundHostname' => "$authority/{$bucket->name()}"];
            if ($version !== null) $options['version'] = $version;
            $get = $normal->signedUrl($expires(), $options);
            $put = $normal->signedUrl($expires(), ['method' => 'PUT', 'contentType' => 'text/plain'] + $options);
            check(str_starts_with($get, "https://$authority/{$bucket->name()}/signed.txt?"), "PHP V2 URL differs: scheme/authority/path");
            if ($scheme === 'http') {
                return 'SDK V2 signer hardcodes https; URL asserted but not sent to the HTTP endpoint';
            }
            check(signedCall($h, 'GET', $get, null, 200) === 'PHP signed private bytes', 'PHP V2 signed bytes differ');
            signedCall($h, 'PUT', $put, 'changed', 403);
            signedCall($h, 'PUT', $put, 'text/plain', 200);
            signedCall($h, 'GET', $put, 'text/plain', 403);
            signedCall($h, 'GET', str_replace('/signed.txt?', '/changed.txt?', $get), null, 403);
            return 'GET/PUT unchanged; changed Content-Type, method and path rejected';
        };
    }

    $notes['php-signed-v4-default-endpoint'] = function () use ($h, $normal, $signer, $expires, $authority, $publicKey) {
        // Without bucketBoundHostname the SDK signs and emits storage.googleapis.com.
        $url = $normal->signedUrl($expires(), ['version' => 'v4', 'method' => 'GET', 'credentialsFetcher' => $signer,
            'contentType' => 'text/plain']);
        check(str_starts_with($url, 'https://storage.googleapis.com/'), 'PHP default V4 URL host changed');
        check(v4SignatureCovers($url, 'GET', 'storage.googleapis.com', 'text/plain', $publicKey), 'default canonical host differs');
        check(!v4SignatureCovers($url, 'GET', $authority, 'text/plain', $publicKey), 'default signer bound the endpoint host');
        try {
            $h->http->request('GET', $url);
        } catch (OriginError) {
            return 'default signer binds storage.googleapis.com; the guard refuses it without contacting Google';
        }
        throw new RuntimeException('default signed URL was sent outside the emulator origin');
    };

    $notes['php-signed-v4-tamper-rejected'] = function () use ($h, $putUrl, $getUrl) {
        signedCall($h, 'PUT', $putUrl, 'changed', 403);
        signedCall($h, 'PUT', $putUrl, null, 403);
        signedCall($h, 'PUT', "$putUrl&tampered=1", 'text/plain', 403);
        signedCall($h, 'PUT', str_replace('/signed.txt?', '/changed.txt?', $putUrl), 'text/plain', 403);
        signedCall($h, 'PUT', $putUrl, 'text/plain', 403, 'changed.invalid');
        signedCall($h, 'GET', $putUrl, 'text/plain', 403);
        signedCall($h, 'PUT', $getUrl, 'text/plain', 403);
        return 'changed/missing Content-Type, query, object, Host and method';
    };

    $notes['php-signed-v4-response-overrides'] = function () use ($h, $signed, $bucket) {
        $disposition = 'attachment; filename="PHP + file.csv"';
        $url = $signed('GET', ['responseDisposition' => $disposition, 'responseType' => 'text/csv']);
        $bytes = signedCall($h, 'GET', $url, 'text/plain', 200, null, function ($response) use ($disposition) {
            check($response->getHeaderLine('Content-Disposition') === $disposition, 'PHP disposition override differs');
            check($response->getHeaderLine('Content-Type') === 'text/csv', 'PHP type override differs');
        });
        check($bytes === 'PHP signed private bytes', 'PHP override bytes differ');
        $changed = str_replace('text%2Fcsv', 'text%2Fhtml', $url);
        check($changed !== $url, 'override tamper did not change the URL');
        signedCall($h, 'GET', $changed, 'text/plain', 403);
        check($bucket->object('signed.txt')->info()['contentType'] === 'text/plain', 'override mutated metadata');
        return 'official response options, exact headers and bytes, value tamper rejected, metadata unchanged';
    };

    $notes['php-signed-anonymous-denied'] = function () use ($h, $bucket) {
        signedCall($h, 'GET', "{$h->endpoint}/{$bucket->name()}/signed.txt", null, 401);
        return 'unsigned XML GET without a bearer token';
    };
    return $notes;
}
