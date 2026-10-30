<?php
declare(strict_types=1);

require __DIR__ . '/vendor/autoload.php';

use Google\Auth\FetchAuthTokenInterface;
use Google\Auth\HttpHandler\HttpHandlerFactory;
use Google\Cloud\Core\AnonymousCredentials;
use Google\Cloud\Core\Exception\ServiceException;
use Google\Cloud\Storage\StorageClient;
use GuzzleHttp\Client;
use GuzzleHttp\HandlerStack;
use Psr\Http\Message\RequestInterface;
use Psr\Http\Message\ResponseInterface;

ini_set('display_errors', 'stderr');
set_error_handler(function (int $severity, string $message, string $file, int $line): bool {
    throw new ErrorException($message, 0, $severity, $file, $line);
});

final class OriginError extends RuntimeException {}

/** Supplies one fixed local fixture token, so the SDK never runs credential discovery. */
final class StaticTokenCredentials implements FetchAuthTokenInterface
{
    public function __construct(private string $token) {}
    public function fetchAuthToken(?callable $httpHandler = null): array { return ['access_token' => $this->token]; }
    public function getCacheKey(): ?string { return null; }
    public function getLastReceivedToken(): array { return ['access_token' => $this->token]; }
}

function check(bool $condition, string $message): void
{
    if (!$condition) throw new RuntimeException($message);
}

/** Runs $call and requires a ServiceException with the given HTTP status; returns it. */
function expectStatus(int $code, callable $call): ServiceException
{
    try {
        $call();
    } catch (ServiceException $e) {
        check($e->getCode() === $code, "expected HTTP $code, got {$e->getCode()}: " . detail($e));
        return $e;
    }
    throw new RuntimeException("expected HTTP $code, got success");
}

function detail(Throwable $error): string
{
    // Resumable upload errors can embed the session URL, which is a bearer capability.
    $message = preg_replace('~https?://[^\s"<>]+~', '[redacted URL]', $error->getMessage());
    return substr(get_class($error) . ': ' . $message, 0, 400);
}

/**
 * Accepts only an explicit loopback HTTP(S) origin without path, query, fragment or userinfo.
 * Returns [scheme, host as it appears in URLs, port].
 */
function emulatorOrigin(string $raw): array
{
    $parts = parse_url($raw);
    $invalid = new InvalidArgumentException('GCS_MIMIC_ENDPOINT must be a loopback HTTP or HTTPS origin');
    if ($parts === false || !in_array($parts['scheme'] ?? '', ['http', 'https'], true) || !isset($parts['host'])
        || isset($parts['user']) || isset($parts['pass']) || isset($parts['query']) || isset($parts['fragment'])
        || !in_array($parts['path'] ?? '', ['', '/'], true)) {
        throw $invalid;
    }
    $host = $parts['host'];
    $address = trim($host, '[]');
    $loopback = $host === 'localhost' || $address === '::1'
        || (filter_var($address, FILTER_VALIDATE_IP, FILTER_FLAG_IPV4) !== false && str_starts_with($address, '127.'));
    if (!$loopback) throw $invalid;
    return [$parts['scheme'], $host, $parts['port'] ?? ($parts['scheme'] === 'https' ? 443 : 80)];
}

function originString(array $origin): string
{
    return "{$origin[0]}://{$origin[1]}:{$origin[2]}";
}

function sameOrigin(array $origin, string $scheme, string $host, ?int $port): bool
{
    $port ??= $scheme === 'https' ? 443 : 80;
    return [$scheme, strtolower($host), $port] === [$origin[0], strtolower($origin[1]), $origin[2]];
}

/**
 * Guzzle middleware that refuses every request outside the emulator origin (including
 * Google APIs and the metadata server) and records resumable chunks and Location headers.
 */
function originGuard(array $origin, ArrayObject $observed): callable
{
    return function (callable $handler) use ($origin, $observed): callable {
        return function (RequestInterface $request, array $options) use ($handler, $origin, $observed) {
            $uri = $request->getUri();
            if (!sameOrigin($origin, $uri->getScheme(), $uri->getHost(), $uri->getPort())) {
                throw new OriginError('refusing request outside the emulator origin');
            }
            if ($request->getMethod() === 'PUT' && $request->hasHeader('Content-Range')) $observed['chunks']++;
            return $handler($request, $options)->then(function (ResponseInterface $response) use ($observed) {
                if ($response->hasHeader('Location')) $observed['locations'][] = $response->getHeaderLine('Location');
                return $response;
            });
        };
    };
}

