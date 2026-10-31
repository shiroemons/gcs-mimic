package local.gcsmimic.scorecard;

import com.google.cloud.ReadChannel;
import com.google.cloud.RestorableState;
import com.google.cloud.WriteChannel;
import com.google.cloud.storage.Blob;
import com.google.cloud.storage.BlobId;
import com.google.cloud.storage.BlobInfo;
import com.google.cloud.storage.BlobWriteSession;
import com.google.cloud.storage.BucketInfo;
import com.google.cloud.storage.Storage;
import com.google.cloud.storage.Storage.BlobListOption;
import com.google.cloud.storage.Storage.BlobWriteOption;
import com.google.cloud.storage.StorageBatch;
import com.google.cloud.storage.StorageBatchResult;
import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.nio.ByteBuffer;
import java.nio.channels.WritableByteChannel;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.HashSet;
import java.util.List;
import java.util.Map;
import java.util.concurrent.TimeUnit;

/** Ordinary Java SDK paths that the shared cases do not reach, over HTTP. */
final class SdkCases extends Cases {
  private static final int CHUNK = 256 * 1024;

  SdkCases(Probe.Context context) {
    super(context);
  }

  @Override
  void register(List<Probe.Case> cases) {
    cases.add(new Probe.Case("sdk.blob_handle_roundtrip", this::blobHandle));
    cases.add(new Probe.Case("sdk.default_upload_crc32c", this::defaultCrc32c));
    cases.add(new Probe.Case("sdk.create_from_path_download_to", this::createFromPath));
    cases.add(new Probe.Case("sdk.write_channel_streaming", this::writeChannel));
    cases.add(new Probe.Case("sdk.read_channel_chunked", this::readChannel));
    cases.add(new Probe.Case("sdk.list_iterate_all", this::iterateAll));
    cases.add(new Probe.Case("sdk.storage_batch", this::batch));
    cases.add(new Probe.Case("sdk.default_retry_resumable_recovery", this::resumableRecovery));
    cases.add(new Probe.Case("sdk.blob_write_session_default", this::writeSessionDefault));
  }

  static void writeAll(WritableByteChannel channel, byte[] data, int from, int to) throws IOException {
    ByteBuffer buffer = ByteBuffer.wrap(data, from, to - from);
    while (buffer.hasRemaining()) {
      channel.write(buffer);
    }
  }

  // Blob handles go through reload/update/copyTo/exists, which add their own projection and
  // generation parameters to the requests.
  private void blobHandle() {
    String b = bucket().getName();
    Blob blob = upload(b, "h.txt", "handle").reload();
    check(blob != null && blob.getSize() == 6, "reload lost the object");
    Blob updated = blob.toBuilder().setContentType("text/x-handle").setMetadata(Map.of("k", "v")).build().update();
    Blob again = updated.reload();
    check("text/x-handle".equals(again.getContentType()) && Map.of("k", "v").equals(again.getMetadata()),
        "reloaded metadata differs: " + again.getContentType() + " " + again.getMetadata());
    Blob copy = again.copyTo(b, "copy.txt").getResult();
    check("copy.txt".equals(copy.getName()) && Arrays.equals(read(b, "copy.txt"), bytes("handle")), "copy bytes");
    check(again.exists(), "exists() false before delete");
    check(again.delete(), "delete() reported a missing object");
    check(!again.exists(), "exists() true after delete");
  }

