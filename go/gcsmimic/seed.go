package gcsmimic

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"net/http"
	"os"
	"strings"
	"testing"
	"unicode/utf8"

	"github.com/shiroemons/gcs-mimic/go/internal/mimicproc"
)

// SeedItem is one input of Seed: a Dir, an FS or an Object.
type SeedItem interface {
	seed(ctx context.Context, sd *seeding) error
}

// Object is an object that Seed writes with the given content.
type Object struct {
	Bucket  string
	Name    string
	Content []byte
	// ContentType is stored as given; empty stores application/octet-stream.
	ContentType string
}

// TreeOption configures a Dir or FS input.
type TreeOption func(*tree)

// Into writes every file of the tree into bucket, named by its path below the tree root,
// instead of reading the top-level directories as buckets. It lets fixed test data fill a
// TempBucket, whose name changes on every run.
func Into(bucket string) TreeOption {
	return func(t *tree) { t.into = bucket }
}

// Dir reads a fixture tree from the directory at path, laid out as for WithSeedDir: each
// top-level directory is a bucket, and each file below it an object named by its path below
// that directory, joined with "/". Top-level files are skipped, as are .DS_Store and files
// whose names start with "._". A top-level directory creates its bucket even when empty; an
// empty directory below it creates nothing. A symbolic link or another special file fails the
// call before anything is written.
func Dir(path string, opts ...TreeOption) SeedItem {
	t := &tree{label: path, root: path}
	for _, opt := range opts {
		opt(t)
	}
	return t
}

// FS reads a fixture tree from fsys, such as an embed.FS or a testing/fstest.MapFS, laid out
// as for Dir.
func FS(fsys fs.FS, opts ...TreeOption) SeedItem {
	t := &tree{label: "the fs.FS", fsys: fsys}
	for _, opt := range opts {
		opt(t)
	}
	return t
}

// Seed writes the items in order, creating each missing bucket through the JSON API first. It
// writes fixtures through POST /_internal/objects, one object at a time, without notifications.
// Files of a Dir or FS take their content type from their name's extension and their owner as
// the startup seed sets them, so the same tree gives the same objects either way. Seed is not
// atomic: a failure stops it and fails tb, keeping what it wrote before. Unlike the startup
// seed, it gives an object written again a new generation.
func (s *Server) Seed(tb testing.TB, items ...SeedItem) {
	tb.Helper()
	if err := s.seed(tb.Context(), tb.Logf, items); err != nil {
		tb.Fatalf("gcsmimic: seed: %v", err)
	}
}

func (s *Server) seed(ctx context.Context, logf func(string, ...any), items []SeedItem) error {
	sd := &seeding{server: s, logf: logf, created: map[string]bool{}}
	for _, item := range items {
		if item == nil {
			return errors.New("nil SeedItem")
		}
		if err := item.seed(ctx, sd); err != nil {
			return err
		}
	}
	return nil
}

// seeding is the state of one Seed call.
type seeding struct {
	server  *Server
	logf    func(string, ...any)
	created map[string]bool // buckets this call already made sure of
}

// fixtureMetadata is the JSON part of POST /_internal/objects that Seed sends.
type fixtureMetadata struct {
	Bucket      string `json:"bucket"`
	Name        string `json:"name"`
	ContentType string `json:"contentType,omitempty"`
	// Defaults "seed" lets the server choose the content type from the name, with the startup
	// seed's table, and set the owner the startup seed sets. A copy of the table here would
	// drift from the server's.
	Defaults string `json:"defaults,omitempty"`
	// NotificationMode "suppress" keeps fixtures out of the notifications a test may observe.
	NotificationMode string `json:"notificationMode"`
}

func (sd *seeding) ensureBucket(ctx context.Context, name string) error {
	if sd.created[name] {
		return nil
	}
	if err := sd.server.createBucket(ctx, name); err != nil {
		return err
	}
	sd.created[name] = true
	return nil
}

func (sd *seeding) upload(ctx context.Context, metadata fixtureMetadata, content io.Reader) error {
	if err := sd.ensureBucket(ctx, metadata.Bucket); err != nil {
		return err
	}
	encoded, err := json.Marshal(metadata)
	if err != nil {
		return fmt.Errorf("encode the metadata of %s/%s: %w", metadata.Bucket, metadata.Name, err)
	}
	body, contentType, err := mimicproc.FixtureBody(encoded, content)
	if err != nil {
		return fmt.Errorf("frame %s/%s: %w", metadata.Bucket, metadata.Name, err)
	}
	if err := sd.server.admin(ctx, http.MethodPost, "/_internal/objects", body, contentType, nil); err != nil {
		return fmt.Errorf("write %s/%s: %w", metadata.Bucket, metadata.Name, err)
	}
	return nil
}

