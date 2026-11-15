using System.Net;
using System.Net.Http.Headers;
using Google.Apis.Auth.OAuth2;
using Google.Apis.Upload;
using Google.Cloud.Storage.V1;
using Object = Google.Apis.Storage.v1.Data.Object;

internal static partial class Probe
{
    private const string SignerEmail = "local-admin@example.invalid";
    private static UrlSigner Signer = null!;

    private static IEnumerable<Case> SignedCases() =>
    [
        new("signed.v4_get", token => SignedTransfer(SigningVersion.V4, HttpMethod.Get, token)),
        new("signed.v4_put", token => SignedTransfer(SigningVersion.V4, HttpMethod.Put, token)),
        new("signed.v4_resumable_upload", SignedResumableUpload),
        new("signed.v4_response_overrides", SignedResponseOverrides),
        new("signed.v2_get", token => SignedTransfer(SigningVersion.V2, HttpMethod.Get, token)),
        new("signed.post_policy_v4", PostPolicy),
    ];

    // Offline signing with the test-only key; no token endpoint is contacted.
    private static UrlSigner CreateSigner()
    {
        var path = Environment.GetEnvironmentVariable("SCORECARD_SIGNING_KEY") is { Length: > 0 } key ? key
            : throw new ArgumentException("set SCORECARD_SIGNING_KEY");
        var credential = new ServiceAccountCredential(new ServiceAccountCredential.Initializer(SignerEmail).FromPrivateKey(File.ReadAllText(path)));
        return UrlSigner.FromCredential(credential);
    }

    // The signature covers SCORECARD_ENDPOINT itself, so the URL is sent exactly as generated.
    private static UrlSigner.Options SigningOptions(SigningVersion version) =>
        UrlSigner.Options.FromDuration(TimeSpan.FromMinutes(5)).WithSigningVersion(version)
            .WithScheme(Origin.Scheme).WithHost(Origin.Host).WithPort(Origin.Port);

    private static async Task SignedTransfer(SigningVersion version, HttpMethod method, CancellationToken token)
    {
        var bucket = await NewBucket(token);
        if (method == HttpMethod.Get) await Upload(bucket.Name, "signed.txt", Bytes("signed"), token);
        var template = UrlSigner.RequestTemplate.FromBucket(bucket.Name).WithObjectName("signed.txt").WithHttpMethod(method);
        var url = await Signer.SignAsync(template, SigningOptions(version), token);
        using var http = Factory.Client();
        using var request = new HttpRequestMessage(method, url);
        if (method == HttpMethod.Put) request.Content = new ByteArrayContent(Bytes("uploaded"));
        using var response = await http.SendAsync(request, token);
        var body = await response.Content.ReadAsByteArrayAsync(token);
        Check((int)response.StatusCode == 200, $"status {(int)response.StatusCode}: {Preview(body)}");
        var expected = method == HttpMethod.Get ? body : await Read(bucket.Name, "signed.txt", token);
        Check(expected.SequenceEqual(Bytes(method == HttpMethod.Get ? "signed" : "uploaded")), "signed bytes");
    }

    private static async Task SignedResumableUpload(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        var data = RandomBytes(2 * Chunk + 3);
        // ResumableHttpMethod signs a POST with the x-goog-resumable: start header.
        var template = UrlSigner.RequestTemplate.FromBucket(bucket.Name).WithObjectName("signed-resumable.bin")
            .WithHttpMethod(UrlSigner.ResumableHttpMethod);
        var url = await Signer.SignAsync(template, SigningOptions(SigningVersion.V4), token);
        var log = new Transport.RequestLog();
        using var http = Factory.Client(log);
        var upload = SignedUrlResumableUpload.Create(url, new MemoryStream(data, writable: false),
            new ResumableUploadOptions { HttpClient = http });
        upload.ChunkSize = Chunk;
        Uri? session = null;
        upload.UploadSessionData += started => session = started.UploadUri;
        var result = await upload.UploadAsync(token);
        Check(result.Status == UploadStatus.Completed, $"upload ended {result.Status}: {result.Exception?.Message} [{log}]");
        var requests = log.Entries;
        Check(requests.Count >= 4 && requests[0].Method == HttpMethod.Post && requests[0].ResumableStart && requests[0].Uri == new Uri(url),
            "session was not started with the unchanged signed URL through the guarded client");
        Check(requests[0].Status == HttpStatusCode.Created, $"start status {(int?)requests[0].Status}");
        Check(session is not null && session.Scheme == Origin.Scheme && session.Authority == Origin.Authority,
            $"session URI {session?.ToString() ?? "not reported"} leaves the origin");
        Check(requests.Skip(1).All(entry => entry.Method == HttpMethod.Put && entry.Uri == session),
            "chunks bypassed the guarded client or the session URI");
        Check(requests[^1].Status is HttpStatusCode.OK or HttpStatusCode.Created, $"final chunk status {(int?)requests[^1].Status}");
        Check((await Read(bucket.Name, "signed-resumable.bin", token)).SequenceEqual(data), "bytes differ");
    }

