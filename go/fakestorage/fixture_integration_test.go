//go:build integration

package fakestorage

import (
	"errors"
	"os"
	"testing"
	"time"

	"cloud.google.com/go/storage"
	"google.golang.org/api/googleapi"
)

func TestRustFixtureACLAndRetention(t *testing.T) {
	if os.Getenv("GCS_MIMIC_BINARY") == "" {
		t.Fatal("integration requires GCS_MIMIC_BINARY")
	}
	until := time.Date(2090, 1, 1, 0, 0, 0, 123456000, time.UTC)
	first := Object{ObjectAttrs: ObjectAttrs{BucketName: "fixture-bucket", Name: "first"}, Content: []byte("first")}
	protected := Object{ObjectAttrs: ObjectAttrs{BucketName: "fixture-bucket", Name: "protected",
		ACL:       []storage.ACLRule{{Entity: storage.AllUsers, Role: storage.RoleReader}},
		Retention: &storage.ObjectRetention{Mode: "Locked", RetainUntil: until}}, Content: []byte("retained")}
	server, err := NewServerWithOptions(Options{Scheme: "http", NoListener: true, InitialObjects: []Object{first, protected}})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := server.Close(); err != nil {
			t.Error(err)
		}
	})
	restored, err := server.GetObject(protected.BucketName, protected.Name)
	if err != nil {
		t.Fatal(err)
	}
	if len(restored.ACL) != 1 || restored.ACL[0].Entity != storage.AllUsers || restored.ACL[0].Role != storage.RoleReader || restored.Retention == nil || restored.Retention.Mode != "Locked" || !restored.Retention.RetainUntil.Equal(until) || string(restored.Content) != "retained" {
		t.Fatalf("fixture metadata changed: %+v", restored)
	}
	rejected := Object{ObjectAttrs: ObjectAttrs{BucketName: protected.BucketName, Name: protected.Name}, Content: []byte("denied")}
	var api *googleapi.Error
	if err := server.CreateObjectStreaming(rejected.StreamingObject()); !errors.As(err, &api) || api.Code != 403 {
		t.Fatalf("retention overwrite: %v", err)
	}
	runtime := Object{ObjectAttrs: ObjectAttrs{BucketName: protected.BucketName, Name: "runtime", Retention: &storage.ObjectRetention{Mode: "Unlocked", RetainUntil: until}}, Content: []byte("runtime")}
	if err := server.CreateObjectStreaming(runtime.StreamingObject()); err != nil {
		t.Fatal(err)
	}
	restored, err = server.GetObject(runtime.BucketName, runtime.Name)
	if err != nil || restored.Retention == nil || restored.Retention.Mode != "Unlocked" || !restored.Retention.RetainUntil.Equal(until) {
		t.Fatalf("runtime retention: %+v %v", restored, err)
	}
	for _, rule := range []storage.ACLRule{{Entity: storage.AllUsers, Role: storage.RoleWriter}, {Entity: storage.AllUsers, Role: storage.RoleReader, EntityID: "unpreserved"}} {
		rejected.ObjectAttrs = ObjectAttrs{BucketName: first.BucketName, Name: first.Name, ACL: []storage.ACLRule{rule}}
		if err := server.CreateObjectStreaming(rejected.StreamingObject()); !errors.As(err, &api) || api.Code != 400 {
			t.Fatalf("invalid ACL: %v", err)
		}
	}
	restored, err = server.GetObject(first.BucketName, first.Name)
	if err != nil || string(restored.Content) != "first" {
		t.Fatalf("failed fixture replaced bytes: %+v %v", restored, err)
	}
}
