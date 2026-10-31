using System.Net;
using System.Net.Http.Headers;
using Google;
using Google.Apis.Storage.v1.Data;
using Google.Apis.Upload;
using Google.Cloud.Storage.V1;
using Object = Google.Apis.Storage.v1.Data.Object;

/// <summary>.NET-specific "sdk.*" cases for features only this SDK exposes (signer templates, Service, CSEK rotation, restore).</summary>
internal static partial class Probe
{
    private static IEnumerable<Case> SdkFeatureCases() =>
    [
        new("sdk.signed_url_resumable_upload", SignedUrlResumableUpload),
        new("sdk.signed_request_template_headers", SignedTemplateHeaders),
        new("sdk.signed_query_generation", SignedQueryGeneration),
        new("sdk.service_rewrite_token_loop", ServiceRewriteTokenLoop),
        new("sdk.csek_rotation", CsekRotation),
        new("sdk.bucket_soft_delete_restore", BucketSoftDeleteRestore),
        new("sdk.upload_predefined_acl", UploadPredefinedAcl),
    ];

    private static Dictionary<string, IEnumerable<string>> Single(string name, string value) => new() { [name] = [value] };

    private static async Task SignedUrlResumableUpload(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        var data = RandomBytes(2 * Chunk + 3);
        var template = UrlSigner.RequestTemplate.FromBucket(bucket.Name).WithObjectName("signed-resumable.bin")
            .WithHttpMethod(UrlSigner.ResumableHttpMethod);
        var url = await Signer.SignAsync(template, SigningOptions(SigningVersion.V4), token);
        var log = new Transport.RequestLog();
        using var http = Factory.Client(log);
        var upload = Google.Cloud.Storage.V1.SignedUrlResumableUpload.Create(url, new MemoryStream(data, writable: false),
            new ResumableUploadOptions { HttpClient = http });
        upload.ChunkSize = Chunk;
        var result = await upload.UploadAsync(token);
        Check(result.Status == UploadStatus.Completed, $"upload ended {result.Status}: {result.Exception?.Message} [{log}]");
        var requests = log.Entries;
        Check(requests.Count >= 4 && requests[0].Method == HttpMethod.Post && requests[0].ResumableStart && requests[0].Uri == new Uri(url),
            "session was not started with the unchanged signed URL through the guarded client");
        Check(requests.Skip(1).All(entry => entry.Method == HttpMethod.Put), "chunks bypassed the guarded client");
        Check((await Read(bucket.Name, "signed-resumable.bin", token)).SequenceEqual(data), "bytes differ");
    }

    private static async Task SignedTemplateHeaders(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        var template = UrlSigner.RequestTemplate.FromBucket(bucket.Name).WithObjectName("headers.txt").WithHttpMethod(HttpMethod.Put)
            .WithContentHeaders(Single("Content-Type", "text/plain")).WithRequestHeaders(Single("x-goog-meta-k", "v"));
        var url = await Signer.SignAsync(template, SigningOptions(SigningVersion.V4), token);
        using var http = Factory.Client();
        using var request = new HttpRequestMessage(HttpMethod.Put, url) { Content = new ByteArrayContent(Bytes("with headers")) };
        request.Content.Headers.ContentType = new MediaTypeHeaderValue("text/plain");
        request.Headers.Add("x-goog-meta-k", "v");
        using var response = await http.SendAsync(request, token);
        var body = await response.Content.ReadAsByteArrayAsync(token);
        Check(response.StatusCode == HttpStatusCode.OK, $"status {(int)response.StatusCode}: {Preview(body)}");
        var stored = await Client.GetObjectAsync(bucket.Name, "headers.txt", null, token);
        Check(stored.ContentType == "text/plain" && SameMap(stored.Metadata, ("k", "v")), "signed headers not stored");
        Check((await Read(bucket.Name, "headers.txt", token)).SequenceEqual(Bytes("with headers")), "bytes differ");
    }

    private static async Task SignedQueryGeneration(CancellationToken token)
    {
        var bucket = await NewBucket(new Bucket { Name = Unique(), Versioning = new Bucket.VersioningData { Enabled = true } }, token);
        var first = await Upload(bucket.Name, "v.txt", Bytes("one"), token);
        await Upload(bucket.Name, "v.txt", Bytes("two"), token);
        var template = UrlSigner.RequestTemplate.FromBucket(bucket.Name).WithObjectName("v.txt").WithHttpMethod(HttpMethod.Get)
            .WithQueryParameters(Single("generation", first.Generation!.Value.ToString(System.Globalization.CultureInfo.InvariantCulture)));
        var url = await Signer.SignAsync(template, SigningOptions(SigningVersion.V4), token);
        using var http = Factory.Client();
        using var response = await http.GetAsync(url, token);
        var body = await response.Content.ReadAsByteArrayAsync(token);
        Check(response.StatusCode == HttpStatusCode.OK, $"status {(int)response.StatusCode}: {Preview(body)}");
        Check(body.SequenceEqual(Bytes("one")), $"got '{Preview(body)}' instead of the first generation");
    }

