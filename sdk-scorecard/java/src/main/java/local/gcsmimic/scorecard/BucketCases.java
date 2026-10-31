package local.gcsmimic.scorecard;

import com.google.api.gax.paging.Page;
import com.google.cloud.Binding;
import com.google.cloud.Policy;
import com.google.cloud.storage.Acl;
import com.google.cloud.storage.Blob;
import com.google.cloud.storage.BlobId;
import com.google.cloud.storage.Bucket;
import com.google.cloud.storage.BucketInfo;
import com.google.cloud.storage.BucketInfo.LifecycleRule;
import com.google.cloud.storage.Cors;
import com.google.cloud.storage.HmacKey;
import com.google.cloud.storage.HttpMethod;
import com.google.cloud.storage.Notification;
import com.google.cloud.storage.NotificationInfo;
import com.google.cloud.storage.ServiceAccount;
import com.google.cloud.storage.Storage.BlobListOption;
import com.google.cloud.storage.Storage.BucketListOption;
import com.google.cloud.storage.Storage.BucketSourceOption;
import com.google.cloud.storage.Storage.BucketTargetOption;
import com.google.cloud.storage.StorageClass;
import java.time.Duration;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.function.Function;

/** Bucket and project probes, in the canonical case order. */
final class BucketCases extends Cases {
  private static final String VIEWER = "roles/storage.objectViewer";
  private static final String MEMBER = "user:someone@example.invalid";

  BucketCases(Probe.Context context) {
    super(context);
  }

  @Override
  void register(List<Probe.Case> cases) {
    cases.add(new Probe.Case("bucket.create_get_delete", this::createGetDelete));
    cases.add(new Probe.Case("bucket.list_prefix_paging", this::listPaging));
    cases.add(new Probe.Case("bucket.update_labels", this::labels));
    cases.add(new Probe.Case("bucket.metageneration_precondition", this::metagenerationCondition));
    cases.add(new Probe.Case("bucket.versioning_history", this::versioning));
    cases.add(new Probe.Case("bucket.lifecycle", this::lifecycle));
    cases.add(new Probe.Case("bucket.cors", this::cors));
    cases.add(new Probe.Case("bucket.retention_policy", this::retentionPolicy));
    cases.add(new Probe.Case("bucket.retention_lock", this::retentionLock));
    cases.add(new Probe.Case("bucket.uniform_bucket_level_access", this::ubla));
    cases.add(new Probe.Case("bucket.public_access_prevention", this::pap));
    cases.add(new Probe.Case("bucket.iam_policy", this::iam));
    cases.add(new Probe.Case("bucket.iam_test_permissions", this::permissions));
    cases.add(new Probe.Case("bucket.soft_delete_policy", this::softDeletePolicy));
    cases.add(new Probe.Case("bucket.autoclass", this::autoclass));
    cases.add(new Probe.Case("bucket.acl", this::bucketAcl));
    cases.add(new Probe.Case("bucket.default_object_acl", this::defaultAcl));
    cases.add(new Probe.Case("bucket.notifications", this::notifications));
    cases.add(new Probe.Case("project.hmac_keys", this::hmac));
    cases.add(new Probe.Case("project.service_account", this::serviceAccount));
  }

  private Bucket patch(Bucket bucket, Function<BucketInfo.Builder, BucketInfo.Builder> change) {
    storage.update(change.apply(bucket.toBuilder()).build());
    return reload(bucket.getName());
  }

  private void createGetDelete() {
    Bucket b = bucket();
    check(reload(b.getName()).getName().equals(b.getName()), "get returned another bucket");
    check(storage.delete(b.getName()), "delete reported a missing bucket");
    check(storage.get(b.getName()) == null, "deleted bucket visible");
  }

  private void listPaging() {
    String prefix = unique();
    for (int i = 0; i < 3; i++) {
      storage.create(BucketInfo.of(prefix + "-" + i));
    }
    List<String> found = new ArrayList<>();
    Page<Bucket> page = storage.list(BucketListOption.prefix(prefix), BucketListOption.pageSize(2));
    while (page != null) {
      int count = 0;
      for (Bucket b : page.getValues()) {
        found.add(b.getName());
        count++;
      }
      check(count <= 2, "bucket page exceeds 2");
      page = page.hasNextPage() ? page.getNextPage() : null;
    }
    found.sort(null);
    check(found.equals(List.of(prefix + "-0", prefix + "-1", prefix + "-2")), found.toString());
  }

