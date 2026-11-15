package local.gcsmimic.scorecard;

import com.google.api.core.ApiFuture;
import com.google.api.gax.rpc.ApiException;
import com.google.api.gax.rpc.StatusCode;
import com.google.cloud.storage.Blob;
import com.google.cloud.storage.BlobAppendableUpload;
import com.google.cloud.storage.BlobAppendableUploadConfig;
import com.google.cloud.storage.BlobId;
import com.google.cloud.storage.BlobInfo;
import com.google.cloud.storage.BlobReadSession;
import com.google.cloud.storage.BlobWriteSession;
import com.google.cloud.storage.BlobWriteSessionConfig;
import com.google.cloud.storage.BlobWriteSessionConfigs;
import com.google.cloud.storage.Bucket;
import com.google.cloud.storage.BucketInfo;
import com.google.cloud.storage.GrpcStorageOptions;
import com.google.cloud.ReadChannel;
import com.google.cloud.WriteChannel;
import com.google.cloud.storage.RangeSpec;
import com.google.cloud.storage.ReadProjectionConfigs;
import com.google.cloud.storage.Storage;
import com.google.cloud.storage.Storage.BlobListOption;
import com.google.cloud.storage.Storage.BlobTargetOption;
import com.google.cloud.storage.Storage.BlobWriteOption;
import com.google.cloud.storage.StorageException;
import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.nio.ByteBuffer;
import java.nio.channels.ScatteringByteChannel;
import java.nio.channels.WritableByteChannel;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.concurrent.TimeUnit;

/**
 * gRPC transport cases. Every case ends by checking that all calls used the emulator authority
 * and that no other destination was attempted.
 */
final class GrpcCases extends Cases {
  GrpcCases(Probe.Context context) {
    super(context);
  }

  @Override
  void register(List<Probe.Case> cases) {
    cases.add(new Probe.Case("grpc.bucket_create_get_delete", () -> withGrpc(null, this::bucketLifecycle)));
    cases.add(new Probe.Case("grpc.write_read", () -> withGrpc(null, this::writeRead)));
    cases.add(new Probe.Case("grpc.list", () -> withGrpc(null, this::list)));
    cases.add(new Probe.Case("grpc.update_delete", () -> withGrpc(null, this::updateDelete)));
    cases.add(new Probe.Case("grpc.multi_range_read", () -> withGrpc(null, this::readSession)));
    cases.add(new Probe.Case("grpc.appendable_object", () -> withGrpc(null, this::appendable)));
    cases.add(new Probe.Case("grpc.checksum_rejected", () -> withGrpc(null, this::checksumRejected)));
    cases.add(new Probe.Case("grpc.precondition_failed", () -> withGrpc(null, this::preconditionFailed)));
    cases.add(new Probe.Case("grpc.write_session_bidi",
        () -> withGrpc(BlobWriteSessionConfigs.bidiWrite(), this::writeSession)));
    cases.add(new Probe.Case("grpc.write_session_journaling",
        () -> withGrpc(BlobWriteSessionConfigs.journaling(List.of(caseDir())), this::writeSession)));
  }

  @FunctionalInterface
  private interface GrpcBody {
    void run(Storage grpc) throws Exception;
  }

  private void withGrpc(BlobWriteSessionConfig writeSession, GrpcBody body) throws Exception {
    GrpcStorageOptions.Builder builder = context.grpcOptions();
    if (writeSession != null) {
      builder.setBlobWriteSessionConfig(writeSession);
    }
    GrpcStorageOptions options = builder.build();
    // DirectPath is disabled in grpcOptions(); the options expose no getter for it, so the
    // observable guarantee is requireLocal() below.
    check(context.grpc.host().equals(options.getHost()), "gRPC host " + options.getHost());
    try (Storage grpc = options.getService()) {
      body.run(grpc);
    }
    context.grpc.requireLocal();
  }

  /**
   * Feature cases create their bucket over HTTP so they measure the feature itself; gRPC bucket
   * creation is covered once, by grpc.bucket_create_get_delete.
   */
  private String setupBucket() {
    return bucket().getName();
  }

  private void bucketLifecycle(Storage grpc) {
    String name = unique();
    check(name.equals(grpc.create(BucketInfo.of(name)).getName()), "created bucket name differs");
    Bucket found = grpc.get(name);
    check(found != null && name.equals(found.getName()), "bucket not read back by name");
    check(grpc.delete(name), "delete reported a missing bucket");
    check(grpc.get(name) == null, "deleted bucket visible");
  }

