package gcsmimic

import (
	"context"
	"encoding/json"
	"io"
	"io/fs"
	"maps"
	"mime"
	"mime/multipart"
	"net/http"
	"os"
	"path/filepath"
	"reflect"
	"slices"
	"strings"
	"sync"
	"testing"
	"testing/fstest"
)

// fixtureRequest is one POST /_internal/objects that a test server received.
type fixtureRequest struct {
	metadata map[string]any
	body     string
}

// seedBackend answers bucket creation, refusing the names in existing with 409, and records
// every bucket creation and fixture write.
type seedBackend struct {
	t        *testing.T
	existing []string
	mu       sync.Mutex
	buckets  []string
	fixtures []fixtureRequest
	requests int
}

func (b *seedBackend) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	b.mu.Lock()
	defer b.mu.Unlock()
	b.requests++
	switch {
	case r.Method == http.MethodPost && r.URL.Path == "/storage/v1/b":
		var bucket struct{ Name string }
		if err := json.NewDecoder(r.Body).Decode(&bucket); err != nil {
			b.t.Error(err)
		}
		b.buckets = append(b.buckets, bucket.Name)
		if slices.Contains(b.existing, bucket.Name) {
			writeJSON(w, http.StatusConflict, apiError(http.StatusConflict, "conflict"))
			return
		}
		writeJSON(w, http.StatusOK, `{"name":"`+bucket.Name+`"}`)
	case r.Method == http.MethodPost && r.URL.Path == "/_internal/objects" && r.URL.RawQuery == "":
		b.fixtures = append(b.fixtures, b.readFixture(r))
		writeJSON(w, http.StatusOK, `{"kind":"storage#object"}`)
	default:
		b.t.Errorf("unexpected request %s %s", r.Method, r.URL)
		http.NotFound(w, r)
	}
}

func (b *seedBackend) readFixture(r *http.Request) fixtureRequest {
	mediaType, params, err := mime.ParseMediaType(r.Header.Get("Content-Type"))
	if err != nil || mediaType != "multipart/related" {
		b.t.Errorf("fixture Content-Type %q: %v", r.Header.Get("Content-Type"), err)
		return fixtureRequest{}
	}
	reader := multipart.NewReader(r.Body, params["boundary"])
	var fixture fixtureRequest
	part, err := reader.NextPart()
	if err == nil {
		err = json.NewDecoder(part).Decode(&fixture.metadata)
	}
	if err == nil {
		part, err = reader.NextPart()
	}
	if err == nil {
		var body []byte
		body, err = io.ReadAll(part)
		fixture.body = string(body)
	}
	if err == nil {
		if _, err = reader.NextPart(); err == io.EOF {
			err = nil
		}
	}
	if err != nil {
		b.t.Errorf("fixture framing: %v", err)
	}
	return fixture
}

func writeTree(t *testing.T, root string, files map[string]string) {
	t.Helper()
	for name, content := range files {
		path := filepath.Join(root, filepath.FromSlash(name))
		if strings.HasSuffix(name, "/") {
			if err := os.MkdirAll(path, 0o755); err != nil {
				t.Fatal(err)
			}
			continue
		}
		if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
			t.Fatal(err)
		}
	}
}

func TestSeedSendsTreesWithSeedDefaultsAndObjectsAsWritten(t *testing.T) {
	backend := &seedBackend{t: t, existing: []string{"code"}}
	s := newTestServer(t, backend.ServeHTTP)
	dir := t.TempDir()
	writeTree(t, dir, map[string]string{
		"photos/a.txt": "alpha", "photos/nested/deep/c.json": "{}", "photos/.DS_Store": "os", "photos/._a.txt": "os",
		"photos/empty/": "", "empty-bucket/": "", "README.md": "top-level files are skipped",
	})
	tree := fstest.MapFS{"site/index.html": {Data: []byte("<p>")}, ".DS_Store": {Data: []byte("os")}}
	flat := fstest.MapFS{"top.txt": {Data: []byte("top")}, "sub/deep.bin": {Data: []byte{0, 1}},
		"sub/._top.txt": {Data: []byte("os")}, "sub/empty": {Mode: fs.ModeDir}}
	var logs []string
	logf := func(format string, args ...any) { logs = append(logs, format) }
	err := s.seed(context.Background(), logf, []SeedItem{
		Dir(dir), FS(tree), FS(flat, Into("into-bucket")),
		Object{Bucket: "code", Name: "table.csv", Content: []byte("a,b"), ContentType: "text/csv"},
		Object{Bucket: "code", Name: "untyped", Content: []byte("raw")},
	})
	if err != nil {
		t.Fatal(err)
	}
	backend.mu.Lock()
	defer backend.mu.Unlock()
	// A top-level directory creates its bucket even when empty, as the startup seed does.
	if want := []string{"empty-bucket", "photos", "site", "into-bucket", "code"}; !slices.Equal(backend.buckets, want) {
		t.Errorf("created buckets %q, want %q", backend.buckets, want)
	}
	tree1 := map[string]any{"defaults": "seed", "notificationMode": "suppress"}
	want := []struct {
		metadata map[string]any
		body     string
	}{
		{withName(tree1, "photos", "a.txt"), "alpha"},
		{withName(tree1, "photos", "nested/deep/c.json"), "{}"},
		{withName(tree1, "site", "index.html"), "<p>"},
		{withName(tree1, "into-bucket", "sub/deep.bin"), "\x00\x01"},
		{withName(tree1, "into-bucket", "top.txt"), "top"},
		{map[string]any{"bucket": "code", "name": "table.csv", "contentType": "text/csv", "notificationMode": "suppress"}, "a,b"},
		{map[string]any{"bucket": "code", "name": "untyped", "notificationMode": "suppress"}, "raw"},
	}
	if len(backend.fixtures) != len(want) {
		t.Fatalf("wrote %d fixtures, want %d: %+v", len(backend.fixtures), len(want), backend.fixtures)
	}
	for i, fixture := range backend.fixtures {
		if !reflect.DeepEqual(fixture.metadata, want[i].metadata) || fixture.body != want[i].body {
			t.Errorf("fixture %d = %v %q, want %v %q", i, fixture.metadata, fixture.body, want[i].metadata, want[i].body)
		}
	}
	if len(logs) != 1 || !strings.Contains(logs[0], "top-level entries must be bucket directories") {
		t.Errorf("logs = %q", logs)
	}
}

