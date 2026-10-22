package gcsmimic_test

import (
	"context"
	"crypto/rand"
	"errors"
	"fmt"
	"net/http"
	"testing"
	"time"

	"cloud.google.com/go/storage"
	"google.golang.org/api/googleapi"
	"google.golang.org/api/iterator"
	"google.golang.org/api/option"
	storageapi "google.golang.org/api/storage/v1"
)

func versioningBucket(t *testing.T, attrs *storage.BucketAttrs) (context.Context, *storage.BucketHandle) {
	t.Helper()
	ctx, client, httpClient, endpoint := newEmulatorClient(t)
	var nonce [8]byte
	if _, err := rand.Read(nonce[:]); err != nil {
		t.Fatal(err)
	}
	bucket := client.Bucket(fmt.Sprintf("sdk-go-versioning-%x", nonce))
	if attrs.DefaultEventBasedHold {
		// The high-level client's create conversion omits this field in the pinned release.
		api, err := storageapi.NewService(ctx, option.WithEndpoint(endpoint.String()+"/storage/v1/"), option.WithHTTPClient(httpClient), option.WithoutAuthentication())
		if err != nil {
			t.Fatal(err)
		}
		if _, err := api.Buckets.Insert("sdk-versioning", &storageapi.Bucket{Name: bucket.BucketName(), DefaultEventBasedHold: true}).Context(ctx).Do(); err != nil {
			t.Fatal(err)
		}
	} else {
		if err := bucket.Create(ctx, "sdk-versioning", attrs); err != nil {
			t.Fatal(err)
		}
	}
	t.Cleanup(func() {
		cleanupCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		objects := bucket.Objects(cleanupCtx, &storage.Query{Versions: true})
		for {
			attrs, err := objects.Next()
			if errors.Is(err, iterator.Done) {
				break
			}
			if err != nil {
				t.Errorf("cleanup listing: %v", err)
				return
			}
			object := bucket.Object(attrs.Name).Generation(attrs.Generation)
			if attrs.EventBasedHold {
				if _, err := object.Update(cleanupCtx, storage.ObjectAttrsToUpdate{EventBasedHold: false}); err != nil {
					t.Errorf("cleanup release: %v", err)
					continue
				}
			}
			if err := object.Delete(cleanupCtx); err != nil {
				t.Errorf("cleanup generation: %v", err)
			}
		}
		if err := bucket.Delete(cleanupCtx); err != nil {
			t.Errorf("cleanup bucket: %v", err)
		}
	})
	return ctx, bucket
}

func writeVersion(t *testing.T, ctx context.Context, object *storage.ObjectHandle, body string) *storage.ObjectAttrs {
	t.Helper()
	writer := object.NewWriter(ctx)
	if _, err := writer.Write([]byte(body)); err != nil {
		t.Fatal(err)
	}
	if err := writer.Close(); err != nil {
		t.Fatal(err)
	}
	return writer.Attrs()
}

func TestObjectVersioning(t *testing.T) {
	ctx, bucket := versioningBucket(t, &storage.BucketAttrs{VersioningEnabled: true})
	object := bucket.Object("history/雪.txt")
	first := writeVersion(t, ctx, object, "first version")
	second := writeVersion(t, ctx, object, "second version")
	if second.Generation <= first.Generation {
		t.Fatalf("generations did not advance: %d -> %d", first.Generation, second.Generation)
	}
	readObject(t, ctx, object.Generation(first.Generation), []byte("first version"), first.Generation)
	readObject(t, ctx, object, []byte("second version"), second.Generation)
	versions := bucket.Objects(ctx, &storage.Query{Versions: true})
	versions.PageInfo().MaxSize = 1
	for i, generation := range []int64{first.Generation, second.Generation} {
		attrs, err := versions.Next()
		if err != nil {
			t.Fatal(err)
		}
		if attrs.Generation != generation || attrs.Name != object.ObjectName() || (i == 0) != !attrs.Deleted.IsZero() {
			t.Fatalf("version %d: %+v", i, attrs)
		}
	}
	if _, err := versions.Next(); !errors.Is(err, iterator.Done) {
		t.Fatalf("version iterator end: %v", err)
	}
	if err := object.Generation(first.Generation).Delete(ctx); err != nil {
		t.Fatal(err)
	}
	requireObjectMissing(t, ctx, object.Generation(first.Generation))
	readObject(t, ctx, object, []byte("second version"), second.Generation)
	if err := object.Delete(ctx); err != nil {
		t.Fatal(err)
	}
	requireObjectMissing(t, ctx, object)
	readObject(t, ctx, object.Generation(second.Generation), []byte("second version"), second.Generation)
}

func TestObjectEventBasedHold(t *testing.T) {
	ctx, bucket := versioningBucket(t, &storage.BucketAttrs{DefaultEventBasedHold: true})
	object := bucket.Object("held")
	created := writeVersion(t, ctx, object, "held bytes")
	if !created.EventBasedHold {
		t.Fatal("bucket default did not place an event-based hold")
	}
	err := object.Delete(ctx)
	requireObjectStatus(t, err, http.StatusForbidden)
	var apiError *googleapi.Error
	if !errors.As(err, &apiError) || len(apiError.Errors) == 0 || apiError.Errors[0].Reason != "objectUnderActiveHold" {
		t.Fatalf("held delete reason: %v", err)
	}
	readObject(t, ctx, object, []byte("held bytes"), created.Generation)
	released, err := object.If(storage.Conditions{MetagenerationMatch: created.Metageneration}).Update(ctx, storage.ObjectAttrsToUpdate{EventBasedHold: false})
	if err != nil {
		t.Fatal(err)
	}
	if released.EventBasedHold || released.Generation != created.Generation || released.Metageneration != created.Metageneration+1 {
		t.Fatalf("release: %+v", released)
	}
	if err := object.Delete(ctx); err != nil {
		t.Fatal(err)
	}
	requireObjectMissing(t, ctx, object)
}
