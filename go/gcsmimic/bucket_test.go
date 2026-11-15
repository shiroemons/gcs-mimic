package gcsmimic

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"runtime"
	"strings"
	"sync"
	"testing"
	"time"

	"cloud.google.com/go/storage"
	"google.golang.org/api/googleapi"
)

// validBucketName applies Cloud Storage's rules for a name without dots, as gcs-mimic's
// src/bucket/name.rs does.
func validBucketName(name string) bool {
	alphanumeric := func(b byte) bool { return b >= 'a' && b <= 'z' || b >= '0' && b <= '9' }
	if len(name) < 3 || len(name) > 63 || !alphanumeric(name[0]) || !alphanumeric(name[len(name)-1]) ||
		strings.HasPrefix(name, "goog") || strings.Contains(strings.ReplaceAll(name, "0", "o"), "google") {
		return false
	}
	for i := range len(name) {
		if !alphanumeric(name[i]) && name[i] != '-' && name[i] != '_' {
			return false
		}
	}
	return true
}

func TestTempBucketNamesAreValidUniqueAndNameTheTest(t *testing.T) {
	testNames := []string{
		"TestUpload", "TestUpload/case_#01", "TestGoogleStorage/G00GLE-goOgle", "Test日本語/ünïcode",
		"___", strings.Repeat("TestAVeryLongNameThatKeepsGoing", 5), "Test/-/-trailing-",
	}
	names := make(chan string, 100*len(testNames))
	var wg sync.WaitGroup
	for range 100 {
		wg.Go(func() {
			for _, testName := range testNames {
				names <- tempBucketName(testName)
			}
		})
	}
	wg.Wait()
	close(names)
	seen := map[string]bool{}
	for name := range names {
		if !validBucketName(name) || !strings.HasPrefix(name, "tmp-") || !strings.Contains(name, "-"+runID+"-") {
			t.Errorf("invalid temporary bucket name %q", name)
		}
		if seen[name] {
			t.Errorf("duplicate name %q", name)
		}
		seen[name] = true
	}
	for testName, label := range map[string]string{
		"TestUpload/case_#01":  "tmp-testupload-case-01-",
		"Test日本語/ünïcode":      "tmp-test-n-code-",
		"___":                  "tmp-test-",
		"TestGoogle/g00gle_Go": "tmp-testgoo-gle-g00-gle-go-",
	} {
		if name := tempBucketName(testName); !strings.HasPrefix(name, label) {
			t.Errorf("tempBucketName(%q) = %q, want the prefix %q", testName, name, label)
		}
	}
}

// recorded is one request a test HTTP server received.
type recorded struct {
	method, path, query, contentType string
	body                             []byte
}

func record(t *testing.T, r *http.Request) recorded {
	body, err := io.ReadAll(r.Body)
	if err != nil {
		t.Error(err)
	}
	return recorded{method: r.Method, path: r.URL.Path, query: r.URL.RawQuery, contentType: r.Header.Get("Content-Type"), body: body}
}

func TestTempBucketCreatesThenPurgesAndWaitsOutABusyServer(t *testing.T) {
	var mu sync.Mutex
	var requests []recorded
	purges := 0
	s := newTestServer(t, func(w http.ResponseWriter, r *http.Request) {
		request := record(t, r)
		mu.Lock()
		requests = append(requests, request)
		mu.Unlock()
		switch {
		case r.Method == http.MethodPost && r.URL.Path == "/storage/v1/b":
			var bucket map[string]any
			if err := json.Unmarshal(request.body, &bucket); err != nil {
				t.Error(err)
			}
			writeJSON(w, http.StatusOK, `{"name":"`+bucket["name"].(string)+`"}`)
		case r.Method == http.MethodPatch:
			writeJSON(w, http.StatusOK, `{"name":"`+strings.TrimPrefix(r.URL.Path, "/storage/v1/b/")+`"}`)
		case r.Method == http.MethodDelete && strings.HasPrefix(r.URL.Path, "/_internal/buckets/"):
			mu.Lock()
			purges++
			busy := purges%3 != 0
			mu.Unlock()
			if busy {
				w.Header().Set("Retry-After", "0")
				writeJSON(w, http.StatusServiceUnavailable, apiError(http.StatusServiceUnavailable, "backendError"))
				return
			}
			writeJSON(w, http.StatusOK, `{"objects":3,"cleanupPending":true}`)
		default:
			t.Errorf("unexpected request %s %s", r.Method, r.URL)
			http.NotFound(w, r)
		}
	})
	var plain, held string
	t.Run("plain", func(t *testing.T) { plain = s.TempBucket(t).BucketName() })
	t.Run("held", func(t *testing.T) {
		held = s.TempBucket(t, WithObjectRetention(),
			WithBucketAttrs(&storage.BucketAttrs{VersioningEnabled: true, DefaultEventBasedHold: true})).BucketName()
	})
	mu.Lock()
	defer mu.Unlock()
	var got []string
	for _, request := range requests {
		got = append(got, request.method+" "+request.path)
		if request.method == http.MethodDelete && (request.query != "" || len(request.body) != 0) {
			t.Errorf("purge carried a query %q or a body %q", request.query, request.body)
		}
	}
	want := []string{
		"POST /storage/v1/b",
		"DELETE /_internal/buckets/" + plain, "DELETE /_internal/buckets/" + plain, "DELETE /_internal/buckets/" + plain,
		"POST /storage/v1/b", "PATCH /storage/v1/b/" + held,
		"DELETE /_internal/buckets/" + held, "DELETE /_internal/buckets/" + held, "DELETE /_internal/buckets/" + held,
	}
	if strings.Join(got, "\n") != strings.Join(want, "\n") {
		t.Fatalf("requests:\n%s\nwant:\n%s", strings.Join(got, "\n"), strings.Join(want, "\n"))
	}
	if !strings.Contains(requests[0].query, "project=local-project") || strings.Contains(requests[0].query, "enableObjectRetention") {
		t.Errorf("plain create query %q", requests[0].query)
	}
	create := requests[4]
	var body map[string]any
	if err := json.Unmarshal(create.body, &body); err != nil {
		t.Fatal(err)
	}
	if versioning, _ := body["versioning"].(map[string]any); versioning["enabled"] != true ||
		!strings.Contains(create.query, "enableObjectRetention=true") {
		t.Errorf("held create lost its settings: %s?%s", create.body, create.query)
	}
	if !strings.Contains(string(requests[5].body), `"defaultEventBasedHold":true`) {
		t.Errorf("the default hold was not set: %s", requests[5].body)
	}
}

