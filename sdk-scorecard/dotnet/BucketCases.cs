using System.Net;
using Google;
using Google.Apis.Storage.v1.Data;
using Google.Cloud.Storage.V1;

internal static partial class Probe
{
    private static IEnumerable<Case> BucketCases() =>
    [
        new("bucket.create_get_delete", BucketCrud),
        new("bucket.list_prefix_paging", BucketPaging),
        new("bucket.update_labels", Labels),
        new("bucket.metageneration_precondition", BucketCondition),
        new("bucket.versioning_history", Versioning),
        new("bucket.lifecycle", Lifecycle),
        new("bucket.cors", Cors),
        new("bucket.retention_policy", RetentionPolicy),
        new("bucket.retention_lock", RetentionLock),
        new("bucket.uniform_bucket_level_access", Ubla),
        new("bucket.public_access_prevention", Pap),
        new("bucket.iam_policy", Iam),
        new("bucket.iam_test_permissions", Permissions),
        new("bucket.soft_delete_policy", SoftDeletePolicy),
        new("bucket.autoclass", Autoclass),
        new("bucket.acl", BucketAcl),
        new("bucket.default_object_acl", DefaultObjectAcl),
        new("bucket.notifications", Notifications),
        new("project.hmac_keys", Hmac),
        new("project.service_account", ServiceAccountEmail),
    ];

    private static Task<Bucket> Patch(Bucket bucket, CancellationToken token) => Client.PatchBucketAsync(bucket, null, token);

    private static Task<Bucket> Get(string bucket, CancellationToken token, Projection? projection = null) =>
        Client.GetBucketAsync(bucket, new GetBucketOptions { Projection = projection }, token);

    private static async Task BucketCrud(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        Check((await Get(bucket.Name, token)).Name == bucket.Name);
        await Client.DeleteBucketAsync(bucket.Name, null, token);
        await ExpectStatus(HttpStatusCode.NotFound, () => Get(bucket.Name, token));
    }

    private static async Task BucketPaging(CancellationToken token)
    {
        var prefix = Unique();
        for (var i = 0; i < 3; i++) await Client.CreateBucketAsync(Project, $"{prefix}-{i}", null, token);
        var found = new List<string>();
        var pages = Client.ListBucketsAsync(Project, new ListBucketsOptions { Prefix = prefix, PageSize = 2 }).AsRawResponses();
        await foreach (var page in pages.WithCancellation(token))
        {
            var values = page.Items ?? [];
            Check(values.Count <= 2, "bucket page exceeds 2");
            found.AddRange(values.Select(bucket => bucket.Name));
        }
        found.Sort(StringComparer.Ordinal);
        Check(found.SequenceEqual(Enumerable.Range(0, 3).Select(i => $"{prefix}-{i}")), string.Join(",", found));
    }

    private static async Task Labels(CancellationToken token)
    {
        var bucket = await NewBucket(new Bucket { Name = Unique(), Labels = new Dictionary<string, string> { ["a"] = "1", ["b"] = "2" } }, token);
        // ModifyBucketLabels is the SDK's public way to remove one label while setting another.
        await Client.ModifyBucketLabelsAsync(bucket.Name, new Dictionary<string, string?> { ["a"] = null, ["c"] = "3" }, null, token);
        var labels = (await Get(bucket.Name, token)).Labels ?? new Dictionary<string, string>();
        Check(SameMap(labels, ("b", "2"), ("c", "3")),
            string.Join(",", labels.Select(pair => $"{pair.Key}={pair.Value}")));
    }

    private static async Task BucketCondition(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        await ExpectStatus(HttpStatusCode.PreconditionFailed, () => Client.PatchBucketAsync(
            new Bucket { Name = bucket.Name, StorageClass = "NEARLINE" }, new PatchBucketOptions { IfMetagenerationMatch = 999 }, token));
    }

