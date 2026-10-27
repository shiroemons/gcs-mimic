package gcsmimic_test

import (
	"context"
	"crypto/rand"
	"errors"
	"fmt"
	"strings"
	"testing"
	"time"

	"cloud.google.com/go/storage"
	"google.golang.org/api/googleapi"
)

func TestUnsupportedFailsFast(t *testing.T) {
	ctx, client, _, _ := newEmulatorClient(t)
	var nonce [8]byte
	if _, err := rand.Read(nonce[:]); err != nil {
		t.Fatal(err)
	}
	bucket := client.Bucket(fmt.Sprintf("sdk-go-unsupported-%x", nonce))
	if err := bucket.Create(ctx, "local-project", nil); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		cleanup, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		if err := bucket.Delete(cleanup); err != nil {
			t.Errorf("cleanup: %v", err)
		}
	})
	for _, test := range []struct {
		name string
		call func(context.Context) error
	}{
		{"SoftDeleted", func(ctx context.Context) error {
			_, err := bucket.Objects(ctx, &storage.Query{SoftDeleted: true}).Next()
			return err
		}},
		{"IncludeFoldersAsPrefixes", func(ctx context.Context) error {
			_, err := bucket.Objects(ctx, &storage.Query{Delimiter: "/", IncludeFoldersAsPrefixes: true}).Next()
			return err
		}},
		{"BucketACL", func(ctx context.Context) error { _, err := bucket.ACL().List(ctx); return err }},
		{"DefaultObjectACL", func(ctx context.Context) error { _, err := bucket.DefaultObjectACL().List(ctx); return err }},
		{"ObjectACL", func(ctx context.Context) error { _, err := bucket.Object("missing").ACL().List(ctx); return err }},
		{"Notifications", func(ctx context.Context) error { _, err := bucket.Notifications(ctx); return err }},
		{"HMACKeys", func(ctx context.Context) error {
			_, err := client.ListHMACKeys(ctx, "local-project").Next()
			return err
		}},
		{"ServiceAccount", func(ctx context.Context) error { _, err := client.ServiceAccount(ctx, "local-project"); return err }},
		{"Restore", func(ctx context.Context) error {
			_, err := bucket.Object("missing").Generation(1).Restore(ctx, &storage.RestoreOptions{})
			return err
		}},
	} {
		t.Run(test.name, func(t *testing.T) {
			deadline, cancel := context.WithTimeout(context.Background(), 10*time.Second)
			defer cancel()
			start := time.Now()
			err := test.call(deadline)
			var apiError *googleapi.Error
			if !errors.As(err, &apiError) || apiError.Code != 400 || len(apiError.Errors) == 0 || apiError.Errors[0].Reason != "badRequest" {
				t.Fatalf("expected 400 badRequest, got %T: %v", err, err)
			}
			if time.Since(start) >= 3*time.Second || deadline.Err() != nil || storage.ShouldRetry(err) {
				t.Fatalf("unsupported call did not fail fast: %v", err)
			}
			if errors.Is(err, storage.ErrBucketNotExist) || errors.Is(err, storage.ErrObjectNotExist) || !strings.Contains(apiError.Message, "gcs-mimic") {
				t.Fatalf("unsupported call is not distinguishable: %v", err)
			}
		})
	}
	if _, err := client.Bucket(bucket.BucketName() + "-missing").Attrs(ctx); !errors.Is(err, storage.ErrBucketNotExist) {
		t.Fatalf("missing bucket: %v", err)
	}
	if _, err := bucket.Object("missing").Attrs(ctx); !errors.Is(err, storage.ErrObjectNotExist) {
		t.Fatalf("missing object: %v", err)
	}
}
