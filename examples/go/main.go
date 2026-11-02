// Command example uses the official Go client against a local gcs-mimic.
package main

import (
	"context"
	"crypto/rand"
	"fmt"
	"io"
	"log"
	"os"
	"strings"
	"time"

	"cloud.google.com/go/storage"
	"google.golang.org/api/iterator"
)

func main() {
	// The client retries without limit, so fail instead of waiting forever
	// when the emulator is not running.
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	endpoint := os.Getenv("GCS_MIMIC_ENDPOINT")
	if endpoint == "" {
		endpoint = "http://localhost:4443"
	}

	// With STORAGE_EMULATOR_HOST set, the client sends every request to the
	// emulator without authentication.
	os.Setenv("STORAGE_EMULATOR_HOST", endpoint)
	client, err := storage.NewClient(ctx)
	if err != nil {
		log.Fatal(err)
	}
	defer client.Close()

	// Bucket names are lowercase; rand.Text returns uppercase letters.
	bucketName := "example-go-" + strings.ToLower(rand.Text()[:10])
	bucket := client.Bucket(bucketName)
	if err := bucket.Create(ctx, "local-project", nil); err != nil {
		log.Fatal(err)
	}
	fmt.Println("created bucket", bucketName)

	object := bucket.Object("hello.txt")
	writer := object.NewWriter(ctx)
	if _, err := io.WriteString(writer, "Hello from gcs-mimic!"); err != nil {
		log.Fatal(err)
	}
	if err := writer.Close(); err != nil {
		log.Fatal(err)
	}
	fmt.Println("uploaded hello.txt")

	objects := bucket.Objects(ctx, nil)
	for {
		attrs, err := objects.Next()
		if err == iterator.Done {
			break
		}
		if err != nil {
			log.Fatal(err)
		}
		fmt.Printf("listed %s (%d bytes)\n", attrs.Name, attrs.Size)
	}

	reader, err := object.NewReader(ctx)
	if err != nil {
		log.Fatal(err)
	}
	content, err := io.ReadAll(reader)
	reader.Close()
	if err != nil {
		log.Fatal(err)
	}
	fmt.Printf("downloaded: %s\n", content)

	if err := object.Delete(ctx); err != nil {
		log.Fatal(err)
	}
	fmt.Println("deleted hello.txt")
	if err := bucket.Delete(ctx); err != nil {
		log.Fatal(err)
	}
	fmt.Println("deleted bucket", bucketName)
}
