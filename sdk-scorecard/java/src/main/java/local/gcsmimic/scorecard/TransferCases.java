package local.gcsmimic.scorecard;

import com.google.cloud.storage.Blob;
import com.google.cloud.storage.BlobId;
import com.google.cloud.storage.BlobInfo;
import com.google.cloud.storage.BlobWriteSession;
import com.google.cloud.storage.BlobWriteSessionConfig;
import com.google.cloud.storage.BlobWriteSessionConfigs;
import com.google.cloud.storage.ParallelCompositeUploadBlobWriteSessionConfig.BufferAllocationStrategy;
import com.google.cloud.storage.ParallelCompositeUploadBlobWriteSessionConfig.ExecutorSupplier;
import com.google.cloud.storage.Storage;
import com.google.cloud.storage.transfermanager.DownloadResult;
import com.google.cloud.storage.transfermanager.ParallelDownloadConfig;
import com.google.cloud.storage.transfermanager.ParallelUploadConfig;
import com.google.cloud.storage.transfermanager.TransferManager;
import com.google.cloud.storage.transfermanager.TransferManagerConfig;
import com.google.cloud.storage.transfermanager.TransferStatus;
import com.google.cloud.storage.transfermanager.UploadResult;
import java.io.OutputStream;
import java.nio.channels.WritableByteChannel;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import java.util.concurrent.TimeUnit;

/** Transfer Manager and BlobWriteSession configurations, over HTTP. */
final class TransferCases extends Cases {
  // Small fixed parallelism keeps the run light and deterministic.
  private static final int WORKERS = 4;
  private static final int MIB = 1024 * 1024;

  TransferCases(Probe.Context context) {
    super(context);
  }

  @Override
  void register(List<Probe.Case> cases) {
    cases.add(new Probe.Case("sdk.transfer_manager_upload", this::tmUpload));
    cases.add(new Probe.Case("sdk.transfer_manager_download", this::tmDownload));
    cases.add(new Probe.Case("sdk.transfer_manager_parallel_composite", this::tmParallelComposite));
    cases.add(new Probe.Case("sdk.write_session_parallel_composite", this::sessionParallelComposite));
    cases.add(new Probe.Case("sdk.write_session_buffer_to_disk", this::sessionBufferToDisk));
  }

  private TransferManagerConfig.Builder manager() {
    return TransferManagerConfig.newBuilder()
        .setStorageOptions(context.httpOptions(Probe.Retries.NONE).build())
        .setMaxWorkers(WORKERS);
  }

  private static ParallelUploadConfig uploadTo(String bucket) {
    return ParallelUploadConfig.newBuilder().setBucketName(bucket)
        .setUploadBlobInfoFactory(ParallelUploadConfig.UploadBlobInfoFactory.transformFileName(
            file -> "up/" + Path.of(file).getFileName()))
        .build();
  }

  private static void requireUploads(List<UploadResult> results, int expected) {
    check(results.size() == expected, "upload results " + results.size());
    for (UploadResult result : results) {
      check(result.getStatus() == TransferStatus.SUCCESS,
          "upload " + result.getInput().getName() + " " + result.getStatus() + ": " + failure(result));
    }
  }

  private static String failure(UploadResult result) {
    return result.getStatus() == TransferStatus.SUCCESS ? "" : String.valueOf(result.getException());
  }

  /** Remaining object names, which reveal leftover parallel-composite parts. */
  private List<String> objectNames(String bucket) {
    return names(storage.list(bucket).iterateAll());
  }

  private void tmUpload() throws Exception {
    String b = bucket().getName();
    Path dir = caseDir();
    List<Path> files = new ArrayList<>();
    List<byte[]> contents = List.of(pattern(300 * 1024, 10), pattern(MIB + 1, 11), pattern(10, 12));
    for (int i = 0; i < contents.size(); i++) {
      files.add(Files.write(dir.resolve("file-" + i + ".bin"), contents.get(i)));
    }
    try (TransferManager manager = manager().build().getService()) {
      requireUploads(manager.uploadFiles(files, uploadTo(b)).getUploadResults(), files.size());
    }
    for (int i = 0; i < contents.size(); i++) {
      check(Arrays.equals(read(b, "up/file-" + i + ".bin"), contents.get(i)), "file-" + i + " bytes differ");
    }
  }