    private const string ReportDisposition = "attachment; filename=\"report.csv\"";

    private static async Task SignedResponseOverrides(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        await Upload(new Object { Bucket = bucket.Name, Name = "report.txt", ContentType = "text/plain" }, Bytes("a,b\n"), token);
        var template = UrlSigner.RequestTemplate.FromBucket(bucket.Name).WithObjectName("report.txt").WithHttpMethod(HttpMethod.Get)
            .WithQueryParameters(new Dictionary<string, IEnumerable<string>>
            {
                ["response-content-disposition"] = [ReportDisposition],
                ["response-content-type"] = ["text/csv"],
            });
        var url = await Signer.SignAsync(template, SigningOptions(SigningVersion.V4), token);
        using var http = Factory.Client();
        using var response = await http.GetAsync(url, token);
        var body = await response.Content.ReadAsByteArrayAsync(token);
        Check((int)response.StatusCode == 200, $"status {(int)response.StatusCode}: {Preview(body)}");
        Check(body.SequenceEqual(Bytes("a,b\n")), "signed bytes");
        var headers = (RawHeader(response, "Content-Disposition"), RawHeader(response, "Content-Type"));
        Check(headers == (ReportDisposition, "text/csv"), $"override headers {headers}");
        var stored = await Client.GetObjectAsync(bucket.Name, "report.txt", null, token);
        Check(stored.ContentType == "text/plain", $"stored content type {stored.ContentType}");
    }

    // The single value of a content header as received; the typed accessors would re-serialize a parsed value.
    private static string? RawHeader(HttpResponseMessage response, string name) =>
        response.Content.Headers.NonValidated.TryGetValues(name, out var values) && values.Count == 1 ? values.ToString() : null;

    private static async Task PostPolicy(CancellationToken token)
    {
        var bucket = await NewBucket(token);
        var policy = UrlSigner.PostPolicy.ForBucketAndKey(bucket.Name, "post.txt");
        var signed = await Signer.SignAsync(policy, SigningOptions(SigningVersion.V4), token);
        using var form = new MultipartFormDataContent();
        foreach (var (name, value) in signed.Fields) form.Add(new StringContent(value.ToString() ?? ""), name);
        var file = new ByteArrayContent(Bytes("posted"));
        file.Headers.ContentType = new MediaTypeHeaderValue("application/octet-stream");
        // MultipartFormDataContent.Add(content, name, fileName) also emits an RFC 5987 filename*;
        // the SDK only supplies the fields, so the form uses the plain filename other probes send.
        file.Headers.ContentDisposition = new ContentDispositionHeaderValue("form-data") { Name = "\"file\"", FileName = "\"post.txt\"" };
        form.Add(file);
        using var http = Factory.Client();
        using var response = await http.PostAsync(signed.PostUrl, form, token);
        var body = await response.Content.ReadAsByteArrayAsync(token);
        Check((int)response.StatusCode is >= 200 and < 300, $"status {(int)response.StatusCode}: {Preview(body)}");
        Check((await Read(bucket.Name, "post.txt", token)).SequenceEqual(Bytes("posted")), "posted bytes");
    }

    private static string Preview(byte[] body) => System.Text.Encoding.UTF8.GetString(body.AsSpan(0, Math.Min(body.Length, 200)));
}
