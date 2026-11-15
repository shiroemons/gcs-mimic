package local.gcsmimic.scorecard;

import com.google.api.client.http.HttpContent;
import com.google.api.client.http.HttpExecuteInterceptor;
import com.google.api.client.http.HttpRequest;
import com.google.api.client.http.HttpRequestInitializer;
import com.google.cloud.ServiceOptions;
import com.google.cloud.http.HttpTransportOptions;
import java.io.IOException;
import java.util.List;
import java.util.concurrent.CopyOnWriteArrayList;

/**
 * Requests that the Storage clients built with {@link #attach} hand to the guarded transport, with
 * the headers and body length that say which bytes they carry. Which bytes an upload resends, or
 * how a download is sliced, is otherwise invisible through the SDK's public API.
 */
final class RequestLog {
  record Entry(String method, String url, String contentRange, String range, long contentLength) {
    @Override
    public String toString() {
      return method + " " + url + " " + contentRange + " " + contentLength;
    }
  }

  private final List<Entry> entries = new CopyOnWriteArrayList<>();

  /** A mark for {@link #since(int)}. */
  int size() {
    return entries.size();
  }

  List<Entry> since(int mark) {
    return List.copyOf(entries.subList(mark, entries.size()));
  }

  /**
   * Transport options that record each request just before it is sent. The interceptor runs after
   * the SDK has set the headers and content, and keeps any interceptor already installed.
   */
  HttpTransportOptions attach(HttpTransportOptions base) {
    return new HttpTransportOptions(base.toBuilder()) {
      @Override
      public HttpRequestInitializer getHttpRequestInitializer(ServiceOptions<?, ?> options) {
        HttpRequestInitializer delegate = super.getHttpRequestInitializer(options);
        return request -> {
          delegate.initialize(request);
          HttpExecuteInterceptor previous = request.getInterceptor();
          request.setInterceptor(sent -> {
            if (previous != null) {
              previous.intercept(sent);
            }
            record(sent);
          });
        };
      }
    };
  }

  private void record(HttpRequest request) throws IOException {
    HttpContent content = request.getContent();
    entries.add(new Entry(request.getRequestMethod(), request.getUrl().build(),
        request.getHeaders().getContentRange(), request.getHeaders().getRange(),
        content == null ? 0 : content.getLength()));
  }
}
