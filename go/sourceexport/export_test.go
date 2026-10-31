package sourceexport

import (
	"bufio"
	"bytes"
	"context"
	"crypto/md5"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/fsouza/fake-gcs-server/fakestorage"
)

func sourceFixture(t *testing.T) *fakestorage.Server {
	t.Helper()
	source, err := fakestorage.NewServerWithOptions(fakestorage.Options{NoListener: true, Scheme: "http"})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(source.Stop)
	source.CreateBucketWithOpts(fakestorage.CreateBucketOpts{Name: "fixture-bucket", VersioningEnabled: true})
	source.CreateBucketWithOpts(fakestorage.CreateBucketOpts{Name: "empty-bucket"})
	for _, object := range []fakestorage.Object{
		{ObjectAttrs: fakestorage.ObjectAttrs{BucketName: "fixture-bucket", Name: "a//../日本語/", Generation: 900, ContentType: "text/plain", StorageClass: "NEARLINE", Metadata: map[string]string{"source": "archived"}}, Content: []byte("old body")},
		{ObjectAttrs: fakestorage.ObjectAttrs{BucketName: "fixture-bucket", Name: "a//../日本語/", Generation: 7, ContentType: "text/plain", Metadata: map[string]string{"source": "live"}}, Content: []byte("new body")},
		{ObjectAttrs: fakestorage.ObjectAttrs{BucketName: "fixture-bucket", Name: "archive-only", Generation: 11}, Content: []byte("retired body")},
	} {
		object.Created = time.Date(2020, 1, 2, 3, 4, 5, 123456000, time.UTC)
		object.Updated = object.Created
		object.CustomTime = object.Created
		source.CreateObject(object)
	}
	if err := source.Backend().DeleteObject("fixture-bucket", "archive-only"); err != nil {
		t.Fatal(err)
	}
	return source
}

