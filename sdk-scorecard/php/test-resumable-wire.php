<?php
declare(strict_types=1);

// Capture actual socket traffic without persisting session URLs or credentials.
if (PHP_SAPI === 'cli-server') {
    if ($_SERVER['REQUEST_URI'] === '/ready') return;
    $data = str_repeat('a', 3 * 1024 * 1024 + 123);
    $hash = 'md5=' . base64_encode(md5($data, true)) . ',crc32c=' . base64_encode(hash('crc32c', $data, true));
    header('Content-Type: application/json');
    if ($_SERVER['REQUEST_METHOD'] === 'POST') {
        file_get_contents('php://input');
        header('Location: http://' . $_SERVER['HTTP_HOST'] . '/session');
        return;
    }
    $range = $_SERVER['HTTP_CONTENT_RANGE'] ?? '';
    $body = file_get_contents('php://input');
    $valid = preg_match('~^bytes (\d+)-(\d+)/(\d+)$~', $range, $match) === 1
        && strlen($body) === (int)$match[2] - (int)$match[1] + 1
        && ($_SERVER['HTTP_X_GOOG_HASH'] ?? '') === $hash;
    if (!$valid) {
        http_response_code(400);
        echo '{"error":{"code":400,"message":"wire assertion failed"}}';
        return;
    }
    file_put_contents(getenv('SCORECARD_WIRE_LOG'), json_encode([
        'range' => $range, 'hash' => $hash, 'length' => strlen($body),
    ], JSON_THROW_ON_ERROR) . "\n", FILE_APPEND);
    if ((int)$match[2] + 1 < (int)$match[3]) {
        http_response_code(308);
        header('Range: bytes=0-' . $match[2]);
        return;
    }
    echo json_encode(['name' => 'big.bin', 'bucket' => 'wire-bucket', 'generation' => '1', 'size' => (string)strlen($data),
        'crc32c' => base64_encode(hash('crc32c', $data, true)), 'md5Hash' => base64_encode(md5($data, true))], JSON_THROW_ON_ERROR);
    return;
}

require __DIR__ . '/vendor/autoload.php';

use Google\Auth\HttpHandler\HttpHandlerFactory;
use Google\Cloud\Core\AnonymousCredentials;
use Google\Cloud\Storage\StorageClient;
use GuzzleHttp\Client;

$listener = stream_socket_server('tcp://127.0.0.1:0');
if (!$listener) throw new RuntimeException('cannot allocate loopback port');
$address = stream_socket_get_name($listener, false);
fclose($listener);
$log = tempnam(sys_get_temp_dir(), 'scorecard-wire-');
$serverLog = tempnam(sys_get_temp_dir(), 'scorecard-server-');
putenv('SCORECARD_WIRE_LOG=' . $log);
$server = proc_open([PHP_BINARY, '-S', $address, __FILE__],
    [0 => ['pipe', 'r'], 1 => ['file', $serverLog, 'a'], 2 => ['file', $serverLog, 'a']], $pipes);
if (!is_resource($server)) throw new RuntimeException('cannot start wire server');
try {
    $http = new Client(['timeout' => 5, 'proxy' => '', 'allow_redirects' => false]);
    $origin = 'http://' . $address;
    for ($attempt = 0; ; $attempt++) {
        try { $http->get($origin . '/ready'); break; }
        catch (\GuzzleHttp\Exception\ConnectException $e) {
            if ($attempt === 50) throw $e;
            usleep(20000);
        }
    }
    $storage = new StorageClient(['projectId' => 'local-project', 'apiEndpoint' => $origin,
        'credentialsFetcher' => new AnonymousCredentials(), 'retries' => 0,
        'httpHandler' => HttpHandlerFactory::build($http)]);
    $storage->bucket('wire-bucket')->upload(str_repeat('a', 3 * 1024 * 1024 + 123),
        ['name' => 'big.bin', 'resumable' => true, 'chunkSize' => 256 * 1024]);
    $chunks = array_map(fn($line) => json_decode($line, true, 512, JSON_THROW_ON_ERROR),
        file($log, FILE_IGNORE_NEW_LINES));
    if (count($chunks) !== 13 || $chunks[0]['range'] !== 'bytes 0-262143/3145851'
        || $chunks[12]['range'] !== 'bytes 3145728-3145850/3145851') {
        throw new RuntimeException('unexpected resumable wire chunks');
    }
    echo 'PASS: PHP SDK sends the whole-object ' . $chunks[0]['hash']
        . " on all 13 real HTTP chunks, including 12 non-final chunks\n";
} finally {
    proc_terminate($server);
    proc_close($server);
    unlink($log);
    unlink($serverLog);
}
