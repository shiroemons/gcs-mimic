//go:build integration

package fakestorage

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"strconv"
	"strings"
	"testing"
	"time"

	"google.golang.org/api/googleapi"
)

func TestRustRuntimeFixtureSDKHandlerAndParallelIsolation(t *testing.T) {
	if os.Getenv("GCS_MIMIC_BINARY") == "" {
		t.Fatal("integration requires GCS_MIMIC_BINARY")
	}
	for _, noListener := range []bool{false, true} {
		t.Run(map[bool]string{false: "listener", true: "facade"}[noListener], func(t *testing.T) {
			t.Parallel()
			created := time.Date(2020, 1, 2, 3, 4, 5, 123456000, time.UTC)
			fixture := Object{ObjectAttrs: ObjectAttrs{BucketName: "fixture-bucket", Name: "dir/日本語 +%?.txt", Generation: 420000,
				Created: created, Updated: created, ContentType: "text/plain", Metadata: map[string]string{"fixture": "initial"}}, Content: []byte("Go fixture bytes")}
			s, err := NewServerWithOptions(Options{Scheme: "http", NoListener: noListener, InitialObjects: []Object{fixture}, Writer: os.Stderr})
			if err != nil {
				t.Fatal(err)
			}
			t.Cleanup(func() {
				if err := s.Close(); err != nil {
					t.Error(err)
				}
			})
			object, err := s.GetObjectWithGeneration(fixture.BucketName, fixture.Name, fixture.Generation)
			if err != nil {
				t.Fatal(err)
			}
			if string(object.Content) != string(fixture.Content) || object.Generation != fixture.Generation ||
				!object.Created.Equal(created) || object.Metadata["fixture"] != "initial" || object.Md5Hash == "" || object.Crc32c == "" {
				t.Fatalf("fixture identity mismatch: %+v", object)
			}
			fixture.Content = []byte("collision must not replace")
			err = s.CreateObjectStreaming(fixture.StreamingObject())
			var api *googleapi.Error
			if !errors.As(err, &api) || api.Code != 409 {
				t.Fatalf("same-generation collision: %v", err)
			}
			client := s.Client()
			defer client.Close()
			ctx := context.Background()
			handle := client.Bucket(fixture.BucketName).Object("sdk-resumable.txt")
			writer := handle.NewWriter(ctx)
			writer.ChunkSize = 256 << 10
			if _, err := writer.Write([]byte("SDK bytes")); err != nil {
				t.Fatal(err)
			}
			if err := writer.Close(); err != nil {
				t.Fatal(err)
			}
			if writer.Attrs().Generation <= fixture.Generation {
				t.Fatal("fixture generation high-water mark was not advanced")
			}
			page, err := s.ListObjectsWithOptionsPaginated(fixture.BucketName, ListOptions{MaxResults: 1})
			if err != nil || len(page.Objects) != 1 || page.NextPageToken == "" {
				t.Fatalf("first page: %+v %v", page, err)
			}
			page, err = s.ListObjectsWithOptionsPaginated(fixture.BucketName, ListOptions{MaxResults: 1, PageToken: page.NextPageToken})
			if err != nil || len(page.Objects) != 1 || page.NextPageToken != "" {
				t.Fatalf("last page: %+v %v", page, err)
			}
			proxy := httptest.NewServer(s.HTTPHandler())
			defer proxy.Close()
			request, err := http.NewRequest(http.MethodGet, proxy.URL+"/fixture-bucket/sdk-resumable.txt", nil)
			if err != nil {
				t.Fatal(err)
			}
			request.Host = strings.TrimPrefix(s.PublicURL(), "http://")
			response, err := http.DefaultClient.Do(request)
			if err != nil {
				t.Fatal(err)
			}
			data, readErr := io.ReadAll(response.Body)
			if closeErr := response.Body.Close(); readErr != nil || closeErr != nil || string(data) != "SDK bytes" {
				t.Fatalf("HTTPHandler: %q %v %v", data, readErr, closeErr)
			}
		})
	}
}

