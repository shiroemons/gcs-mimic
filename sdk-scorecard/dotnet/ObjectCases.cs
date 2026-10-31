using System.Net;
using System.Net.Http.Headers;
using System.Security.Cryptography;
using Google;
using Google.Apis.Storage.v1.Data;
using Google.Cloud.Storage.V1;
using Object = Google.Apis.Storage.v1.Data.Object;

internal static partial class Probe
{
    private static IEnumerable<Case> ObjectCases() =>
    [
        new("object.write_read_small", Small),
        new("object.resumable_large", Resumable),
        new("object.metadata_on_write", MetadataOnWrite),
        new("object.checksum_rejected", BadCrc),
        new("object.md5_accepted", Md5),
        new("object.range_reads", Ranges),
        new("object.update_metadata", UpdateMetadata),
        new("object.delete_not_found", Delete),
        new("object.does_not_exist_precondition", CreateCondition),
        new("object.generation_match_precondition", GenerationCondition),
        new("object.metageneration_match_precondition", MetagenerationCondition),
        new("object.list_prefix_delimiter", Delimiter),
        new("object.list_paging", ObjectPaging),
        new("object.list_offsets", Offsets),
        new("object.list_match_glob", Glob),
        new("object.copy_with_metadata", Copy),
        new("object.rewrite_storage_class", Rewrite),
        new("object.compose", Compose),
        new("object.move", Move),
        new("object.acl", ObjectAcl),
        new("object.holds", Holds),
        new("object.retention", ObjectRetention),
        new("object.custom_time", CustomTime),
        new("object.storage_class_on_write", StorageClass),
        // Never run: listed in Unsupported because the compressed half of the workflow has no public API.
        new("object.gzip_transcoding", _ => throw new NotSupportedException()),
        new("object.customer_supplied_key", Csek),
        new("object.soft_delete_restore", Restore),
    ];

    private static Task<Object> PatchObject(Object patch, CancellationToken token, PatchObjectOptions? options = null) =>
        Client.PatchObjectAsync(patch, options, token);

    private static async Task Small(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        var blob = await Upload(bucket.Name, "dir/日本語 +%?.txt", Bytes("hello"), token);
        Check((await Read(bucket.Name, blob.Name, token)).SequenceEqual(Bytes("hello")), "bytes differ");
    }

    private static async Task Resumable(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        var data = new byte[3 * 1024 * 1024 + 123];
        Array.Fill(data, (byte)'a');
        // The SDK always uploads through a resumable session; a 256 KiB chunk size forces several chunks.
        var blob = await Upload(bucket.Name, "big.bin", data, token, new UploadObjectOptions { ChunkSize = 256 * 1024 });
        Check((await Client.GetObjectAsync(bucket.Name, blob.Name, null, token)).Size == (ulong)data.Length, "size differs");
        Check((await Read(bucket.Name, blob.Name, token)).SequenceEqual(data), "bytes differ");
    }

    private static async Task MetadataOnWrite(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        await Upload(new Object
        {
            Bucket = bucket.Name, Name = "meta.txt", ContentType = "text/plain", CacheControl = "no-cache",
            ContentDisposition = "attachment", ContentLanguage = "ja", Metadata = new Dictionary<string, string> { ["k"] = "v" },
        }, Bytes("x"), token);
        var blob = await Client.GetObjectAsync(bucket.Name, "meta.txt", null, token);
        Check((blob.ContentType, blob.CacheControl, blob.ContentDisposition, blob.ContentLanguage) == ("text/plain", "no-cache", "attachment", "ja") &&
            SameMap(blob.Metadata, ("k", "v")), "metadata differs");
    }

    private static async Task BadCrc(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        // The public way to supply a checksum is the destination resource's crc32c field.
        var data = new byte[64 * 1024];
        Array.Fill(data, (byte)'a');
        try
        {
            await Upload(new Object { Bucket = bucket.Name, Name = "crc.txt", Crc32c = "AAAAAA==" }, data, token);
            throw new InvalidOperationException("wrong CRC32C accepted");
        }
        catch (GoogleApiException error) when (error.HttpStatusCode == HttpStatusCode.BadRequest)
        {
            var message = error.Message.ToLowerInvariant();
            Check(message.Contains("checksum") || message.Contains("crc32c"), error.Message);
        }
        Check(!await Exists(bucket.Name, "crc.txt", token), "rejected upload left an object");
    }

