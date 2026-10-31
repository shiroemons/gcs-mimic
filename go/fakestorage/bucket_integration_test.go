//go:build integration

package fakestorage

import (
	"context"
	"errors"
	"io"
	"os"
	"testing"

	"google.golang.org/api/googleapi"
)

func TestRustBucketHelpersRetainOptionsAndProtectObjects(t *testing.T) {
	if os.Getenv("GCS_MIMIC_BINARY") == "" {
		t.Fatal("integration requires GCS_MIMIC_BINARY")
	}
	server, err := NewServerWithOptions(Options{Scheme: "http", NoListener: true})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := server.Close(); err != nil {
			t.Error(err)
		}
	})
	server.CreateBucket("plain-bucket")
	server.CreateBucketWithOpts(CreateBucketOpts{Name: "held-bucket", VersioningEnabled: true, DefaultEventBasedHold: true})
	server.CreateBucketWithOpts(CreateBucketOpts{Name: "held-bucket", VersioningEnabled: true})
	client := server.Client()
	defer client.Close()
	ctx := context.Background()
	attrs, err := client.Bucket("held-bucket").Attrs(ctx)
	if err != nil || !attrs.VersioningEnabled || !attrs.DefaultEventBasedHold {
		t.Fatalf("bucket settings: %+v %v", attrs, err)
	}
	object := client.Bucket("held-bucket").Object("held")
	writer := object.NewWriter(ctx)
	if _, err := io.WriteString(writer, "held bytes"); err != nil {
		t.Fatal(err)
	}
	if err := writer.Close(); err != nil {
		t.Fatal(err)
	}
	var api *googleapi.Error
	if err := object.Delete(ctx); !errors.As(err, &api) || api.Code != 403 {
		t.Fatalf("default hold did not protect bytes: %v", err)
	}
}
