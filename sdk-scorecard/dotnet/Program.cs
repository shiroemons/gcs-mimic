using System.Net;
using System.Security.Cryptography;
using System.Text;
using System.Text.Encodings.Web;
using System.Text.Json;
using Google;
using Google.Apis.Storage.v1.Data;
using Google.Cloud.Storage.V1;
using Object = Google.Apis.Storage.v1.Data.Object;

/// <summary>Emulator-neutral official .NET SDK probes; stdout is JSONL evidence.</summary>
internal static partial class Probe
{
    private const string SdkName = "Google.Cloud.Storage.V1 5.0.0";
    private static readonly TimeSpan CaseTimeout = TimeSpan.FromSeconds(60);

    private static Uri Origin = null!;
    private static string Project = "";
    private static Transport.LocalFactory Factory = null!;
    private static StorageClient Client = null!;

    private sealed record Case(string Id, Func<CancellationToken, Task> Run);

    // Decided statically from the SDK surface, never from a run's outcome.
    private static readonly Dictionary<string, string> Unsupported = new()
    {
        ["object.gzip_transcoding"] = "downloading a gzip-encoded object without decompression " +
            "(downloads always decompress; StorageClientBuilder.GZipEnabled is internal)",
    };

    // Case failures are data; only a harness failure (configuration, transport check) exits nonzero.
    public static async Task<int> Main(string[] args)
    {
        try
        {
            return await Run(args);
        }
        catch (Exception error)
        {
            Console.Error.WriteLine($"FAIL: {error}");
            return 1;
        }
    }

    private static async Task<int> Run(string[] args)
    {
        Origin = Transport.LocalOrigin(Environment.GetEnvironmentVariable("SCORECARD_ENDPOINT")
            ?? throw new ArgumentException("set SCORECARD_ENDPOINT"));
        Project = Environment.GetEnvironmentVariable("SCORECARD_PROJECT") is { Length: > 0 } project ? project : "local-project";
        Factory = new Transport.LocalFactory(Origin, Transport.LoadCa(Origin, Environment.GetEnvironmentVariable("SCORECARD_CA_CERT")));
        if (args.SequenceEqual(["--transport-check"])) return await TransportCheck();
        if (args.Length != 0) throw new ArgumentException("usage: Probe [--transport-check]");
        Client = new StorageClientBuilder
        {
            BaseUri = new Uri(Origin, "storage/v1/").ToString(),
            UnauthenticatedAccess = true,
            HttpClientFactory = Factory,
        }.Build();
        Signer = CreateSigner();
        await RunCases();
        return 0;
    }

    private static async Task<int> TransportCheck()
    {
        using var http = Factory.Client();
        try
        {
            await http.GetAsync("https://storage.googleapis.com/");
            throw new InvalidOperationException("external origin accepted");
        }
        catch (HttpRequestException error) when (error.Message == Transport.Escaped) { }
        using var response = await http.GetAsync(new Uri(Origin, "/"));
        Check(response.StatusCode == HttpStatusCode.OK, $"status {(int)response.StatusCode}");
        Console.WriteLine("PASS: .NET transport CA/SAN and exact-origin guard");
        return 0;
    }

    // The shared cases first, in the cross-SDK order, then the .NET-specific "sdk.*" cases.
    private static IEnumerable<Case> Cases() => BucketCases().Concat(ObjectCases()).Concat(SignedCases()).Concat(SdkCases());

    private static async Task RunCases()
    {
        var filter = Environment.GetEnvironmentVariable("SCORECARD_RUN") ?? "";
        foreach (var (id, run) in Cases())
        {
            if (!id.Contains(filter, StringComparison.Ordinal)) continue;
            var area = id.StartsWith("project.", StringComparison.Ordinal) ? "bucket" : id.Split('.')[0];
            if (Unsupported.TryGetValue(id, out var missing))
            {
                Emit(id, area, false, $"{SdkName} has no public API for {missing}", unsupported: true);
                continue;
            }
            using var cancel = new CancellationTokenSource(CaseTimeout);
            string? detail = null;
            try
            {
                await run(cancel.Token).WaitAsync(CaseTimeout);
            }
            catch (TimeoutException)
            {
                detail = "TimeoutException: case exceeded 60 seconds";
            }
            catch (Exception error)
            {
                detail = $"{error.GetType().Name}: {error.Message}";
            }
            if (area == "sdk") await RemoveScratchBuckets(id);
            Scratch.Clear();
            Emit(id, area, detail is null, detail);
        }
    }