    private static async Task Versioning(CancellationToken token)
    {
        var bucket = await NewBucket(new Bucket { Name = Unique(), Versioning = new Bucket.VersioningData { Enabled = true } }, token);
        var first = await Upload(bucket.Name, "v.txt", Bytes("one"), token);
        await Upload(bucket.Name, "v.txt", Bytes("two"), token);
        var versions = await Names(Client.ListObjectsAsync(bucket.Name, null, new ListObjectsOptions { Versions = true }));
        Check(versions.Count == 2, "version count");
        var old = await Read(bucket.Name, "v.txt", token, new DownloadObjectOptions { Generation = first.Generation });
        Check(old.SequenceEqual(Bytes("one")), "old bytes");
    }

    private static async Task Lifecycle(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        var rule = new Bucket.LifecycleData.RuleData
        {
            Action = new Bucket.LifecycleData.RuleData.ActionData { Type = "Delete" },
            Condition = new Bucket.LifecycleData.RuleData.ConditionData { Age = 30 },
        };
        await Patch(new Bucket { Name = bucket.Name, Lifecycle = new Bucket.LifecycleData { Rule = [rule] } }, token);
        var rules = (await Get(bucket.Name, token)).Lifecycle?.Rule ?? [];
        Check(rules.Count == 1 && rules[0].Condition?.Age == 30, $"{rules.Count} rules");
    }

    private static async Task Cors(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        var cors = new Bucket.CorsData { Origin = ["https://app.example"], Method = ["GET"], MaxAgeSeconds = 3600 };
        await Patch(new Bucket { Name = bucket.Name, Cors = [cors] }, token);
        var stored = (await Get(bucket.Name, token)).Cors ?? [];
        Check(stored.Count == 1 && stored[0].Origin?.SequenceEqual(["https://app.example"]) == true, $"{stored.Count} CORS entries");
    }

    private static Task<Bucket> Retain(string bucket, CancellationToken token) =>
        Patch(new Bucket { Name = bucket, RetentionPolicy = new Bucket.RetentionPolicyData { RetentionPeriod = 3600 } }, token);

    private static async Task RetentionPolicy(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        await Retain(bucket.Name, token);
        await Upload(bucket.Name, "r", Bytes("r"), token);
        await ExpectStatus(HttpStatusCode.Forbidden, () => Client.DeleteObjectAsync(bucket.Name, "r", null, token));
    }

    private static async Task RetentionLock(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        var retained = await Retain(bucket.Name, token);
        await Client.LockBucketRetentionPolicyAsync(bucket.Name, retained.Metageneration!.Value, null, token);
        Check((await Get(bucket.Name, token)).RetentionPolicy?.IsLocked == true, "policy not locked");
    }

    private static async Task Ubla(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        await Patch(new Bucket
        {
            Name = bucket.Name,
            IamConfiguration = new Bucket.IamConfigurationData
            {
                UniformBucketLevelAccess = new Bucket.IamConfigurationData.UniformBucketLevelAccessData { Enabled = true },
            },
        }, token);
        Check((await Get(bucket.Name, token)).IamConfiguration?.UniformBucketLevelAccess?.Enabled == true, "UBLA not enabled");
    }

    private static async Task Pap(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        await Patch(new Bucket { Name = bucket.Name, IamConfiguration = new Bucket.IamConfigurationData { PublicAccessPrevention = "enforced" } }, token);
        Check((await Get(bucket.Name, token)).IamConfiguration?.PublicAccessPrevention == "enforced", "PAP not enforced");
    }

    private static async Task Iam(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        var v3 = new GetBucketIamPolicyOptions { RequestedPolicyVersion = 3 };
        var policy = await Client.GetBucketIamPolicyAsync(bucket.Name, v3, token);
        policy.Bindings ??= [];
        policy.Bindings.Add(new Policy.BindingsData { Role = "roles/storage.objectViewer", Members = ["user:someone@example.invalid"] });
        await Client.SetBucketIamPolicyAsync(bucket.Name, policy, null, token);
        policy = await Client.GetBucketIamPolicyAsync(bucket.Name, v3, token);
        Check((policy.Bindings ?? []).Any(binding => binding.Role == "roles/storage.objectViewer" &&
            binding.Members?.Contains("user:someone@example.invalid") == true), "missing binding");
    }

