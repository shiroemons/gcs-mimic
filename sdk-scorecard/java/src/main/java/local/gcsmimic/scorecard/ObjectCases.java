package local.gcsmimic.scorecard;

import com.google.api.gax.paging.Page;
import com.google.cloud.ReadChannel;
import com.google.cloud.WriteChannel;
import com.google.cloud.storage.Acl;
import com.google.cloud.storage.Blob;
import com.google.cloud.storage.BlobId;
import com.google.cloud.storage.BlobInfo;
import com.google.cloud.storage.Bucket;
import com.google.cloud.storage.BucketInfo;
import com.google.cloud.storage.CopyWriter;
import com.google.cloud.storage.Storage;
import com.google.cloud.storage.Storage.BlobListOption;
import com.google.cloud.storage.Storage.BlobSourceOption;
import com.google.cloud.storage.Storage.BlobTargetOption;
import com.google.cloud.storage.Storage.BlobWriteOption;
import com.google.cloud.storage.StorageClass;
import com.google.cloud.storage.StorageException;
import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.nio.ByteBuffer;
import java.nio.channels.Channels;
import java.security.MessageDigest;
import java.security.SecureRandom;
import java.time.Duration;
import java.time.OffsetDateTime;
import java.time.ZoneOffset;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Base64;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.zip.GZIPOutputStream;

/** Object probes, in the canonical case order. */
final class ObjectCases extends Cases {
  ObjectCases(Probe.Context context) {
    super(context);
  }

  @Override
  void register(List<Probe.Case> cases) {
    cases.add(new Probe.Case("object.write_read_small", this::small));
    cases.add(new Probe.Case("object.resumable_large", this::resumable));
    cases.add(new Probe.Case("object.metadata_on_write", this::metadataOnWrite));
    cases.add(new Probe.Case("object.checksum_rejected", this::badCrc));
    cases.add(new Probe.Case("object.md5_accepted", this::md5));
    cases.add(new Probe.Case("object.range_reads", this::ranges));
    cases.add(new Probe.Case("object.update_metadata", this::updateMetadata));
    cases.add(new Probe.Case("object.delete_not_found", this::delete));
    cases.add(new Probe.Case("object.does_not_exist_precondition", this::createCondition));
    cases.add(new Probe.Case("object.generation_match_precondition", this::generationCondition));
    cases.add(new Probe.Case("object.metageneration_match_precondition", this::metagenerationCondition));
    cases.add(new Probe.Case("object.list_prefix_delimiter", this::delimiter));
    cases.add(new Probe.Case("object.list_paging", this::paging));
    cases.add(new Probe.Case("object.list_offsets", this::offsets));
    cases.add(new Probe.Case("object.list_match_glob", this::glob));
    cases.add(new Probe.Case("object.copy_with_metadata", this::copy));
    cases.add(new Probe.Case("object.rewrite_storage_class", this::rewrite));
    cases.add(new Probe.Case("object.compose", this::compose));
    cases.add(new Probe.Case("object.move", this::move));
    cases.add(new Probe.Case("object.acl", this::acl));
    cases.add(new Probe.Case("object.holds", this::holds));
    cases.add(new Probe.Case("object.retention", this::retention));
    cases.add(new Probe.Case("object.custom_time", this::customTime));
    cases.add(new Probe.Case("object.storage_class_on_write", this::storageClass));
    cases.add(new Probe.Case("object.gzip_transcoding", this::transcoding));
    cases.add(new Probe.Case("object.customer_supplied_key", this::csek));
    cases.add(new Probe.Case("object.soft_delete_restore", this::restore));
  }

  private static byte[] filled(int size) {
    byte[] data = new byte[size];
    Arrays.fill(data, (byte) 'a');
    return data;
  }

  private void write(BlobInfo info, byte[] data, BlobWriteOption... options) throws IOException {
    try (WriteChannel writer = storage.writer(info, options)) {
      writer.setChunkSize(256 * 1024);
      ByteBuffer buffer = ByteBuffer.wrap(data);
      while (buffer.hasRemaining()) {
        writer.write(buffer);
      }
    }
  }

  private byte[] range(BlobId id, long start, long limit) throws IOException {
    try (ReadChannel reader = storage.reader(id)) {
      reader.seek(start);
      if (limit >= 0) {
        reader.limit(limit);
      }
      return Channels.newInputStream(reader).readAllBytes();
    }
  }

  private void rewriteTo(BlobId source, BlobInfo target) {
    CopyWriter copy = storage.copy(Storage.CopyRequest.newBuilder().setSource(source).setTarget(target).build());
    while (!copy.isDone()) {
      copy.copyChunk();
    }
  }

  private void small() {
    String b = bucket().getName();
    upload(b, "dir/日本語 +%?.txt", "hello");
    check(Arrays.equals(read(b, "dir/日本語 +%?.txt"), bytes("hello")), "bytes differ");
  }

