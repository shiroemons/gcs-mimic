package local.gcsmimic.scorecard;

import com.google.cloud.storage.BlobInfo;
import com.google.cloud.storage.HttpMethod;
import com.google.cloud.storage.PostPolicyV4;
import com.google.cloud.storage.Storage.PostPolicyV4Option;
import com.google.cloud.storage.Storage.SignUrlOption;
import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.HttpURLConnection;
import java.net.URL;
import java.nio.charset.StandardCharsets;
import java.util.Arrays;
import java.util.List;
import java.util.Map;
import java.util.concurrent.TimeUnit;

/**
 * Signed URL and POST policy probes. Requests go through the same guarded connection factory as
 * the SDK and use the generated URL unchanged.
 */
final class SignedCases extends Cases {
  private record Response(int status, byte[] body) {
    String preview() {
      String text = new String(body, StandardCharsets.UTF_8);
      return "status " + status + ": " + (text.length() > 200 ? text.substring(0, 200) : text);
    }
  }

  SignedCases(Probe.Context context) {
    super(context);
  }

  @Override
  void register(List<Probe.Case> cases) {
    cases.add(new Probe.Case("signed.v4_get", () -> signed(SignUrlOption.withV4Signature(), HttpMethod.GET)));
    cases.add(new Probe.Case("signed.v4_put", () -> signed(SignUrlOption.withV4Signature(), HttpMethod.PUT)));
    cases.add(new Probe.Case("signed.v2_get", () -> signed(SignUrlOption.withV2Signature(), HttpMethod.GET)));
    cases.add(new Probe.Case("signed.post_policy_v4", this::postPolicy));
  }

  private Response send(URL url, String method, String contentType, byte[] body) throws IOException {
    HttpURLConnection connection = context.http.open(url);
    try {
      connection.setRequestMethod(method);
      if (body != null) {
        connection.setDoOutput(true);
        if (contentType != null) {
          connection.setRequestProperty("Content-Type", contentType);
        }
        connection.setFixedLengthStreamingMode(body.length);
        try (OutputStream out = connection.getOutputStream()) {
          out.write(body);
        }
      }
      int status = connection.getResponseCode();
      InputStream stream = status >= 400 ? connection.getErrorStream() : connection.getInputStream();
      byte[] content = stream == null ? new byte[0] : stream.readAllBytes();
      return new Response(status, content);
    } finally {
      connection.disconnect();
    }
  }

  private void signed(SignUrlOption version, HttpMethod method) throws IOException {
    String b = bucket().getName();
    BlobInfo info = BlobInfo.newBuilder(b, "signed.txt").build();
    if (method == HttpMethod.GET) {
      upload(b, "signed.txt", "signed");
    }
    URL url = storage.signUrl(info, 5, TimeUnit.MINUTES, version, SignUrlOption.httpMethod(method),
        SignUrlOption.withHostName(context.http.origin().toString()), SignUrlOption.withPathStyle(),
        SignUrlOption.signWith(context.signer));
    Response response = send(url, method.name(), null, method == HttpMethod.PUT ? bytes("uploaded") : null);
    check(response.status() == 200, response.preview());
    check(method == HttpMethod.GET ? Arrays.equals(response.body(), bytes("signed"))
        : Arrays.equals(read(b, "signed.txt"), bytes("uploaded")), "signed bytes");
  }

  private void postPolicy() throws IOException {
    String b = bucket().getName();
    PostPolicyV4 policy = storage.generateSignedPostPolicyV4(BlobInfo.newBuilder(b, "post.txt").build(), 5,
        TimeUnit.MINUTES, PostPolicyV4Option.signWith(context.signer));
    // POST policies sign the policy document, not the host, so the path-style emulator URL is
    // used as in the other probes.
    String boundary = "scorecard-" + unique();
    ByteArrayOutputStream form = new ByteArrayOutputStream();
    for (Map.Entry<String, String> field : policy.getFields().entrySet()) {
      form.writeBytes(bytes("--" + boundary + "\r\nContent-Disposition: form-data; name=\"" + field.getKey()
          + "\"\r\n\r\n" + field.getValue() + "\r\n"));
    }
    form.writeBytes(bytes("--" + boundary + "\r\nContent-Disposition: form-data; name=\"file\"; filename=\"post.txt\""
        + "\r\nContent-Type: application/octet-stream\r\n\r\nposted\r\n--" + boundary + "--\r\n"));
    Response response = send(new URL(context.http.origin() + "/" + b), "POST",
        "multipart/form-data; boundary=" + boundary, form.toByteArray());
    check(response.status() >= 200 && response.status() < 300, response.preview());
    check(Arrays.equals(read(b, "post.txt"), bytes("posted")), "posted bytes");
  }
}
