package gcsmimic

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"net/url"
	"os"
	"os/exec"
	"strings"
	"testing"
	"time"

	"github.com/shiroemons/gcs-mimic/go/internal/fakechild"
	"github.com/shiroemons/gcs-mimic/go/internal/mimicproc"
)

// The test executable acts as gcs-mimic, so lifecycle checks need no Rust build.
func TestMain(m *testing.M) { fakechild.Main(m, mimicproc.ModuleVersion) }

func fakeBinary() Option { return WithExecutable(os.Args[0]) }

// serveArgs pairs each serve flag of s's command line with its value.
func serveArgs(t *testing.T, s *Server) map[string]string {
	t.Helper()
	args := s.proc.Cmd().Args
	if len(args) < 2 || args[1] != "serve" {
		t.Fatalf("command line: %q", args)
	}
	pairs := map[string]string{}
	for index := 2; index+1 < len(args); index += 2 {
		pairs[args[index]] = args[index+1]
	}
	return pairs
}

func TestStartRunsTheNativeProfileOverLoopbackHTTP(t *testing.T) {
	s := Start(t, fakeBinary())
	args := serveArgs(t, s)
	for flag, want := range map[string]string{"--listen": "127.0.0.1:0", "--scheme": "http", "--backend": "memory",
		"--admin-enabled": "true", "--auth-mode": "permissive", "--seed-project": "local-project"} {
		if args[flag] != want {
			t.Errorf("%s = %q, want %q", flag, args[flag], want)
		}
	}
	for _, flag := range []string{"--compat-profile", "--data-dir", "--buckets", "--seed-dir", "--unix-socket"} {
		if _, set := args[flag]; set {
			t.Errorf("default start passed %s", flag)
		}
	}
	endpoint, err := url.Parse(s.URL())
	if err != nil || endpoint.Scheme != "http" || endpoint.Hostname() != "127.0.0.1" || endpoint.Port() == "" || endpoint.Port() == "0" {
		t.Fatalf("URL() = %q, %v", s.URL(), err)
	}
	if s.Version() != mimicproc.ModuleVersion {
		t.Fatalf("Version() = %q", s.Version())
	}
	response, err := s.HTTPClient().Post(s.URL()+"/echo", "text/plain", strings.NewReader("exact bytes"))
	if err != nil {
		t.Fatal(err)
	}
	data, err := io.ReadAll(response.Body)
	if closeErr := response.Body.Close(); err != nil || closeErr != nil || string(data) != "exact bytes" {
		t.Fatalf("echo: %q %v %v", data, err, closeErr)
	}
	for _, target := range []string{"https://storage.googleapis.com/storage/v1/b", "http://metadata.google.internal/",
		"http://localhost:" + endpoint.Port() + "/", "https://" + endpoint.Host + "/"} {
		if _, err := s.HTTPClient().Get(target); err == nil {
			t.Errorf("HTTPClient reached %s", target)
		}
	}
	if _, err := s.HTTPClient().Get(s.URL() + "/redirect"); err == nil {
		t.Error("a redirect left the server's origin")
	}
	if client := s.Client(t); client == nil {
		t.Fatal("Client returned nil")
	}
}

func TestCloseStopsTheChildAndRepeatsItsResult(t *testing.T) {
	s, err := New(context.Background(), fakeBinary())
	if err != nil {
		t.Fatal(err)
	}
	if err := s.Close(); err != nil {
		t.Fatal(err)
	}
	select {
	case <-s.proc.Done():
	default:
		t.Fatal("Close returned before the child exited")
	}
	if err := s.Close(); err != nil {
		t.Fatal(err)
	}
	if _, err := s.HTTPClient().Get(s.URL()); err == nil {
		t.Fatal("a closed server still answers")
	}
}

func TestStartContextBoundsTheStartupOnly(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	s, err := New(ctx, fakeBinary())
	if err != nil {
		t.Fatal(err)
	}
	defer func() {
		if err := s.Close(); err != nil {
			t.Error(err)
		}
	}()
	cancel()
	time.Sleep(50 * time.Millisecond)
	select {
	case <-s.proc.Done():
		t.Fatal("canceling the startup context stopped a running server")
	default:
	}
	t.Setenv("GCSM_TEST_CHILD_MODE", fakechild.NeverReady)
	deadline, stop := context.WithTimeout(context.Background(), 100*time.Millisecond)
	defer stop()
	if _, err := New(deadline, fakeBinary()); !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("startup deadline: %v", err)
	}
}