  // Divide-and-conquer starts above the SDK's fixed 128 MiB threshold (DefaultQos).
  private void tmDownload() throws Exception {
    String b = bucket().getName();
    Path dir = caseDir();
    Path big = dir.resolve("big.bin");
    try (OutputStream out = Files.newOutputStream(big)) {
      byte[] block = pattern(MIB, 13);
      for (int i = 0; i < 128; i++) {
        block[0] = (byte) i;
        out.write(block);
      }
      out.write(7);
    }
    storage.createFrom(BlobInfo.newBuilder(b, "big.bin").build(), big);
    upload(b, "small.txt", "small");
    List<BlobInfo> blobs = List.of(reload(BlobId.of(b, "big.bin")), reload(BlobId.of(b, "small.txt")));
    Path target = Files.createDirectory(dir.resolve("out"));
    int mark = context.http.openedCount();
    List<DownloadResult> results;
    try (TransferManager manager = manager().setAllowDivideAndConquerDownload(true)
        .setPerWorkerBufferSize(16 * MIB).build().getService()) {
      results = manager.downloadBlobs(blobs, ParallelDownloadConfig.newBuilder().setBucketName(b)
          .setDownloadDirectory(target).build()).getDownloadResults();
    }
    for (DownloadResult result : results) {
      check(result.getStatus() == TransferStatus.SUCCESS, "download " + result.getInput().getName() + " "
          + result.getStatus() + ": " + (result.getStatus() == TransferStatus.SUCCESS ? "" : result.getException()));
    }
    check(Files.mismatch(big, target.resolve("big.bin")) == -1, "big.bin bytes differ");
    check("small".equals(Files.readString(target.resolve("small.txt"))), "small.txt bytes differ");
    long ranged = context.http.openedSince(mark).stream()
        .filter(url -> url.contains("/o/big.bin") && url.contains("alt=media")).count();
    check(ranged > 1, "big.bin was fetched in " + ranged + " request(s), not divided");
  }

  // Parallel composite upload starts above 4 x perWorkerBufferSize when more than two workers run.
  private void tmParallelComposite() throws Exception {
    String b = bucket().getName();
    byte[] data = pattern(9 * MIB + 5, 14);
    Path file = Files.write(caseDir().resolve("pcu.bin"), data);
    try (TransferManager manager = manager().setAllowParallelCompositeUpload(true)
        .setPerWorkerBufferSize(MIB).build().getService()) {
      requireUploads(manager.uploadFiles(List.of(file), uploadTo(b)).getUploadResults(), 1);
    }
    requireComposite(b, "up/pcu.bin", data);
  }

  private void requireComposite(String b, String name, byte[] data) {
    Blob blob = reload(BlobId.of(b, name));
    check(blob.getComponentCount() != null && blob.getComponentCount() > 1,
        "not a composite object: componentCount " + blob.getComponentCount());
    check(Arrays.equals(read(b, name), data), name + " bytes differ");
    check(objectNames(b).equals(List.of(name)), "parts left behind: " + objectNames(b));
  }

  private BlobInfo writeWithSession(BlobWriteSessionConfig config, String b, String name, byte[] data)
      throws Exception {
    try (Storage configured = context.httpOptions(Probe.Retries.NONE).setBlobWriteSessionConfig(config)
        .build().getService()) {
      BlobWriteSession session = configured.blobWriteSession(BlobInfo.newBuilder(b, name).build());
      try (WritableByteChannel channel = session.open()) {
        SdkCases.writeAll(channel, data, 0, data.length);
      }
      return session.getResult().get(45, TimeUnit.SECONDS);
    }
  }

  private void sessionParallelComposite() throws Exception {
    String b = bucket().getName();
    byte[] data = pattern(5 * MIB + 1, 15);
    writeWithSession(BlobWriteSessionConfigs.parallelCompositeUpload()
        .withBufferAllocationStrategy(BufferAllocationStrategy.fixedPool(WORKERS, MIB))
        .withExecutorSupplier(ExecutorSupplier.fixedPool(WORKERS)), b, "pcu.bin", data);
    requireComposite(b, "pcu.bin", data);
  }

  private void sessionBufferToDisk() throws Exception {
    String b = bucket().getName();
    byte[] data = pattern(2 * MIB + 9, 16);
    BlobInfo result = writeWithSession(BlobWriteSessionConfigs.bufferToDiskThenUpload(caseDir()), b, "disk.bin", data);
    check(result.getSize() == data.length && crc32c(data).equals(result.getCrc32c()),
        "result size " + result.getSize() + " crc32c " + result.getCrc32c());
    check(Arrays.equals(read(b, "disk.bin"), data), "disk.bin bytes differ");
  }
}