    private static async Task ServiceRewriteTokenLoop(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        var data = RandomBytes(3 * 1024 * 1024 + 9);
        await Upload(bucket.Name, "src", data, token);
        // CopyObject cannot bound bytes per call; the generated request exposes maxBytesRewrittenPerCall.
        // The limit applies only when location or storage class changes, so the destination changes class.
        var request = Client.Service.Objects.Rewrite(new Object { StorageClass = "NEARLINE" }, bucket.Name, "src", bucket.Name, "dst");
        request.MaxBytesRewrittenPerCall = 1024 * 1024;
        var calls = 0;
        RewriteResponse response;
        do
        {
            response = await request.ExecuteAsync(token);
            calls++;
            request.RewriteToken = response.RewriteToken;
        } while (response.Done != true && calls < 20);
        Check(response.Done == true && calls >= 2, $"done={response.Done} after {calls} calls");
        Check(response.TotalBytesRewritten == data.Length, $"totalBytesRewritten {response.TotalBytesRewritten}");
        Check((await Read(bucket.Name, "dst", token)).SequenceEqual(data), "rewrite bytes");
    }

    private static async Task CsekRotation(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        var oldKey = EncryptionKey.Generate();
        var newKey = EncryptionKey.Generate();
        await Upload(bucket.Name, "k.txt", Bytes("rotate"), token, new UploadObjectOptions { EncryptionKey = oldKey });
        await Client.CopyObjectAsync(bucket.Name, "k.txt", bucket.Name, "k.txt",
            new CopyObjectOptions { SourceEncryptionKey = oldKey, EncryptionKey = newKey }, token);
        Check((await Read(bucket.Name, "k.txt", token, new DownloadObjectOptions { EncryptionKey = newKey })).SequenceEqual(Bytes("rotate")), "new key bytes");
        foreach (var key in new[] { oldKey, EncryptionKey.None })
        {
            try
            {
                await Read(bucket.Name, "k.txt", token, new DownloadObjectOptions { EncryptionKey = key });
                throw new InvalidOperationException(key == oldKey ? "old key still reads" : "keyless read succeeded");
            }
            catch (GoogleApiException) { }
        }
        using var keyed = new StorageClientBuilder
        {
            BaseUri = new Uri(Origin, "storage/v1/").ToString(), UnauthenticatedAccess = true, HttpClientFactory = Factory, EncryptionKey = newKey,
        }.Build();
        using var output = new MemoryStream();
        await keyed.DownloadObjectAsync(bucket.Name, "k.txt", output, null, token);
        Check(output.ToArray().SequenceEqual(Bytes("rotate")), "client default key bytes");
    }

    private static async Task BucketSoftDeleteRestore(CancellationToken token)
    {
        var bucket = await NewBucket(new Bucket
        {
            Name = Unique(), SoftDeletePolicy = new Bucket.SoftDeletePolicyData { RetentionDurationSeconds = 604800 },
        }, token);
        var blob = await Upload(bucket.Name, "kept.txt", Bytes("kept"), token);
        var generation = (await Get(bucket.Name, token)).Generation;
        Check(generation is not null, "bucket generation missing");
        await Client.DeleteObjectAsync(bucket.Name, "kept.txt", null, token);
        await Client.DeleteBucketAsync(bucket.Name, null, token);
        await ExpectStatus(HttpStatusCode.NotFound, () => Get(bucket.Name, token));
        var deleted = await Client.GetBucketAsync(bucket.Name, new GetBucketOptions { SoftDeleted = true, Generation = generation }, token);
        Check(deleted.Name == bucket.Name, "soft-deleted bucket not found");
        await Client.RestoreBucketAsync(bucket.Name, generation!.Value, null, token);
        Check((await Get(bucket.Name, token)).Name == bucket.Name, "restored bucket missing");
        await Client.RestoreObjectAsync(bucket.Name, "kept.txt", blob.Generation!.Value, null, token);
        Check((await Read(bucket.Name, "kept.txt", token)).SequenceEqual(Bytes("kept")), "restored object bytes");
    }

    private static async Task UploadPredefinedAcl(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        await Upload(bucket.Name, "public.txt", Bytes("x"), token, new UploadObjectOptions { PredefinedAcl = PredefinedObjectAcl.PublicRead });
        var acl = (await Client.GetObjectAsync(bucket.Name, "public.txt", new GetObjectOptions { Projection = Projection.Full }, token)).Acl ?? [];
        Check(acl.Any(entry => entry.Entity == "allUsers" && entry.Role == "READER"), "predefined ACL missing");
    }
}
