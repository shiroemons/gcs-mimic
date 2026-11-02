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
  private Main() {}

  public static void main(String[] args) throws Exception {
    String endpoint = System.getenv().getOrDefault("GCS_MIMIC_ENDPOINT", "http://localhost:4443");

    try (Storage storage = HttpStorageOptions.newBuilder()
        .setHost(endpoint)
        .setProjectId("local-project")
        .setCredentials(NoCredentials.getInstance())
        .build()
        .getService()) {
      String bucket = "example-java-" + UUID.randomUUID().toString().substring(0, 8);
      BlobId object = BlobId.of(bucket, "hello.txt");

      storage.create(BucketInfo.of(bucket));
      System.out.println("created bucket " + bucket);
      try {
        BlobInfo info = BlobInfo.newBuilder(object).setContentType("text/plain").build();
        storage.create(info, "Hello from gcs-mimic!".getBytes(StandardCharsets.UTF_8));
        System.out.println("uploaded " + object.getName());

        for (Blob blob : storage.list(bucket).iterateAll()) {
          System.out.println("listed " + blob.getName() + " (" + blob.getSize() + " bytes)");
        }

        String content = new String(storage.readAllBytes(object), StandardCharsets.UTF_8);
        System.out.println("downloaded: " + content);
      } finally {
        // Clean up even when a step above failed, so reruns start from an empty emulator.
        storage.delete(object);
        storage.delete(bucket);
        System.out.println("deleted " + object.getName() + " and bucket " + bucket);
      }
    }
  }
}
