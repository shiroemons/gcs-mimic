// Bucket and project cases: the first 21 shared IDs.
#include "harness.h"
#include <chrono>
#include <map>
#include <optional>

namespace scorecard {
namespace {

using std::chrono::seconds;

gcs::BucketMetadata PatchedBucket(Ctx& c, std::string const& bucket,
                                  gcs::BucketMetadataPatchBuilder const& patch) {
  return Require(c.sdk.PatchBucket(bucket, patch), "patch bucket");
}

void BucketCreateGetDelete(Ctx& c) {
  auto const bucket = NewBucket(c);
  CheckEqual(BucketInfo(c, bucket).name(), bucket, "bucket name");
  Require(c.sdk.DeleteBucket(bucket), "delete bucket");
  ExpectHttp(c.sdk.GetBucketMetadata(bucket), 404, "deleted bucket still visible");
}

void BucketListPrefixPaging(Ctx& c) {
  auto const prefix = UniqueName();
  std::vector<std::string> expected;
  for (int i = 0; i < 3; ++i) {
    expected.push_back(prefix + "-" + std::to_string(i));
    NewBucketNamed(c, expected.back());
  }
  // ListBucketsExtended is the only public reader that exposes page
  // boundaries; ListBuckets flattens them.
  std::vector<std::string> found;
  for (auto& page : c.sdk.ListBucketsExtended(gcs::Prefix(prefix), gcs::MaxResults(2))) {
    auto values = Require(std::move(page), "list buckets");
    Check(values.buckets.size() <= 2, "bucket page exceeds 2");
    for (auto const& bucket : values.buckets) found.push_back(bucket.name());
  }
  std::sort(found.begin(), found.end());
  CheckEqual(found, expected, "bucket listing");
}

void BucketUpdateLabels(Ctx& c) {
  gcs::BucketMetadata original;
  original.mutable_labels() = {{"a", "1"}, {"b", "2"}};
  auto const bucket = NewBucket(c, original);
  auto updated = BucketInfo(c, bucket);
  updated.mutable_labels() = {{"b", "2"}, {"c", "3"}};
  Require(c.sdk.PatchBucket(bucket, BucketInfo(c, bucket), updated), "patch labels");
  auto const labels = BucketInfo(c, bucket).labels();
  Check(labels == (std::map<std::string, std::string>{{"b", "2"}, {"c", "3"}}),
        "labels differ: " + std::to_string(labels.size()) + " entries");
}

void BucketMetagenerationPrecondition(Ctx& c) {
  auto const bucket = NewBucket(c);
  ExpectHttp(c.sdk.PatchBucket(bucket, gcs::BucketMetadataPatchBuilder().SetStorageClass("NEARLINE"),
                               gcs::IfMetagenerationMatch(999)),
             412, "patch with a wrong metageneration");
}

void BucketVersioningHistory(Ctx& c) {
  gcs::BucketMetadata metadata;
  metadata.enable_versioning();
  auto const bucket = NewBucket(c, metadata);
  auto const first = Upload(c, bucket, "v.txt", "one");
  Upload(c, bucket, "v.txt", "two");
  Check(ListNames(c, bucket, gcs::Versions(true)).size() == 2, "version count");
  CheckEqual(Download(c, bucket, "v.txt", gcs::Generation(first.generation())), "one",
             "old generation bytes");
}

void BucketLifecycle(Ctx& c) {
  auto const bucket = NewBucket(c);
  gcs::BucketLifecycle lifecycle;
  lifecycle.rule.emplace_back(gcs::LifecycleRule::MaxAge(30), gcs::LifecycleRule::Delete());
  PatchedBucket(c, bucket, gcs::BucketMetadataPatchBuilder().SetLifecycle(lifecycle));
  auto const info = BucketInfo(c, bucket);
  Check(info.has_lifecycle() && info.lifecycle().rule.size() == 1 &&
            info.lifecycle().rule[0].condition().age == 30,
        "lifecycle rule differs");
}

void BucketCors(Ctx& c) {
  auto const bucket = NewBucket(c);
  gcs::CorsEntry entry;
  entry.max_age_seconds = 3600;
  entry.method = {"GET"};
  entry.origin = {"https://app.example"};
  PatchedBucket(c, bucket, gcs::BucketMetadataPatchBuilder().SetCors({entry}));
  auto const cors = BucketInfo(c, bucket).cors();
  Check(cors.size() == 1 && cors[0].origin == std::vector<std::string>{"https://app.example"},
        "CORS differs");
}

void BucketRetentionPolicy(Ctx& c) {
  auto const bucket = NewBucket(c);
  PatchedBucket(c, bucket, gcs::BucketMetadataPatchBuilder().SetRetentionPolicy(seconds(3600)));
  Upload(c, bucket, "r", "r");
  ExpectHttp(c.sdk.DeleteObject(bucket, "r"), 403, "delete under retention");
}

void BucketRetentionLock(Ctx& c) {
  auto const bucket = NewBucket(c);
  PatchedBucket(c, bucket, gcs::BucketMetadataPatchBuilder().SetRetentionPolicy(seconds(3600)));
  auto const before = BucketInfo(c, bucket);
  Require(c.sdk.LockBucketRetentionPolicy(
              bucket, static_cast<std::uint64_t>(before.metageneration())),
          "lock retention policy");
  auto const after = BucketInfo(c, bucket);
  Check(after.has_retention_policy() && after.retention_policy().is_locked,
        "policy not locked");
}

void BucketUniformAccess(Ctx& c) {
  auto const bucket = NewBucket(c);
  gcs::BucketIamConfiguration config;
  config.uniform_bucket_level_access = gcs::UniformBucketLevelAccess{true, {}};
  PatchedBucket(c, bucket, gcs::BucketMetadataPatchBuilder().SetIamConfiguration(config));
  auto const info = BucketInfo(c, bucket);
  Check(info.has_iam_configuration() &&
            info.iam_configuration().uniform_bucket_level_access.has_value() &&
            info.iam_configuration().uniform_bucket_level_access->enabled,
        "UBLA not enabled");
}

void BucketPublicAccessPrevention(Ctx& c) {
  auto const bucket = NewBucket(c);
  gcs::BucketIamConfiguration config;
  config.public_access_prevention = "enforced";
  PatchedBucket(c, bucket, gcs::BucketMetadataPatchBuilder().SetIamConfiguration(config));
  auto const info = BucketInfo(c, bucket);
  Check(info.has_iam_configuration() &&
            info.iam_configuration().public_access_prevention == std::optional<std::string>("enforced"),
        "PAP not enforced");
}

void BucketIamPolicy(Ctx& c) {
  auto const bucket = NewBucket(c);
  constexpr char kRole[] = "roles/storage.objectViewer";
  constexpr char kMember[] = "user:someone@example.invalid";
  auto policy = Require(c.sdk.GetNativeBucketIamPolicy(bucket, gcs::RequestedPolicyVersion(3)),
                        "get IAM policy");
  policy.bindings().emplace_back(kRole, std::vector<std::string>{kMember});
  Require(c.sdk.SetNativeBucketIamPolicy(bucket, policy), "set IAM policy");
  policy = Require(c.sdk.GetNativeBucketIamPolicy(bucket, gcs::RequestedPolicyVersion(3)),
                   "get IAM policy again");
  bool found = false;
  for (auto const& binding : policy.bindings()) {
    for (auto const& member : binding.members()) {
      found = found || (binding.role() == kRole && member == kMember);
    }
  }
  Check(found, "missing binding");
}

void BucketIamTestPermissions(Ctx& c) {
  auto const bucket = NewBucket(c);
  auto const granted = Require(
      c.sdk.TestBucketIamPermissions(bucket, {"storage.buckets.get", "storage.objects.list"}),
      "test IAM permissions");
  Check(!granted.empty(), "no permissions");
}

void BucketSoftDeletePolicy(Ctx& c) {
  auto const bucket = NewBucket(c);
  PatchedBucket(c, bucket,
                gcs::BucketMetadataPatchBuilder().SetSoftDeletePolicy(seconds(864000)));
  auto const info = BucketInfo(c, bucket);
  Check(info.has_soft_delete_policy() &&
            info.soft_delete_policy().retention_duration == seconds(864000),
        "soft delete policy differs");
}

void BucketAutoclass(Ctx& c) {
  gcs::BucketMetadata metadata;
  metadata.set_autoclass(gcs::BucketAutoclass(true));
  auto const bucket = NewBucket(c, metadata);
  auto const info = BucketInfo(c, bucket);
  Check(info.has_autoclass() && info.autoclass().enabled, "autoclass not enabled");
}

void BucketAcl(Ctx& c) {
  auto const bucket = NewBucket(c);
  Require(c.sdk.CreateBucketAcl(bucket, "allAuthenticatedUsers",
                                gcs::BucketAccessControl::ROLE_READER()),
          "create bucket ACL");
  bool found = false;
  for (auto const& entry : Require(c.sdk.ListBucketAcl(bucket), "list bucket ACL")) {
    found = found || (entry.entity() == "allAuthenticatedUsers" && entry.role() == "READER");
  }
  Check(found, "ACL missing");
}

void BucketDefaultObjectAcl(Ctx& c) {
  auto const bucket = NewBucket(c);
  Require(c.sdk.CreateDefaultObjectAcl(bucket, "allAuthenticatedUsers",
                                       gcs::ObjectAccessControl::ROLE_READER()),
          "create default object ACL");
  bool found = false;
  for (auto const& entry :
       Require(c.sdk.ListDefaultObjectAcl(bucket), "list default object ACL")) {
    found = found || (entry.entity() == "allAuthenticatedUsers" && entry.role() == "READER");
  }
  Check(found, "ACL missing");
}

void BucketNotifications(Ctx& c) {
  auto const bucket = NewBucket(c);
  auto const topic = "projects/" + c.config.project + "/topics/scorecard";
  auto const created = Require(
      c.sdk.CreateNotification(bucket, topic, gcs::payload_format::JsonApiV1(),
                               gcs::NotificationMetadata()),
      "create notification");
  bool found = false;
  for (auto const& entry : Require(c.sdk.ListNotifications(bucket), "list notifications")) {
    found = found || entry.id() == created.id();
  }
  Check(found, "notification missing");
  Require(c.sdk.DeleteNotification(bucket, created.id()), "delete notification");
}

void ProjectHmacKeys(Ctx& c) {
  auto const created = Require(c.sdk.CreateHmacKey("sa@example.invalid"), "create HMAC key");
  Check(!created.second.empty() && !created.first.access_id().empty(), "empty HMAC key");
  Require(c.sdk.UpdateHmacKey(created.first.access_id(),
                              gcs::HmacKeyMetadata().set_state(gcs::HmacKeyMetadata::state_inactive())),
          "deactivate HMAC key");
  Require(c.sdk.DeleteHmacKey(created.first.access_id()), "delete HMAC key");
}

void ProjectServiceAccount(Ctx& c) {
  auto const account =
      Require(c.sdk.GetServiceAccountForProject(c.config.project), "get service account");
  Check(account.email_address().find('@') != std::string::npos, "service account missing");
}

}  // namespace

void AddBucketCases(std::vector<Case>& cases) {
  cases.insert(cases.end(),
               {
                   {"bucket.create_get_delete", BucketCreateGetDelete},
                   {"bucket.list_prefix_paging", BucketListPrefixPaging},
                   {"bucket.update_labels", BucketUpdateLabels},
                   {"bucket.metageneration_precondition", BucketMetagenerationPrecondition},
                   {"bucket.versioning_history", BucketVersioningHistory},
                   {"bucket.lifecycle", BucketLifecycle},
                   {"bucket.cors", BucketCors},
                   {"bucket.retention_policy", BucketRetentionPolicy},
                   {"bucket.retention_lock", BucketRetentionLock},
                   {"bucket.uniform_bucket_level_access", BucketUniformAccess},
                   {"bucket.public_access_prevention", BucketPublicAccessPrevention},
                   {"bucket.iam_policy", BucketIamPolicy},
                   {"bucket.iam_test_permissions", BucketIamTestPermissions},
                   {"bucket.soft_delete_policy", BucketSoftDeletePolicy},
                   {"bucket.soft_delete_restore", nullptr,
                    "google-cloud-cpp 3.14.0 has no public API for restoring a soft-deleted "
                    "bucket (Client has RestoreObject only)"},
                   {"bucket.autoclass", BucketAutoclass},
                   {"bucket.acl", BucketAcl},
                   {"bucket.default_object_acl", BucketDefaultObjectAcl},
                   {"bucket.notifications", BucketNotifications},
                   {"project.hmac_keys", ProjectHmacKeys},
                   {"project.service_account", ProjectServiceAccount},
               });
}

}  // namespace scorecard