  private void labels() {
    Bucket b = storage.create(BucketInfo.newBuilder(unique()).setLabels(Map.of("a", "1", "b", "2")).build());
    // The Java SDK removes a label only when it is sent explicitly as null.
    Map<String, String> next = new HashMap<>();
    next.put("a", null);
    next.put("b", "2");
    next.put("c", "3");
    Map<String, String> labels = patch(b, x -> x.setLabels(next)).getLabels();
    check(Map.of("b", "2", "c", "3").equals(labels), String.valueOf(labels));
  }

  private void metagenerationCondition() throws Exception {
    Bucket stale = bucket();
    // BucketTargetOption.metagenerationMatch() reads the value from BucketInfo, whose setter is
    // not public; a bucket snapshot made stale by one update supplies a mismatching value.
    storage.update(stale.toBuilder().setLabels(Map.of("touch", "1")).build());
    expectStatus(412, () -> storage.update(stale.toBuilder().setStorageClass(StorageClass.NEARLINE).build(),
        BucketTargetOption.metagenerationMatch()));
  }

  private void versioning() {
    Bucket b = storage.create(BucketInfo.newBuilder(unique()).setVersioningEnabled(true).build());
    Blob first = upload(b.getName(), "v.txt", "one");
    upload(b.getName(), "v.txt", "two");
    int versions = 0;
    for (Blob ignored : storage.list(b.getName(), BlobListOption.versions(true)).iterateAll()) {
      versions++;
    }
    check(versions == 2, "version count " + versions);
    byte[] old = storage.readAllBytes(BlobId.of(b.getName(), "v.txt", first.getGeneration()));
    check(Arrays.equals(old, bytes("one")), "old bytes");
  }

  private void lifecycle() {
    LifecycleRule rule = new LifecycleRule(LifecycleRule.LifecycleAction.newDeleteAction(),
        LifecycleRule.LifecycleCondition.newBuilder().setAge(30).build());
    List<? extends LifecycleRule> rules = patch(bucket(), x -> x.setLifecycleRules(List.of(rule))).getLifecycleRules();
    check(rules != null && rules.size() == 1 && Integer.valueOf(30).equals(rules.get(0).getCondition().getAge()),
        String.valueOf(rules));
  }

  private void cors() {
    Cors rule = Cors.newBuilder().setOrigins(List.of(Cors.Origin.of("https://app.example")))
        .setMethods(List.of(HttpMethod.GET)).setMaxAgeSeconds(3600).build();
    List<Cors> cors = patch(bucket(), x -> x.setCors(List.of(rule))).getCors();
    check(cors != null && cors.size() == 1
        && cors.get(0).getOrigins().equals(List.of(Cors.Origin.of("https://app.example"))), String.valueOf(cors));
  }

  private void retentionPolicy() throws Exception {
    Bucket b = patch(bucket(), x -> x.setRetentionPeriodDuration(Duration.ofHours(1)));
    Blob blob = upload(b.getName(), "r", "r");
    expectStatus(403, () -> storage.delete(blob.getBlobId()));
  }

  private void retentionLock() {
    Bucket b = patch(bucket(), x -> x.setRetentionPeriodDuration(Duration.ofHours(1)));
    storage.lockRetentionPolicy(b, BucketTargetOption.metagenerationMatch());
    check(Boolean.TRUE.equals(reload(b.getName()).retentionPolicyIsLocked()), "policy not locked");
  }

  private void ubla() {
    Bucket b = bucket();
    Bucket after = patch(b, x -> x.setIamConfiguration(b.getIamConfiguration().toBuilder()
        .setIsUniformBucketLevelAccessEnabled(true).build()));
    check(Boolean.TRUE.equals(after.getIamConfiguration().isUniformBucketLevelAccessEnabled()), "UBLA not enabled");
  }

  private void pap() {
    Bucket b = bucket();
    Bucket after = patch(b, x -> x.setIamConfiguration(b.getIamConfiguration().toBuilder()
        .setPublicAccessPrevention(BucketInfo.PublicAccessPrevention.ENFORCED).build()));
    check(after.getIamConfiguration().getPublicAccessPrevention() == BucketInfo.PublicAccessPrevention.ENFORCED,
        "PAP not enforced");
  }

