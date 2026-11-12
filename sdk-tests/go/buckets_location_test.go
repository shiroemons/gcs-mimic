package gcsmimic_test

import (
	"context"
	"crypto/rand"
	"errors"
	"fmt"
	"net/http"
	"strings"
	"testing"
	"time"

	"cloud.google.com/go/storage"
)

func TestBucketLocations(t *testing.T) {
	ctx, client, _, _ := newEmulatorClient(t)
	var nonce [8]byte
	if _, err := rand.Read(nonce[:]); err != nil {
		t.Fatal(err)
	}
	project := fmt.Sprintf("sdk-locations-%x", nonce)
	for index, tc := range []struct{ location, kind string }{
		{"ASIA-NORTHEAST1", "region"},
		{"asia-northeast1", "region"},
		{"NAM4", "dual-region"},
	} {
		t.Run(tc.location, func(t *testing.T) {
			bucket := client.Bucket(fmt.Sprintf("sdk-locations-%x-%d", nonce, index))
			if fakeGCSServerProfile() {
				// The profile reports one fixed region and refuses another location instead of
				// storing a placement the caller did not get.
				err := bucket.Create(ctx, project, &storage.BucketAttrs{Location: tc.location})
				requireObjectStatus(t, err, http.StatusBadRequest)
				if _, err := bucket.Attrs(ctx); !errors.Is(err, storage.ErrBucketNotExist) {
					t.Fatalf("a refused create must leave no bucket: %v", err)
				}
				return
			}
			if err := bucket.Create(ctx, project, &storage.BucketAttrs{Location: tc.location}); err != nil {
				t.Fatal(err)
			}
			t.Cleanup(func() {
				cleanupCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
				defer cancel()
				if err := bucket.Delete(cleanupCtx); err != nil {
					t.Error(err)
				}
			})
			attrs, err := bucket.Attrs(ctx)
			if err != nil {
				t.Fatal(err)
			}
			if attrs.Location != strings.ToUpper(tc.location) || attrs.LocationType != tc.kind {
				t.Fatalf("location=%q locationType=%q", attrs.Location, attrs.LocationType)
			}
		})
	}
}
