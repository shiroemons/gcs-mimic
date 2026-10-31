using System.Net.Http.Headers;
using Google.Apis.Auth.OAuth2;
using Google.Cloud.Storage.V1;

internal static partial class Probe
{
    private const string SignerEmail = "local-admin@example.invalid";
    private static UrlSigner Signer = null!;

    private static IEnumerable<Case> SignedCases() =>
    [
        new("signed.v4_get", token => SignedTransfer(SigningVersion.V4, HttpMethod.Get, token)),
        new("signed.v4_put", token => SignedTransfer(SigningVersion.V4, HttpMethod.Put, token)),
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
