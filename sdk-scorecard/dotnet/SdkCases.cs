using System.IO.Compression;
using System.Net;
using System.Security.Cryptography;
using Google;
using Google.Apis.Download;
using Google.Apis.Storage.v1.Data;
using Google.Apis.Upload;
using Google.Cloud.Storage.V1;
using Object = Google.Apis.Storage.v1.Data.Object;

/// <summary>
/// .NET-specific "sdk.*" cases: default paths of Google.Cloud.Storage.V1 that the shared cases do not reach.
/// Every check in a case must hold for it to pass.
/// </summary>
internal static partial class Probe
{
    private const int Chunk = 256 * 1024;

    private static IEnumerable<Case> SdkCases() =>
    [
        new("sdk.download_hash_validation", DownloadHashValidation),
        new("sdk.download_chunked", DownloadChunked),
        new("sdk.update_object_implicit_preconditions", UpdateObjectPreconditions),
        new("sdk.update_bucket_etag_ignored", UpdateBucketEtagIgnored),
        new("sdk.iam_policy_etag", IamPolicyEtag),
        new("sdk.resumable_resume_from_uri", ResumeFromUri),
        new("sdk.object_uploader_session", ObjectUploaderSession),
        new("sdk.upload_progress", UploadProgress),
        new("sdk.download_gzip_default", DownloadGzipDefault),
        new("sdk.list_auto_paging", ListAutoPaging),
        new("sdk.conditional_replace_and_read", ConditionalReplaceAndRead),
        new("sdk.delete_bucket_with_objects", DeleteBucketWithObjects),
        new("sdk.list_fields_partial", ListFieldsPartial),
        .. SdkFeatureCases(),
    ];

    private static byte[] RandomBytes(int length) => RandomNumberGenerator.GetBytes(length);

    /// <summary>Records progress synchronously; <see cref="Progress{T}"/> would post reports asynchronously.</summary>
    private sealed class Recorder<T> : IProgress<T>
    {
        private readonly List<T> items = [];

        public void Report(T value)
        {
            lock (items) items.Add(value);
        }

        internal IReadOnlyList<T> Items
        {
            get { lock (items) return items.ToList(); }
        }
    }

    private static async Task DownloadHashValidation(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        var data = RandomBytes(300 * 1024 + 11);
        await Upload(bucket.Name, "hash.bin", data, token);
        using var output = new MemoryStream();
        // The returned Object records the response's x-goog-hash, which the SDK has just compared with the bytes.
        var recorded = await Client.DownloadObjectAsync(bucket.Name, "hash.bin", output, null, token);
        Check(output.ToArray().SequenceEqual(data), "bytes differ");
        Check(recorded.Crc32c == Crc32c.Base64(data), $"recorded crc32c {recorded.Crc32c ?? "missing"}, want {Crc32c.Base64(data)}");
    }

    private static async Task DownloadChunked(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        var data = RandomBytes(1024 * 1024 + 5);
        await Upload(bucket.Name, "chunked.bin", data, token);
        var progress = new Recorder<IDownloadProgress>();
        using var output = new MemoryStream();
        await Client.DownloadObjectAsync(bucket.Name, "chunked.bin", output, new DownloadObjectOptions { ChunkSize = Chunk }, token, progress);
        Check(output.ToArray().SequenceEqual(data), "bytes differ");
        var reports = progress.Items;
        Check(reports.Count >= 2, $"{reports.Count} progress reports");
        Check(reports[^1].Status == DownloadStatus.Completed && reports[^1].BytesDownloaded == data.Length,
            $"last report {reports[^1].Status} at {reports[^1].BytesDownloaded}");
    }

    private static async Task UpdateObjectPreconditions(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        await Upload(bucket.Name, "u.txt", Bytes("x"), token);
        var stale = await Client.GetObjectAsync(bucket.Name, "u.txt", null, token);
        var fresh = await Client.GetObjectAsync(bucket.Name, "u.txt", null, token);
        // UpdateObject sends the fetched generation/metageneration as preconditions and its ETag as If-Match.
        fresh.Metadata = new Dictionary<string, string> { ["k"] = "first" };
        await Client.UpdateObjectAsync(fresh, null, token);
        Check(SameMap((await Client.GetObjectAsync(bucket.Name, "u.txt", null, token)).Metadata, ("k", "first")), "first update missing");
        stale.Metadata = new Dictionary<string, string> { ["k"] = "stale" };
        await ExpectStatus(HttpStatusCode.PreconditionFailed, () => Client.UpdateObjectAsync(stale, null, token));
        Check(SameMap((await Client.GetObjectAsync(bucket.Name, "u.txt", null, token)).Metadata, ("k", "first")), "stale update changed metadata");
    }

