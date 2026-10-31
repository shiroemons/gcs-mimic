package local.gcsmimic.scorecard;

import com.google.api.client.http.javanet.NetHttpTransport;
import java.io.IOException;
import java.io.InputStream;
import java.net.HttpURLConnection;
import java.net.Proxy;
import java.net.URI;
import java.net.URL;
import java.nio.file.Files;
import java.nio.file.Path;
import java.security.GeneralSecurityException;
import java.security.KeyStore;
import java.security.cert.Certificate;
import java.security.cert.CertificateFactory;
import java.util.List;
import java.util.Locale;
import java.util.concurrent.CopyOnWriteArrayList;
import javax.net.ssl.HttpsURLConnection;
import javax.net.ssl.SSLContext;
import javax.net.ssl.SSLSocketFactory;
import javax.net.ssl.TrustManagerFactory;

/**
 * The only HTTP path used by the probe: the SDK transport and the signed-request client both open
 * connections here, so every request (including redirects and resumable session URLs) is checked
 * against the configured loopback origin before a socket is opened.
 */
final class LocalTransport {
  static final String ESCAPED = "request escaped emulator";

  private final URI origin;
  private final SSLSocketFactory sockets;
  private final NetHttpTransport transport;
  private final List<String> opened = new CopyOnWriteArrayList<>();

  LocalTransport(URI origin, String caCertPath) throws IOException, GeneralSecurityException {
    this.origin = origin;
    if ("https".equals(origin.getScheme())) {
      if (caCertPath == null || caCertPath.isEmpty()) {
        throw new IllegalArgumentException("SCORECARD_CA_CERT is required for an HTTPS endpoint");
      }
      this.sockets = trustOnly(Path.of(caCertPath));
    } else {
      this.sockets = null;
    }
    this.transport = new NetHttpTransport.Builder().setConnectionFactory(this::open).build();
  }

  /** Accepts only a bare loopback origin with an explicit port, mirroring the other probes. */
  static URI localOrigin(String raw) {
    if (raw == null) {
      throw new IllegalArgumentException("SCORECARD_ENDPOINT is required");
    }
    String trimmed = raw.endsWith("/") ? raw.substring(0, raw.length() - 1) : raw;
    URI uri = URI.create(trimmed);
    String host = uri.getHost();
    String scheme = uri.getScheme();
    if (!("http".equals(scheme) || "https".equals(scheme)) || host == null
        || !(host.equals("127.0.0.1") || host.equals("localhost") || host.equals("[::1]"))
        || uri.getPort() < 1 || uri.getRawUserInfo() != null || uri.getRawQuery() != null
        || uri.getRawFragment() != null || !uri.getRawPath().isEmpty()) {
      throw new IllegalArgumentException("SCORECARD_ENDPOINT must be a bare loopback origin with an explicit port");
    }
    return uri;
  }

  // Trusting only the fixture CA keeps the JVM default trust store (and any public CA) out of
  // the probe; hostname/SAN verification stays with HttpsURLConnection's default checks.
  private static SSLSocketFactory trustOnly(Path caPem) throws IOException, GeneralSecurityException {
    KeyStore store = KeyStore.getInstance(KeyStore.getDefaultType());
    store.load(null, null);
    int index = 0;
    try (InputStream in = Files.newInputStream(caPem)) {
      for (Certificate certificate : CertificateFactory.getInstance("X.509").generateCertificates(in)) {
        store.setCertificateEntry("scorecard-ca-" + index++, certificate);
      }
    }
    if (index == 0) {
      throw new GeneralSecurityException("no certificate found in " + caPem);
    }
    TrustManagerFactory trust = TrustManagerFactory.getInstance(TrustManagerFactory.getDefaultAlgorithm());
    trust.init(store);
    SSLContext context = SSLContext.getInstance("TLS");
    context.init(null, trust.getTrustManagers(), null);
    return context.getSocketFactory();
  }

  URI origin() {
    return origin;
  }

  /** Number of guarded requests opened so far; a mark for {@link #openedSince(int)}. */
  int openedCount() {
    return opened.size();
  }

  /**
   * URLs of the requests opened after {@code mark}. SDK-internal request fan-out (for example
   * ranged downloads) is otherwise invisible through the public API.
   */
  List<String> openedSince(int mark) {
    return List.copyOf(opened.subList(mark, opened.size()));
  }

  NetHttpTransport transport() {
    return transport;
  }

  void requireOrigin(URL target) throws IOException {
    int port = target.getPort() == -1 ? target.getDefaultPort() : target.getPort();
    if (!origin.getScheme().equals(target.getProtocol().toLowerCase(Locale.ROOT))
        || !origin.getHost().equalsIgnoreCase(target.getHost()) || origin.getPort() != port
        || target.getUserInfo() != null) {
      throw new IOException(ESCAPED);
    }
  }

  /** Opens a guarded connection without a proxy and without following redirects. */
  HttpURLConnection open(URL target) throws IOException {
    requireOrigin(target);
    opened.add(target.toString());
    HttpURLConnection connection = (HttpURLConnection) target.openConnection(Proxy.NO_PROXY);
    if (connection instanceof HttpsURLConnection https) {
      https.setSSLSocketFactory(sockets);
    }
    connection.setInstanceFollowRedirects(false);
    connection.setConnectTimeout(5000);
    connection.setReadTimeout(20000);
    return connection;
  }
}
