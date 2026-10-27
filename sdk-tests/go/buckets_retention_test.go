package gcsmimic_test

import (
	"context"
	"net/http"
	"testing"
	"time"

	"cloud.google.com/go/storage"
)

func TestBucketRetentionPolicy(t *testing.T) {
	base, bucket := versioningBucket(t, &storage.BucketAttrs{RetentionPolicy: &storage.RetentionPolicy{RetentionPeriod: time.Hour}})
	ctx, cancel := context.WithTimeout(base, 30*time.Second)
	defer cancel()
	t.Cleanup(func() {
		_, _ = bucket.Update(base, storage.BucketAttrsToUpdate{RetentionPolicy: &storage.RetentionPolicy{}})
	})
	attrs, err := bucket.Attrs(ctx)
	if err != nil || attrs.RetentionPolicy == nil || attrs.RetentionPolicy.RetentionPeriod != time.Hour || attrs.RetentionPolicy.EffectiveTime.IsZero() {
		t.Fatalf("retention attributes: %+v, %v", attrs, err)
	}
	object := bucket.Object("retained")
	writeVersion(t, ctx, object, "retained bytes")
	objectAttrs, err := object.Attrs(ctx)
	if err != nil || objectAttrs.RetentionExpirationTime.IsZero() {
		t.Fatalf("retention expiration: %+v, %v", objectAttrs, err)
	}
	requireObjectStatus(t, object.Delete(ctx), http.StatusForbidden)
	if _, err := bucket.Update(ctx, storage.BucketAttrsToUpdate{RetentionPolicy: &storage.RetentionPolicy{}}); err != nil {
		t.Fatal(err)
	}
	if err := object.Delete(ctx); err != nil {
		t.Fatal(err)
	}
	attrs, err = bucket.Update(ctx, storage.BucketAttrsToUpdate{RetentionPolicy: &storage.RetentionPolicy{RetentionPeriod: 2 * time.Hour}})
	if err != nil {
		t.Fatal(err)
	}
	if err := bucket.If(storage.BucketConditions{MetagenerationMatch: attrs.MetaGeneration}).LockRetentionPolicy(ctx); err != nil {
		t.Fatal(err)
	}
	attrs, err = bucket.Attrs(ctx)
	if err != nil || attrs.RetentionPolicy == nil || !attrs.RetentionPolicy.IsLocked {
		t.Fatalf("locked retention: %+v, %v", attrs, err)
	}
	_, err = bucket.Update(ctx, storage.BucketAttrsToUpdate{RetentionPolicy: &storage.RetentionPolicy{RetentionPeriod: time.Hour}})
	requireObjectStatus(t, err, http.StatusBadRequest)
}