  private void defaultCrc32c() throws Exception {
    Storage defaults = context.httpOptions(Probe.Retries.SDK_DEFAULT).build().getService();
    String b = bucket().getName();
    byte[] small = pattern(1000, 1);
    Blob direct = defaults.create(BlobInfo.newBuilder(b, "direct").build(), small);
    check(crc32c(small).equals(direct.getCrc32c()), "direct upload crc32c " + direct.getCrc32c());
    byte[] large = pattern(3 * CHUNK + 11, 2);
    try (WriteChannel writer = defaults.writer(BlobInfo.newBuilder(b, "chunked").build())) {
      writer.setChunkSize(CHUNK);
      writeAll(writer, large, 0, large.length);
    }
    check(crc32c(large).equals(reload(BlobId.of(b, "chunked")).getCrc32c()), "chunked upload crc32c");
    // createFrom(Path) is a separate resumable implementation from the WriteChannel above.
    Path file = Files.write(caseDir().resolve("bad.bin"), pattern(CHUNK + 1, 3));
    BlobInfo wrong = BlobInfo.newBuilder(b, "bad").setCrc32c("AAAAAA==").build();
    expectChecksumRejection(() -> defaults.createFrom(wrong, file, BlobWriteOption.crc32cMatch()));
    check(storage.get(BlobId.of(b, "bad")) == null, "rejected upload left an object");
  }

  private void createFromPath() throws IOException {
    String b = bucket().getName();
    Path dir = caseDir();
    byte[] data = pattern(5 * 1024 * 1024 + 3, 4);
    Path source = Files.write(dir.resolve("source.bin"), data);
    storage.createFrom(BlobInfo.newBuilder(b, "file.bin").build(), source);
    Blob blob = reload(BlobId.of(b, "file.bin"));
    Path target = dir.resolve("download.bin");
    blob.downloadTo(target);
    check(Arrays.equals(Files.readAllBytes(target), data), "downloadTo(Path) bytes differ");
    ByteArrayOutputStream stream = new ByteArrayOutputStream();
    blob.downloadTo(stream);
    check(Arrays.equals(stream.toByteArray(), data), "downloadTo(OutputStream) bytes differ");
  }

  private void writeChannel() throws IOException {
    String b = bucket().getName();
    for (int size : new int[] {2 * CHUNK, 2 * CHUNK - 1, 2 * CHUNK + 1, 0}) {
      String name = "size-" + size;
      byte[] data = pattern(size, size);
      try (WriteChannel writer = storage.writer(BlobInfo.newBuilder(b, name).build())) {
        writer.setChunkSize(CHUNK);
        writeAll(writer, data, 0, size);
      }
      Blob blob = storage.get(BlobId.of(b, name));
      check(blob != null && blob.getSize() == size, name + " size " + (blob == null ? null : blob.getSize()));
      check(Arrays.equals(read(b, name), data), name + " bytes differ");
    }
  }

  private static byte[] drain(ReadChannel reader, int limit) throws IOException {
    ByteArrayOutputStream out = new ByteArrayOutputStream();
    ByteBuffer buffer = ByteBuffer.allocate(8 * 1024);
    while (out.size() < limit) {
      buffer.clear().limit(Math.min(buffer.capacity(), limit - out.size()));
      int n = reader.read(buffer);
      if (n < 0) {
        break;
      }
      out.write(buffer.array(), 0, n);
    }
    return out.toByteArray();
  }

  private void readChannel() throws IOException {
    String b = bucket().getName();
    byte[] data = pattern(3 * 1024 * 1024 + 17, 5);
    BlobId id = upload(b, "chunked.bin", data).getBlobId();
    try (ReadChannel reader = storage.reader(BlobId.of(b, "chunked.bin"))) {
      reader.setChunkSize(64 * 1024);
      check(Arrays.equals(drain(reader, Integer.MAX_VALUE), data), "sequential bytes differ");
    }
    int start = 1024 * 1024 + 5;
    try (ReadChannel reader = storage.reader(id)) {
      reader.setChunkSize(64 * 1024);
      reader.seek(start);
      byte[] head = drain(reader, 100_000);
      RestorableState<ReadChannel> state = reader.capture();
      try (ReadChannel resumed = state.restore()) {
        byte[] tail = drain(resumed, Integer.MAX_VALUE);
        check(Arrays.equals(head, Arrays.copyOfRange(data, start, start + head.length)), "seek bytes differ");
        check(Arrays.equals(tail, Arrays.copyOfRange(data, start + head.length, data.length)),
            "restored channel bytes differ (" + tail.length + " bytes)");
      }
    }
  }

