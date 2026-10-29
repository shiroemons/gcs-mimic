package gcsmimic_test

import (
	"crypto/rand"
	"errors"
	"fmt"
	"strings"
	"testing"

	"cloud.google.com/go/storage"
	"google.golang.org/api/googleapi"
	"google.golang.org/api/iterator"
)

func TestHMACKeysAndServiceAccount(t *testing.T) {
	ctx, client, _, _ := newEmulatorClient(t)
	var nonce [8]byte
	if _, err := rand.Read(nonce[:]); err != nil {
		t.Fatal(err)
	}
	// A fresh account keeps repeated runs below the per-account key limit.
	email := fmt.Sprintf("sdk-go-%x@local-project.iam.gserviceaccount.com", nonce)
	key, err := client.CreateHMACKey(ctx, "local-project", email)
	if err != nil {
		t.Fatal(err)
	}
	if len(key.Secret) != 40 || len(key.AccessID) != 61 || key.State != storage.Active ||
		key.ServiceAccountEmail != email || key.ProjectID != "local-project" || key.Etag == "" ||
		key.ID != "local-project/"+key.AccessID || key.CreatedTime.IsZero() {
		t.Fatalf("created key %+v", key)
	}
	handle := client.HMACKeyHandle("local-project", key.AccessID)
	got, err := handle.Get(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if got.Secret != "" || got.AccessID != key.AccessID || got.Etag != key.Etag || got.State != storage.Active {
		t.Fatalf("fetched key %+v", got)
	}

	listed := func(opts ...storage.HMACKeyOption) []string {
		t.Helper()
		var ids []string
		it := client.ListHMACKeys(ctx, "local-project", append(opts, storage.ForHMACKeyServiceAccountEmail(email))...)
		for {
			item, err := it.Next()
			if errors.Is(err, iterator.Done) {
				return ids
			}
			if err != nil {
				t.Fatal(err)
			}
			ids = append(ids, item.AccessID)
		}
	}
	if ids := listed(); len(ids) != 1 || ids[0] != key.AccessID {
		t.Fatalf("listed %v", ids)
	}

	var apiError *googleapi.Error
	if err := handle.Delete(ctx); !errors.As(err, &apiError) || apiError.Code != 400 {
		t.Fatalf("deleting an ACTIVE key: %v", err)
	}
	if _, err := handle.Update(ctx, storage.HMACKeyAttrsToUpdate{State: storage.Inactive, Etag: "stale"}); !errors.As(err, &apiError) || apiError.Code != 412 {
		t.Fatalf("stale etag update: %v", err)
	}
	inactive, err := handle.Update(ctx, storage.HMACKeyAttrsToUpdate{State: storage.Inactive, Etag: key.Etag})
	if err != nil {
		t.Fatal(err)
	}
	if inactive.State != storage.Inactive || inactive.Etag == key.Etag {
		t.Fatalf("updated key %+v", inactive)
	}
	if err := handle.Delete(ctx); err != nil {
		t.Fatal(err)
	}
	deleted, err := handle.Get(ctx)
	if err != nil || deleted.State != storage.Deleted {
		t.Fatalf("deleted key %+v: %v", deleted, err)
	}
	if ids := listed(); len(ids) != 0 {
		t.Fatalf("deleted key listed by default: %v", ids)
	}
	if ids := listed(storage.ShowDeletedHMACKeys()); len(ids) != 1 || ids[0] != key.AccessID {
		t.Fatalf("showDeletedKeys listed %v", ids)
	}

	account, err := client.ServiceAccount(ctx, "123456")
	if err != nil {
		t.Fatal(err)
	}
	if account != "service-123456@gs-project-accounts.iam.gserviceaccount.com" {
		t.Fatalf("service account %q", account)
	}
	if account, err := client.ServiceAccount(ctx, "local-project"); err != nil || !strings.HasSuffix(account, "@gs-project-accounts.iam.gserviceaccount.com") {
		t.Fatalf("service account %q: %v", account, err)
	}
}
