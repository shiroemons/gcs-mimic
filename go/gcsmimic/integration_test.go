//go:build integration

package gcsmimic

import (
	"context"
	"errors"
	"io"
	"os"
	"path/filepath"
	"slices"
	"sync"
	"testing"
	"testing/fstest"
	"time"

	"cloud.google.com/go/storage"
	"github.com/shiroemons/gcs-mimic/go/internal/mimicproc"
	"google.golang.org/api/googleapi"
	"google.golang.org/api/iterator"
)

func requireBinary(t *testing.T) {
	t.Helper()
	if os.Getenv("GCSM_BINARY") == "" {
		t.Fatal("integration requires GCSM_BINARY")
	}
}

func writeObject(t *testing.T, ctx context.Context, object *storage.ObjectHandle, content string) *storage.ObjectAttrs {
	t.Helper()
	writer := object.NewWriter(ctx)
	if _, err := io.WriteString(writer, content); err != nil {
		t.Fatal(err)
	}
	if err := writer.Close(); err != nil {
		t.Fatal(err)
	}
	return writer.Attrs()
}

func readObject(t *testing.T, ctx context.Context, object *storage.ObjectHandle) string {
	t.Helper()
	reader, err := object.NewReader(ctx)
	if err != nil {
		t.Fatal(err)
	}
	data, err := io.ReadAll(reader)
	if closeErr := reader.Close(); err != nil || closeErr != nil {
		t.Fatal(errors.Join(err, closeErr))
	}
	return string(data)
}

func objectNames(t *testing.T, ctx context.Context, bucket *storage.BucketHandle) []string {
	t.Helper()
	var names []string
	items := bucket.Objects(ctx, nil)
	for {
		attrs, err := items.Next()
		if errors.Is(err, iterator.Done) {
			return names
		}
		if err != nil {
			t.Fatal(err)
		}
		names = append(names, attrs.Name)
	}
}

func TestRustStartWritesReadsAndKeepsServersApart(t *testing.T) {
	requireBinary(t)
	var servers [2]*Server
	var wg sync.WaitGroup
	for i := range servers {
		wg.Go(func() {
			s, err := New(context.Background())
			if err != nil {
				t.Error(err)
				return
			}
			t.Cleanup(func() {
				if err := s.Close(); err != nil {
					t.Error(err)
				}
			})
			servers[i] = s
		})
	}
	wg.Wait()
	if t.Failed() {
		t.FailNow()
	}
	ctx := context.Background()
	first, second := servers[0].Client(t), servers[1].Client(t)
	if servers[0].Version() != mimicproc.ModuleVersion || servers[0].URL() == servers[1].URL() {
		t.Fatalf("versions %q, URLs %q and %q", servers[0].Version(), servers[0].URL(), servers[1].URL())
	}
	if err := first.Bucket("only-first").Create(ctx, defaultProject, nil); err != nil {
		t.Fatal(err)
	}
	object := first.Bucket("only-first").Object("dir/hello.txt")
	writeObject(t, ctx, object, "hello")
	if got := readObject(t, ctx, object); got != "hello" {
		t.Fatalf("read %q", got)
	}
	if _, err := second.Bucket("only-first").Attrs(ctx); !errors.Is(err, storage.ErrBucketNotExist) {
		t.Fatalf("the second server sees the first one's bucket: %v", err)
	}
}

func TestRustTempBucketDeletesProtectedObjectsAndEveryGeneration(t *testing.T) {
	requireBinary(t)
	s := Start(t)
	client := s.Client(t)
	ctx := context.Background()
	var names []string
	other := s.TempBucket(t)
	t.Run("protected", func(t *testing.T) {
		retaining := s.TempBucket(t, WithObjectRetention(), WithBucketAttrs(&storage.BucketAttrs{VersioningEnabled: true}))
		holding := s.TempBucket(t, WithBucketAttrs(&storage.BucketAttrs{DefaultEventBasedHold: true}))
		names = []string{retaining.BucketName(), holding.BucketName()}
		attrs, err := retaining.Attrs(ctx)
		if err != nil || !attrs.VersioningEnabled || attrs.ObjectRetentionMode != "Enabled" {
			t.Fatalf("retaining bucket settings: %+v %v", attrs, err)
		}
		if attrs, err := holding.Attrs(ctx); err != nil || !attrs.DefaultEventBasedHold {
			t.Fatalf("holding bucket settings: %+v %v", attrs, err)
		}
		versioned := retaining.Object("versioned")
		writeObject(t, ctx, versioned, "first")
		writeObject(t, ctx, versioned, "second") // leaves a noncurrent generation
		held := retaining.Object("held")
		writeObject(t, ctx, held, "held")
		if _, err := held.Update(ctx, storage.ObjectAttrsToUpdate{TemporaryHold: true}); err != nil {
			t.Fatal(err)
		}
		writer := retaining.Object("retained").NewWriter(ctx)
		writer.Retention = &storage.ObjectRetention{Mode: "Locked", RetainUntil: time.Now().Add(24 * time.Hour)}
		if _, err := io.WriteString(writer, "kept"); err != nil {
			t.Fatal(err)
		}
		if err := writer.Close(); err != nil {
			t.Fatal(err)
		}
		eventHeld := holding.Object("event-held")
		writeObject(t, ctx, eventHeld, "held by default")
		var api *googleapi.Error
		for _, object := range []*storage.ObjectHandle{held, eventHeld} {
			if err := object.Delete(ctx); !errors.As(err, &api) || api.Code != 403 {
				t.Fatalf("the API deleted a held object: %v", err)
			}
		}
		writeObject(t, ctx, other.Object("unrelated"), "stays")
	})
	for _, name := range names {
		if _, err := client.Bucket(name).Attrs(ctx); !errors.Is(err, storage.ErrBucketNotExist) {
			t.Fatalf("temporary bucket %s outlived its test: %v", name, err)
		}
	}
	if got := readObject(t, ctx, other.Object("unrelated")); got != "stays" {
		t.Fatalf("the cleanup touched another bucket: %q", got)
	}
}