func TestOptionsReachTheCommandLine(t *testing.T) {
	seedDir, dataDir := t.TempDir(), t.TempDir()
	s := Start(t, fakeBinary(), WithProject("other-project"), WithBuckets("first", "second"),
		WithSeedDir(seedDir), WithDataDir(dataDir), WithArgs("--default-location", "EU"))
	args := serveArgs(t, s)
	for flag, want := range map[string]string{"--seed-project": "other-project", "--backend": "disk", "--data-dir": dataDir,
		"--seed-dir": seedDir, "--default-location": "EU",
		"--buckets": `{"version":1,"buckets":[{"name":"first"},{"name":"second"}]}`} {
		if args[flag] != want {
			t.Errorf("%s = %q, want %q", flag, args[flag], want)
		}
	}
	if err := s.Close(); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(dataDir); err != nil {
		t.Fatalf("Close removed the caller's data directory: %v", err)
	}
	document := `{"version":1,"buckets":[{"name":"cors","cors":[{"origin":["*"]}]}]}`
	s = Start(t, fakeBinary(), WithBucketsDocument([]byte(document)))
	if got := serveArgs(t, s)["--buckets"]; got != document {
		t.Fatalf("--buckets = %q", got)
	}
}

func TestOptionsRefuseManagedFlagsAndConflicts(t *testing.T) {
	for _, tc := range []struct {
		name string
		opts []Option
		want string
	}{
		{"listener", []Option{WithArgs("--listen=0.0.0.0:4443")}, "--listen is set by gcsmimic"},
		{"profile", []Option{WithArgs("--compat-profile", "fake-gcs-server")}, "fakestorage"},
		{"seed directory", []Option{WithArgs("--seed-dir", "fixtures")}, "use WithSeedDir"},
		{"project", []Option{WithArgs("--seed-project", "p")}, "use WithProject"},
		{"both bucket forms", []Option{WithBuckets("a"), WithBucketsDocument([]byte(`{"version":1,"buckets":[]}`))}, "either"},
		{"empty project", []Option{WithProject("")}, "project ID"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			opts := append([]Option{WithExecutable("gcs-mimic-must-not-run")}, tc.opts...)
			if _, err := New(context.Background(), opts...); err == nil || !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("want an error naming %q, got %v", tc.want, err)
			}
		})
	}
}

func TestStartFailuresSayWhatToDo(t *testing.T) {
	t.Run("missing executable", func(t *testing.T) {
		_, err := New(context.Background(), WithExecutable("gcs-mimic-deliberately-missing"))
		if !errors.Is(err, exec.ErrNotFound) || !strings.Contains(err.Error(), "GCSM_BINARY") {
			t.Fatalf("want a not-found error that names GCSM_BINARY, got %v", err)
		}
	})
	t.Run("exit before ready", func(t *testing.T) {
		t.Setenv("GCSM_TEST_CHILD_MODE", fakechild.ExitBeforeReady)
		_, err := New(context.Background(), fakeBinary())
		if err == nil || !strings.Contains(err.Error(), "exited before readiness") ||
			!strings.Contains(err.Error(), "fake child refused its options") {
			t.Fatalf("want the exit and the child's last output, got %v", err)
		}
	})
	t.Run("another release", func(t *testing.T) {
		t.Setenv("GCSM_TEST_CHILD_VERSION", "0.0.1")
		_, err := New(context.Background(), fakeBinary())
		var unsupported *UnsupportedBinaryError
		if !errors.As(err, &unsupported) || unsupported.Version != "0.0.1" || unsupported.Path != os.Args[0] {
			t.Fatalf("want *UnsupportedBinaryError, got %v", err)
		}
		if want := "reported version 0.0.1; gcsmimic " + mimicproc.ModuleVersion + " supports "; !strings.Contains(err.Error(), want) {
			t.Fatalf("message %q lacks %q", err, want)
		}
	})
	t.Run("renamed variable", func(t *testing.T) {
		t.Setenv("GCSM_BINARY", "")
		t.Setenv("GCS_MIMIC_BINARY", "old-binary")
		if _, err := New(context.Background()); err == nil || !strings.Contains(err.Error(), "WithExecutable") {
			t.Fatalf("want the rename advice, got %v", err)
		}
	})
}

// newTestServer connects the helpers to handler instead of a gcs-mimic process.
func newTestServer(t *testing.T, handler http.HandlerFunc) *Server {
	t.Helper()
	backend := httptest.NewServer(handler)
	t.Cleanup(backend.Close)
	endpoint, err := url.Parse(backend.URL)
	if err != nil {
		t.Fatal(err)
	}
	s, err := newServer(endpoint, backend.Client(), defaultProject)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := s.Close(); err != nil {
			t.Error(err)
		}
	})
	return s
}

// writeJSON answers a request with a JSON body and status.
func writeJSON(w http.ResponseWriter, status int, body string) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_, _ = io.WriteString(w, body)
}

// apiError is the JSON error envelope gcs-mimic answers with.
func apiError(status int, reason string) string {
	return fmt.Sprintf(`{"error":{"code":%d,"message":%q,"errors":[{"reason":%q}]}}`, status, reason, reason)
}