  private void resumable() throws IOException {
    String b = bucket().getName();
    byte[] data = filled(3 * 1024 * 1024 + 123);
    write(BlobInfo.newBuilder(b, "big.bin").build(), data);
    check(reload(BlobId.of(b, "big.bin")).getSize() == data.length, "size differs");
    check(Arrays.equals(read(b, "big.bin"), data), "bytes differ");
  }

  private void metadataOnWrite() {
    String b = bucket().getName();
    storage.create(BlobInfo.newBuilder(b, "meta.txt").setContentType("text/plain").setCacheControl("no-cache")
        .setContentDisposition("attachment").setContentLanguage("ja").setMetadata(Map.of("k", "v")).build(), bytes("x"));
    Blob blob = reload(BlobId.of(b, "meta.txt"));
    check(List.of("text/plain", "no-cache", "attachment", "ja").equals(Arrays.asList(blob.getContentType(),
        blob.getCacheControl(), blob.getContentDisposition(), blob.getContentLanguage()))
        && Map.of("k", "v").equals(blob.getMetadata()), "metadata differs");
  }

  private void badCrc() throws Exception {
    String b = bucket().getName();
    // crc32cMatch() sends the BlobInfo value instead of a computed one; create(byte[]) always
    // recomputes the checksum, so the resumable writer is the public path for a supplied CRC32C.
    BlobInfo info = BlobInfo.newBuilder(b, "crc.txt").setCrc32c("AAAAAA==").build();
    expectChecksumRejection(() -> write(info, bytes("payload"), BlobWriteOption.crc32cMatch()));
    check(storage.get(BlobId.of(b, "crc.txt")) == null, "rejected upload left an object");
  }

  @SuppressWarnings("deprecation") // md5Match() is the SDK's only way to send a supplied MD5.
  private void md5() throws Exception {
    String b = bucket().getName();
    byte[] data = bytes("md5 payload");
    String digest = Base64.getEncoder().encodeToString(MessageDigest.getInstance("MD5").digest(data));
    write(BlobInfo.newBuilder(b, "md5.txt").setMd5(digest).build(), data, BlobWriteOption.md5Match());
    check(digest.equals(reload(BlobId.of(b, "md5.txt")).getMd5()), "MD5 differs");
  }

  private void ranges() throws IOException {
    String b = bucket().getName();
    BlobId id = upload(b, "range.txt", "0123456789").getBlobId();
    check(Arrays.equals(range(id, 2, 6), bytes("2345")), "closed range");
    check(Arrays.equals(range(id, 7, -1), bytes("789")), "open range");
    check(Arrays.equals(range(id, -3, -1), bytes("789")), "suffix range");
  }

  private void updateMetadata() {
    String b = bucket().getName();
    Blob blob = storage.create(BlobInfo.newBuilder(b, "u.txt").setMetadata(Map.of("a", "1", "b", "2")).build(), bytes("x"));
    // The Java SDK removes a metadata key only when it is sent explicitly as null.
    Map<String, String> next = new HashMap<>();
    next.put("a", null);
    next.put("c", "3");
    storage.update(blob.toBuilder().setContentType("text/csv").setMetadata(next).build());
    Blob after = reload(blob.getBlobId());
    check("text/csv".equals(after.getContentType()) && Map.of("b", "2", "c", "3").equals(after.getMetadata()),
        String.valueOf(after.getMetadata()));
  }

  private void delete() {
    String b = bucket().getName();
    BlobId id = upload(b, "d.txt", "x").getBlobId();
    check(storage.delete(id), "delete reported a missing object");
    check(storage.get(id) == null, "deleted object visible");
    // Storage.delete reports HTTP 404 as false rather than throwing.
    check(!storage.delete(id), "second delete did not report not found");
  }

  private void createCondition() throws Exception {
    String b = bucket().getName();
    upload(b, "once.txt", "first", BlobTargetOption.doesNotExist());
    expectStatus(412, () -> upload(b, "once.txt", "second", BlobTargetOption.doesNotExist()));
    check(Arrays.equals(read(b, "once.txt"), bytes("first")), "precondition changed bytes");
  }

  private void generationCondition() throws Exception {
    String b = bucket().getName();
    Blob blob = upload(b, "g.txt", "x");
    BlobId id = BlobId.of(b, "g.txt");
    expectStatus(412, () -> storage.delete(id, BlobSourceOption.generationMatch(blob.getGeneration() + 1)));
    check(storage.delete(id, BlobSourceOption.generationMatch(blob.getGeneration())), "conditional delete failed");
  }

