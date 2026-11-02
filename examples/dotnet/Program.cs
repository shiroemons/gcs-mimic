using System.Text;
using Google.Cloud.Storage.V1;

var endpoint = Environment.GetEnvironmentVariable("GCS_MIMIC_ENDPOINT") ?? "http://localhost:4443";

var storage = new StorageClientBuilder
{
    BaseUri = endpoint + "/storage/v1/", // the local gcs-mimic instead of storage.googleapis.com
    UnauthenticatedAccess = true, // anonymous: no Google credentials needed
}.Build();

var bucket = "example-dotnet-" + Guid.NewGuid().ToString("N")[..8];
await storage.CreateBucketAsync("local-project", bucket); // any project ID works with the emulator
Console.WriteLine($"created bucket {bucket}");

var objectName = "hello.txt";
await storage.UploadObjectAsync(bucket, objectName, "text/plain",
    new MemoryStream(Encoding.UTF8.GetBytes("Hello from gcs-mimic!")));
Console.WriteLine($"uploaded {objectName}");

await foreach (var obj in storage.ListObjectsAsync(bucket))
{
    Console.WriteLine($"listed {obj.Name}");
}

var download = new MemoryStream();
await storage.DownloadObjectAsync(bucket, objectName, download);
Console.WriteLine($"downloaded: {Encoding.UTF8.GetString(download.ToArray())}");

await storage.DeleteObjectAsync(bucket, objectName);
await storage.DeleteBucketAsync(bucket);
Console.WriteLine($"deleted {objectName} and bucket {bucket}");
