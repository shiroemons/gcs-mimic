// Package gcsmimic runs gcs-mimic, a local Google Cloud Storage emulator, for Go tests.
//
// Start runs one gcs-mimic process per call and stops it when the test ends. The process
// serves the Cloud Storage JSON API over plain HTTP on a loopback port with the native
// profile, a memory catalog, permissive authentication and local administration, which the
// helpers use: TempBucket gives a test its own bucket and deletes it afterwards, Seed loads
// fixtures, and Reset returns the server to its state at startup.
//
// The usual layout starts one server per package in TestMain with New, and gives each test a
// TempBucket so that parallel tests never see each other's objects. Reset clears every bucket,
// so it suits tests that run one after another.
//
// The package starts the binary as a child process; it does not embed the emulator. Without
// WithExecutable or GCSM_BINARY it uses gcs-mimic on PATH when that is a supported release, and
// otherwise downloads the module's own release once from GitHub Releases, checks it against the
// SHA-256 recorded in the module, and keeps it in the user cache directory. GCSM_DOWNLOAD=off
// turns the download off. Code written for fake-gcs-server's Go API can keep that API with the
// fakestorage package.
package gcsmimic

import (
	"context"
	"errors"
	"fmt"
	"io/fs"
	"net/http"
	"net/url"
	"os/exec"
	"sync"
	"testing"

	"cloud.google.com/go/storage"
	"github.com/shiroemons/gcs-mimic/go/internal/mimicproc"
	"google.golang.org/api/option"
)

// UnsupportedBinaryError reports a gcs-mimic executable outside the releases this package
// supports, naming the executable and both versions. It is the same type as
// fakestorage.UnsupportedBinaryError; detect it with errors.As.
type UnsupportedBinaryError = mimicproc.UnsupportedBinaryError

// outputTail is how much of the child's last output a failed start reports. gcs-mimic explains
// a refused setting in one JSON line on stderr.
const outputTail = 4 << 10

// Server is one running gcs-mimic process.
type Server struct {
	proc     *mimicproc.Process
	endpoint *url.URL
	client   *http.Client
	project  string
	// sdk creates the helpers' buckets and backs the handles TempBucket returns; Close closes it.
	sdk       *storage.Client
	closeOnce sync.Once
	closeErr  error
}

// Start runs a server for tb and stops it when tb and its subtests end. It fails tb if the
// server cannot start.
func Start(tb testing.TB, opts ...Option) *Server {
	tb.Helper()
	s, err := New(tb.Context(), opts...)
	if err != nil {
		tb.Fatalf("gcsmimic: %v", err)
	}
	tb.Cleanup(func() {
		if err := s.Close(); err != nil {
			tb.Errorf("gcsmimic: close: %v", err)
		}
	})
	return s
}

// New runs a server and returns once it is ready, for code without a testing.TB such as
// TestMain. ctx bounds the startup only; the server runs until Close.
func New(ctx context.Context, opts ...Option) (*Server, error) {
	c, err := newConfig(opts)
	if err != nil {
		return nil, err
	}
	if err := mimicproc.CheckRenamedBinary(c.executable, "WithExecutable"); err != nil {
		return nil, err
	}
	extra, err := c.extraArgs()
	if err != nil {
		return nil, err
	}
	executable, err := mimicproc.Resolve(ctx, c.executable)
	if err != nil {
		return nil, err
	}
	proc, err := mimicproc.Start(ctx, mimicproc.Spec{
		Product:    "gcsmimic",
		Executable: executable,
		Serve: mimicproc.Serve{Project: c.project, Listen: "127.0.0.1:0", Scheme: "http",
			DataDir: c.dataDir},
		Extra:      extra,
		Output:     c.logs,
		OutputTail: outputTail,
	})
	if err != nil {
		if errors.Is(err, exec.ErrNotFound) || errors.Is(err, fs.ErrNotExist) {
			err = fmt.Errorf("%w; build or download gcs-mimic, then set GCSM_BINARY, use WithExecutable "+
				"or put it on PATH (see go/README.md in the gcs-mimic repository)", err)
		}
		return nil, err
	}
	endpoint := proc.Endpoint()
	client := &http.Client{Transport: proc.RoundTripper(func(target *url.URL) bool {
		return target.Scheme == endpoint.Scheme && target.Host == endpoint.Host
	})}
	s, err := newServer(endpoint, client, c.project)
	if err != nil {
		return nil, errors.Join(err, proc.Stop())
	}
	s.proc = proc
	return s, nil
}

// newServer connects the helpers to endpoint through client. Unit tests use it with an HTTP
// server of their own, without a process.
func newServer(endpoint *url.URL, client *http.Client, project string) (*Server, error) {
	s := &Server{endpoint: endpoint, client: client, project: project}
	sdk, err := s.NewClient(context.Background())
	if err != nil {
		return nil, fmt.Errorf("create the helpers' client: %w", err)
	}
	s.sdk = sdk
	return s, nil
}

// Close stops the server, forcibly after ten seconds, and waits for its process to exit. It
// keeps a WithDataDir directory. Every call returns the result of the first.
func (s *Server) Close() error {
	s.closeOnce.Do(func() {
		if s.sdk != nil {
			s.closeErr = s.sdk.Close()
		}
		if s.proc != nil {
			s.closeErr = errors.Join(s.closeErr, s.proc.Stop())
		}
	})
	return s.closeErr
}

// URL is the server's base URL, such as http://127.0.0.1:PORT. STORAGE_EMULATOR_HOST accepts
// it unchanged.
func (s *Server) URL() string { return s.endpoint.String() }

// HTTPClient returns a client that reaches only this server. It refuses any other origin before
// dialing, so code under test cannot reach Google or the metadata server by accident. Requests
// have no overall time limit; bound them with their context.
func (s *Server) HTTPClient() *http.Client { return s.client }

// Version is the gcs-mimic release the server reported when it started.
func (s *Server) Version() string {
	if s.proc == nil {
		return ""
	}
	return s.proc.Version()
}

// Client returns an official Go client for this server, closed when tb ends. It fails tb if
// the client cannot be created.
func (s *Server) Client(tb testing.TB) *storage.Client {
	tb.Helper()
	// The client must outlive tb.Context, which ends before the cleanups that may still use it.
	client, err := s.NewClient(context.Background())
	if err != nil {
		tb.Fatalf("gcsmimic: client: %v", err)
	}
	tb.Cleanup(func() {
		if err := client.Close(); err != nil {
			tb.Errorf("gcsmimic: close client: %v", err)
		}
	})
	return client
}

// NewClient returns an official Go client for this server, which the caller closes. The
// client uses the JSON API, sends no credentials and never looks for them.
func (s *Server) NewClient(ctx context.Context) (*storage.Client, error) {
	return storage.NewClient(ctx, storage.WithJSONReads(), storage.WithDisabledClientMetrics(),
		option.WithEndpoint(s.URL()+"/storage/v1/"), option.WithHTTPClient(s.client),
		option.WithoutAuthentication())
}