  private void metagenerationCondition() throws Exception {
    Blob blob = upload(bucket().getName(), "m.txt", "x");
    expectStatus(412, () -> storage.update(blob.toBuilder().setContentType("text/plain").build(),
        BlobTargetOption.metagenerationMatch(blob.getMetageneration() + 5)));
  }

  private void delimiter() {
    String b = bucket().getName();
    seed(b, "a/1", "a/2", "b/1", "c");
    List<String> objects = new ArrayList<>();
    List<String> prefixes = new ArrayList<>();
    for (Blob blob : storage.list(b, BlobListOption.delimiter("/")).iterateAll()) {
      (blob.isDirectory() ? prefixes : objects).add(blob.getName());
    }
    objects.sort(null);
    prefixes.sort(null);
    check(objects.equals(List.of("c")), "names differ: " + objects);
    check(prefixes.equals(List.of("a/", "b/")), "prefixes differ: " + prefixes);
    check(names(storage.list(b, BlobListOption.prefix("a/")).iterateAll()).equals(List.of("a/1", "a/2")),
        "prefix listing");
  }

  private void paging() {
    String b = bucket().getName();
    seed(b, "p1", "p2", "p3", "p4", "p5");
    List<String> found = new ArrayList<>();
    int pages = 0;
    Page<Blob> page = storage.list(b, BlobListOption.pageSize(2));
    while (page != null) {
      List<String> values = names(page.getValues());
      check(values.size() <= 2, "object page exceeds 2");
      found.addAll(values);
      pages++;
      page = page.hasNextPage() ? page.getNextPage() : null;
    }
    found.sort(null);
    check(pages >= 3 && found.equals(List.of("p1", "p2", "p3", "p4", "p5")), found.toString());
  }

  private void offsets() {
    String b = bucket().getName();
    seed(b, "a", "b", "c", "d");
    List<String> found = names(storage.list(b, BlobListOption.startOffset("b"), BlobListOption.endOffset("d")).iterateAll());
    check(found.equals(List.of("b", "c")), "offset listing: " + found);
  }

  private void glob() {
    String b = bucket().getName();
    seed(b, "x/a.txt", "x/b.csv", "y/c.txt");
    List<String> found = names(storage.list(b, BlobListOption.matchGlob("**/*.txt")).iterateAll());
    check(found.equals(List.of("x/a.txt", "y/c.txt")), "glob listing: " + found);
  }

  private void copy() {
    String source = bucket().getName();
    String destination = bucket().getName();
    upload(source, "src", "copy me");
    // Storage.copy issues objects.rewrite and accepts destination metadata, like Python's rewrite.
    rewriteTo(BlobId.of(source, "src"), BlobInfo.newBuilder(destination, "dst").setContentType("text/plain")
        .setMetadata(Map.of("copied", "yes")).build());
    Blob dest = reload(BlobId.of(destination, "dst"));
    check("text/plain".equals(dest.getContentType()) && Map.of("copied", "yes").equals(dest.getMetadata()),
        "copy metadata");
    check(Arrays.equals(read(destination, "dst"), bytes("copy me")), "copy bytes");
  }

  private void rewrite() {
    String b = bucket().getName();
    byte[] data = filled(2 * 1024 * 1024 + 7);
    upload(b, "src", data);
    rewriteTo(BlobId.of(b, "src"), BlobInfo.newBuilder(b, "dst").setStorageClass(StorageClass.NEARLINE).build());
    check(reload(BlobId.of(b, "dst")).getStorageClass() == StorageClass.NEARLINE, "storage class differs");
    check(Arrays.equals(read(b, "dst"), data), "rewrite bytes");
  }

  private void compose() {
    String b = bucket().getName();
    seed(b, "p1", "p2", "p3");
    Blob dest = storage.compose(Storage.ComposeRequest.newBuilder().addSource("p1", "p2", "p3")
        .setTarget(BlobInfo.newBuilder(b, "all").build()).build());
    check(Integer.valueOf(3).equals(dest.getComponentCount()), "component count");
    check(Arrays.equals(read(b, "all"), bytes("p1p2p3")), "compose bytes");
  }

  private void move() {
    String b = bucket().getName();
    upload(b, "old", "moved");
    storage.moveBlob(Storage.MoveBlobRequest.newBuilder().setSource(BlobId.of(b, "old"))
        .setTarget(BlobId.of(b, "new")).build());
    check(storage.get(BlobId.of(b, "old")) == null, "source remains");
    check(Arrays.equals(read(b, "new"), bytes("moved")), "move bytes");
  }

  // Like the Python probe, the ACL is written by patching the object's full ACL list.
  private void acl() {
    BlobId id = upload(bucket().getName(), "acl.txt", "x").getBlobId();
    BlobId live = BlobId.of(id.getBucket(), id.getName());
    List<Acl> acls = new ArrayList<>(storage.listAcls(live));
    acls.add(Acl.of(Acl.User.ofAllUsers(), Acl.Role.READER));
    storage.update(BlobInfo.newBuilder(live).setAcl(acls).build());
    check(BucketCases.grantsRead(storage.listAcls(live), Acl.User.ofAllUsers()), "ACL missing");
  }

