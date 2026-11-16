package gcsmimic

import (
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"strings"
)

// Option configures the server that Start or New runs.
type Option func(*config)

// defaultProject is gcs-mimic's own default seed project.
const defaultProject = "local-project"

type config struct {
	executable string
	project    string
	buckets    []string
	document   []byte
	seedDir    string
	dataDir    string
	logs       io.Writer
	args       []string
}

// WithExecutable names the gcs-mimic binary to start. Without it, GCSM_BINARY is used; failing
// that, gcs-mimic on PATH when it is a supported release, and otherwise the module's own release,
// downloaded once into the user cache directory unless GCSM_DOWNLOAD is off.
func WithExecutable(path string) Option {
	return func(c *config) { c.executable = path }
}

// WithProject sets the project that owns the startup buckets and the buckets TempBucket and
// Seed create (--seed-project). The default is local-project.
func WithProject(id string) Option {
	return func(c *config) { c.project = id }
}

// WithBuckets creates buckets with default settings before the server is ready. Reset keeps
// them, empty. Repeated calls add names.
func WithBuckets(names ...string) Option {
	return func(c *config) { c.buckets = append(c.buckets, names...) }
}

// WithBucketsDocument passes a bucket initialization document (version 1) unchanged, for
// startup buckets with settings such as CORS or IAM. Reset applies it again. It replaces
// WithBuckets; the two cannot be combined.
func WithBucketsDocument(document []byte) Option {
	return func(c *config) { c.document = append([]byte(nil), document...) }
}

// WithSeedDir loads a fixture tree before the server is ready (--seed-dir): each top-level
// directory is a bucket and each file below it an object. Reset loads it again.
func WithSeedDir(path string) Option {
	return func(c *config) { c.seedDir = path }
}

// WithDataDir keeps the catalog on disk in path instead of in memory. The directory belongs to
// the caller: Close leaves it in place.
func WithDataDir(path string) Option {
	return func(c *config) { c.dataDir = path }
}

// WithLogs copies the server's output to w. The copy runs on its own goroutine until the
// server stops, so w must stay usable that long; testing.TB's Log is not, after a test ends.
func WithLogs(w io.Writer) Option {
	return func(c *config) { c.logs = w }
}

// WithArgs appends serve arguments, such as a memory budget. Arguments that this package sets
// itself are refused, because the helpers rely on them.
func WithArgs(args ...string) Option {
	return func(c *config) { c.args = append(c.args, args...) }
}

// managedFlags are the serve options this package sets, with the way to change each, if any.
var managedFlags = map[string]string{
	"--listen":         "the package always listens on a loopback port of its choice",
	"--listen-http":    "the package always serves plain HTTP on one loopback port",
	"--scheme":         "the package always serves plain HTTP",
	"--unix-socket":    "the package always listens on TCP loopback",
	"--compat-profile": "the package uses the native profile; fakestorage uses the fake-gcs-server one",
	"--auth-mode":      "the package always runs with permissive authentication",
	"--admin-enabled":  "the helpers need local administration",
	"--seed-project":   "use WithProject",
	"--backend":        "use WithDataDir for the disk backend",
	"--data-dir":       "use WithDataDir",
	"--buckets":        "use WithBuckets or WithBucketsDocument",
	"--buckets-file":   "use WithBucketsDocument",
	"--seed-dir":       "use WithSeedDir",
}

func newConfig(opts []Option) (config, error) {
	c := config{project: defaultProject}
	for _, opt := range opts {
		opt(&c)
	}
	if c.project == "" {
		return config{}, errors.New("WithProject needs a project ID")
	}
	if len(c.buckets) != 0 && c.document != nil {
		return config{}, errors.New("use either WithBuckets or WithBucketsDocument")
	}
	for _, arg := range c.args {
		name, _, _ := strings.Cut(arg, "=")
		if hint, managed := managedFlags[name]; managed {
			return config{}, fmt.Errorf("WithArgs: %s is set by gcsmimic; %s", name, hint)
		}
	}
	return c, nil
}

// extraArgs are the serve arguments after the shared ones.
func (c config) extraArgs() ([]string, error) {
	var args []string
	switch {
	case len(c.buckets) != 0:
		document, err := bucketsDocument(c.buckets)
		if err != nil {
			return nil, err
		}
		args = append(args, "--buckets", string(document))
	case c.document != nil:
		args = append(args, "--buckets", string(c.document))
	}
	if c.seedDir != "" {
		args = append(args, "--seed-dir", c.seedDir)
	}
	return append(args, c.args...), nil
}

// bucketsDocument writes the version 1 document for buckets with default settings. Their
// project is the document's default, the seed project.
func bucketsDocument(names []string) ([]byte, error) {
	type entry struct {
		Name string `json:"name"`
	}
	document := struct {
		Version int     `json:"version"`
		Buckets []entry `json:"buckets"`
	}{Version: 1}
	for _, name := range names {
		document.Buckets = append(document.Buckets, entry{Name: name})
	}
	encoded, err := json.Marshal(document)
	if err != nil {
		return nil, fmt.Errorf("encode the startup buckets: %w", err)
	}
	return encoded, nil
}
