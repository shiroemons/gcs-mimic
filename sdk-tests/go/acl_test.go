package gcsmimic_test

import (
	"context"
	"crypto/rand"
	"errors"
	"fmt"
	"testing"
	"time"

	"cloud.google.com/go/storage"
)

func hasRule(rules []storage.ACLRule, entity storage.ACLEntity, role storage.ACLRole) bool {
	for _, rule := range rules {
		if rule.Entity == entity && rule.Role == role {
			return true
		}
	}
	return false
}

// TestJSONACL stores and reads back bucket, default object and object ACL entries. The
// entries are metadata only: gcs-mimic does not yet use them to grant or deny access.
func TestJSONACL(t *testing.T) {
	ctx, client, _, _ := newEmulatorClient(t)
	var nonce [8]byte
	if _, err := rand.Read(nonce[:]); err != nil {
		t.Fatal(err)
	}
	bucket := client.Bucket(fmt.Sprintf("sdk-go-acl-%x", nonce))
	if err := bucket.Create(ctx, "local-project", nil); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		cleanup, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		if err := bucket.Object("acl.txt").Delete(cleanup); err != nil && !errors.Is(err, storage.ErrObjectNotExist) {
			t.Errorf("cleanup object: %v", err)
		}
		if err := bucket.Delete(cleanup); err != nil && !errors.Is(err, storage.ErrBucketNotExist) {
			t.Errorf("cleanup: %v", err)
		}
	})

	if err := bucket.ACL().Set(ctx, storage.AllAuthenticatedUsers, storage.RoleReader); err != nil {
		t.Fatalf("set bucket ACL: %v", err)
	}
	rules, err := bucket.ACL().List(ctx)
	if err != nil || !hasRule(rules, storage.AllAuthenticatedUsers, storage.RoleReader) {
		t.Fatalf("bucket ACL %+v: %v", rules, err)
	}
	if err := bucket.DefaultObjectACL().Set(ctx, storage.AllAuthenticatedUsers, storage.RoleReader); err != nil {
		t.Fatalf("set default object ACL: %v", err)
	}

	object := bucket.Object("acl.txt")
	writer := object.NewWriter(ctx)
	if _, err := writer.Write([]byte("x")); err != nil {
		t.Fatal(err)
	}
	if err := writer.Close(); err != nil {
		t.Fatalf("write object: %v", err)
	}
	rules, err = object.ACL().List(ctx)
	if err != nil || !hasRule(rules, storage.AllAuthenticatedUsers, storage.RoleReader) {
		t.Fatalf("default ACL not inherited %+v: %v", rules, err)
	}
	if err := object.ACL().Set(ctx, storage.AllUsers, storage.RoleReader); err != nil {
		t.Fatalf("set object ACL: %v", err)
	}
	if rules, err = object.ACL().List(ctx); err != nil || !hasRule(rules, storage.AllUsers, storage.RoleReader) {
		t.Fatalf("object ACL %+v: %v", rules, err)
	}
	if err := object.ACL().Delete(ctx, storage.AllUsers); err != nil {
		t.Fatalf("delete object ACL: %v", err)
	}
	if rules, err = object.ACL().List(ctx); err != nil || hasRule(rules, storage.AllUsers, storage.RoleReader) {
		t.Fatalf("object ACL entry not deleted %+v: %v", rules, err)
	}
}