  private void holds() throws Exception {
    Blob blob = upload(bucket().getName(), "hold.txt", "x");
    BlobId id = BlobId.of(blob.getBucket(), blob.getName());
    Blob held = storage.update(blob.toBuilder().setTemporaryHold(true).build());
    expectStatus(403, () -> storage.delete(id));
    storage.update(held.toBuilder().setTemporaryHold(false).build());
    check(storage.delete(id), "released object delete failed");
  }

  private void retention() throws Exception {
    Bucket b = bucket(Storage.BucketTargetOption.enableObjectRetention(true));
    Blob blob = upload(b.getName(), "ret.txt", "x");
    BlobInfo.Retention retention = BlobInfo.Retention.newBuilder().setMode(BlobInfo.Retention.Mode.UNLOCKED)
        .setRetainUntilTime(OffsetDateTime.now(ZoneOffset.UTC).plusHours(1)).build();
    storage.update(blob.toBuilder().setRetention(retention).build());
    BlobId id = BlobId.of(b.getName(), "ret.txt");
    BlobInfo.Retention stored = reload(id).getRetention();
    check(stored != null && stored.getMode() == BlobInfo.Retention.Mode.UNLOCKED, "retention not stored");
    expectStatus(403, () -> storage.delete(id));
  }

  private void customTime() {
    Blob blob = upload(bucket().getName(), "ct.txt", "x");
    OffsetDateTime want = OffsetDateTime.of(2026, 1, 2, 3, 4, 5, 0, ZoneOffset.UTC);
    storage.update(blob.toBuilder().setCustomTimeOffsetDateTime(want).build());
    OffsetDateTime got = reload(BlobId.of(blob.getBucket(), blob.getName())).getCustomTimeOffsetDateTime();
    check(got != null && got.isEqual(want), "customTime differs: " + got);
  }

  private void storageClass() {
    String b = bucket().getName();
    storage.create(BlobInfo.newBuilder(b, "cold").setStorageClass(StorageClass.COLDLINE).build(), bytes("x"));
    check(reload(BlobId.of(b, "cold")).getStorageClass() == StorageClass.COLDLINE, "storage class differs");
  }

  private void transcoding() throws IOException {
    String b = bucket().getName();
    byte[] plain = bytes("compress me ".repeat(50));
    ByteArrayOutputStream buffer = new ByteArrayOutputStream();
    try (GZIPOutputStream gzip = new GZIPOutputStream(buffer)) {
      gzip.write(plain);
    }
    byte[] zipped = buffer.toByteArray();
    storage.create(BlobInfo.newBuilder(b, "gz.txt").setContentEncoding("gzip").setContentType("text/plain").build(), zipped);
    check(Arrays.equals(read(b, "gz.txt", BlobSourceOption.shouldReturnRawInputStream(false)), plain),
        "decompressed bytes");
    check(Arrays.equals(read(b, "gz.txt", BlobSourceOption.shouldReturnRawInputStream(true)), zipped),
        "compressed bytes");
  }

  private void csek() {
    String b = bucket().getName();
    byte[] raw = new byte[32];
    new SecureRandom().nextBytes(raw);
    String key = Base64.getEncoder().encodeToString(raw);
    storage.create(BlobInfo.newBuilder(b, "csek.txt").build(), bytes("secret"), BlobTargetOption.encryptionKey(key));
    try {
      read(b, "csek.txt");
      throw new AssertionError("expected an API error without the key, got success");
    } catch (StorageException expected) {
      // Any API error proves the object is unreadable without its key.
    }
    check(Arrays.equals(read(b, "csek.txt", BlobSourceOption.decryptionKey(key)), bytes("secret")), "keyed bytes");
  }

  private void restore() {
    Bucket bucket = bucket();
    storage.update(bucket.toBuilder().setSoftDeletePolicy(BucketInfo.SoftDeletePolicy.newBuilder()
        .setRetentionDuration(Duration.ofSeconds(604800)).build()).build());
    String b = bucket.getName();
    long generation = upload(b, "sd.txt", "restore me").getGeneration();
    check(storage.delete(BlobId.of(b, "sd.txt")), "delete failed");
    List<String> deleted = names(storage.list(b, BlobListOption.softDeleted(true)).iterateAll());
    check(deleted.equals(List.of("sd.txt")), "soft-deleted listing: " + deleted);
    storage.restore(BlobId.of(b, "sd.txt", generation));
    check(Arrays.equals(read(b, "sd.txt"), bytes("restore me")), "restored bytes");
  }
}
