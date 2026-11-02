// Command example creates a bucket in a local gcs-mimic, uploads, lists,
// downloads and deletes an object, and removes the bucket again.
package main

import (
	"context"
	"crypto/rand"
	"errors"
	"fmt"
	"io"
	"log"
	"os"
	"strings"

	"cloud.google.com/go/storage"
	"google.golang.org/api/iterator"
)

const projectID = "local-project"

func main() {
	if err := run(context.Background()); err != nil {
		log.Fatal(err)
	}
}

func run(ctx context.Context) (err error) {
	endpoint := os.Getenv("GCS_MIMIC_ENDPOINT")
	if endpoint == "" {
		endpoint = "http://localhost:4443"
	}
	// With STORAGE_EMULATOR_HOST set, the client sends every request to the
	// emulator without authentication.
	os.Setenv("STORAGE_EMULATOR_HOST", endpoint)
	client, err := storage.NewClient(ctx)
	if err != nil {
		return fmt.Errorf("create client: %w", err)
	}
	defer client.Close()

	// Bucket names are lowercase; rand.Text is uppercase base32.
	bucket := client.Bucket("example-go-" + strings.ToLower(rand.Text()[:10]))
	if err := bucket.Create(ctx, projectID, nil); err != nil {
		return fmt.Errorf("create bucket: %w", err)
	}
	defer func() {
		if deleteErr := bucket.Delete(ctx); deleteErr != nil {
			err = errors.Join(err, fmt.Errorf("delete bucket: %w", deleteErr))
		}
	}()
	fmt.Println("created bucket", bucket.BucketName())

	object := bucket.Object("hello.txt")
	writer := object.NewWriter(ctx)
	writer.ContentType = "text/plain"
	if _, err := io.WriteString(writer, "Hello from the Go client!\n"); err != nil {
		writer.Close()
		return fmt.Errorf("upload: %w", err)
	}
	if err := writer.Close(); err != nil {
		return fmt.Errorf("upload: %w", err)
	}
	defer func() {
		if deleteErr := object.Delete(ctx); deleteErr != nil {
			err = errors.Join(err, fmt.Errorf("delete object: %w", deleteErr))
		}
	}()
	fmt.Println("uploaded", object.ObjectName())

	objects := bucket.Objects(ctx, nil)
	for {
		attrs, err := objects.Next()
		if errors.Is(err, iterator.Done) {
			break
		}
		if err != nil {
			return fmt.Errorf("list objects: %w", err)
		}
		fmt.Printf("listed %s (%d bytes)\n", attrs.Name, attrs.Size)
	}

	reader, err := object.NewReader(ctx)
	if err != nil {
		return fmt.Errorf("download: %w", err)
	}
	defer reader.Close()
	content, err := io.ReadAll(reader)
	if err != nil {
		return fmt.Errorf("download: %w", err)
	}
	fmt.Printf("downloaded: %s", content)
	return nil
}
