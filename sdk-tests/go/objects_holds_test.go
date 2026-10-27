package gcsmimic_test

import (
	"net/http"
	"testing"
	"time"

	"cloud.google.com/go/storage"
)

func TestObjectTemporaryHoldAndCustomTime(t *testing.T) {
	ctx, bucket := versioningBucket(t, &storage.BucketAttrs{})
	object := bucket.Object("temporary-hold")
	stamp := time.Date(2026, 10, 20, 1, 2, 3, 0, time.UTC)
	writer := object.NewWriter(ctx)
	writer.TemporaryHold = true
	writer.CustomTime = stamp
	if _, err := writer.Write([]byte("protected bytes")); err != nil {
		t.Fatal(err)
	}
	if err := writer.Close(); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _, _ = object.Update(ctx, storage.ObjectAttrsToUpdate{TemporaryHold: false}) })
	attrs, err := object.Attrs(ctx)
	if err != nil || !attrs.TemporaryHold || !attrs.CustomTime.Equal(stamp) {
		t.Fatalf("uploaded attributes: %+v, %v", attrs, err)
	}
	requireObjectStatus(t, object.Delete(ctx), http.StatusForbidden)
	if _, err := object.Update(ctx, storage.ObjectAttrsToUpdate{CustomTime: stamp.Add(-time.Second)}); err == nil {
		t.Fatal("custom time moved backwards")
	}
	attrs, err = object.Update(ctx, storage.ObjectAttrsToUpdate{TemporaryHold: false, CustomTime: stamp.Add(time.Second)})
	if err != nil || attrs.TemporaryHold || !attrs.CustomTime.Equal(stamp.Add(time.Second)) {
		t.Fatalf("released attributes: %+v, %v", attrs, err)
	}
	attrs, err = object.Update(ctx, storage.ObjectAttrsToUpdate{TemporaryHold: true})
	if err != nil || !attrs.TemporaryHold {
		t.Fatalf("set hold: %+v, %v", attrs, err)
	}
	if _, err := object.Update(ctx, storage.ObjectAttrsToUpdate{TemporaryHold: false}); err != nil {
		t.Fatal(err)
	}
	if err := object.Delete(ctx); err != nil {
		t.Fatal(err)
	}
}