// seedTree writes a fixture tree with one bucket and returns its root.
func seedTree(t *testing.T) string {
	t.Helper()
	root := t.TempDir()
	for name, content := range map[string]string{"fixtures/a.txt": "alpha", "fixtures/b.json": "{}",
		"fixtures/nested/c.html": "<p>", "fixtures/d.unknown-extension": "raw", "fixtures/.DS_Store": "os"} {
		path := filepath.Join(root, filepath.FromSlash(name))
		if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	return root
}

// fixtureView is what Seed promises to make alike for the startup seed and a runtime seed.
type fixtureView struct {
	content, contentType, owner string
	acl                         []string
}

func fixtures(t *testing.T, ctx context.Context, bucket *storage.BucketHandle) map[string]fixtureView {
	t.Helper()
	views := map[string]fixtureView{}
	for _, name := range objectNames(t, ctx, bucket) {
		attrs, err := bucket.Object(name).Attrs(ctx)
		if err != nil {
			t.Fatal(err)
		}
		var acl []string
		for _, rule := range attrs.ACL {
			acl = append(acl, string(rule.Entity)+"="+string(rule.Role))
		}
		slices.Sort(acl)
		views[name] = fixtureView{content: readObject(t, ctx, bucket.Object(name)), contentType: attrs.ContentType,
			owner: attrs.Owner, acl: acl}
	}
	return views
}

func TestRustSeedMatchesTheStartupSeed(t *testing.T) {
	requireBinary(t)
	ctx := context.Background()
	root := seedTree(t)
	startup := Start(t, WithSeedDir(root))
	want := fixtures(t, ctx, startup.Client(t).Bucket("fixtures"))
	if len(want) != 4 {
		t.Fatalf("startup seed: %+v", want)
	}
	live := Start(t)
	client := live.Client(t)
	live.Seed(t, Dir(root))
	if got := fixtures(t, ctx, client.Bucket("fixtures")); !equalViews(got, want) {
		t.Fatalf("Dir seed:\n%+v\nstartup seed:\n%+v", got, want)
	}
	into := live.TempBucket(t)
	live.Seed(t, FS(os.DirFS(filepath.Join(root, "fixtures")), Into(into.BucketName())))
	if got := fixtures(t, ctx, into); !equalViews(got, want) {
		t.Fatalf("FS seed into a TempBucket:\n%+v\nstartup seed:\n%+v", got, want)
	}
	live.Seed(t, Object{Bucket: "code", Name: "typed.txt", Content: []byte("typed"), ContentType: "text/x-test"})
	attrs, err := client.Bucket("code").Object("typed.txt").Attrs(ctx)
	if err != nil || attrs.ContentType != "text/x-test" || readObject(t, ctx, client.Bucket("code").Object("typed.txt")) != "typed" {
		t.Fatalf("in-code object: %+v %v", attrs, err)
	}
	embedded := fstest.MapFS{"memory/x.txt": {Data: []byte("in memory")}}
	live.Seed(t, FS(embedded))
	if got := readObject(t, ctx, client.Bucket("memory").Object("x.txt")); got != "in memory" {
		t.Fatalf("fs.FS bucket tree: %q", got)
	}
}

func equalViews(got, want map[string]fixtureView) bool {
	if len(got) != len(want) {
		return false
	}
	for name, view := range want {
		other, found := got[name]
		if !found || other.content != view.content || other.contentType != view.contentType ||
			other.owner != view.owner || !slices.Equal(other.acl, view.acl) {
			return false
		}
	}
	return true
}

func TestRustResetReturnsToTheStartupState(t *testing.T) {
	requireBinary(t)
	ctx := context.Background()
	root := seedTree(t)
	document := `{"version":1,"buckets":[{"name":"startup","versioning":{"enabled":true},` +
		`"cors":[{"origin":["https://app.example"],"method":["GET"],"maxAgeSeconds":60}]}]}`
	s := Start(t, WithBucketsDocument([]byte(document)), WithSeedDir(root))
	client := s.Client(t)
	writeObject(t, ctx, client.Bucket("startup").Object("later"), "added after startup")
	writeObject(t, ctx, client.Bucket("fixtures").Object("a.txt"), "changed")
	if err := client.Bucket("created-later").Create(ctx, defaultProject, nil); err != nil {
		t.Fatal(err)
	}
	s.Reset(t)
	attrs, err := client.Bucket("startup").Attrs(ctx)
	if err != nil || !attrs.VersioningEnabled || len(attrs.CORS) != 1 || attrs.CORS[0].Origins[0] != "https://app.example" {
		t.Fatalf("startup bucket after reset: %+v %v", attrs, err)
	}
	if names := objectNames(t, ctx, client.Bucket("startup")); len(names) != 0 {
		t.Fatalf("the startup bucket kept objects: %q", names)
	}
	if got := readObject(t, ctx, client.Bucket("fixtures").Object("a.txt")); got != "alpha" {
		t.Fatalf("the startup fixture was not restored: %q", got)
	}
	if _, err := client.Bucket("created-later").Attrs(ctx); !errors.Is(err, storage.ErrBucketNotExist) {
		t.Fatalf("a bucket created after startup survived: %v", err)
	}
}
