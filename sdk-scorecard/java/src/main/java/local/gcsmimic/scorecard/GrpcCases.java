package local.gcsmimic.scorecard;

import com.google.api.core.ApiFuture;
import com.google.cloud.storage.Blob;
import com.google.cloud.storage.BlobAppendableUpload;
import com.google.cloud.storage.BlobAppendableUploadConfig;
import com.google.cloud.storage.BlobId;
import com.google.cloud.storage.BlobInfo;
import com.google.cloud.storage.BlobReadSession;
import com.google.cloud.storage.BlobWriteSession;
import com.google.cloud.storage.BlobWriteSessionConfig;
import com.google.cloud.storage.BlobWriteSessionConfigs;
import com.google.cloud.storage.BucketInfo;
import com.google.cloud.storage.GrpcStorageOptions;
import com.google.cloud.storage.RangeSpec;
import com.google.cloud.storage.ReadProjectionConfigs;
import com.google.cloud.storage.Storage;
import com.google.cloud.storage.Storage.BlobListOption;
import java.io.ByteArrayOutputStream;
import java.nio.ByteBuffer;
import java.nio.channels.ScatteringByteChannel;
import java.nio.channels.WritableByteChannel;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
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
    cases.add(new Probe.Case("sdk.grpc_basic_object", () -> withGrpc(null, this::basicObject)));
    cases.add(new Probe.Case("sdk.grpc_blob_read_session", () -> withGrpc(null, this::readSession)));
    cases.add(new Probe.Case("sdk.grpc_write_session_bidi",
        () -> withGrpc(BlobWriteSessionConfigs.bidiWrite(), this::writeSession)));
    cases.add(new Probe.Case("sdk.grpc_write_session_journaling",
        () -> withGrpc(BlobWriteSessionConfigs.journaling(List.of(caseDir())), this::writeSession)));
    cases.add(new Probe.Case("sdk.grpc_appendable_upload", () -> withGrpc(null, this::appendable)));
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
   * creation is covered once, by sdk.grpc_basic_object.
   */
  private String setupBucket() {
    return bucket().getName();
  }

  private void basicObject(Storage grpc) {
    String b = grpc.create(BucketInfo.of(unique())).getName();
    byte[] data = pattern(300 * 1024 + 3, 20);
    grpc.create(BlobInfo.newBuilder(b, "a/one").setContentType("application/x-one").build(), data);
    grpc.create(BlobInfo.newBuilder(b, "a/two").build(), bytes("two"));
    check(Arrays.equals(grpc.readAllBytes(BlobId.of(b, "a/one")), data), "bytes differ");
    check(names(grpc.list(b, BlobListOption.prefix("a/")).iterateAll()).equals(List.of("a/one", "a/two")),
        "listing differs");
    Blob one = grpc.get(BlobId.of(b, "a/one"));
    check(one != null && "application/x-one".equals(one.getContentType()) && one.getSize() == data.length,
        "metadata on write differs");
    grpc.update(one.toBuilder().setMetadata(Map.of("k", "v")).build());
    check(Map.of("k", "v").equals(grpc.get(BlobId.of(b, "a/one")).getMetadata()), "update not stored");
    check(grpc.delete(BlobId.of(b, "a/one")), "delete reported a missing object");
    check(grpc.get(BlobId.of(b, "a/one")) == null, "deleted object visible");
  }

  private void readSession(Storage grpc) throws Exception {
    String b = setupBucket();
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
    // GetObject mask is exercised by sdk.grpc_basic_object.
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