func TestRustRuntimeEventOptionsKeepInitialFixturesQuietAndPublishAllSelectedEvents(t *testing.T) {
	if os.Getenv("GCS_MIMIC_BINARY") == "" {
		t.Fatal("integration requires GCS_MIMIC_BINARY")
	}
	messages := make(chan map[string]string, 16)
	publisher := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		var payload struct {
			Messages []struct {
				Attributes map[string]string `json:"attributes"`
			} `json:"messages"`
		}
		if err := json.NewDecoder(io.LimitReader(r.Body, (128<<10)+1)).Decode(&payload); err != nil {
			t.Error(err)
			http.Error(w, "invalid publish", 400)
			return
		}
		if r.URL.Path != "/v1/projects/local-project/topics/events:publish" {
			t.Errorf("wrong topic: %s", r.URL.Path)
		}
		for _, message := range payload.Messages {
			messages <- message.Attributes
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = io.WriteString(w, `{"messageIds":["local-message"]}`)
	}))
	defer publisher.Close()
	s, err := NewServerWithOptions(Options{
		NoListener: true, Scheme: "http", PubSubEmulatorHost: publisher.URL,
		EventOptions:   EventManagerOptions{ProjectID: "local-project", TopicName: "events", Bucket: "event-bucket", ObjectPrefix: "prefix/", NotifyOn: EventNotificationOptions{Finalize: true, Delete: true, MetadataUpdate: true, Archive: true}},
		InitialObjects: []Object{{ObjectAttrs: ObjectAttrs{BucketName: "event-bucket", Name: "prefix/initial"}, Content: []byte("initial")}},
	})
	if err != nil {
		t.Fatal(err)
	}
	defer func() {
		if err := s.Close(); err != nil {
			t.Error(err)
		}
	}()
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	request := func(method, path, body string, want int) {
		r, err := http.NewRequestWithContext(ctx, method, s.endpoint.String()+path, strings.NewReader(body))
		if err != nil {
			t.Fatal(err)
		}
		r.Header.Set("Content-Type", "application/json")
		response, err := s.HTTPClient().Do(r)
		if err != nil {
			t.Fatal(err)
		}
		data, readErr := io.ReadAll(io.LimitReader(response.Body, (64<<10)+1))
		closeErr := response.Body.Close()
		if readErr != nil || closeErr != nil || response.StatusCode != want {
			t.Fatalf("%s %s: %d %s %v %v", method, path, response.StatusCode, data, readErr, closeErr)
		}
	}
	request(http.MethodPatch, "/storage/v1/b/event-bucket", `{"versioning":{"enabled":true}}`, 200)
	fixture := Object{ObjectAttrs: ObjectAttrs{BucketName: "event-bucket", Name: "prefix/value"}, Content: []byte("first")}
	if err := s.CreateObjectStreaming(fixture.StreamingObject()); err != nil {
		t.Fatal(err)
	}
	first, err := s.GetObject("event-bucket", "prefix/value")
	if err != nil {
		t.Fatal(err)
	}
	fixture.Content = []byte("second")
	if err := s.CreateObjectStreaming(fixture.StreamingObject()); err != nil {
		t.Fatal(err)
	}
	request(http.MethodPatch, "/storage/v1/b/event-bucket/o/prefix%2Fvalue", `{"metadata":{"changed":"yes"}}`, 200)
	request(http.MethodDelete, "/storage/v1/b/event-bucket/o/prefix%2Fvalue?generation="+strconv.FormatInt(first.Generation, 10), "", 204)
	request(http.MethodPost, "/upload/storage/v1/b/event-bucket/o?uploadType=media&name=prefix%2Fvalue&ifGenerationMatch=0", "rejected", 412)
	for _, ignored := range []Object{{ObjectAttrs: ObjectAttrs{BucketName: "event-bucket", Name: "outside-prefix"}, Content: []byte("ignored")}, {ObjectAttrs: ObjectAttrs{BucketName: "other-bucket", Name: "prefix/value"}, Content: []byte("ignored")}} {
		if err := s.CreateObjectStreaming(ignored.StreamingObject()); err != nil {
			t.Fatal(err)
		}
	}
	counts := map[string]int{}
	deadline := time.NewTimer(5 * time.Second)
	defer deadline.Stop()
	for range 5 {
		select {
		case attributes := <-messages:
			if attributes["bucketId"] != "event-bucket" || attributes["objectId"] != "prefix/value" {
				t.Fatalf("initial or filtered object emitted a message: %+v", attributes)
			}
			counts[attributes["eventType"]]++
		case <-deadline.C:
			t.Fatalf("missing selected events: %v", counts)
		}
	}
	for event, want := range map[string]int{"OBJECT_FINALIZE": 2, "OBJECT_ARCHIVE": 1, "OBJECT_METADATA_UPDATE": 1, "OBJECT_DELETE": 1} {
		if counts[event] != want {
			t.Fatalf("event delivery: %v", counts)
		}
	}
	select {
	case extra := <-messages:
		t.Fatalf("unexpected initial, filtered or failed-write event: %+v", extra)
	case <-time.After(100 * time.Millisecond):
	}
}