  private void writeRead(Storage grpc) throws Exception {
    String b = setupBucket();
    byte[] data = pattern(1024 * 1024 + 5, 20);
    BlobId id = BlobId.of(b, "chunked.bin");
    try (WriteChannel writer = grpc.writer(BlobInfo.newBuilder(id).setContentType("application/x-one").build())) {
      writer.setChunkSize(256 * 1024);
      SdkCases.writeAll(writer, data, 0, data.length);
    }
    check(Arrays.equals(grpc.readAllBytes(id), data), "bytes differ");
    Blob blob = grpc.get(id);
    check(blob != null && "application/x-one".equals(blob.getContentType()) && blob.getSize() == data.length,
        "metadata on write differs");
    // seek + limit is the public ranged read of a ReadChannel: the 5 bytes at offset 10.
    ByteBuffer part = ByteBuffer.allocate(5);
    try (ReadChannel reader = grpc.reader(id)) {
      reader.seek(10);
      reader.limit(15);
      while (part.hasRemaining() && reader.read(part) >= 0) {
        // keep reading until the range is full or the channel ends
      }
    }
    check(Arrays.equals(part.array(), Arrays.copyOfRange(data, 10, 15)) && !part.hasRemaining(),
        "ranged read differs");
  }

  private void list(Storage grpc) {
    String b = setupBucket();
    for (String name : List.of("a/1", "a/2", "b")) {
      grpc.create(BlobInfo.newBuilder(b, name).build(), bytes(name));
    }
    List<String> top = new ArrayList<>();
    for (Blob entry : grpc.list(b, BlobListOption.delimiter("/")).iterateAll()) {
      top.add(entry.isDirectory() ? "prefix:" + entry.getName() : entry.getName());
    }
    top.sort(null);
    check(top.equals(List.of("b", "prefix:a/")), "delimiter listing " + top);
    check(names(grpc.list(b, BlobListOption.prefix("a/")).iterateAll()).equals(List.of("a/1", "a/2")),
        "prefix listing differs");
  }

  private void updateDelete(Storage grpc) {
    String b = setupBucket();
    BlobId id = BlobId.of(b, "meta.bin");
    Blob created = grpc.create(BlobInfo.newBuilder(id).build(), bytes("meta"));
    grpc.update(created.toBuilder().setMetadata(Map.of("k", "v")).build());
    Blob read = grpc.get(id);
    check(read != null && Map.of("k", "v").equals(read.getMetadata()), "update not stored");
    check(grpc.delete(id), "delete reported a missing object");
    check(grpc.get(id) == null, "deleted object visible");
  }

  /**
   * The SDK surfaces a gRPC failure as a StorageException whose cause chain holds the gax
   * ApiException; its status code is the gRPC code itself, which getCode() would only map to HTTP.
   */
  private static StatusCode.Code grpcCode(Throwable failed) {
    for (Throwable t = failed; t != null; t = t.getCause()) {
      if (t instanceof ApiException api) {
        return api.getStatusCode().getCode();
      }
    }
    throw new AssertionError("no gRPC status in " + failed);
  }

  private static String messageChain(Throwable failed) {
    StringBuilder chain = new StringBuilder();
    for (Throwable t = failed; t != null; t = t.getCause()) {
      chain.append(t.getMessage()).append('\n');
    }
    return chain.toString().toLowerCase(Locale.ROOT);
  }

  /** Runs a call expected to fail over gRPC; the failure may be wrapped in an IOException. */
  private static Throwable grpcFailure(Probe.Body call) throws Exception {
    try {
      call.run();
    } catch (StorageException | IOException failed) {
      return failed;
    }
    throw new AssertionError("expected a gRPC failure, got success");
  }

  private void checksumRejected(Storage grpc) throws Exception {
    String b = setupBucket();
    // crc32cMatch() sends the BlobInfo value; create(byte[]) always computes its own, so the
    // writer is the public path for a declared checksum.
    BlobInfo info = BlobInfo.newBuilder(b, "crc.txt").setCrc32c("AAAAAA==").build();
    Throwable failed = grpcFailure(() -> {
      try (WriteChannel writer = grpc.writer(info, BlobWriteOption.crc32cMatch())) {
        SdkCases.writeAll(writer, bytes("payload"), 0, 7);
      }
    });
    String message = messageChain(failed);
    check(grpcCode(failed) == StatusCode.Code.INVALID_ARGUMENT
        && (message.contains("checksum") || message.contains("crc32c")), failed.toString());
    check(grpc.get(BlobId.of(b, "crc.txt")) == null, "rejected write left an object");
  }

  private void preconditionFailed(Storage grpc) throws Exception {
    String b = setupBucket();
    BlobId id = BlobId.of(b, "once.txt");
    grpc.create(BlobInfo.newBuilder(id).build(), bytes("first"), BlobTargetOption.doesNotExist());
    Throwable failed = grpcFailure(
        () -> grpc.create(BlobInfo.newBuilder(id).build(), bytes("second"), BlobTargetOption.doesNotExist()));
    check(grpcCode(failed) == StatusCode.Code.FAILED_PRECONDITION, failed.toString());
    check(Arrays.equals(grpc.readAllBytes(id), bytes("first")), "object lost its first bytes");
  }