    /// <remarks>
    /// Unlike UpdateObjectAsync, UpdateBucketAsync in 5.0.0 sends no ifMetagenerationMatch; it sends only the
    /// fetched ETag as If-Match. Real Cloud Storage ignores If-Match on bucket PUT and PATCH (observed
    /// 2026-10-31): a stale ETag still gets 200 and the change is applied. The expected behaviour is therefore
    /// that the stale update succeeds, not 412 as for objects.
    /// </remarks>
    private static async Task UpdateBucketEtagIgnored(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        var stale = await Get(bucket.Name, token);
        var fresh = await Get(bucket.Name, token);
        fresh.Labels = new Dictionary<string, string> { ["k"] = "first" };
        await Client.UpdateBucketAsync(fresh, null, token);
        Check(SameMap((await Get(bucket.Name, token)).Labels, ("k", "first")), "first update missing");
        stale.Labels = new Dictionary<string, string> { ["k"] = "stale" };
        await Client.UpdateBucketAsync(stale, null, token);
        Check(SameMap((await Get(bucket.Name, token)).Labels, ("k", "stale")), "stale update was not applied");
    }

    private static async Task IamPolicyEtag(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        var v3 = new GetBucketIamPolicyOptions { RequestedPolicyVersion = 3 };
        var stale = await Client.GetBucketIamPolicyAsync(bucket.Name, v3, token);
        var fresh = await Client.GetBucketIamPolicyAsync(bucket.Name, v3, token);
        static Policy.BindingsData Viewer(string member) => new() { Role = "roles/storage.objectViewer", Members = [member] };
        (fresh.Bindings ??= []).Add(Viewer("user:first@example.invalid"));
        await Client.SetBucketIamPolicyAsync(bucket.Name, fresh, null, token);
        (stale.Bindings ??= []).Add(Viewer("user:stale@example.invalid"));
        await ExpectStatus(HttpStatusCode.PreconditionFailed, () => Client.SetBucketIamPolicyAsync(bucket.Name, stale, null, token));
        var members = ((await Client.GetBucketIamPolicyAsync(bucket.Name, v3, token)).Bindings ?? [])
            .Where(binding => binding.Role == "roles/storage.objectViewer").SelectMany(binding => binding.Members ?? []).ToList();
        Check(members.Contains("user:first@example.invalid") && !members.Contains("user:stale@example.invalid"), string.Join(",", members));
    }

    private static async Task ResumeFromUri(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        var data = RandomBytes(4 * Chunk);
        var uri = await Client.InitiateUploadSessionAsync(bucket.Name, "resume.bin", "application/octet-stream", data.Length, null, token);
        Transport.RequireOrigin(Origin, uri);

        // The first attempt dies after two chunks, as if the process had crashed mid-upload.
        var firstLog = new Transport.RequestLog();
        using (var firstClient = Factory.Client(firstLog))
        {
            var first = ResumableUpload.CreateFromUploadUri(uri, new FailingStream(data, 2 * Chunk), new ResumableUploadOptions { HttpClient = firstClient });
            first.ChunkSize = Chunk;
            var failed = await first.UploadAsync(token);
            Check(failed.Status == UploadStatus.Failed, $"interrupted upload ended {failed.Status}");
        }
        Check(firstLog.Entries.Count(entry => entry.Method == HttpMethod.Put) >= 2, "first attempt sent no chunks through the guarded client");

        var secondLog = new Transport.RequestLog();
        using var secondClient = Factory.Client(secondLog);
        var second = ResumableUpload.CreateFromUploadUri(uri, new MemoryStream(data, writable: false), new ResumableUploadOptions { HttpClient = secondClient });
        second.ChunkSize = Chunk;
        var resumed = await second.ResumeAsync(uri, token);
        Check(resumed.Status == UploadStatus.Completed, $"resumed upload ended {resumed.Status}: {resumed.Exception?.Message}");
        var requests = secondLog.Entries;
        Check(requests.Count > 0 && requests[0].ContentRange == $"bytes */{data.Length}", $"first resumed request {requests.FirstOrDefault()?.ContentRange}");
        Check(requests.All(entry => entry.Uri.AbsolutePath == uri.AbsolutePath), "resumed request left the session URI");
        var resent = requests.Sum(entry => entry.ContentLength ?? 0);
        Check(resent > 0 && resent < data.Length, $"resume resent {resent} of {data.Length} bytes");
        Check((await Read(bucket.Name, "resume.bin", token)).SequenceEqual(data), "bytes differ");
    }

