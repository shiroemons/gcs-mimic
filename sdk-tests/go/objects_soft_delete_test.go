package gcsmimic_test

import (
	"context"
	"net/http"
	"testing"
	"time"

	"cloud.google.com/go/storage"
	"google.golang.org/api/iterator"
)

func TestObjectSoftDeleteRestore(t *testing.T) {
	base, bucket := versioningBucket(t, &storage.BucketAttrs{SoftDeletePolicy: &storage.SoftDeletePolicy{RetentionDuration: 7 * 24 * time.Hour}})
	ctx, cancel := context.WithTimeout(base, 30*time.Second)
	defer cancel()
	attrs, err := bucket.Attrs(ctx)
	if err != nil || attrs.SoftDeletePolicy == nil || attrs.SoftDeletePolicy.RetentionDuration != 7*24*time.Hour || attrs.SoftDeletePolicy.EffectiveTime.IsZero() {
		t.Fatalf("soft delete policy: %+v, %v", attrs, err)
	}
	object := bucket.Object("history/雪.txt")
	original := writeVersion(t, ctx, object, "recoverable bytes")
	if err := object.Delete(ctx); err != nil {
		t.Fatal(err)
	}
	if _, err := object.Attrs(ctx); err == nil {
		t.Fatal("deleted object is live")
	}
	deleted := bucket.Objects(ctx, &storage.Query{SoftDeleted: true})
	deleted.PageInfo().MaxSize = 1
	listed, err := deleted.Next()
	if err != nil || listed.Generation != original.Generation || listed.SoftDeleteTime.IsZero() || listed.HardDeleteTime.Sub(listed.SoftDeleteTime) != 7*24*time.Hour {
		t.Fatalf("soft deleted listing: %+v, %v", listed, err)
	}
	if _, err := deleted.Next(); err != iterator.Done {
		t.Fatalf("unexpected retained generation: %v", err)
	}
	selected := object.Generation(original.Generation)
	if _, err := selected.SoftDeleted().Attrs(ctx); err != nil {
		t.Fatal(err)
	}
	restored, err := selected.Restore(ctx, &storage.RestoreOptions{})
	if err != nil || restored.Generation <= original.Generation || restored.Metageneration != 1 || !restored.SoftDeleteTime.IsZero() || !restored.HardDeleteTime.IsZero() {
		t.Fatalf("restore: %+v, %v", restored, err)
	}
	readObject(t, ctx, object, []byte("recoverable bytes"), restored.Generation)
	if _, err := selected.SoftDeleted().Attrs(ctx); err != nil {
		t.Fatalf("restore removed original: %v", err)
	}
	_, err = selected.If(storage.Conditions{DoesNotExist: true}).Restore(ctx, &storage.RestoreOptions{})
	requireObjectStatus(t, err, http.StatusPreconditionFailed)
	if _, err := bucket.Update(ctx, storage.BucketAttrsToUpdate{SoftDeletePolicy: &storage.SoftDeletePolicy{RetentionDuration: 0}}); err != nil {
		t.Fatal(err)
	}
}