func withName(base map[string]any, bucket, name string) map[string]any {
	metadata := maps.Clone(base)
	metadata["bucket"], metadata["name"] = bucket, name
	return metadata
}

func TestSeedRefusesWhatTheStartupSeedRefusesBeforeWriting(t *testing.T) {
	backend := &seedBackend{t: t}
	s := newTestServer(t, backend.ServeHTTP)
	ignore := func(string, ...any) {}
	link := t.TempDir()
	writeTree(t, link, map[string]string{"bucket/first.txt": "first", "bucket/target.txt": "target"})
	symlinkErr := os.Symlink("target.txt", filepath.Join(link, "bucket", "link.txt"))
	file := filepath.Join(t.TempDir(), "file")
	writeTree(t, filepath.Dir(file), map[string]string{"file": "not a directory"})
	type refusal struct {
		item SeedItem
		want string
	}
	cases := map[string]refusal{
		"a symbolic link in an FS": {FS(fstest.MapFS{"bucket/first.txt": {Data: []byte("first")},
			"bucket/link": {Data: []byte("first.txt"), Mode: fs.ModeSymlink}}), "bucket/link: a symbolic link"},
		"a named pipe": {FS(fstest.MapFS{"bucket/a": {Data: []byte("a")}, "bucket/pipe": {Mode: fs.ModeNamedPipe}}),
			"a fixture must be a regular file"},
		"a name that is not UTF-8": {FS(fstest.MapFS{"bucket/a": {Data: []byte("a")}, "bucket/\xff": {Data: []byte("x")}}),
			"must be UTF-8"},
		"a missing directory":      {Dir(filepath.Join(t.TempDir(), "missing")), "seed directory"},
		"a file as the directory":  {Dir(file), "must be a directory"},
		"an object without a name": {Object{Bucket: "bucket"}, "needs a Bucket and a Name"},
	}
	if symlinkErr == nil {
		cases["a symbolic link in a directory"] = refusal{Dir(link), "bucket/link.txt: a symbolic link"}
	} else {
		t.Logf("symbolic links unavailable, so only the fs.FS link case runs: %v", symlinkErr)
	}
	for name, tc := range cases {
		if err := s.seed(context.Background(), ignore, []SeedItem{tc.item}); err == nil || !strings.Contains(err.Error(), tc.want) {
			t.Errorf("%s: want an error naming %q, got %v", name, tc.want, err)
		}
	}
	backend.mu.Lock()
	defer backend.mu.Unlock()
	if backend.requests != 0 {
		t.Fatalf("a refused tree still sent %d requests: buckets %q, fixtures %d", backend.requests, backend.buckets, len(backend.fixtures))
	}
}

func TestSeedFailsTheTestOnAWriteError(t *testing.T) {
	s := newTestServer(t, func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path == "/storage/v1/b" {
			writeJSON(w, http.StatusOK, `{"name":"bucket"}`)
			return
		}
		writeJSON(w, http.StatusBadRequest, apiError(http.StatusBadRequest, "invalid"))
	})
	tb := &fatalRecorder{TB: t}
	tb.run(func() { s.Seed(tb, Object{Bucket: "bucket", Name: "object"}) })
	if !strings.Contains(tb.message, "write bucket/object") || !strings.Contains(tb.message, "400") {
		t.Fatalf("message = %q", tb.message)
	}
}
