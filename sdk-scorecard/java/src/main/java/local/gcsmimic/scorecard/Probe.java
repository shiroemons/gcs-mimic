package local.gcsmimic.scorecard;

import com.google.auth.oauth2.ServiceAccountCredentials;
import com.google.cloud.NoCredentials;
import com.google.cloud.ServiceOptions;
import com.google.cloud.http.HttpTransportOptions;
import com.google.cloud.storage.GrpcStorageOptions;
import com.google.cloud.storage.HttpStorageOptions;
import com.google.cloud.storage.Storage;
import com.google.cloud.storage.StorageOptions;
import java.io.FileDescriptor;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.PrintStream;
import java.net.HttpURLConnection;
import java.net.URI;
import java.net.URL;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Comparator;
import java.util.List;
import java.util.Map;
import java.util.concurrent.ExecutionException;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.Future;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.TimeoutException;
import java.util.stream.Stream;

/** Emulator-neutral official Java SDK probes; stdout is JSONL evidence. */
public final class Probe {
  static final String SDK_VERSION = "google-cloud-storage 2.76.0";

  /**
   * Cases this SDK version cannot express through its public API. Decided statically so a
   * runtime failure can never be reclassified as "unsupported". Every scorecard workflow has a
   * public google-cloud-storage 2.76.0 API, so the map is empty.
   */
  static final Map<String, String> UNSUPPORTED = Map.of();

  private Probe() {}

  @FunctionalInterface
  interface Body {
    void run() throws Exception;
  }

  record Case(String id, Body body) {}

  enum Retries {
    NONE,
    SDK_DEFAULT
  }

  static final class Context {
    final Storage storage;
    final LocalTransport http;
    final ServiceAccountCredentials signer;
    final String project;
    final GrpcGuard grpc;
    /** Every temporary file of the run lives below this directory, removed when the run ends. */
    final Path temp;

    Context(LocalTransport http, String project, ServiceAccountCredentials signer, GrpcGuard grpc, Path temp) {
      this.http = http;
      this.project = project;
      this.signer = signer;
      this.grpc = grpc;
      this.temp = temp;
      this.storage = httpOptions(Retries.NONE).build().getService();
    }

    /**
     * HTTP options on the guarded transport. Most cases disable retries so an emulator error
     * surfaces as the case result; cases about the SDK's default behavior keep its retry settings.
     */
    HttpStorageOptions.Builder httpOptions(Retries retries) {
      HttpStorageOptions.Builder builder = HttpStorageOptions.newBuilder()
          .setHost(http.origin().toString())
          .setProjectId(project)
          .setCredentials(NoCredentials.getInstance())
          .setTransportOptions(HttpTransportOptions.newBuilder()
              .setHttpTransportFactory(http::transport)
              .setConnectTimeout(5000)
              .setReadTimeout(20000)
              .build());
      return retries == Retries.NONE ? builder.setRetrySettings(ServiceOptions.getNoRetrySettings()) : builder;
    }

    /** Plaintext gRPC options pinned to the emulator: no credentials, DirectPath or metrics export. */
    GrpcStorageOptions.Builder grpcOptions() {
      return StorageOptions.grpc()
          .setHost(grpc.host())
          .setProjectId(project)
          .setCredentials(NoCredentials.getInstance())
          .setAttemptDirectPath(false)
          .setEnableGrpcClientMetrics(false)
          .setGrpcInterceptorProvider(grpc)
          .setRetrySettings(ServiceOptions.getNoRetrySettings());
    }
  }

  public static void main(String[] args) throws Exception {
    PrintStream out = new PrintStream(new FileOutputStream(FileDescriptor.out), true, StandardCharsets.UTF_8);
    URI origin = LocalTransport.localOrigin(System.getenv("SCORECARD_ENDPOINT"));
    LocalTransport http = new LocalTransport(origin, System.getenv("SCORECARD_CA_CERT"));
    if (args.length == 1 && args[0].equals("--transport-check")) {
      transportCheck(http, out);
      return;
    }
    if (args.length != 0) {
      throw new IllegalArgumentException("usage: Probe [--transport-check]");
    }
    GrpcGuard grpc = GrpcGuard.install(System.getenv("SCORECARD_GRPC_ENDPOINT"));
    String project = envOr("SCORECARD_PROJECT", "local-project");
    // TransferManagerConfig.newBuilder() builds StorageOptions.getDefaultInstance(), whose default
    // project lookup asks the metadata server unless this property (checked first) is present.
    System.setProperty("GOOGLE_CLOUD_PROJECT", project);
    Path temp = Files.createTempDirectory("sc-java-");
    try {
      Context context = new Context(http, project, signer(), grpc, temp);
      List<Case> cases = new ArrayList<>();
      new BucketCases(context).register(cases);
      new ObjectCases(context).register(cases);
      new SignedCases(context).register(cases);
      new SdkCases(context).register(cases);
      new TransferCases(context).register(cases);
      new GrpcCases(context).register(cases);
      String filter = envOr("SCORECARD_RUN", "");
      for (Case c : cases) {
        if (c.id().contains(filter)) {
          out.println(run(c));
        }
      }
      out.flush();
    } finally {
      deleteTree(temp);
    }
    // Timed-out cases may leave daemon worker threads behind; they must not keep the JVM alive.
    System.exit(0);
  }

