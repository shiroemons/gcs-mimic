package local.gcsmimic.scorecard;

import com.google.api.gax.grpc.GrpcInterceptorProvider;
import io.grpc.CallOptions;
import io.grpc.Channel;
import io.grpc.ClientCall;
import io.grpc.ClientInterceptor;
import io.grpc.MethodDescriptor;
import java.io.IOException;
import java.net.InetSocketAddress;
import java.net.Proxy;
import java.net.ProxySelector;
import java.net.SocketAddress;
import java.net.URI;
import java.util.List;
import java.util.Set;
import java.util.concurrent.ConcurrentHashMap;

/**
 * Keeps the gRPC transport on the emulator. The interceptor rejects any call whose channel
 * authority differs from the configured endpoint, and the process-wide proxy selector (which
 * grpc-java and JDK HTTP clients without an explicit proxy consult before connecting) records
 * every non-loopback destination and routes it to a closed local port so it cannot leave the
 * machine. The HTTP probe transport uses {@code Proxy.NO_PROXY} and its own origin guard.
 */
final class GrpcGuard extends ProxySelector implements GrpcInterceptorProvider {
  // Port 9 (discard) is not served on test hosts; a CONNECT to it fails immediately.
  private static final Proxy BLACKHOLE = new Proxy(Proxy.Type.HTTP, new InetSocketAddress("127.0.0.1", 9));

  private final String authority;
  private final Set<String> authorities = ConcurrentHashMap.newKeySet();
  private final Set<String> foreign = ConcurrentHashMap.newKeySet();

  private GrpcGuard(String authority) {
    this.authority = authority;
  }

  /** Validates a plaintext loopback {@code host:port} and installs the process-wide monitor. */
  static GrpcGuard install(String raw) {
    if (raw == null || raw.isEmpty()) {
      throw new IllegalArgumentException("SCORECARD_GRPC_ENDPOINT is required");
    }
    URI uri = URI.create("http://" + raw);
    String host = uri.getHost();
    if (host == null || !isLoopback(host) || uri.getPort() < 1 || !uri.getRawPath().isEmpty()
        || uri.getRawUserInfo() != null || uri.getRawQuery() != null || uri.getRawFragment() != null) {
      throw new IllegalArgumentException("SCORECARD_GRPC_ENDPOINT must be a loopback host:port");
    }
    GrpcGuard guard = new GrpcGuard(uri.getRawAuthority());
    ProxySelector.setDefault(guard);
    return guard;
  }

  private static boolean isLoopback(String host) {
    return host.equals("127.0.0.1") || host.equals("localhost") || host.equals("[::1]") || host.equals("::1");
  }

  /** Host value for {@code GrpcStorageOptions}; the {@code http} scheme selects plaintext. */
  String host() {
    return "http://" + authority;
  }

  String authority() {
    return authority;
  }

  /** Fails unless every gRPC call used the emulator authority and nothing tried to leave. */
  void requireLocal() {
    if (!foreign.isEmpty()) {
      throw new AssertionError("non-loopback connection attempted: " + foreign);
    }
    if (!Set.of(authority).equals(authorities)) {
      throw new AssertionError("gRPC authorities " + authorities + ", expected only " + authority);
    }
  }

  @Override
  public List<Proxy> select(URI uri) {
    String host = uri.getHost();
    if (host != null && isLoopback(host)) {
      return List.of(Proxy.NO_PROXY);
    }
    foreign.add(String.valueOf(uri));    return List.of(BLACKHOLE);
  }

  @Override
  public void connectFailed(URI uri, SocketAddress address, IOException error) {
    // Failures are reported to the caller; nothing to retry here.
  }

  @Override
  public List<ClientInterceptor> getInterceptors() {
    return List.of(new ClientInterceptor() {
      @Override
      public <Q, R> ClientCall<Q, R> interceptCall(MethodDescriptor<Q, R> method, CallOptions options, Channel next) {
        String actual = next.authority();
        authorities.add(String.valueOf(actual));
        if (!authority.equals(actual)) {
          throw new IllegalStateException("gRPC call escaped emulator: " + actual);
        }
        return next.newCall(method, options);
      }
    });
  }
}