    private static async Task Permissions(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        var granted = await Client.TestBucketIamPermissionsAsync(bucket.Name, ["storage.buckets.get", "storage.objects.list"], null, token);
        Check(granted.Count > 0, "no permissions");
    }

    private static Task<Bucket> SoftDelete(string bucket, long seconds, CancellationToken token) =>
        Patch(new Bucket { Name = bucket, SoftDeletePolicy = new Bucket.SoftDeletePolicyData { RetentionDurationSeconds = seconds } }, token);

    private static async Task SoftDeletePolicy(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        await SoftDelete(bucket.Name, 864000, token);
        Check((await Get(bucket.Name, token)).SoftDeletePolicy?.RetentionDurationSeconds == 864000, "soft delete policy differs");
    }

    private static async Task Autoclass(CancellationToken token)
    {
        var bucket = await NewBucket(new Bucket { Name = Unique(), Autoclass = new Bucket.AutoclassData { Enabled = true } }, token);
        Check((await Get(bucket.Name, token)).Autoclass?.Enabled == true, "autoclass not enabled");
    }

    private const string AuthenticatedUsers = "allAuthenticatedUsers";

    private static async Task BucketAcl(CancellationToken token)
    {
        var bucket = await Get((await NewBucket(token)).Name, token, Projection.Full);
        var acl = bucket.Acl ?? [];
        acl.Add(new BucketAccessControl { Entity = AuthenticatedUsers, Role = "READER" });
        await Client.PatchBucketAsync(new Bucket { Name = bucket.Name, Acl = acl }, new PatchBucketOptions { Projection = Projection.Full }, token);
        var stored = (await Get(bucket.Name, token, Projection.Full)).Acl ?? [];
        Check(stored.Any(entry => entry.Entity == AuthenticatedUsers && entry.Role == "READER"), "ACL missing");
    }

    private static async Task DefaultObjectAcl(CancellationToken token)
    {
        var bucket = await Get((await NewBucket(token)).Name, token, Projection.Full);
        var acl = bucket.DefaultObjectAcl ?? [];
        acl.Add(new ObjectAccessControl { Entity = AuthenticatedUsers, Role = "READER" });
        await Client.PatchBucketAsync(new Bucket { Name = bucket.Name, DefaultObjectAcl = acl }, new PatchBucketOptions { Projection = Projection.Full }, token);
        var stored = (await Get(bucket.Name, token, Projection.Full)).DefaultObjectAcl ?? [];
        Check(stored.Any(entry => entry.Entity == AuthenticatedUsers && entry.Role == "READER"), "ACL missing");
    }

    private static async Task Notifications(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        var created = await Client.CreateNotificationAsync(bucket.Name, new Notification
        {
            Topic = $"//pubsub.googleapis.com/projects/{Project}/topics/scorecard",
            PayloadFormat = "JSON_API_V1",
        }, null, token);
        var listed = await Client.ListNotificationsAsync(bucket.Name, null, token);
        Check(listed.Any(notification => notification.Id == created.Id), "notification missing");
        await Client.DeleteNotificationAsync(bucket.Name, created.Id, null, token);
    }

    private static async Task Hmac(CancellationToken token)
    {
        var key = await Client.CreateHmacKeyAsync(Project, "sa@example.invalid", null, token);
        Check(!string.IsNullOrEmpty(key.Secret) && !string.IsNullOrEmpty(key.Metadata?.AccessId), "empty HMAC key");
        key.Metadata!.State = "INACTIVE";
        await Client.UpdateHmacKeyAsync(key.Metadata, null, token);
        await Client.DeleteHmacKeyAsync(Project, key.Metadata.AccessId, null, token);
    }

    private static async Task ServiceAccountEmail(CancellationToken token) =>
        Check((await Client.GetStorageServiceAccountEmailAsync(Project, null, token)).Contains('@'), "service account missing");
}
