using System.Net;
using System.Net.Http.Headers;
using Google.Apis.Storage.v1.Data;
using Google.Cloud.Storage.V1;
using Object = Google.Apis.Storage.v1.Data.Object;

/// <summary>.NET-specific "sdk.*" cases for features only this SDK exposes (signer templates, Service, predefined ACLs).</summary>
internal static partial class Probe
{
    private static IEnumerable<Case> SdkFeatureCases() =>
    [
        new("sdk.signed_request_template_headers", SignedTemplateHeaders),
        new("sdk.signed_query_generation", SignedQueryGeneration),
        new("sdk.service_rewrite_token_loop", ServiceRewriteTokenLoop),
        new("sdk.upload_predefined_acl", UploadPredefinedAcl),
    ];

    private static Dictionary<string, IEnumerable<string>> Single(string name, string value) => new() { [name] = [value] };

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

    private static async Task UploadPredefinedAcl(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        await Upload(bucket.Name, "public.txt", Bytes("x"), token, new UploadObjectOptions { PredefinedAcl = PredefinedObjectAcl.PublicRead });
        var acl = (await Client.GetObjectAsync(bucket.Name, "public.txt", new GetObjectOptions { Projection = Projection.Full }, token)).Acl ?? [];
        Check(acl.Any(entry => entry.Entity == "allUsers" && entry.Role == "READER"), "predefined ACL missing");
    }
}
