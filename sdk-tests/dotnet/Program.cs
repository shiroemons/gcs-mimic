using System.Net;
using System.Net.Http.Json;
using DotNet.Testcontainers.Builders;
using Google.Apis.Http;
using Google.Cloud.Storage.V1;

internal static class Program
{
    internal static Uri LocalOrigin(string raw)
    {
        if (!Uri.TryCreate(raw, UriKind.Absolute, out var uri) || uri.Scheme != "http" ||
            uri.UserInfo.Length != 0 || uri.Query.Length != 0 || uri.Fragment.Length != 0 ||
            uri.AbsolutePath != "/" || uri.IsDefaultPort ||
            !(uri.Host is "127.0.0.1" or "localhost" or "[::1]"))
            throw new ArgumentException("endpoint must be a bare loopback HTTP origin with an explicit port");
        return new Uri(uri.GetLeftPart(UriPartial.Authority) + "/");
    }

    internal static void RequireOrigin(Uri origin, Uri? target)
    {
        if (target is null || target.Scheme != origin.Scheme || target.Authority != origin.Authority || target.UserInfo.Length != 0)
            throw new HttpRequestException("request escaped the emulator origin");
    }

    internal sealed class OriginHandler(Uri origin, HttpMessageHandler inner) : DelegatingHandler(inner)
    {
        internal int Chunks { get; private set; }
        protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken token)
        {
            RequireOrigin(origin, request.RequestUri);
            if (request.Content?.Headers.ContentRange is not null) Chunks++;
            var response = await base.SendAsync(request, token);
            if (response.Headers.Location is { IsAbsoluteUri: true } location) RequireOrigin(origin, location);
            return response;
        }
    }

    internal sealed class LocalFactory(Uri origin) : HttpClientFactory
    {
        internal OriginHandler? Guard { get; private set; }
        protected override HttpMessageHandler CreateHandler(CreateHttpClientArgs args)
        {
            Guard = new OriginHandler(origin, base.CreateHandler(args));
            return Guard;
        }
        protected override HttpClientHandler CreateClientHandler() => new() { UseProxy = false, AllowAutoRedirect = false };
    }

    private static void Check(bool condition, string message)
    {
        if (!condition) throw new InvalidOperationException(message);
    }

    private static async Task Scenario(Uri origin)
    {
        var factory = new LocalFactory(origin);
        var storage = new StorageClientBuilder { BaseUri = new Uri(origin, "storage/v1/").ToString(), UnauthenticatedAccess = true,
            HttpClientFactory = factory }.Build();
        var bucket = "sdk-dotnet-" + Guid.NewGuid().ToString("N");
        await storage.CreateBucketAsync("local-project", bucket);
        try
        {
            var payload = new byte[576 * 1024];
            for (var index = 0; index < payload.Length; index++) payload[index] = (byte)index;
            var uploaded = await storage.UploadObjectAsync(bucket, "stream/日本語 +%?.bin", "application/octet-stream",
                new MemoryStream(payload, writable: false), new UploadObjectOptions { ChunkSize = 256 * 1024, IfGenerationMatch = 0 });
            Check(uploaded.Generation > 0, "generation missing");
            Check(factory.Guard?.Chunks >= 3, "SDK did not send three resumable chunks");
            using var downloaded = new MemoryStream();
            await storage.DownloadObjectAsync(bucket, uploaded.Name, downloaded);
            Check(payload.AsSpan().SequenceEqual(downloaded.ToArray()), "resumable bytes differ");
            uploaded.Metadata = new Dictionary<string, string> { ["fixture"] = "updated" };
            await storage.UpdateObjectAsync(uploaded);
            Check((await storage.GetObjectAsync(bucket, uploaded.Name)).Metadata["fixture"] == "updated", "metadata update missing");
            var count = 0;
            await foreach (var item in storage.ListObjectsAsync(bucket, "stream/")) count++;
            Check(count == 1, "list fixture differs");
            try
            {
                await storage.UploadObjectAsync(bucket, uploaded.Name, "text/plain", new MemoryStream([1]),
                    new UploadObjectOptions { IfGenerationMatch = 0 });
                throw new InvalidOperationException("create-if-absent overwrite succeeded");
            }
            catch (Google.GoogleApiException error) when (error.HttpStatusCode == HttpStatusCode.PreconditionFailed) { }
            Console.WriteLine("dotnet-http: resumable, metadata, generation, conditions, read, list PASS");
        }
        finally
        {
            await foreach (var item in storage.ListObjectsAsync(bucket)) await storage.DeleteObjectAsync(item);
            await storage.DeleteBucketAsync(bucket);
        }
    }

    private static void SelfCheck()
    {
        var origin = LocalOrigin("http://127.0.0.1:4443");
        foreach (var bad in new[] { "https://storage.googleapis.com", "http://metadata.google.internal:80", "http://127.0.0.1:4443/path",
                     "http://user@127.0.0.1:4443", "http://127.0.0.1:4443?x" })
        {
            var rejected = false;
            try { LocalOrigin(bad); } catch (ArgumentException) { rejected = true; }
            Check(rejected, "invalid origin accepted");
        }
        try { RequireOrigin(origin, new Uri("http://127.0.0.1:4444/upload")); }
        catch (HttpRequestException) { Console.WriteLine("dotnet-origin-guard: PASS"); return; }
        throw new InvalidOperationException("cross-origin upload accepted");
    }

    public static async Task Main(string[] args)
    {
        if (args.SequenceEqual(["--self-check"])) { SelfCheck(); return; }
        if (args.SequenceEqual(["--container"]))
        {
            var image = Environment.GetEnvironmentVariable("GCS_MIMIC_IMAGE") ?? throw new ArgumentException("set GCS_MIMIC_IMAGE to the exact image under test");
            await using var container = new ContainerBuilder(image).WithPortBinding(4443, true)
                .WithEnvironment("GCS_MIMIC_SCHEME", "http").WithEnvironment("GCS_MIMIC_ADMIN_ENABLED", "true")
                .WithWaitStrategy(Wait.ForUnixContainer().UntilHttpRequestIsSucceeded(request => request.ForPort(4443).ForPath("/_internal/healthcheck")))
                .Build();
            await container.StartAsync();
            var origin = LocalOrigin($"http://{container.Hostname}:{container.GetMappedPublicPort(4443)}");
            using var client = new HttpClient(new OriginHandler(origin, new HttpClientHandler { UseProxy = false, AllowAutoRedirect = false }));
            using var advertised = await client.PutAsJsonAsync(new Uri(origin, "_internal/config"),
                new { externalUrl = origin.GetLeftPart(UriPartial.Authority), publicHost = origin.Authority });
            advertised.EnsureSuccessStatusCode();
            await Scenario(origin);
            return;
        }
        await Scenario(LocalOrigin(Environment.GetEnvironmentVariable("GCS_MIMIC_ENDPOINT") ?? throw new ArgumentException("set GCS_MIMIC_ENDPOINT")));
    }
}
