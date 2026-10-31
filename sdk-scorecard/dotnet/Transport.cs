using System.Net.Security;
using System.Security.Cryptography.X509Certificates;
using Google.Apis.Http;

/// <summary>
/// Restricts every SDK and signed request to the selected loopback origin and trusts only the
/// scorecard CA, so a probe can never reach Google Cloud, the metadata server or an ambient proxy.
/// </summary>
internal static class Transport
{
    internal const string Escaped = "request escaped the emulator origin";

    internal static Uri LocalOrigin(string raw)
    {
        if (!Uri.TryCreate(raw.TrimEnd('/'), UriKind.Absolute, out var uri) || uri.Scheme is not ("http" or "https") ||
            uri.UserInfo.Length != 0 || uri.Query.Length != 0 || uri.Fragment.Length != 0 ||
            uri.AbsolutePath != "/" || uri.IsDefaultPort ||
            !(uri.Host is "127.0.0.1" or "localhost" or "[::1]"))
            throw new ArgumentException("SCORECARD_ENDPOINT must be a bare loopback origin with an explicit port");
        return new Uri(uri.GetLeftPart(UriPartial.Authority) + "/");
    }

    internal static void RequireOrigin(Uri origin, Uri? target)
    {
        if (target is null || target.Scheme != origin.Scheme || target.Authority != origin.Authority || target.UserInfo.Length != 0)
            throw new HttpRequestException(Escaped);
    }

    // Checked below Google.Apis' handler chain, so resumable session URLs and any redirect target are covered too.
    private sealed class OriginHandler(Uri origin, HttpMessageHandler inner, RequestLog? log) : DelegatingHandler(inner)
    {
        protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken token)
        {
            RequireOrigin(origin, request.RequestUri);
            HttpResponseMessage? response = null;
            try
            {
                response = await base.SendAsync(request, token);
            }
            finally
            {
                log?.Add(request, response?.StatusCode);
            }
            if (response.Headers.Location is { IsAbsoluteUri: true } location) RequireOrigin(origin, location);
            return response;
        }
    }

    /// <summary>Requests that passed the origin guard of one client, so a case can prove its transfer used that client.</summary>
    internal sealed class RequestLog
    {
        private readonly List<Entry> entries = [];

        internal sealed record Entry(HttpMethod Method, Uri Uri, string? ContentRange, long? ContentLength, bool ResumableStart,
            System.Net.HttpStatusCode? Status)
        {
            public override string ToString() =>
                $"{Method} {Uri.AbsolutePath}{(ResumableStart ? " start" : "")} {ContentRange} -> {(Status is { } status ? (int)status : 0)}";
        }

        internal void Add(HttpRequestMessage request, System.Net.HttpStatusCode? status)
        {
            var content = request.Content?.Headers;
            var entry = new Entry(request.Method, request.RequestUri!, content?.ContentRange?.ToString(), content?.ContentLength,
                request.Headers.TryGetValues("x-goog-resumable", out var values) && values.Contains("start"), status);
            lock (entries) entries.Add(entry);
        }

        public override string ToString() => string.Join("; ", Entries);

        internal IReadOnlyList<Entry> Entries
        {
            get { lock (entries) return entries.ToList(); }
        }
    }

    internal sealed class LocalFactory(Uri origin, X509Certificate2? ca) : HttpClientFactory
    {
        protected override HttpMessageHandler CreateHandler(CreateHttpClientArgs args) =>
            new OriginHandler(origin, base.CreateHandler(args), null);

        protected override HttpClientHandler CreateClientHandler()
        {
            var handler = new HttpClientHandler { UseProxy = false, AllowAutoRedirect = false };
            if (ca is not null) handler.ServerCertificateCustomValidationCallback = (_, certificate, _, errors) => Trusted(ca, certificate, errors);
            return handler;
        }

        // Signed requests must reach the server unchanged, so redirects are never followed by Google.Apis either.
        internal ConfigurableHttpClient Client(RequestLog? log = null)
        {
            var args = new CreateHttpClientArgs { GZipEnabled = false };
            var client = new ConfigurableHttpClient(new ConfigurableMessageHandler(new OriginHandler(origin, base.CreateHandler(args), log)));
            client.MessageHandler.FollowRedirect = false;
            client.Timeout = TimeSpan.FromSeconds(20);
            return client;
        }
    }

    // The OS trust store is ignored: the chain must end at the scorecard CA and the SAN must match the host.
    private static bool Trusted(X509Certificate2 ca, X509Certificate2? certificate, SslPolicyErrors errors)
    {
        if (certificate is null || errors.HasFlag(SslPolicyErrors.RemoteCertificateNotAvailable) ||
            errors.HasFlag(SslPolicyErrors.RemoteCertificateNameMismatch))
        {
            Console.Error.WriteLine($"certificate rejected: {errors}");
            return false;
        }
        using var chain = new X509Chain();
        chain.ChainPolicy.TrustMode = X509ChainTrustMode.CustomRootTrust;
        chain.ChainPolicy.CustomTrustStore.Add(ca);
        chain.ChainPolicy.RevocationMode = X509RevocationMode.NoCheck;
        if (chain.Build(certificate)) return true;
        Console.Error.WriteLine("certificate rejected: " + string.Join("; ", chain.ChainStatus.Select(status => status.StatusInformation.Trim())));
        return false;
    }

    internal static X509Certificate2? LoadCa(Uri origin, string? path)
    {
        if (!string.IsNullOrEmpty(path)) return X509Certificate2.CreateFromPem(File.ReadAllText(path));
        if (origin.Scheme == "https") throw new ArgumentException("SCORECARD_CA_CERT is required for an HTTPS endpoint");
        return null;
    }
}