  private void readSession(Storage grpc) throws Exception {
    String b = setupBucket();
    grpc.create(BlobInfo.newBuilder(b, "digits.txt").build(), bytes("0123456789"));
    try (BlobReadSession digits = grpc.blobReadSession(BlobId.of(b, "digits.txt")).get(30, TimeUnit.SECONDS)) {
      ApiFuture<byte[]> head = digits.readAs(ReadProjectionConfigs.asFutureBytes().withRangeSpec(RangeSpec.of(0, 3)));
      ApiFuture<byte[]> tail = digits.readAs(ReadProjectionConfigs.asFutureBytes().withRangeSpec(RangeSpec.of(6, 4)));
      check(Arrays.equals(head.get(30, TimeUnit.SECONDS), bytes("012")), "range (0, 3) differs");
      check(Arrays.equals(tail.get(30, TimeUnit.SECONDS), bytes("6789")), "range (6, 4) differs");
    }
    byte[] data = pattern(1024 * 1024 + 9, 21);
    grpc.create(BlobInfo.newBuilder(b, "ranges.bin").build(), data);
    try (BlobReadSession session = grpc.blobReadSession(BlobId.of(b, "ranges.bin")).get(30, TimeUnit.SECONDS)) {
      long[][] ranges = {{0, 10}, {1000, 500}, {data.length - 7, 7}};
      List<ApiFuture<byte[]>> reads = new ArrayList<>();
      for (long[] range : ranges) {
        reads.add(session.readAs(ReadProjectionConfigs.asFutureBytes().withRangeSpec(RangeSpec.of(range[0], range[1]))));
      }
      for (int i = 0; i < ranges.length; i++) {
        int from = (int) ranges[i][0];
        byte[] got = reads.get(i).get(30, TimeUnit.SECONDS);
        check(Arrays.equals(got, Arrays.copyOfRange(data, from, from + (int) ranges[i][1])),
            "range " + from + "+" + ranges[i][1] + " differs (" + got.length + " bytes)");
      }
      ByteArrayOutputStream all = new ByteArrayOutputStream();
      try (ScatteringByteChannel channel = session.readAs(ReadProjectionConfigs.asChannel())) {
        ByteBuffer buffer = ByteBuffer.allocate(64 * 1024);
        while (channel.read(buffer) >= 0) {
          all.write(buffer.array(), 0, buffer.position());
          buffer.clear();
        }
      }
      check(Arrays.equals(all.toByteArray(), data), "channel projection differs");
    }
  }

  private void writeSession(Storage grpc) throws Exception {
    String b = setupBucket();
    byte[] data = pattern(1024 * 1024 + 3, 22);
    BlobWriteSession session = grpc.blobWriteSession(BlobInfo.newBuilder(b, "session.bin").build());
    try (WritableByteChannel channel = session.open()) {
      SdkCases.writeAll(channel, data, 0, data.length);
    }
    BlobInfo result = session.getResult().get(30, TimeUnit.SECONDS);
    check(result.getSize() == data.length && crc32c(data).equals(result.getCrc32c()),
        "result size " + result.getSize() + " crc32c " + result.getCrc32c());
    check(Arrays.equals(grpc.readAllBytes(BlobId.of(b, "session.bin")), data), "bytes differ");
  }

  private void appendable(Storage grpc) throws Exception {
    String b = setupBucket();
    byte[] first = pattern(100 * 1024 + 1, 23);
    byte[] second = pattern(50 * 1024 + 2, 24);
    BlobId id = BlobId.of(b, "append.bin");
    BlobAppendableUpload upload = grpc.blobAppendableUpload(BlobInfo.newBuilder(id).build(),
        BlobAppendableUploadConfig.of());
    BlobAppendableUpload.AppendableUploadWriteableByteChannel channel = upload.open();
    SdkCases.writeAll(channel, first, 0, first.length);
    channel.flush();
    // A size-only read mask keeps this case about appendable semantics; the default all-fields
    // GetObject mask is exercised by grpc.write_read.
    Blob partial = grpc.get(id, Storage.BlobGetOption.fields(Storage.BlobField.SIZE));
    check(partial != null && partial.getSize() == first.length,
        "unfinalized size " + (partial == null ? null : partial.getSize()) + ", expected " + first.length);
    SdkCases.writeAll(channel, second, 0, second.length);
    channel.finalizeAndClose();
    BlobInfo result = upload.getResult().get(30, TimeUnit.SECONDS);
    byte[] all = new byte[first.length + second.length];
    System.arraycopy(first, 0, all, 0, first.length);
    System.arraycopy(second, 0, all, first.length, second.length);
    check(result.getSize() == all.length, "finalized size " + result.getSize());
    check(Arrays.equals(grpc.readAllBytes(id), all), "appended bytes differ");
  }
}
