package local.gcsmimic;

import com.google.api.client.http.javanet.NetHttpTransport;
import com.google.cloud.NoCredentials;
import com.google.cloud.WriteChannel;
import com.google.cloud.http.HttpTransportOptions;
import com.google.cloud.storage.Blob;
import com.google.cloud.storage.BlobId;
import com.google.cloud.storage.BlobInfo;
import com.google.cloud.storage.BucketInfo;
import com.google.cloud.storage.HttpStorageOptions;
import com.google.cloud.storage.Storage;
import java.io.IOException;
import java.net.HttpURLConnection;
import java.net.Proxy;
import java.net.URI;
import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.time.Duration;
import java.util.Arrays;
import java.util.Map;
import java.util.UUID;
import java.util.concurrent.atomic.AtomicInteger;
import org.testcontainers.containers.GenericContainer;
import org.testcontainers.containers.wait.strategy.Wait;

public final class Scenario {
  private Scenario() {}

  static URI localOrigin(String raw) {
    URI uri = URI.create(raw);
    String host = uri.getHost();
    if (!"http".equals(uri.getScheme()) || host == null ||
        !(host.equals("127.0.0.1") || host.equals("localhost") || host.equals("[::1]")) ||
        uri.getPort() < 1 || uri.getRawUserInfo() != null || uri.getRawQuery() != null ||
        uri.getRawFragment() != null || !(uri.getRawPath().isEmpty() || uri.getRawPath().equals("/"))) {
      throw new IllegalArgumentException("endpoint must be a bare loopback HTTP origin with an explicit port");
    }
    return URI.create("http://" + uri.getRawAuthority());
  }

  static void requireOrigin(URI expected, URI target) throws IOException {
    if (!expected.getScheme().equals(target.getScheme()) ||
        !expected.getRawAuthority().equals(target.getRawAuthority()) || target.getRawUserInfo() != null) {
      throw new IOException("request escaped the emulator origin");
    }
  }

  static HttpURLConnection connection(URI origin, URI target) throws IOException {
    requireOrigin(origin, target);
    HttpURLConnection connection = (HttpURLConnection) target.toURL().openConnection(Proxy.NO_PROXY);
    connection.setInstanceFollowRedirects(false);
    connection.setConnectTimeout(5000);
    connection.setReadTimeout(15000);
    return connection;
  }

  static void advertise(URI endpoint) throws IOException {
    HttpURLConnection request = connection(endpoint, endpoint.resolve("/_internal/config"));
    try {
      request.setRequestMethod("PUT");
      request.setDoOutput(true);
      request.setRequestProperty("Content-Type", "application/json");
      byte[] body = ("{\"externalUrl\":\"" + endpoint + "\",\"publicHost\":\"" +
          endpoint.getRawAuthority() + "\"}").getBytes(StandardCharsets.UTF_8);
      try (var stream = request.getOutputStream()) { stream.write(body); }
      check(request.getResponseCode() == 200, "endpoint advertisement failed");
    } finally { request.disconnect(); }
  }

  static void check(boolean condition, String message) {
    if (!condition) throw new AssertionError(message);
  }

  static void scenario(URI endpoint) throws Exception {
    AtomicInteger resumableRequests = new AtomicInteger();
    var transport = new NetHttpTransport.Builder().setConnectionFactory(url -> {
      URI target = URI.create(url.toString());
      if (target.getRawQuery() != null && target.getRawQuery().contains("uploadType=resumable")) {
        resumableRequests.incrementAndGet();
      }
      return connection(endpoint, target);
    }).build();
    Storage storage = HttpStorageOptions.newBuilder().setHost(endpoint.toString())
        .setProjectId("local-project").setCredentials(NoCredentials.getInstance())
        .setTransportOptions(HttpTransportOptions.newBuilder().setHttpTransportFactory(() -> transport)
            .setConnectTimeout(5000).setReadTimeout(15000).build()).build().getService();
    String bucket = "sdk-java-" + UUID.randomUUID();
    storage.create(BucketInfo.of(bucket));
    try {
      BlobInfo small = BlobInfo.newBuilder(bucket, "metadata/日本語 +%?.txt")
          .setContentType("text/plain").setMetadata(Map.of("fixture", "initial")).build();
      storage.create(small, "Java multipart bytes".getBytes(StandardCharsets.UTF_8));
      Blob updated = storage.update(small.toBuilder().setMetadata(Map.of("fixture", "updated")).build());
      check(updated.getMetadata().get("fixture").equals("updated"), "PATCH/override metadata update failed");
      byte[] payload = new byte[576 * 1024];
      for (int i = 0; i < payload.length; i++) payload[i] = (byte) i;
      BlobId id = BlobId.of(bucket, "stream/resumable.bin");
      try (WriteChannel writer = storage.writer(BlobInfo.newBuilder(id).setContentType("application/octet-stream").build(),
          Storage.BlobWriteOption.doesNotExist())) {
        writer.setChunkSize(256 * 1024);
        ByteBuffer data = ByteBuffer.wrap(payload);
        while (data.hasRemaining()) writer.write(data);
      }
      check(resumableRequests.get() > 0, "SDK did not initiate a resumable upload");
      check(Arrays.equals(storage.readAllBytes(id), payload), "resumable bytes differ");
      check(storage.get(id).getGeneration() > 0, "generation missing");
      check(storage.list(bucket, Storage.BlobListOption.prefix("stream/")).iterateAll().iterator().hasNext(), "listing missing fixture");
      System.out.println("java-http: multipart, PATCH override, resumable, generation, read, list PASS");
    } finally {
      for (Blob blob : storage.list(bucket, Storage.BlobListOption.versions(true)).iterateAll()) storage.delete(blob.getBlobId());
      storage.delete(bucket);
      transport.shutdown();
    }
  }

  static void selfCheck() throws IOException {
    URI origin = localOrigin("http://127.0.0.1:4443");
    for (String bad : new String[] {"https://storage.googleapis.com", "http://metadata.google.internal:80",
        "http://127.0.0.1:4443/path", "http://user@127.0.0.1:4443", "http://127.0.0.1:4443?x"}) {
      boolean rejected = false;
      try { localOrigin(bad); } catch (IllegalArgumentException expected) { rejected = true; }
      check(rejected, "invalid origin accepted");
    }
    try { requireOrigin(origin, URI.create("http://127.0.0.1:4444/upload")); }
    catch (IOException expected) { System.out.println("java-origin-guard: PASS"); return; }
    throw new AssertionError("cross-origin session was accepted");
  }

  public static void main(String[] args) throws Exception {
    if (args.length == 1 && args[0].equals("--self-check")) { selfCheck(); return; }
    if (args.length == 1 && args[0].equals("--container")) {
      String image = System.getenv("GCS_MIMIC_IMAGE");
      if (image == null || image.isBlank()) throw new IllegalArgumentException("set GCS_MIMIC_IMAGE to the exact image under test");
      try (GenericContainer<?> container = new GenericContainer<>(image).withExposedPorts(4443)
          .withEnv("GCS_MIMIC_SCHEME", "http").withEnv("GCS_MIMIC_ADMIN_ENABLED", "true")
          .waitingFor(Wait.forHttp("/_internal/healthcheck").forPort(4443).forStatusCode(200))
          .withStartupTimeout(Duration.ofSeconds(60))) {
        container.start();
        URI endpoint = localOrigin("http://" + container.getHost() + ":" + container.getMappedPort(4443));
        advertise(endpoint);
        scenario(endpoint);
      }
      return;
    }
    scenario(localOrigin(System.getenv("GCS_MIMIC_ENDPOINT")));
  }
}