func TestExportRealSourceHistoryAndExactMetadata(t *testing.T) {
	source := sourceFixture(t)
	destination := filepath.Join(t.TempDir(), "snapshot")
	if output := os.Getenv("GCS_MIMIC_SOURCE_FIXTURE_DIR"); output != "" {
		destination = output
	}
	var barrier sync.RWMutex
	if err := Export(context.Background(), source, &barrier, destination); err != nil {
		t.Fatal(err)
	}
	file, err := os.Open(filepath.Join(destination, "snapshot.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	defer file.Close()
	scanner := bufio.NewScanner(file)
	scanner.Buffer(make([]byte, 4096), 128<<10)
	hash := sha256.New()
	var memberships []string
	var names []string
	var deleted []string
	var bodies []string
	rows := 0
	for scanner.Scan() {
		line := scanner.Bytes()
		var row struct {
			Kind, SourceRevision, Name, Membership, Blob, Created string
			Attributes                                            json.RawMessage
			Buckets, Generations, Bytes                           uint64
			ManifestSHA256                                        string `json:"manifest_sha256"`
		}
		if err := json.Unmarshal(line, &row); err != nil {
			t.Fatal(err)
		}
		if row.Kind == "complete" {
			if row.ManifestSHA256 != hex.EncodeToString(hash.Sum(nil)) || row.Buckets != 2 || row.Generations != 3 || row.Bytes != 28 {
				t.Fatalf("bad trailer: %s", line)
			}
			continue
		}
		_, _ = hash.Write(append(append([]byte(nil), line...), '\n'))
		rows++
		if row.Kind == "bucket" {
			bucket, err := source.Backend().GetBucket(row.Name)
			if err != nil || row.Created != bucket.TimeCreated.Format(time.RFC3339Nano) {
				t.Fatalf("bucket precision changed: %s %v", line, err)
			}
		}
		if row.Kind != "object" {
			continue
		}
		var attrs struct {
			Generation            int64
			Deleted, StorageClass string
		}
		if err := json.Unmarshal(row.Attributes, &attrs); err != nil {
			t.Fatal(err)
		}
		selected, err := source.Backend().GetObjectWithGeneration("fixture-bucket", row.Name, attrs.Generation)
		if err != nil {
			t.Fatal(err)
		}
		original, err := json.Marshal(selected.ObjectAttrs)
		if err != nil {
			t.Fatal(err)
		}
		if !bytes.Equal(original, row.Attributes) {
			t.Fatalf("metadata changed: %s != %s", original, row.Attributes)
		}
		body, err := os.ReadFile(filepath.Join(destination, "blobs", row.Blob))
		if err != nil {
			t.Fatal(err)
		}
		memberships = append(memberships, row.Membership)
		names = append(names, row.Name)
		deleted = append(deleted, attrs.Deleted)
		bodies = append(bodies, string(body))
		_ = selected.Close()
	}
	if err := scanner.Err(); err != nil {
		t.Fatal(err)
	}
	if rows != 6 || strings.Join(memberships, ",") != "archived,live,archived" || strings.Join(bodies, ",") != "old body,new body,retired body" || names[0] != "a//../日本語/" || deleted[0] == "" || deleted[2] == "" {
		t.Fatalf("lost history: %v %v %v %v", memberships, names, deleted, bodies)
	}
	if len(deleted[0]) == 0 {
		t.Fatal("missing exact archive timestamp")
	}
}

type pausedContext struct {
	context.Context
	entered, resume chan struct{}
	once            sync.Once
}

func (c *pausedContext) Err() error {
	c.once.Do(func() { close(c.entered); <-c.resume })
	return c.Context.Err()
}

func TestExportBarrierBlocksHTTPAndDirectWrites(t *testing.T) {
	source := sourceFixture(t)
	var barrier sync.RWMutex
	ctx := &pausedContext{Context: context.Background(), entered: make(chan struct{}), resume: make(chan struct{})}
	exported := make(chan error, 1)
	destination := filepath.Join(t.TempDir(), "snapshot")
	go func() { exported <- Export(ctx, source, &barrier, destination) }()
	<-ctx.entered
	direct, handler, transport := make(chan struct{}), make(chan struct{}), make(chan struct{})
	go func() {
		barrier.RLock()
		defer barrier.RUnlock()
		source.CreateObject(fakestorage.Object{ObjectAttrs: fakestorage.ObjectAttrs{BucketName: "fixture-bucket", Name: "a//../日本語/", Generation: 42}, Content: []byte("after export")})
		close(direct)
	}()
	go func() {
		writer := httptest.NewRecorder()
		Handler(&barrier, source.HTTPHandler()).ServeHTTP(writer, httptest.NewRequest(http.MethodPost, "http://storage.googleapis.com/storage/v1/b?project=local", strings.NewReader(`{"name":"handler-after-export"}`)))
		if writer.Code != 200 {
			t.Errorf("HTTP write: %d %s", writer.Code, writer.Body.String())
		}
		close(handler)
	}()
	go func() {
		response, err := Transport(&barrier, source.HTTPClient().Transport).RoundTrip(httptest.NewRequest(http.MethodPost, "http://storage.googleapis.com/storage/v1/b?project=local", strings.NewReader(`{"name":"transport-after-export"}`)))
		if err != nil {
			t.Error(err)
		}
		if response != nil {
			if response.StatusCode != 200 {
				t.Errorf("transport write: %d", response.StatusCode)
			}
			_ = response.Body.Close()
		}
		close(transport)
	}()
	for _, done := range []chan struct{}{direct, handler, transport} {
		select {
		case <-done:
			t.Fatal("mutation crossed the exclusive capture barrier")
		case <-time.After(20 * time.Millisecond):
		}
	}
	close(ctx.resume)
	if err := <-exported; err != nil {
		t.Fatal(err)
	}
	for _, done := range []chan struct{}{direct, handler, transport} {
		select {
		case <-done:
		case <-time.After(time.Second):
			t.Fatal("barrier was not released")
		}
	}
	manifest, err := os.ReadFile(filepath.Join(destination, "snapshot.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	if bytes.Contains(manifest, []byte("after-export")) || bytes.Contains(manifest, []byte(`"Generation":42`)) {
		t.Fatal("a write entered the frozen inventory")
	}
	body, err := os.ReadFile(filepath.Join(destination, "blobs", "0000000000000002"))
	if err != nil || string(body) != "new body" {
		t.Fatalf("selected live bytes changed during capture: %q %v", body, err)
	}
}

func TestExportFailureKeepsSourceAndDoesNotPublish(t *testing.T) {
	source := sourceFixture(t)
	var barrier sync.RWMutex
	root := t.TempDir()
	destination := filepath.Join(root, "snapshot")
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	if err := Export(ctx, source, &barrier, destination); !errors.Is(err, context.Canceled) {
		t.Fatalf("cancel: %v", err)
	}
	if _, err := os.Lstat(destination); !errors.Is(err, os.ErrNotExist) {
		t.Fatal("failed capture was published")
	}
	if err := os.Mkdir(destination, 0700); err != nil {
		t.Fatal(err)
	}
	if err := Export(context.Background(), source, &barrier, destination); err == nil {
		t.Fatal("existing destination accepted")
	}
	stage := filepath.Join(root, "stage")
	if err := os.Mkdir(stage, 0700); err != nil {
		t.Fatal(err)
	}
	if err := publish(stage, destination); err == nil {
		t.Fatal("atomic publication replaced an existing empty directory")
	}
	if _, err := source.Backend().GetObjectWithGeneration("fixture-bucket", "a//../日本語/", 900); err != nil {
		t.Fatal(err)
	}
}

type failedBlob struct {
	write, sync error
	closed      bool
}

func (f *failedBlob) Write(data []byte) (int, error) {
	if f.write != nil {
		return 0, f.write
	}
	return len(data), nil
}
func (f *failedBlob) Sync() error  { return f.sync }
func (f *failedBlob) Close() error { f.closed = true; return nil }

func TestBodyWriteAndSyncFailuresNeverPublishManifestRow(t *testing.T) {
	checksum := md5.Sum([]byte("hello"))
	for _, failure := range []string{"write", "sync", "checksum", "short"} {
		t.Run(failure, func(t *testing.T) {
			fault := errors.New("injected " + failure)
			file := &failedBlob{}
			if failure == "write" {
				file.write = fault
			}
			if failure == "sync" {
				file.sync = fault
			}
			var manifest bytes.Buffer
			c := capture{ctx: context.Background(), hash: sha256.New(), manifest: &manifest, openBlob: func(string) (durableBlob, error) { return file, nil }}
			body := "hello"
			if failure == "short" {
				body = "hell"
			}
			md5sum := base64.StdEncoding.EncodeToString(checksum[:])
			if failure == "checksum" {
				md5sum = "invalid"
			}
			err := c.object("bucket", "name", "live", 5, json.RawMessage(`{}`), strings.NewReader(body), md5sum, "mnG7TA==")
			if err == nil || !file.closed || manifest.Len() != 0 || c.objects != 0 {
				t.Fatalf("failure published state: %v %+v %s", err, c, manifest.Bytes())
			}
			if (failure == "write" || failure == "sync") && !errors.Is(err, fault) {
				t.Fatal(err)
			}
		})
	}
}

func TestPublishedCaptureSurvivesFinalParentSyncFailureAndRetry(t *testing.T) {
	root := t.TempDir()
	stage, destination := filepath.Join(root, "stage"), filepath.Join(root, "snapshot")
	if err := os.Mkdir(stage, 0700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(stage, "snapshot.jsonl"), []byte("complete capture"), 0600); err != nil {
		t.Fatal(err)
	}
	fault := errors.New("injected parent sync failure")
	err := publishSnapshot(stage, destination, func(string) error { return fault })
	var published *PublishedError
	if !errors.As(err, &published) || !errors.Is(err, fault) || published.Destination != destination {
		t.Fatalf("ambiguous publication outcome: %v", err)
	}
	if data, err := os.ReadFile(filepath.Join(destination, "snapshot.jsonl")); err != nil || string(data) != "complete capture" {
		t.Fatalf("published data removed: %q %v", data, err)
	}
	if err := os.Mkdir(stage, 0700); err != nil {
		t.Fatal(err)
	}
	if err := publishSnapshot(stage, destination, syncDirectory); err == nil {
		t.Fatal("retry overwrote complete destination")
	}
	if data, err := os.ReadFile(filepath.Join(destination, "snapshot.jsonl")); err != nil || string(data) != "complete capture" {
		t.Fatalf("retry changed published data: %q %v", data, err)
	}
}

func TestInvalidUTF8CannotBeSilentlyReplacedByJSON(t *testing.T) {
	for _, value := range []any{map[string]string{"header": string([]byte{255})}, map[string]string{string([]byte{255}): "metadata"}} {
		if _, err := attributes(value); err == nil {
			t.Fatal("invalid source text was replaced instead of rejected")
		}
	}
	source := sourceFixture(t)
	source.CreateObject(fakestorage.Object{ObjectAttrs: fakestorage.ObjectAttrs{BucketName: "fixture-bucket", Name: string([]byte{255})}, Content: []byte("bad name")})
	var barrier sync.RWMutex
	destination := filepath.Join(t.TempDir(), "snapshot")
	if err := Export(context.Background(), source, &barrier, destination); err == nil {
		t.Fatal("invalid source name published")
	}
	if _, err := os.Lstat(destination); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("failed capture was visible: %v", err)
	}
}

func TestAttributeBoundsRejectLargeStringsAndEmptyEntryArraysBeforeEncoding(t *testing.T) {
	if _, err := attributes(map[string]string{"value": strings.Repeat("x", 60<<10)}); err != nil {
		t.Fatalf("valid bounded metadata rejected: %v", err)
	}
	for _, value := range []any{map[string]string{"value": strings.Repeat("x", 256<<10)}, map[string][]string{"entries": make([]string, 70_000)}} {
		if _, err := attributes(value); err == nil || !strings.Contains(err.Error(), "64 KiB") {
			t.Fatalf("oversized attributes accepted: %v", err)
		}
	}
}

func TestInterruptedBodyVerificationRemovesOwnedStagingAndKeepsSource(t *testing.T) {
	source := sourceFixture(t)
	source.CreateObject(fakestorage.Object{ObjectAttrs: fakestorage.ObjectAttrs{BucketName: "fixture-bucket", Name: "bad-checksum", Generation: 123}, Content: []byte("unchanged source")})
	selected, err := source.Backend().GetObjectWithGeneration("fixture-bucket", "bad-checksum", 123)
	if err != nil {
		t.Fatal(err)
	}
	attrs := selected.ObjectAttrs
	_ = selected.Close()
	attrs.Md5Hash = "deliberately wrong source checksum"
	corrupt, err := source.Backend().PatchObject("fixture-bucket", "bad-checksum", attrs)
	if err != nil {
		t.Fatal(err)
	}
	_ = corrupt.Close()
	root := t.TempDir()
	var barrier sync.RWMutex
	if err := Export(context.Background(), source, &barrier, filepath.Join(root, "snapshot")); err == nil {
		t.Fatal("corrupt source body was accepted")
	}
	entries, err := os.ReadDir(root)
	if err != nil || len(entries) != 0 {
		t.Fatalf("incomplete capture escaped cleanup: %v %v", entries, err)
	}
	object, err := source.GetObjectWithGeneration("fixture-bucket", "bad-checksum", 123)
	if err != nil || string(object.Content) != "unchanged source" || object.Md5Hash != "deliberately wrong source checksum" {
		t.Fatalf("export changed source state: %+v %v", object, err)
	}
}
