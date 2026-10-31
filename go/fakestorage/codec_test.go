package fakestorage

import (
	"encoding/json"
	"reflect"
	"strings"
	"testing"
	"time"

	"cloud.google.com/go/storage"
)

func TestObjectJSONPreservesSourceFixtureMetadata(t *testing.T) {
	stamp := time.Date(2020, 1, 2, 3, 4, 5, 123456000, time.UTC)
	attrs := ObjectAttrs{BucketName: "bucket", Name: "dir/日本語", Size: 9007199254740993,
		Generation: 9007199254740995, StorageClass: "COLDLINE", ContentType: "text/plain",
		ContentEncoding: "gzip", ContentDisposition: "attachment", ContentLanguage: "ja",
		CacheControl: "no-cache", Crc32c: "AAAAAA==", Md5Hash: "hash", Etag: "etag",
		Created: stamp, Updated: stamp, Deleted: stamp, CustomTime: stamp,
		Metadata: map[string]string{"fixture": "value"},
		ACL: []storage.ACLRule{{Entity: "project-owners-42", EntityID: "id", Role: storage.RoleOwner,
			Domain: "example.invalid", Email: "owner@example.invalid",
			ProjectTeam: &storage.ProjectTeam{ProjectNumber: "42", Team: "owners"}}},
		Retention: &storage.ObjectRetention{Mode: "Unlocked", RetainUntil: stamp}}
	encoded, err := json.Marshal(Object{ObjectAttrs: attrs, Content: []byte("must not export content")})
	if err != nil {
		t.Fatal(err)
	}
	for _, field := range []string{`"bucket":"bucket"`, `"size":"9007199254740993"`,
		`"generation":"9007199254740995"`, `"created":"2020-01-02T03:04:05.123456Z"`,
		`"projectTeam":{"projectNumber":"42","team":"owners"}`,
		`"retention":{"mode":"Unlocked","retainUntilTime":"2020-01-02T03:04:05.123456Z"}`} {
		if !strings.Contains(string(encoded), field) {
			t.Fatalf("missing source JSON field %s: %s", field, encoded)
		}
	}
	if strings.Contains(string(encoded), "Content") || strings.Contains(string(encoded), "must not export") {
		t.Fatalf("object content leaked into metadata: %s", encoded)
	}
	var restored Object
	if err := json.Unmarshal(encoded, &restored); err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(restored.ObjectAttrs, attrs) || len(restored.Content) != 0 {
		t.Fatalf("metadata changed: %+v", restored)
	}
}

func TestObjectJSONDecodeRejectsInvalidIdentityAndClearsAbsentMetadata(t *testing.T) {
	for _, encoded := range []string{`{"size":1}`, `{"generation":"9223372036854775808"}`, `{"created":"invalid"}`} {
		attrs := ObjectAttrs{Name: "unchanged"}
		if err := json.Unmarshal([]byte(encoded), &attrs); err == nil || attrs.Name != "unchanged" {
			t.Fatalf("invalid metadata succeeded or changed destination: %+v %v", attrs, err)
		}
	}
	attrs := ObjectAttrs{ACL: []storage.ACLRule{{Entity: storage.AllUsers}}, Retention: &storage.ObjectRetention{Mode: "Locked"}}
	if err := json.Unmarshal([]byte(`{"name":"new","unknownField":true}`), &attrs); err != nil {
		t.Fatal(err)
	}
	if attrs.Name != "new" || attrs.ACL != nil || attrs.Retention != nil {
		t.Fatalf("absent fields retained stale metadata: %+v", attrs)
	}
}