func TestTempBucketRefusesANameOfItsOwn(t *testing.T) {
	s := newTestServer(t, func(w http.ResponseWriter, r *http.Request) {
		t.Errorf("unexpected request %s %s", r.Method, r.URL)
	})
	tb := &fatalRecorder{TB: t}
	tb.run(func() { s.TempBucket(tb, WithBucketAttrs(&storage.BucketAttrs{Name: "chosen"})) })
	if !strings.Contains(tb.message, "leave BucketAttrs.Name empty") {
		t.Fatalf("message = %q", tb.message)
	}
}

// fatalRecorder records the Fatalf of a helper instead of failing the test. Like testing's
// FailNow, Fatalf ends the goroutine that run starts.
type fatalRecorder struct {
	testing.TB
	message string
}

func (r *fatalRecorder) Fatalf(format string, args ...any) {
	r.message = fmt.Sprintf(format, args...)
	runtime.Goexit()
}

func (r *fatalRecorder) run(helper func()) {
	done := make(chan struct{})
	go func() {
		defer close(done)
		helper()
	}()
	<-done
}

func TestPurgeGivesUpOnAStuckServerAndChecksA404(t *testing.T) {
	var mu sync.Mutex
	attempts := 0
	bucketExists := false
	s := newTestServer(t, func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		defer mu.Unlock()
		switch {
		case r.URL.Path == "/_internal/buckets/busy":
			attempts++
			w.Header().Set("Retry-After", "0")
			writeJSON(w, http.StatusServiceUnavailable, apiError(http.StatusServiceUnavailable, "backendError"))
		case strings.HasPrefix(r.URL.Path, "/_internal/buckets/"):
			writeJSON(w, http.StatusNotFound, apiError(http.StatusNotFound, "notFound"))
		case r.Method == http.MethodGet && bucketExists:
			writeJSON(w, http.StatusOK, `{"name":"gone"}`)
		default:
			writeJSON(w, http.StatusNotFound, apiError(http.StatusNotFound, "notFound"))
		}
	})
	ctx := context.Background()
	err := s.purge(ctx, "busy")
	if err == nil || !strings.Contains(err.Error(), "stayed busy for 5 attempts") || !strings.Contains(err.Error(), "upload") || attempts != 5 {
		t.Fatalf("busy purge after %d attempts: %v", attempts, err)
	}
	if err := s.purge(ctx, "gone"); err != nil {
		t.Fatalf("a bucket the test already deleted: %v", err)
	}
	mu.Lock()
	bucketExists = true
	mu.Unlock()
	if err := s.purge(ctx, "gone"); err == nil || !strings.Contains(err.Error(), "predate") {
		t.Fatalf("a 404 for a bucket that still exists: %v", err)
	}
}

func TestRetryDelayFollowsRetryAfterWithinItsBound(t *testing.T) {
	for value, want := range map[string]time.Duration{"0": 0, "1": time.Second, "30": maxRetryDelay, "": time.Second,
		"soon": time.Second, "-1": time.Second} {
		err := &googleapi.Error{Code: http.StatusServiceUnavailable, Header: http.Header{"Retry-After": {value}}}
		if got := retryDelay(err); got != want {
			t.Errorf("Retry-After %q: %v, want %v", value, got, want)
		}
	}
}