    private static async Task Md5(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        var data = Bytes("md5 payload");
        var digest = Convert.ToBase64String(MD5.HashData(data));
        await Upload(new Object { Bucket = bucket.Name, Name = "md5.txt", Md5Hash = digest }, data, token);
        Check((await Client.GetObjectAsync(bucket.Name, "md5.txt", null, token)).Md5Hash == digest, "MD5 differs");
    }

    private static async Task Ranges(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        await Upload(bucket.Name, "range.txt", Bytes("0123456789"), token);
        async Task Expect(long? from, long? to, string want, string label)
        {
            var got = await Read(bucket.Name, "range.txt", token, new DownloadObjectOptions { Range = new RangeHeaderValue(from, to) });
            Check(got.SequenceEqual(Bytes(want)), label);
        }
        await Expect(2, 5, "2345", "closed range");
        await Expect(7, null, "789", "open range");
        await Expect(null, 3, "789", "suffix range");
    }

    private static async Task UpdateMetadata(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        await Upload(new Object { Bucket = bucket.Name, Name = "u.txt", Metadata = new Dictionary<string, string> { ["a"] = "1", ["b"] = "2" } },
            Bytes("x"), token);
        // A null map value in a PATCH body removes that key.
        await PatchObject(new Object
        {
            Bucket = bucket.Name, Name = "u.txt", ContentType = "text/csv",
            Metadata = new Dictionary<string, string> { ["a"] = null!, ["c"] = "3" },
        }, token);
        var blob = await Client.GetObjectAsync(bucket.Name, "u.txt", null, token);
        var metadata = blob.Metadata ?? new Dictionary<string, string>();
        Check(blob.ContentType == "text/csv" && SameMap(metadata, ("b", "2"), ("c", "3")),
            string.Join(",", metadata.Select(pair => $"{pair.Key}={pair.Value}")));
    }

    private static async Task Delete(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        await Upload(bucket.Name, "d.txt", Bytes("x"), token);
        await Client.DeleteObjectAsync(bucket.Name, "d.txt", null, token);
        Check(!await Exists(bucket.Name, "d.txt", token), "deleted object visible");
        await ExpectStatus(HttpStatusCode.NotFound, () => Client.DeleteObjectAsync(bucket.Name, "d.txt", null, token));
    }

    private static async Task CreateCondition(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        var once = new UploadObjectOptions { IfGenerationMatch = 0 };
        await Upload(bucket.Name, "once.txt", Bytes("first"), token, once);
        await ExpectStatus(HttpStatusCode.PreconditionFailed, () => Upload(bucket.Name, "once.txt", Bytes("second"), token, once));
        Check((await Read(bucket.Name, "once.txt", token)).SequenceEqual(Bytes("first")), "precondition changed bytes");
    }

    private static async Task GenerationCondition(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        var blob = await Upload(bucket.Name, "g.txt", Bytes("x"), token);
        await ExpectStatus(HttpStatusCode.PreconditionFailed, () =>
            Client.DeleteObjectAsync(blob, new DeleteObjectOptions { IfGenerationMatch = blob.Generation + 1 }, token));
        await Client.DeleteObjectAsync(blob, new DeleteObjectOptions { IfGenerationMatch = blob.Generation }, token);
    }

    private static async Task MetagenerationCondition(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        var blob = await Upload(bucket.Name, "m.txt", Bytes("x"), token);
        await ExpectStatus(HttpStatusCode.PreconditionFailed, () => PatchObject(
            new Object { Bucket = bucket.Name, Name = "m.txt", ContentType = "text/plain" }, token,
            new PatchObjectOptions { IfMetagenerationMatch = blob.Metageneration + 5 }));
    }

    private static async Task Delimiter(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        await Seed(bucket.Name, token, "a/1", "a/2", "b/1", "c");
        var objects = new List<string>();
        var prefixes = new List<string>();
        var pages = Client.ListObjectsAsync(bucket.Name, null, new ListObjectsOptions { Delimiter = "/" }).AsRawResponses();
        await foreach (var page in pages.WithCancellation(token))
        {
            objects.AddRange((page.Items ?? []).Select(item => item.Name));
            prefixes.AddRange(page.Prefixes ?? []);
        }
        prefixes.Sort(StringComparer.Ordinal);
        Check(objects.SequenceEqual(["c"]), "names differ");
        Check(prefixes.SequenceEqual(["a/", "b/"]), "prefixes differ");
        Check((await Names(Client.ListObjectsAsync(bucket.Name, "a/"))).SequenceEqual(["a/1", "a/2"]), "prefix listing");
    }