  private void iam() {
    String name = bucket().getName();
    Policy policy = storage.getIamPolicy(name, BucketSourceOption.requestedPolicyVersion(3));
    List<Binding> bindings = new ArrayList<>(policy.getBindingsList());
    bindings.add(Binding.newBuilder().setRole(VIEWER).setMembers(List.of(MEMBER)).build());
    storage.setIamPolicy(name, policy.toBuilder().setBindings(bindings).build());
    policy = storage.getIamPolicy(name, BucketSourceOption.requestedPolicyVersion(3));
    check(policy.getBindingsList().stream()
        .anyMatch(b -> b.getRole().equals(VIEWER) && b.getMembers().contains(MEMBER)), "missing binding");
  }

  private void permissions() {
    List<Boolean> allowed = storage.testIamPermissions(bucket().getName(),
        List.of("storage.buckets.get", "storage.objects.list"));
    check(allowed.contains(Boolean.TRUE), "no permissions");
  }

  private void softDeletePolicy() {
    BucketInfo.SoftDeletePolicy policy = BucketInfo.SoftDeletePolicy.newBuilder()
        .setRetentionDuration(Duration.ofSeconds(864000)).build();
    BucketInfo.SoftDeletePolicy after = patch(bucket(), x -> x.setSoftDeletePolicy(policy)).getSoftDeletePolicy();
    check(after != null && Duration.ofSeconds(864000).equals(after.getRetentionDuration()),
        "soft delete policy differs");
  }

  private void autoclass() {
    Bucket b = storage.create(BucketInfo.newBuilder(unique())
        .setAutoclass(BucketInfo.Autoclass.newBuilder().setEnabled(true).build()).build());
    BucketInfo.Autoclass autoclass = reload(b.getName()).getAutoclass();
    check(autoclass != null && Boolean.TRUE.equals(autoclass.getEnabled()), "autoclass not enabled");
  }

  static boolean grantsRead(List<Acl> acls, Acl.Entity entity) {
    return acls.stream().anyMatch(a -> a.getEntity().equals(entity) && a.getRole() == Acl.Role.READER);
  }

  // Like the Python probe, the ACL is written by patching the resource's full ACL list.
  private void bucketAcl() {
    String name = bucket().getName();
    List<Acl> acls = new ArrayList<>(storage.listAcls(name));
    acls.add(Acl.of(Acl.User.ofAllAuthenticatedUsers(), Acl.Role.READER));
    storage.update(BucketInfo.newBuilder(name).setAcl(acls).build());
    check(grantsRead(storage.listAcls(name), Acl.User.ofAllAuthenticatedUsers()), "ACL missing");
  }

  private void defaultAcl() {
    String name = bucket().getName();
    List<Acl> acls = new ArrayList<>(storage.listDefaultAcls(name));
    acls.add(Acl.of(Acl.User.ofAllAuthenticatedUsers(), Acl.Role.READER));
    storage.update(BucketInfo.newBuilder(name).setDefaultAcl(acls).build());
    check(grantsRead(storage.listDefaultAcls(name), Acl.User.ofAllAuthenticatedUsers()), "ACL missing");
  }

  private void notifications() {
    String name = bucket().getName();
    NotificationInfo info = NotificationInfo.newBuilder("projects/" + context.project + "/topics/scorecard")
        .setPayloadFormat(NotificationInfo.PayloadFormat.JSON_API_V1).build();
    Notification notification = storage.createNotification(name, info);
    check(storage.listNotifications(name).stream()
        .anyMatch(n -> n.getNotificationId().equals(notification.getNotificationId())), "notification missing");
    check(storage.deleteNotification(name, notification.getNotificationId()), "notification delete not found");
  }

  private void hmac() {
    HmacKey key = storage.createHmacKey(ServiceAccount.of("sa@example.invalid"));
    check(key.getSecretKey() != null && !key.getSecretKey().isEmpty()
        && key.getMetadata().getAccessId() != null && !key.getMetadata().getAccessId().isEmpty(), "empty HMAC key");
    HmacKey.HmacKeyMetadata inactive = storage.updateHmacKeyState(key.getMetadata(), HmacKey.HmacKeyState.INACTIVE);
    storage.deleteHmacKey(inactive);
  }

  private void serviceAccount() {
    ServiceAccount account = storage.getServiceAccount(context.project);
    check(account != null && account.getEmail() != null && account.getEmail().contains("@"), "service account missing");
  }
}
