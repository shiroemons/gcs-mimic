package gcsmimic_test

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"testing"
	"time"

	"cloud.google.com/go/storage"
	"google.golang.org/api/googleapi"
)

func TestBucketUniformAccess(t *testing.T) {
	ctx, client, _, _ := newEmulatorClient(t)
	bucket := client.Bucket(fmt.Sprintf("sdk-ubla-%d", time.Now().UnixNano()))
	if err := bucket.Create(ctx, "sdk-project", &storage.BucketAttrs{
		UniformBucketLevelAccess: storage.UniformBucketLevelAccess{Enabled: true},
	}); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		cleanup, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		if err := bucket.Delete(cleanup); err != nil {
			t.Errorf("cleanup: %v", err)
		}
	})
	attrs, err := bucket.Attrs(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if !attrs.UniformBucketLevelAccess.Enabled || attrs.UniformBucketLevelAccess.LockedTime.Sub(attrs.Created) != 90*24*time.Hour {
		t.Fatalf("uniform access: %+v", attrs.UniformBucketLevelAccess)
	}
	writer := bucket.Object("rejected").NewWriter(ctx)
	writer.PredefinedACL = "publicRead"
	_, _ = writer.Write([]byte("must not publish"))
	var apiError *googleapi.Error
	if err := writer.Close(); !errors.As(err, &apiError) || apiError.Code != http.StatusBadRequest {
		t.Fatalf("predefined ACL error: %v", err)
	}
	if _, err := bucket.Object("rejected").Attrs(ctx); !errors.Is(err, storage.ErrObjectNotExist) {
		t.Fatalf("rejected ACL write published an object: %v", err)
	}
	attrs, err = bucket.Update(ctx, storage.BucketAttrsToUpdate{
		UniformBucketLevelAccess: &storage.UniformBucketLevelAccess{Enabled: false},
	})
	if err != nil {
		t.Fatal(err)
	}
	if attrs.UniformBucketLevelAccess.Enabled || !attrs.UniformBucketLevelAccess.LockedTime.IsZero() {
		t.Fatalf("disabled uniform access: %+v", attrs.UniformBucketLevelAccess)
	}
}
