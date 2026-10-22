package gcsmimic_test

import (
	"context"
	"crypto/rand"
	"fmt"
	"maps"
	"net/http"
	"testing"
	"time"

	"cloud.google.com/go/storage"
)

func TestBucketMetadataUpdate(t *testing.T) {
	ctx, client, _, _ := newEmulatorClient(t)
	var nonce [8]byte
	if _, err := rand.Read(nonce[:]); err != nil {
		t.Fatal(err)
	}
	bucket := client.Bucket(fmt.Sprintf("sdk-go-bucket-update-%x", nonce))
	if err := bucket.Create(ctx, "sdk-bucket-update", nil); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		cleanupCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		if err := bucket.Delete(cleanupCtx); err != nil {
			t.Errorf("cleanup bucket: %v", err)
		}
	})
	original, err := bucket.Attrs(ctx)
	if err != nil {
		t.Fatal(err)
	}
	update := storage.BucketAttrsToUpdate{StorageClass: "NEARLINE", VersioningEnabled: true, DefaultEventBasedHold: true}
	update.SetLabel("keep", "one")
	update.SetLabel("remove", "two")
	first, err := bucket.Update(ctx, update)
	if err != nil {
		t.Fatal(err)
	}
	if !maps.Equal(first.Labels, map[string]string{"keep": "one", "remove": "two"}) ||
		first.StorageClass != "NEARLINE" || !first.VersioningEnabled || !first.DefaultEventBasedHold {
		t.Fatalf("initial update: %+v", first)
	}
	requireBucketUpdate(t, ctx, bucket, original, first)

	update = storage.BucketAttrsToUpdate{}
	update.SetLabel("add", "three")
	update.DeleteLabel("remove")
	matched := bucket.If(storage.BucketConditions{MetagenerationMatch: first.MetaGeneration})
	second, err := matched.Update(ctx, update)
	if err != nil {
		t.Fatal(err)
	}
	if !maps.Equal(second.Labels, map[string]string{"keep": "one", "add": "three"}) ||
		second.StorageClass != first.StorageClass || !second.VersioningEnabled || !second.DefaultEventBasedHold {
		t.Fatalf("label merge/removal or omitted settings: %+v", second)
	}
	requireBucketUpdate(t, ctx, bucket, first, second)
	_, err = matched.Update(ctx, storage.BucketAttrsToUpdate{StorageClass: "ARCHIVE"})
	requireObjectStatus(t, err, http.StatusPreconditionFailed)
	_, err = bucket.If(storage.BucketConditions{MetagenerationNotMatch: second.MetaGeneration}).Update(ctx,
		storage.BucketAttrsToUpdate{StorageClass: "ARCHIVE"})
	requireObjectStatus(t, err, http.StatusPreconditionFailed)
	afterFailure, err := bucket.Attrs(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if afterFailure.MetaGeneration != second.MetaGeneration || afterFailure.Etag != second.Etag ||
		!afterFailure.Updated.Equal(second.Updated) || afterFailure.StorageClass != second.StorageClass ||
		!maps.Equal(afterFailure.Labels, second.Labels) || !afterFailure.VersioningEnabled || !afterFailure.DefaultEventBasedHold {
		t.Fatalf("failed conditions changed bucket: %+v", afterFailure)
	}

	third, err := bucket.If(storage.BucketConditions{MetagenerationNotMatch: first.MetaGeneration}).Update(ctx,
		storage.BucketAttrsToUpdate{StorageClass: "STANDARD", VersioningEnabled: false, DefaultEventBasedHold: false})
	if err != nil {
		t.Fatal(err)
	}
	if third.StorageClass != "STANDARD" || third.VersioningEnabled || third.DefaultEventBasedHold || !maps.Equal(third.Labels, second.Labels) {
		t.Fatalf("explicit false settings or preserved labels: %+v", third)
	}
	requireBucketUpdate(t, ctx, bucket, second, third)

	update = storage.BucketAttrsToUpdate{}
	update.DeleteLabel("keep")
	update.DeleteLabel("add")
	update.DeleteLabel("absent")
	last, err := bucket.Update(ctx, update)
	if err != nil {
		t.Fatal(err)
	}
	if len(last.Labels) != 0 {
		t.Fatalf("delete-only update retained labels: %v", last.Labels)
	}
	requireBucketUpdate(t, ctx, bucket, third, last)
}

func requireBucketUpdate(t *testing.T, ctx context.Context, bucket *storage.BucketHandle, before, updated *storage.BucketAttrs) {
	t.Helper()
	if updated.MetaGeneration != before.MetaGeneration+1 || updated.Etag == before.Etag ||
		updated.Updated.Before(before.Updated) || updated.Updated.Before(updated.Created) || updated.Name != before.Name ||
		!updated.Created.Equal(before.Created) || updated.Location != before.Location {
		t.Fatalf("bucket identity/revision transition: before=%+v after=%+v", before, updated)
	}
	got, err := bucket.Attrs(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if got.MetaGeneration != updated.MetaGeneration || got.Etag != updated.Etag || !got.Updated.Equal(updated.Updated) ||
		got.StorageClass != updated.StorageClass || got.VersioningEnabled != updated.VersioningEnabled ||
		got.DefaultEventBasedHold != updated.DefaultEventBasedHold || !maps.Equal(got.Labels, updated.Labels) {
		t.Fatalf("Attrs differs from Update response: got=%+v update=%+v", got, updated)
	}
}

func TestStrictAuthBucketMetadataUpdate(t *testing.T) {
	ctx, bucket, object, payload := strictFixture(t, "bucket-update")
	_, reader := newStrictClient(t, "local-bucket-reader-token")
	policy, err := bucket.IAM().Policy(ctx)
	if err != nil {
		t.Fatal(err)
	}
	policy.Add(bucketReaderMember, "roles/storage.legacyBucketReader")
	if err := bucket.IAM().SetPolicy(ctx, policy); err != nil {
		t.Fatal(err)
	}
	readerBucket := reader.Bucket(bucket.BucketName())
	before, err := readerBucket.Attrs(ctx)
	if err != nil {
		t.Fatalf("read-only principal Attrs: %v", err)
	}
	update := storage.BucketAttrsToUpdate{}
	update.SetLabel("owner", "admin")
	_, err = readerBucket.Update(ctx, update)
	requireStrictError(t, err, http.StatusForbidden, "forbidden")
	after, err := bucket.Attrs(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if after.MetaGeneration != before.MetaGeneration || after.Etag != before.Etag ||
		!after.Updated.Equal(before.Updated) || !maps.Equal(after.Labels, before.Labels) {
		t.Fatalf("denied update changed bucket: before=%+v after=%+v", before, after)
	}
	updated, err := bucket.Update(ctx, update)
	if err != nil {
		t.Fatalf("admin Update: %v", err)
	}
	if !maps.Equal(updated.Labels, map[string]string{"owner": "admin"}) {
		t.Fatalf("admin labels: %v", updated.Labels)
	}
	requireBucketUpdate(t, ctx, bucket, before, updated)
	requireStrictObjectUnchanged(t, ctx, bucket, object, payload)
}
