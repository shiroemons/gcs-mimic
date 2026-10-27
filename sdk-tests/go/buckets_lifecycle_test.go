package gcsmimic_test

import (
	"context"
	"fmt"
	"reflect"
	"testing"
	"time"

	"cloud.google.com/go/storage"
)

func TestBucketLifecycleRules(t *testing.T) {
	ctx, client, _, _ := newEmulatorClient(t)
	bucket := client.Bucket(fmt.Sprintf("sdk-lifecycle-%d", time.Now().UnixNano()))
	lifecycle := storage.Lifecycle{Rules: []storage.LifecycleRule{{
		Action:    storage.LifecycleAction{Type: storage.DeleteAction},
		Condition: storage.LifecycleCondition{AgeInDays: 30},
	}}}
	if err := bucket.Create(ctx, "sdk-project", &storage.BucketAttrs{Lifecycle: lifecycle}); err != nil {
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
	if !reflect.DeepEqual(attrs.Lifecycle, lifecycle) {
		t.Fatalf("created lifecycle: %+v", attrs.Lifecycle)
	}
	lifecycle.Rules[0].Action = storage.LifecycleAction{Type: storage.SetStorageClassAction, StorageClass: "COLDLINE"}
	lifecycle.Rules[0].Condition = storage.LifecycleCondition{AgeInDays: 60, MatchesPrefix: []string{"archive/"}}
	updated, err := bucket.Update(ctx, storage.BucketAttrsToUpdate{Lifecycle: &lifecycle})
	if err != nil {
		t.Fatal(err)
	}
	if updated.MetaGeneration != attrs.MetaGeneration+1 {
		t.Fatal("metageneration did not advance once")
	}
	attrs, err = bucket.Attrs(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(attrs.Lifecycle, lifecycle) {
		t.Fatalf("updated lifecycle: %+v", attrs.Lifecycle)
	}
	if _, err := bucket.Update(ctx, storage.BucketAttrsToUpdate{Lifecycle: &storage.Lifecycle{}}); err != nil {
		t.Fatal(err)
	}
	attrs, err = bucket.Attrs(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if len(attrs.Lifecycle.Rules) != 0 {
		t.Fatalf("lifecycle not cleared: %+v", attrs.Lifecycle)
	}
}