    private static readonly JsonWriterOptions JsonOptions = new() { Encoder = JavaScriptEncoder.UnsafeRelaxedJsonEscaping };

    private static void Emit(string id, string area, bool pass, string? detail, bool unsupported = false)
    {
        using var buffer = new MemoryStream();
        using (var json = new Utf8JsonWriter(buffer, JsonOptions))
        {
            json.WriteStartObject();
            json.WriteString("sdk", "dotnet");
            json.WriteString("id", id);
            json.WriteString("area", area);
            json.WriteBoolean("pass", pass);
            if (unsupported) json.WriteBoolean("unsupported", true);
            if (detail is not null) json.WriteString("detail", detail.Length > 300 ? detail[..300] : detail);
            json.WriteEndObject();
        }
        Console.Out.WriteLine(Encoding.UTF8.GetString(buffer.ToArray()));
        Console.Out.Flush();
    }

    private static void Check(bool condition, string message = "unexpected result")
    {
        if (!condition) throw new InvalidOperationException(message);
    }

    private static async Task ExpectStatus(HttpStatusCode status, Func<Task> call)
    {
        try
        {
            await call();
        }
        catch (GoogleApiException error) when (error.HttpStatusCode == status)
        {
            return;
        }
        throw new InvalidOperationException($"expected HTTP {(int)status}, got success");
    }

    private static string Unique() => "sc-dn-" + Convert.ToHexStringLower(RandomNumberGenerator.GetBytes(6));

    // Buckets created by the running case; the sdk.* cases delete them afterwards.
    private static readonly List<string> Scratch = [];

    private static Task<Bucket> NewBucket(CancellationToken token, CreateBucketOptions? options = null) =>
        NewBucket(new Bucket { Name = Unique() }, token, options);

    private static Task<Bucket> NewBucket(Bucket bucket, CancellationToken token, CreateBucketOptions? options = null)
    {
        Scratch.Add(bucket.Name);
        return Client.CreateBucketAsync(Project, bucket, options, token);
    }

    private static Task<Object> Upload(string bucket, string name, byte[] data, CancellationToken token,
        UploadObjectOptions? options = null) =>
        Client.UploadObjectAsync(bucket, name, null, new MemoryStream(data, writable: false), options, token);

    private static Task<Object> Upload(Object destination, byte[] data, CancellationToken token,
        UploadObjectOptions? options = null) =>
        Client.UploadObjectAsync(destination, new MemoryStream(data, writable: false), options, token);

    private static async Task<byte[]> Read(string bucket, string name, CancellationToken token, DownloadObjectOptions? options = null)
    {
        using var output = new MemoryStream();
        await Client.DownloadObjectAsync(bucket, name, output, options, token);
        return output.ToArray();
    }

    private static async Task<bool> Exists(string bucket, string name, CancellationToken token)
    {
        try
        {
            await Client.GetObjectAsync(bucket, name, null, token);
            return true;
        }
        catch (GoogleApiException error) when (error.HttpStatusCode == HttpStatusCode.NotFound)
        {
            return false;
        }
    }

    private static byte[] Bytes(string text) => Encoding.UTF8.GetBytes(text);

    private static bool SameMap(IDictionary<string, string>? actual, params (string Key, string Value)[] want) =>
        (actual?.Count ?? 0) == want.Length &&
        want.All(pair => actual!.TryGetValue(pair.Key, out var value) && value == pair.Value);

    private static async Task<List<string>> Names(IAsyncEnumerable<Object> objects)
    {
        var names = new List<string>();
        await foreach (var item in objects) names.Add(item.Name);
        names.Sort(StringComparer.Ordinal);
        return names;
    }

    private static async Task Seed(string bucket, CancellationToken token, params string[] names)
    {
        foreach (var name in names) await Upload(bucket, name, Bytes(name), token);
    }
}