  static void deleteTree(Path root) throws IOException {
    if (!Files.exists(root)) {
      return;
    }
    try (Stream<Path> paths = Files.walk(root)) {
      for (Path path : paths.sorted(Comparator.reverseOrder()).toList()) {
        Files.deleteIfExists(path);
      }
    }
  }

  static String envOr(String name, String fallback) {
    String value = System.getenv(name);
    return value == null || value.isEmpty() ? fallback : value;
  }

  static ServiceAccountCredentials signer() throws Exception {
    String key = System.getenv("SCORECARD_SIGNING_KEY");
    if (key == null || key.isEmpty()) {
      throw new IllegalArgumentException("SCORECARD_SIGNING_KEY is required");
    }
    // Offline RSA signing only; the credentials never request a token.
    return ServiceAccountCredentials.fromPkcs8(null, "local-admin@example.invalid",
        Files.readString(Path.of(key)), null, null);
  }

  static void transportCheck(LocalTransport http, PrintStream out) throws Exception {
    try {
      http.open(new URL("https://storage.googleapis.com/"));
      throw new AssertionError("external origin accepted");
    } catch (java.io.IOException error) {
      if (!LocalTransport.ESCAPED.equals(error.getMessage())) {
        throw error;
      }
    }
    HttpURLConnection connection = http.open(new URL(http.origin() + "/"));
    try {
      int status = connection.getResponseCode();
      if (status != 200) {
        throw new AssertionError("transport check GET returned " + status);
      }
    } finally {
      connection.disconnect();
    }
    out.println("PASS: Java transport CA/SAN and exact-origin guard");
  }

  static String run(Case c) {
    String id = c.id();
    String area = id.startsWith("project.") ? "bucket" : id.substring(0, id.indexOf('.'));
    StringBuilder line = new StringBuilder("{\"sdk\":\"java\",\"id\":").append(json(id))
        .append(",\"area\":").append(json(area));
    String unsupported = UNSUPPORTED.get(id);
    if (unsupported != null) {
      return line.append(",\"pass\":false,\"unsupported\":true,\"detail\":")
          .append(json(SDK_VERSION + " has no public API for " + unsupported)).append('}').toString();
    }
    String failure = execute(c.body());
    if (failure == null) {
      return line.append(",\"pass\":true}").toString();
    }
    return line.append(",\"pass\":false,\"detail\":").append(json(truncate(failure, 300))).append('}').toString();
  }

  /** Runs one case with a 60-second limit and returns null on success or the failure detail. */
  static String execute(Body body) {
    ExecutorService worker = Executors.newSingleThreadExecutor(task -> {
      Thread thread = new Thread(task, "scorecard-case");
      thread.setDaemon(true);
      return thread;
    });
    try {
      Future<?> result = worker.submit(() -> {
        body.run();
        return null;
      });
      try {
        result.get(60, TimeUnit.SECONDS);
        return null;
      } catch (TimeoutException timeout) {
        result.cancel(true);
        return "TimeoutException: case exceeded 60 seconds";
      } catch (ExecutionException failed) {
        Throwable cause = failed.getCause() == null ? failed : failed.getCause();        return cause.getClass().getSimpleName() + ": " + cause.getMessage();
      } catch (InterruptedException interrupted) {
        Thread.currentThread().interrupt();
        return "InterruptedException: " + interrupted.getMessage();
      }
    } finally {
      worker.shutdownNow();
    }
  }

  static String truncate(String value, int codePoints) {
    if (value.codePointCount(0, value.length()) <= codePoints) {
      return value;
    }
    return value.substring(0, value.offsetByCodePoints(0, codePoints));
  }

  static String json(String value) {
    StringBuilder out = new StringBuilder("\"");
    for (int i = 0; i < value.length(); i++) {
      char ch = value.charAt(i);
      switch (ch) {
        case '"' -> out.append("\\\"");
        case '\\' -> out.append("\\\\");
        case '\n' -> out.append("\\n");
        case '\r' -> out.append("\\r");
        case '\t' -> out.append("\\t");
        default -> {
          if (ch < 0x20) {
            out.append(String.format("\\u%04x", (int) ch));
          } else {
            out.append(ch);
          }
        }
      }
    }
    return out.append('"').toString();
  }
}