/** Builds the guarded Guzzle client shared by the SDK and the raw signed-URL requests. */
function guardedHttp(array $origin, ArrayObject $observed, ?string $ca, bool $dockerGateway): Client
{
    $stack = HandlerStack::create();
    $stack->push(originGuard($origin, $observed), 'emulator_origin');
    $config = ['handler' => $stack, 'timeout' => 10, 'connect_timeout' => 10, 'allow_redirects' => false,
        'proxy' => '', 'http_errors' => true, 'verify' => $ca ?? true];
    [, $host, $port] = $origin;
    if ($dockerGateway) {
        // Route the loopback origin through Docker's host gateway while URLs, Host,
        // TLS names and signatures keep the original origin.
        $config['curl'] = [CURLOPT_CONNECT_TO => ["$host:$port:host.docker.internal:$port"]];
    } elseif ($host === 'localhost') {
        // Pin localhost so hosts-file changes cannot redirect the test off-machine.
        $config['curl'] = [CURLOPT_RESOLVE => ["localhost:$port:127.0.0.1"]];
    }
    return new Client($config);
}

final class Harness
{
    public array $origin;
    public string $endpoint;
    public ?string $ca;
    public ?string $token;
    public bool $dockerGateway;
    public ArrayObject $observed;
    public Client $http;
    public StorageClient $storage;

    public function __construct()
    {
        $this->origin = emulatorOrigin((string)getenv('GCS_MIMIC_ENDPOINT'));
        $this->endpoint = originString($this->origin);
        $this->ca = null;
        if ($this->origin[0] === 'https') {
            $ca = (string)getenv('GCS_MIMIC_CA_CERT');
            check($ca !== '' && is_file($ca) && is_readable($ca), 'CA certificate is unreadable');
            $this->ca = $ca;
        }
        $token = getenv('GCS_MIMIC_TOKEN');
        $this->token = $token === false || $token === '' ? null : $token;
        $this->dockerGateway = getenv('GCS_MIMIC_DOCKER_GATEWAY') === '1';
        $this->observed = new ArrayObject(['chunks' => 0, 'locations' => []]);
        $this->http = guardedHttp($this->origin, $this->observed, $this->ca, $this->dockerGateway);
        $this->storage = $this->client($this->token, 0);
    }

    /** An SDK client for the emulator; a null token means anonymous credentials. */
    public function client(?string $token, ?int $retries): StorageClient
    {
        $config = ['projectId' => 'local-project', 'apiEndpoint' => $this->endpoint,
            'credentialsFetcher' => $token === null ? new AnonymousCredentials() : new StaticTokenCredentials($token),
            'requestTimeout' => 10, 'httpHandler' => HttpHandlerFactory::build($this->http),
            'authHttpHandler' => HttpHandlerFactory::build($this->http)];
        // null keeps the SDK's default retry policy.
        if ($retries !== null) $config['retries'] = $retries;
        return new StorageClient($config);
    }
}

function uniqueName(string $prefix): string
{
    return $prefix . '-' . bin2hex(random_bytes(6));
}

/** Deletes every live object, then the bucket; used in cleanup paths. */
function dropBucket($bucket): void
{
    if ($bucket === null) return;
    try {
        foreach ($bucket->objects() as $object) {
            try { $object->update(['temporaryHold' => false, 'eventBasedHold' => false]); } catch (ServiceException) {}
            $object->delete();
        }
        $bucket->delete();
    } catch (ServiceException $e) {
        fwrite(STDERR, 'cleanup of ' . $bucket->name() . ' failed: ' . detail($e) . "\n");
    }
}

final class Runner
{
    private array $failures = [];

    public function run(string $id, callable $case): void
    {
        set_time_limit(120);
        try {
            $note = $case();
            echo "PASS $id" . (is_string($note) ? ": $note" : '') . "\n";
        } catch (Throwable $e) {
            $this->failures[] = $id;
            echo "FAIL $id: " . detail($e) . "\n";
        }
        flush();
    }

    public function skip(string $id, string $reason): void
    {
        echo "SKIP $id: $reason\n";
    }

    public function finish(string $label): never
    {
        if ($this->failures) {
            echo "$label: " . count($this->failures) . ' failed: ' . implode(', ', $this->failures) . "\n";
            exit(1);
        }
        echo "$label: all cases passed\n";
        exit(0);
    }
}
