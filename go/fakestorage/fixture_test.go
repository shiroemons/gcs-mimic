package fakestorage

import (
	"encoding/json"
	"io"
	"mime"
	"mime/multipart"
	"strings"
	"testing"
	"time"

	"cloud.google.com/go/storage"
)

func TestFixtureBodyPreservesGenerationTimestampsMetadataAndBytes(t *testing.T) {
	created := time.Date(2020, 1, 2, 3, 4, 5, 123456000, time.UTC)
	object := Object{ObjectAttrs: ObjectAttrs{BucketName: "bucket", Name: "dir/日本語 +%?.txt", Generation: 42,
		Created: created, Updated: created, ContentType: "text/plain", Metadata: map[string]string{"fixture": "value"}},
		Content: []byte("raw\r\nbytes\x00")}
	body, contentType, err := fixtureBody(object.StreamingObject(), runtimeFixture)
	if err != nil {
		t.Fatal(err)
	}
	_, params, err := mime.ParseMediaType(contentType)
	if err != nil {
		t.Fatal(err)
	}
	reader := multipart.NewReader(body, params["boundary"])
	part, err := reader.NextPart()
	if err != nil {
		t.Fatal(err)
	}
	var attrs map[string]any
	if err := json.NewDecoder(part).Decode(&attrs); err != nil {
		t.Fatal(err)
	}
	if attrs["generation"] != "42" || attrs["timeCreated"] != created.Format(time.RFC3339Nano) ||
		attrs["name"] != object.Name || attrs["metadata"].(map[string]any)["fixture"] != "value" {
		t.Fatalf("fixture identity changed: %#v", attrs)
	}
	part, err = reader.NextPart()
	if err != nil {
		t.Fatal(err)
	}
	data, err := io.ReadAll(part)
	if err != nil || string(data) != string(object.Content) {
		t.Fatalf("fixture bytes: %q %v", data, err)
	}
	if _, err := reader.NextPart(); err != io.EOF {
		t.Fatalf("multipart terminator: %v", err)
	}
}

func TestFixtureBodyPreservesACLAndRetentionForBothPublicationModes(t *testing.T) {
	until := time.Date(2090, 1, 1, 0, 0, 0, 123456000, time.UTC)
	object := Object{ObjectAttrs: ObjectAttrs{BucketName: "bucket", Name: "object",
		ACL:       []storage.ACLRule{{Entity: storage.AllUsers, Role: storage.RoleReader}},
		Retention: &storage.ObjectRetention{Mode: "Locked", RetainUntil: until}}, Content: []byte("body")}
	if err := validateFixture(object.ObjectAttrs); err != nil {
		t.Fatal(err)
	}
	for _, mode := range []fixtureMode{initialFixture, runtimeFixture} {
		body, contentType, err := fixtureBody(object.StreamingObject(), mode)
		if err != nil {
			t.Fatal(err)
		}
		_, params, err := mime.ParseMediaType(contentType)
		if err != nil {
			t.Fatal(err)
		}
		part, err := multipart.NewReader(body, params["boundary"]).NextPart()
		if err != nil {
			t.Fatal(err)
		}
		var attrs map[string]any
		if err := json.NewDecoder(part).Decode(&attrs); err != nil {
			t.Fatal(err)
		}
		acl := attrs["acl"].([]any)[0].(map[string]any)
		retention := attrs["retention"].(map[string]any)
		if acl["entity"] != "allUsers" || acl["role"] != "READER" || retention["mode"] != "Locked" || retention["retainUntilTime"] != until.Format(time.RFC3339Nano) {
			t.Fatalf("fixture metadata changed: %#v", attrs)
		}
	}
	object.ACL = []storage.ACLRule{}
	body, contentType, err := fixtureBody(object.StreamingObject(), runtimeFixture)
	if err != nil {
		t.Fatal(err)
	}
	_, params, _ := mime.ParseMediaType(contentType)
	part, err := multipart.NewReader(body, params["boundary"]).NextPart()
	if err != nil {
		t.Fatal(err)
	}
	var attrs map[string]any
	if err := json.NewDecoder(part).Decode(&attrs); err != nil {
		t.Fatal(err)
	}
	if rules, ok := attrs["acl"].([]any); !ok || len(rules) != 0 {
		t.Fatalf("empty ACL was omitted: %#v", attrs)
	}
}

func TestFixtureMetadataIsBounded(t *testing.T) {
	object := Object{ObjectAttrs: ObjectAttrs{Metadata: map[string]string{"large": strings.Repeat("x", 64<<10)}}}
	if _, _, err := fixtureBody(object.StreamingObject(), runtimeFixture); err == nil {
		t.Fatal("oversized metadata was accepted")
	}
}

func TestOnlyInitialFixturesSuppressNotifications(t *testing.T) {
	object := Object{ObjectAttrs: ObjectAttrs{BucketName: "bucket", Name: "object"}, Content: []byte("body")}
	for _, mode := range []fixtureMode{runtimeFixture, initialFixture} {
		body, contentType, err := fixtureBody(object.StreamingObject(), mode)
		if err != nil {
			t.Fatal(err)
		}
		_, params, err := mime.ParseMediaType(contentType)
		if err != nil {
			t.Fatal(err)
		}
		part, err := multipart.NewReader(body, params["boundary"]).NextPart()
		if err != nil {
			t.Fatal(err)
		}
		var attrs map[string]any
		if err := json.NewDecoder(part).Decode(&attrs); err != nil {
			t.Fatal(err)
		}
		value, present := attrs["notificationMode"]
		if mode == initialFixture && value != "suppress" || mode == runtimeFixture && present {
			t.Fatalf("mode %v emitted unexpected metadata: %#v", mode, attrs)
		}
	}
}