    /// <summary>A seekable source that fails once reading reaches a limit, interrupting an upload mid-session.</summary>
    private sealed class FailingStream(byte[] data, int limit) : Stream
    {
        private readonly MemoryStream inner = new(data, writable: false);

        public override bool CanRead => true;
        public override bool CanSeek => true;
        public override bool CanWrite => false;
        public override long Length => inner.Length;
        public override long Position { get => inner.Position; set => inner.Position = value; }
        public override void Flush() { }
        public override long Seek(long offset, SeekOrigin origin) => inner.Seek(offset, origin);
        public override void SetLength(long value) => throw new NotSupportedException();
        public override void Write(byte[] buffer, int offset, int count) => throw new NotSupportedException();

        public override int Read(byte[] buffer, int offset, int count)
        {
            if (inner.Position >= limit) throw new IOException("simulated interruption");
            return inner.Read(buffer, offset, (int)Math.Min(count, limit - inner.Position));
        }
    }

    private static async Task ObjectUploaderSession(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        var data = RandomBytes(2 * Chunk + 17);
        var uploader = Client.CreateObjectUploader(bucket.Name, "uploader.bin", "application/octet-stream",
            new MemoryStream(data, writable: false), new UploadObjectOptions { ChunkSize = Chunk });
        Uri? session = null;
        var reports = new Recorder<IUploadProgress>();
        uploader.UploadSessionData += sessionData => session = sessionData.UploadUri;
        uploader.ProgressChanged += reports.Report;
        var result = await uploader.UploadAsync(token);
        Check(result.Status == UploadStatus.Completed, $"upload ended {result.Status}: {result.Exception?.Message}");
        Check(session is not null, "no UploadSessionData event");
        Transport.RequireOrigin(Origin, session);
        Check(uploader.ResponseBody?.Size == (ulong)data.Length, $"response size {uploader.ResponseBody?.Size}");
        Check(reports.Items.Count > 0 && reports.Items[^1].Status == UploadStatus.Completed, "no completed progress report");
        Check((await Read(bucket.Name, "uploader.bin", token)).SequenceEqual(data), "bytes differ");
    }

    private static async Task UploadProgress(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        var data = RandomBytes(3 * Chunk + 1);
        var progress = new Recorder<IUploadProgress>();
        await Client.UploadObjectAsync(bucket.Name, "progress.bin", null, new MemoryStream(data, writable: false),
            new UploadObjectOptions { ChunkSize = Chunk }, token, progress);
        var reports = progress.Items;
        var sent = reports.Where(report => report.Status == UploadStatus.Uploading).Select(report => report.BytesSent).ToList();
        Check(sent.Zip(sent.Skip(1)).All(pair => pair.First <= pair.Second), "BytesSent decreased");
        Check(sent.Count >= 3, $"{sent.Count} uploading reports for 4 chunks");
        Check(reports[^1].Status == UploadStatus.Completed && reports[^1].BytesSent == data.Length,
            $"last report {reports[^1].Status} at {reports[^1].BytesSent}");
        Check((await Read(bucket.Name, "progress.bin", token)).SequenceEqual(data), "bytes differ");
    }

    private static async Task DownloadGzipDefault(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        var plain = Bytes(string.Concat(Enumerable.Repeat("compress me ", 500)));
        using var zipped = new MemoryStream();
        using (var gzip = new GZipStream(zipped, CompressionLevel.Optimal, leaveOpen: true)) gzip.Write(plain);
        await Upload(new Object { Bucket = bucket.Name, Name = "gz.txt", ContentType = "text/plain", ContentEncoding = "gzip" },
            zipped.ToArray(), token);
        // The default download validates the stored (compressed) CRC32C, then decompresses.
        Check((await Read(bucket.Name, "gz.txt", token)).SequenceEqual(plain), "decompressed bytes differ");
    }

    private static async Task ListAutoPaging(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        string[] want = ["p1", "p2", "p3", "p4", "p5"];
        await Seed(bucket.Name, token, want);
        var paged = new ListObjectsOptions { PageSize = 2 };
        Check((await Names(Client.ListObjectsAsync(bucket.Name, null, paged))).SequenceEqual(want), "auto paging names");
        var found = new List<string>();
        string? pageToken = null;
        var pages = 0;
        do
        {
            var page = await Client.ListObjectsAsync(bucket.Name, null, new ListObjectsOptions { PageSize = 2, PageToken = pageToken })
                .ReadPageAsync(2, token);
            Check(page.Count() <= 2, "page exceeds 2");
            found.AddRange(page.Select(item => item.Name));
            pageToken = string.IsNullOrEmpty(page.NextPageToken) ? null : page.NextPageToken;
            pages++;
        } while (pageToken is not null && pages < 10);
        found.Sort(StringComparer.Ordinal);
        Check(pages >= 3 && found.SequenceEqual(want), $"{pages} pages: {string.Join(",", found)}");
    }