    private static async Task ObjectPaging(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        string[] want = ["p1", "p2", "p3", "p4", "p5"];
        await Seed(bucket.Name, token, want);
        var found = new List<string>();
        var count = 0;
        var pages = Client.ListObjectsAsync(bucket.Name, null, new ListObjectsOptions { PageSize = 2 }).AsRawResponses();
        await foreach (var page in pages.WithCancellation(token))
        {
            var values = page.Items ?? [];
            Check(values.Count <= 2, "object page exceeds 2");
            found.AddRange(values.Select(item => item.Name));
            count++;
        }
        found.Sort(StringComparer.Ordinal);
        Check(count >= 3 && found.SequenceEqual(want), string.Join(",", found));
    }

    private static async Task Offsets(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        await Seed(bucket.Name, token, "a", "b", "c", "d");
        var listed = await Names(Client.ListObjectsAsync(bucket.Name, null, new ListObjectsOptions { StartOffset = "b", EndOffset = "d" }));
        Check(listed.SequenceEqual(["b", "c"]), "offset listing");
    }

    private static async Task Glob(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        await Seed(bucket.Name, token, "x/a.txt", "x/b.csv", "y/c.txt");
        var listed = await Names(Client.ListObjectsAsync(bucket.Name, null, new ListObjectsOptions { MatchGlob = "**/*.txt" }));
        Check(listed.SequenceEqual(["x/a.txt", "y/c.txt"]), "glob listing");
    }

    private static async Task Copy(CancellationToken token)
    {
        var source = await NewBucket(token);
        var destination = await NewBucket(token);
        await Upload(source.Name, "src", Bytes("copy me"), token);
        // CopyObject drives the rewrite loop and sends ExtraMetadata as the destination resource.
        await Client.CopyObjectAsync(source.Name, "src", destination.Name, "dst", new CopyObjectOptions
        {
            ExtraMetadata = new Object { ContentType = "text/plain", Metadata = new Dictionary<string, string> { ["copied"] = "yes" } },
        }, token);
        var copied = await Client.GetObjectAsync(destination.Name, "dst", null, token);
        Check(copied.ContentType == "text/plain" && SameMap(copied.Metadata, ("copied", "yes")), "copy metadata");
        Check((await Read(destination.Name, "dst", token)).SequenceEqual(Bytes("copy me")), "copy bytes");
    }

    private static async Task Rewrite(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        var data = new byte[2 * 1024 * 1024 + 7];
        Array.Fill(data, (byte)'a');
        await Upload(bucket.Name, "src", data, token);
        await Client.CopyObjectAsync(bucket.Name, "src", bucket.Name, "dst",
            new CopyObjectOptions { ExtraMetadata = new Object { StorageClass = "NEARLINE" } }, token);
        Check((await Client.GetObjectAsync(bucket.Name, "dst", null, token)).StorageClass == "NEARLINE", "storage class differs");
        Check((await Read(bucket.Name, "dst", token)).SequenceEqual(data), "rewrite bytes");
    }

    private static async Task Compose(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        string[] parts = ["p1", "p2", "p3"];
        await Seed(bucket.Name, token, parts);
        // StorageClient has no compose method; its documented escape hatch is the bundled generated StorageService.
        var request = new ComposeRequest
        {
            Destination = new Object(),
            SourceObjects = parts.Select(name => new ComposeRequest.SourceObjectsData { Name = name }).ToList(),
        };
        var composed = await Client.Service.Objects.Compose(request, bucket.Name, "all").ExecuteAsync(token);
        Check(composed.ComponentCount == 3, "component count");
        Check((await Read(bucket.Name, "all", token)).SequenceEqual(Bytes("p1p2p3")), "compose bytes");
    }

    private static async Task Move(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        await Upload(bucket.Name, "old", Bytes("moved"), token);
        await Client.MoveObjectAsync(bucket.Name, "old", "new", null, token);
        Check(!await Exists(bucket.Name, "old", token), "source remains");
        Check((await Read(bucket.Name, "new", token)).SequenceEqual(Bytes("moved")), "move bytes");
    }