func (o Object) seed(ctx context.Context, sd *seeding) error {
	if o.Bucket == "" || o.Name == "" {
		return fmt.Errorf("Object needs a Bucket and a Name, not %q and %q", o.Bucket, o.Name)
	}
	return sd.upload(ctx, fixtureMetadata{Bucket: o.Bucket, Name: o.Name, ContentType: o.ContentType,
		NotificationMode: "suppress"}, bytes.NewReader(o.Content))
}

// tree is a Dir or FS input.
type tree struct {
	label string // names the input in errors
	root  string // the directory of a Dir
	fsys  fs.FS
	into  string
}

// treeEntry is a bucket directory, when object is empty, or a file to write.
type treeEntry struct {
	bucket, object, path string
}

func (t *tree) seed(ctx context.Context, sd *seeding) error {
	fsys, err := t.open()
	if err != nil {
		return err
	}
	// Check the whole tree before writing anything, so that a file the startup seed refuses
	// fails the call without a partial upload.
	if err := t.walk(fsys, func(string, ...any) {}, func(treeEntry) error { return nil }); err != nil {
		return fmt.Errorf("%s: %w", t.label, err)
	}
	if t.into != "" {
		if err := sd.ensureBucket(ctx, t.into); err != nil {
			return err
		}
	}
	err = t.walk(fsys, sd.logf, func(entry treeEntry) error {
		if entry.object == "" {
			return sd.ensureBucket(ctx, entry.bucket)
		}
		return t.write(ctx, sd, fsys, entry)
	})
	if err != nil {
		return fmt.Errorf("%s: %w", t.label, err)
	}
	return nil
}

func (t *tree) open() (fs.FS, error) {
	if t.fsys != nil {
		return t.fsys, nil
	}
	info, err := os.Lstat(t.root)
	if err != nil {
		return nil, fmt.Errorf("seed directory: %w", err)
	}
	if !info.IsDir() {
		return nil, fmt.Errorf("seed directory %s must be a directory, not a symlink or a file", t.root)
	}
	return os.DirFS(t.root), nil
}

func (t *tree) write(ctx context.Context, sd *seeding, fsys fs.FS, entry treeEntry) error {
	file, err := fsys.Open(entry.path)
	if err != nil {
		return err
	}
	err = sd.upload(ctx, fixtureMetadata{Bucket: entry.bucket, Name: entry.object, Defaults: "seed",
		NotificationMode: "suppress"}, file)
	return errors.Join(err, file.Close())
}

// walk visits the tree with the startup seed's rules, which go/README.md lists, or with Into's.
func (t *tree) walk(fsys fs.FS, logf func(string, ...any), visit func(treeEntry) error) error {
	return fs.WalkDir(fsys, ".", func(path string, entry fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if path == "." {
			return nil
		}
		if !utf8.ValidString(path) {
			return fmt.Errorf("%q: a fixture name must be UTF-8", path)
		}
		kind := entry.Type()
		if kind&fs.ModeSymlink != 0 {
			return fmt.Errorf("%s: a symbolic link is not a fixture", path)
		}
		if !kind.IsDir() && !kind.IsRegular() {
			return fmt.Errorf("%s: a fixture must be a regular file or a directory", path)
		}
		if !kind.IsDir() && isOSMetadata(entry.Name()) {
			return nil
		}
		if t.into != "" {
			if kind.IsDir() {
				return nil
			}
			return visit(treeEntry{bucket: t.into, object: path, path: path})
		}
		bucket, object, nested := strings.Cut(path, "/")
		switch {
		case !nested && kind.IsDir():
			return visit(treeEntry{bucket: bucket, path: path})
		case !nested:
			logf("gcsmimic: seed skipped %s: top-level entries must be bucket directories", path)
			return nil
		case kind.IsDir():
			return nil
		default:
			return visit(treeEntry{bucket: bucket, object: object, path: path})
		}
	})
}

// isOSMetadata matches the files macOS leaves beside fixtures, which the startup seed skips.
func isOSMetadata(name string) bool {
	return name == ".DS_Store" || strings.HasPrefix(name, "._")
}