    private static async Task ConditionalReplaceAndRead(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        var first = await Upload(bucket.Name, "c.txt", Bytes("one"), token);
        var second = await Upload(bucket.Name, "c.txt", Bytes("two"), token, new UploadObjectOptions { IfGenerationMatch = first.Generation });
        await ExpectStatus(HttpStatusCode.PreconditionFailed, () =>
            Upload(bucket.Name, "c.txt", Bytes("three"), token, new UploadObjectOptions { IfGenerationMatch = first.Generation }));
        Check((await Read(bucket.Name, "c.txt", token)).SequenceEqual(Bytes("two")), "stale replace changed bytes");
        foreach (var options in new[]
                 {
                     new DownloadObjectOptions { IfGenerationMatch = first.Generation },
                     new DownloadObjectOptions { IfMetagenerationMatch = second.Metageneration + 1 },
                 })
        {
            using var output = new MemoryStream();
            await ExpectStatus(HttpStatusCode.PreconditionFailed, () => Client.DownloadObjectAsync(bucket.Name, "c.txt", output, options, token));
            Check(output.Length == 0, "failed conditional read wrote bytes");
        }
    }

    private static async Task DeleteBucketWithObjects(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        await Seed(bucket.Name, token, "a", "b/c", "d");
        Check((await Names(Client.ListObjectsAsync(bucket.Name))).Count == 3, "seeded objects missing");
        await Client.DeleteBucketAsync(bucket.Name, new DeleteBucketOptions { DeleteObjects = true }, token);
        await ExpectStatus(HttpStatusCode.NotFound, () => Get(bucket.Name, token));
        await ExpectStatus(HttpStatusCode.NotFound, () => Names(Client.ListObjectsAsync(bucket.Name)));
    }

    private static async Task ListFieldsPartial(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        string[] want = ["f1", "f2", "f3", "f4", "f5"];
        foreach (var name in want) await Upload(new Object { Bucket = bucket.Name, Name = name, ContentType = "text/plain" }, Bytes(name), token);
        var objects = new List<Object>();
        await foreach (var item in Client.ListObjectsAsync(bucket.Name, null,
                           new ListObjectsOptions { PageSize = 2, Fields = "items(name),nextPageToken" }).WithCancellation(token))
            objects.Add(item);
        Check(objects.Select(item => item.Name).Order(StringComparer.Ordinal).SequenceEqual(want), "partial listing names");
        Check(objects.All(item => item.ContentType is null && item.Size is null && item.Generation is null), "fields not applied");
    }

    /// <summary>CRC32C (Castagnoli), as GCS reports it: base64 of the big-endian checksum.</summary>
    private static class Crc32c
    {
        private static readonly uint[] Table = Enumerable.Range(0, 256).Select(index =>
        {
            var crc = (uint)index;
            for (var bit = 0; bit < 8; bit++) crc = (crc & 1) != 0 ? (crc >> 1) ^ 0x82F63B78u : crc >> 1;
            return crc;
        }).ToArray();

        internal static string Base64(byte[] data)
        {
            var crc = ~0u;
            foreach (var value in data) crc = Table[(crc ^ value) & 0xFF] ^ (crc >> 8);
            crc = ~crc;
            return Convert.ToBase64String([(byte)(crc >> 24), (byte)(crc >> 16), (byte)(crc >> 8), (byte)crc]);
        }
    }

    // Best effort: a cleanup failure is reported on stderr but never changes the case result.
    private static async Task RemoveScratchBuckets(string id)
    {
        using var cancel = new CancellationTokenSource(TimeSpan.FromSeconds(30));
        foreach (var bucket in Scratch)
        {
            try
            {
                await foreach (var item in Client.ListObjectsAsync(bucket, null, new ListObjectsOptions { Versions = true }).WithCancellation(cancel.Token))
                    await Client.DeleteObjectAsync(bucket, item.Name, new DeleteObjectOptions { Generation = item.Generation }, cancel.Token);
                await Client.DeleteBucketAsync(bucket, null, cancel.Token);
            }
            catch (GoogleApiException error) when (error.HttpStatusCode == HttpStatusCode.NotFound) { }
            catch (Exception error)
            {
                Console.Error.WriteLine($"cleanup {id} {bucket}: {error.GetType().Name}: {error.Message}");
            }
        }
    }
}