  private void iterateAll() {
    String b = bucket().getName();
    seed(b, "d/1", "d/2", "d/sub/3", "e", "f");
    check(names(storage.list(b, BlobListOption.pageSize(2)).iterateAll())
        .equals(List.of("d/1", "d/2", "d/sub/3", "e", "f")), "iterateAll names");
    List<String> objects = new ArrayList<>();
    List<String> directories = new ArrayList<>();
    for (Blob blob : storage.list(b, BlobListOption.prefix("d/"), BlobListOption.currentDirectory(),
        BlobListOption.pageSize(1)).iterateAll()) {
      (blob.isDirectory() ? directories : objects).add(blob.getName());
    }
    check(objects.equals(List.of("d/1", "d/2")) && directories.equals(List.of("d/sub/")),
        "currentDirectory " + objects + " " + directories);
    String versioned = storage.create(BucketInfo.newBuilder(unique()).setVersioningEnabled(true).build()).getName();
    upload(versioned, "v", "one");
    upload(versioned, "v", "two");
    HashSet<Long> generations = new HashSet<>();
    for (Blob blob : storage.list(versioned, BlobListOption.versions(true), BlobListOption.pageSize(1)).iterateAll()) {
      generations.add(blob.getGeneration());
    }
    check(generations.size() == 2, "versions across pages " + generations);
  }

  private void batch() {
    String b = bucket().getName();
    seed(b, "x", "y", "z");
    StorageBatch batch = storage.batch();
    StorageBatchResult<Blob> got = batch.get(b, "x");
    StorageBatchResult<Blob> updated = batch.update(BlobInfo.newBuilder(b, "y").setContentType("text/x-batch").build());
    StorageBatchResult<Boolean> deleted = batch.delete(b, "z");
    StorageBatchResult<Blob> missing = batch.get(b, "missing");
    StorageBatchResult<Boolean> missingDelete = batch.delete(b, "missing");
    batch.submit();
    check(got.get() != null && got.get().getSize() == 1, "batch get");
    check("text/x-batch".equals(updated.get().getContentType()), "batch update result");
    check(Boolean.TRUE.equals(deleted.get()), "batch delete result");
    check(missing.get() == null, "batch get of a missing object");
    check(Boolean.FALSE.equals(missingDelete.get()), "batch delete of a missing object");
    check("text/x-batch".equals(reload(BlobId.of(b, "y")).getContentType()), "batch update not stored");
    check(storage.get(BlobId.of(b, "z")) == null, "batch delete not applied");
  }

  private void resumableRecovery() throws IOException {
    Storage defaults = context.httpOptions(Probe.Retries.SDK_DEFAULT).build().getService();
    String b = bucket().getName();
    byte[] data = pattern(3 * CHUNK + 99, 6);
    int split = 2 * CHUNK + 7;
    WriteChannel first = defaults.writer(BlobInfo.newBuilder(b, "resumed.bin").build());
    first.setChunkSize(CHUNK);
    writeAll(first, data, 0, split);
    RestorableState<WriteChannel> state = first.capture();
    try (WriteChannel resumed = state.restore()) {
      writeAll(resumed, data, split, data.length);
    }
    check(Arrays.equals(read(b, "resumed.bin"), data), "resumed bytes differ");
    check(names(storage.list(b).iterateAll()).equals(List.of("resumed.bin")), "unexpected objects");
  }

  private void writeSessionDefault() throws Exception {
    String b = bucket().getName();
    byte[] data = pattern(700 * 1024 + 1, 7);
    BlobWriteSession session = storage.blobWriteSession(BlobInfo.newBuilder(b, "session.bin").build());
    try (WritableByteChannel channel = session.open()) {
      writeAll(channel, data, 0, data.length);
    }
    BlobInfo result = session.getResult().get(30, TimeUnit.SECONDS);
    check(result.getSize() == data.length && crc32c(data).equals(result.getCrc32c()),
        "session result size " + result.getSize() + " crc32c " + result.getCrc32c());
    check(Arrays.equals(read(b, "session.bin"), data), "session bytes differ");
  }
}