    private static async Task ObjectAcl(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        await Upload(bucket.Name, "acl.txt", Bytes("x"), token);
        var full = new GetObjectOptions { Projection = Projection.Full };
        var acl = (await Client.GetObjectAsync(bucket.Name, "acl.txt", full, token)).Acl ?? [];
        acl.Add(new ObjectAccessControl { Entity = "allUsers", Role = "READER" });
        await PatchObject(new Object { Bucket = bucket.Name, Name = "acl.txt", Acl = acl }, token,
            new PatchObjectOptions { Projection = Projection.Full });
        var stored = (await Client.GetObjectAsync(bucket.Name, "acl.txt", full, token)).Acl ?? [];
        Check(stored.Any(entry => entry.Entity == "allUsers" && entry.Role == "READER"), "ACL missing");
    }

    private static async Task Holds(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        await Upload(bucket.Name, "hold.txt", Bytes("x"), token);
        await PatchObject(new Object { Bucket = bucket.Name, Name = "hold.txt", TemporaryHold = true }, token);
        await ExpectStatus(HttpStatusCode.Forbidden, () => Client.DeleteObjectAsync(bucket.Name, "hold.txt", null, token));
        await PatchObject(new Object { Bucket = bucket.Name, Name = "hold.txt", TemporaryHold = false }, token);
        await Client.DeleteObjectAsync(bucket.Name, "hold.txt", null, token);
    }

    private static async Task ObjectRetention(CancellationToken token)
    {
        var bucket = await NewBucket(token, new CreateBucketOptions { ObjectRetentionEnabled = true });
        await Upload(bucket.Name, "ret.txt", Bytes("x"), token);
        await PatchObject(new Object
        {
            Bucket = bucket.Name, Name = "ret.txt",
            Retention = new Object.RetentionData { Mode = "Unlocked", RetainUntilTimeDateTimeOffset = DateTimeOffset.UtcNow.AddHours(1) },
        }, token);
        Check((await Client.GetObjectAsync(bucket.Name, "ret.txt", null, token)).Retention?.Mode == "Unlocked", "retention not stored");
        await ExpectStatus(HttpStatusCode.Forbidden, () => Client.DeleteObjectAsync(bucket.Name, "ret.txt", null, token));
    }

    private static async Task CustomTime(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        await Upload(bucket.Name, "ct.txt", Bytes("x"), token);
        var want = new DateTimeOffset(2026, 1, 2, 3, 4, 5, TimeSpan.Zero);
        await PatchObject(new Object { Bucket = bucket.Name, Name = "ct.txt", CustomTimeDateTimeOffset = want }, token);
        Check((await Client.GetObjectAsync(bucket.Name, "ct.txt", null, token)).CustomTimeDateTimeOffset == want, "customTime differs");
    }

    private static async Task StorageClass(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        await Upload(new Object { Bucket = bucket.Name, Name = "cold", StorageClass = "COLDLINE" }, Bytes("x"), token);
        Check((await Client.GetObjectAsync(bucket.Name, "cold", null, token)).StorageClass == "COLDLINE", "storage class differs");
    }

    private static async Task Csek(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        var key = EncryptionKey.Create(RandomNumberGenerator.GetBytes(32));
        await Upload(bucket.Name, "csek.txt", Bytes("secret"), token, new UploadObjectOptions { EncryptionKey = key });
        try
        {
            await Read(bucket.Name, "csek.txt", token);
            throw new InvalidOperationException("keyless read succeeded");
        }
        catch (GoogleApiException) { }
        var keyed = await Read(bucket.Name, "csek.txt", token, new DownloadObjectOptions { EncryptionKey = key });
        Check(keyed.SequenceEqual(Bytes("secret")), "keyed bytes");
    }

    private static async Task Restore(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        await SoftDelete(bucket.Name, 604800, token);
        var blob = await Upload(bucket.Name, "sd.txt", Bytes("restore me"), token);
        await Client.DeleteObjectAsync(bucket.Name, "sd.txt", null, token);
        var deleted = await Names(Client.ListObjectsAsync(bucket.Name, null, new ListObjectsOptions { SoftDeletedOnly = true }));
        Check(deleted.SequenceEqual(["sd.txt"]), "soft-deleted listing");
        await Client.RestoreObjectAsync(bucket.Name, "sd.txt", blob.Generation!.Value, null, token);
        Check((await Read(bucket.Name, "sd.txt", token)).SequenceEqual(Bytes("restore me")), "restored bytes");
    }
}
