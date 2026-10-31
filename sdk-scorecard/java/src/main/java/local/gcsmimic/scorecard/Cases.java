package local.gcsmimic.scorecard;

import com.google.cloud.storage.Blob;
import com.google.cloud.storage.BlobId;
import com.google.cloud.storage.BlobInfo;
import com.google.cloud.storage.Bucket;
import com.google.cloud.storage.BucketInfo;
import com.google.cloud.storage.Storage;
import com.google.cloud.storage.StorageException;
import java.io.IOException;
import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.security.SecureRandom;
import java.util.ArrayList;
import java.util.Base64;
import java.util.HexFormat;
import java.util.List;
import java.util.Locale;
import java.util.Random;
import java.util.zip.CRC32C;

/** Shared helpers for the probe groups; each group registers its cases in the canonical order. */
abstract class Cases {
  private static final SecureRandom RANDOM = new SecureRandom();

  protected final Probe.Context context;
  protected final Storage storage;

  Cases(Probe.Context context) {
    this.context = context;
    this.storage = context.storage;
  }

  abstract void register(List<Probe.Case> cases);

  static void check(boolean condition, String message) {
    if (!condition) {
      throw new AssertionError(message);
    }
  }

  /** Expects the call to fail with the given HTTP status; other failures propagate unchanged. */
  static void expectStatus(int status, Probe.Body call) throws Exception {
    try {
      call.run();
    } catch (StorageException error) {
      if (error.getCode() == status) {
        return;
      }
      throw error;
    }
    throw new AssertionError("expected HTTP " + status + ", got success");
  }

  /**
   * Expects a checksum-specific HTTP 400. Upload paths surface it as a StorageException, possibly
   * wrapped in an IOException, whose own message can omit the response body; the JSON error text
   * is on the HttpResponseException cause, so the whole cause chain is classified.
   */
  static void expectChecksumRejection(Probe.Body call) throws Exception {
    StorageException error = null;
    try {
      call.run();
    } catch (StorageException | IOException failed) {
      for (Throwable t = failed; t != null && error == null; t = t.getCause()) {
        if (t instanceof StorageException storage) {
          error = storage;
        }
      }
      if (error == null) {
        throw failed;
      }
    }
    check(error != null, "wrong CRC32C accepted");
    StringBuilder chain = new StringBuilder();
    for (Throwable t = error; t != null; t = t.getCause()) {
      chain.append(t.getMessage()).append('\n');
    }
    String message = chain.toString().toLowerCase(Locale.ROOT);
    check(error.getCode() == 400 && (message.contains("checksum") || message.contains("crc32c")),
        error.getCode() + ": " + error.getMessage());
  }

  static String unique() {
    byte[] bytes = new byte[6];
    RANDOM.nextBytes(bytes);
    return "sc-java-" + HexFormat.of().formatHex(bytes);
  }

  static byte[] bytes(String value) {
    return value.getBytes(StandardCharsets.UTF_8);
  }

  Bucket bucket(Storage.BucketTargetOption... options) {
    return storage.create(BucketInfo.of(unique()), options);
  }

  Bucket reload(String bucket) {
    Bucket found = storage.get(bucket);
    check(found != null, "bucket " + bucket + " not found");
    return found;
  }

  Blob reload(BlobId id) {
    Blob found = storage.get(id);
    check(found != null, "object " + id.getName() + " not found");
    return found;
  }

  Blob upload(String bucket, String name, String data, Storage.BlobTargetOption... options) {
    return upload(bucket, name, bytes(data), options);
  }

  Blob upload(String bucket, String name, byte[] data, Storage.BlobTargetOption... options) {
    return storage.create(BlobInfo.newBuilder(bucket, name).build(), data, options);
  }

  byte[] read(String bucket, String name, Storage.BlobSourceOption... options) {
    return storage.readAllBytes(BlobId.of(bucket, name), options);
  }

  void seed(String bucket, String... names) {
    for (String name : names) {
      upload(bucket, name, name);
    }
  }

  /** Deterministic non-repeating-looking bytes, so misplaced ranges or chunks change the content. */
  static byte[] pattern(int size, long seed) {
    byte[] data = new byte[size];
    new Random(seed).nextBytes(data);
    return data;
  }

  /** Base64 of the big-endian CRC32C, the encoding GCS uses in object metadata. */
  static String crc32c(byte[] data) {
    CRC32C crc = new CRC32C();
    crc.update(data);
    return Base64.getEncoder().encodeToString(ByteBuffer.allocate(4).putInt((int) crc.getValue()).array());
  }

  /** A fresh directory below the run's temporary root. */
  Path caseDir() throws IOException {
    return Files.createTempDirectory(context.temp, "case-");
  }

  static List<String> names(Iterable<Blob> blobs) {
    List<String> found = new ArrayList<>();
    for (Blob blob : blobs) {
      found.add(blob.getName());
    }
    found.sort(null);
    return found;
  }
}
