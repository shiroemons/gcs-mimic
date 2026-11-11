package example;

import com.google.cloud.NoCredentials;
import com.google.cloud.storage.Blob;
import com.google.cloud.storage.BlobId;
import com.google.cloud.storage.BlobInfo;
import com.google.cloud.storage.BucketInfo;
import com.google.cloud.storage.HttpStorageOptions;
import com.google.cloud.storage.Storage;
import java.nio.charset.StandardCharsets;
import java.util.UUID;

public final class Main {
  public static void main(String[] args) throws Exception {
    if (System.getenv("GCSM_ENDPOINT") == null && System.getenv("GCS_MIMIC_ENDPOINT") != null) {
      throw new IllegalArgumentException("GCS_MIMIC_ENDPOINT was renamed to GCSM_ENDPOINT");
    }
    String endpoint = System.getenv().getOrDefault("GCSM_ENDPOINT", "http://localhost:4443");

    Storage storage = HttpStorageOptions.newBuilder()
        .setHost(endpoint) // the local gcs-mimic instead of storage.googleapis.com
        .setProjectId("local-project") // any project ID works with the emulator
        .setCredentials(NoCredentials.getInstance()) // anonymous: no Google credentials needed
        .build()
        .getService();

    String bucket = "example-java-" + UUID.randomUUID().toString().substring(0, 8);
    storage.create(BucketInfo.of(bucket));
    System.out.println("created bucket " + bucket);

    BlobId object = BlobId.of(bucket, "hello.txt");
    byte[] data = "Hello from gcs-mimic!".getBytes(StandardCharsets.UTF_8);
    storage.create(BlobInfo.newBuilder(object).build(), data);
    System.out.println("uploaded " + object.getName());

    for (Blob blob : storage.list(bucket).iterateAll()) {
      System.out.println("listed " + blob.getName() + " (" + blob.getSize() + " bytes)");
    }

    String content = new String(storage.readAllBytes(object), StandardCharsets.UTF_8);
    System.out.println("downloaded: " + content);

    storage.delete(object);
    System.out.println("deleted " + object.getName());
    storage.delete(bucket);
    System.out.println("deleted bucket " + bucket);
  }
}
