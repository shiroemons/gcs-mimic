package fakestorage

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"os/signal"
	"runtime"
	"strings"
	"sync"
	"testing"
	"time"
)

// The test executable acts as a child so lifecycle checks need no Rust build or shell.
func TestMain(m *testing.M) {
	if len(os.Args) > 1 && os.Args[1] == "serve" {
		runTestChild()
		return
	}
	os.Exit(m.Run())
}

func runTestChild() {
	listen := ""
	socketPath := ""
	for index, arg := range os.Args {
		if arg == "--listen" {
			listen = os.Args[index+1]
		}
		if arg == "--unix-socket" {
			socketPath = os.Args[index+1]
		}
	}
	if strings.HasPrefix(listen, "exit-before-ready:") {
		os.Exit(17)
	}
	signals := make(chan os.Signal, 1)
	signal.Notify(signals, os.Interrupt)
	if strings.HasPrefix(listen, "never-ready:") {
		<-signals
		return
	}
	network, address := "tcp", "127.0.0.1:0"
	if socketPath != "" {
		network, address = "unix", socketPath
	}
	listener, err := net.Listen(network, address)
	if err != nil {
		panic(err)
	}
	if socketPath != "" {
		if err := os.Chmod(socketPath, 0600); err != nil {
			panic(err)
		}
	}
	server := &http.Server{Handler: http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if strings.HasPrefix(listen, "stall-fixture:") && r.URL.Path == "/_internal/objects" {
			<-r.Context().Done()
			return
		}
		if r.URL.Path == "/redirect" {
			http.Redirect(w, r, "http://metadata.google.internal/latest/meta-data/", http.StatusFound)
			return
		}
		if r.URL.Path == "/wait" {
			<-r.Context().Done()
			return
		}
		w.Header().Set("X-Test-Host", r.Host)
		_, _ = io.Copy(w, r.Body)
	}), ReadHeaderTimeout: time.Second}
	go func() { _ = server.Serve(listener) }()
	ready := map[string]string{"event": "ready", "listen": listener.Addr().String(), "transport": "tcp"}
	if socketPath != "" {
		ready["listen"] = ""
		ready["transport"] = "unix"
		ready["unix_socket"] = socketPath
	}
	_ = json.NewEncoder(os.Stdout).Encode(ready)
	if strings.HasPrefix(listen, "ignore-interrupt:") {
		for range signals {
		}
	}
	<-signals
	_ = server.Close()
}

func testOptions() Options { return Options{Executable: os.Args[0], Scheme: "http"} }

func TestFacadeLifecycleAndIsolation(t *testing.T) {
	t.Parallel()
	var wg sync.WaitGroup
	for range 3 {
		wg.Go(func() {
			s, err := NewServerWithOptions(testOptions())
			if err != nil {
				t.Error(err)
				return
			}
			if s.URL() == "" || !strings.Contains(strings.Join(s.cmd.Args, " "), "--backend memory") {
				t.Error("missing isolated endpoint or default memory backend")
			}
			if err := s.Close(); err != nil {
				t.Error(err)
			}
			if err := s.Close(); err != nil {
				t.Error(err)
			}
			if _, err := s.HTTPClient().Get(s.endpoint.String()); err == nil {
				t.Error("closed facade still accepts requests")
			}
		})
	}
	wg.Wait()
}

func TestNoListenerHandlerAndTransport(t *testing.T) {
	options := testOptions()
	options.NoListener = true
	s, err := NewServerWithOptions(options)
	if runtime.GOOS == "windows" {
		if err == nil || !strings.Contains(err.Error(), "unsupported on Windows") {
			t.Fatalf("Windows must reject without fallback: %v", err)
		}
		return
	}
	if err != nil {
		t.Fatal(err)
	}
	defer s.Stop()
	if s.URL() != "" {
		t.Fatalf("NoListener exposes URL %q", s.URL())
	}
	if info, err := os.Stat(s.socketPath); err != nil || info.Mode().Perm() != 0600 {
		t.Fatalf("socket permissions: %v %v", info, err)
	}
	for _, arg := range s.cmd.Args {
		if arg == "--listen" || arg == "--grpc-listen" {
			t.Fatalf("NoListener opened a TCP path: %v", s.cmd.Args)
		}
	}
	response, err := s.HTTPClient().Post(s.PublicURL()+"/bucket/raw%2Fname", "text/plain", strings.NewReader("fixture"))
	if err != nil {
		t.Fatal(err)
	}
	data, err := io.ReadAll(response.Body)
	if closeErr := response.Body.Close(); err != nil || closeErr != nil || string(data) != "fixture" {
		t.Fatalf("private transport: %s %v %v", data, err, closeErr)
	}
	if response.Header.Get("X-Test-Host") != "storage.googleapis.com" {
		t.Fatal("original Host was changed")
	}
	for _, target := range []string{"https://evil.invalid", "http://metadata.google.internal", "https://storage.googleapis.com.evil.invalid"} {
		if _, err := s.HTTPClient().Get(target); err == nil {
			t.Errorf("egress guard accepted %s", target)
		}
	}
	if _, err := s.HTTPClient().Get(s.PublicURL() + "/redirect"); err == nil {
		t.Fatal("redirect escaped the Unix transport origin guard")
	}
	request := httptest.NewRequest(http.MethodPost, "http://fixture.invalid/path", strings.NewReader("handler"))
	writer := httptest.NewRecorder()
	s.HTTPHandler().ServeHTTP(writer, request)
	if writer.Code != 200 || writer.Body.String() != "handler" || writer.Header().Get("X-Test-Host") != "fixture.invalid" {
		t.Fatalf("handler response: %d %s", writer.Code, writer.Body)
	}
	client := s.Client()
	if err := client.Close(); err != nil {
		t.Fatal(err)
	}
}

func TestNoListenerParallelRequestsDeadlineAndCleanup(t *testing.T) {
	options := testOptions()
	options.NoListener = true
	if runtime.GOOS == "windows" {
		return
	}
	s, err := NewServerWithOptions(options)
	if err != nil {
		t.Fatal(err)
	}
	var clients sync.WaitGroup
	for range 8 {
		clients.Go(func() {
			response, err := s.HTTPClient().Post(s.PublicURL()+"/parallel", "text/plain", strings.NewReader("exact bytes"))
			if err != nil {
				t.Error(err)
				return
			}
			data, err := io.ReadAll(response.Body)
			if closeErr := response.Body.Close(); err != nil || closeErr != nil || string(data) != "exact bytes" {
				t.Errorf("parallel body: %q %v %v", data, err, closeErr)
			}
		})
	}
	clients.Wait()
	ctx, cancel := context.WithTimeout(context.Background(), 50*time.Millisecond)
	defer cancel()
	request, _ := http.NewRequestWithContext(ctx, http.MethodGet, s.PublicURL()+"/wait", nil)
	if _, err := s.HTTPClient().Do(request); !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("Unix deadline: %v", err)
	}
	directory := s.socketDir
	if err := s.Close(); err != nil {
		t.Fatal(err)
	}
	if err := s.Close(); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Lstat(directory); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("owned socket directory remains: %v", err)
	}
}

func TestStartupErrorsCancellationAndOwnedData(t *testing.T) {
	options := testOptions()
	options.Executable = "gcs-mimic-deliberately-missing"
	if _, err := NewServerWithOptions(options); err == nil {
		t.Fatal("missing executable succeeded")
	}
	options = testOptions()
	options.Host = "exit-before-ready"
	if _, err := NewServerWithOptions(options); err == nil || !strings.Contains(err.Error(), "17") {
		t.Fatalf("child error not preserved: %v", err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 100*time.Millisecond)
	defer cancel()
	options = testOptions()
	options.Host = "never-ready"
	if _, err := NewServerWithContext(ctx, options); !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("startup cancellation: %v", err)
	}
	fixtureCtx, stopFixture := context.WithTimeout(context.Background(), 100*time.Millisecond)
	defer stopFixture()
	fixtureOptions := testOptions()
	fixtureOptions.Host = "stall-fixture"
	fixtureOptions.InitialObjects = []Object{{ObjectAttrs: ObjectAttrs{BucketName: "fixture-bucket", Name: "cancel"}, Content: []byte("initial")}}
	if _, err := NewServerWithContext(fixtureCtx, fixtureOptions); !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("initial fixture ignored constructor cancellation: %v", err)
	}
	root := t.TempDir()
	options = testOptions()
	options.StorageRoot = root
	ctx, cancel = context.WithCancel(context.Background())
	s, err := NewServerWithContext(ctx, options)
	if err != nil {
		t.Fatal(err)
	}
	cancel()
	select {
	case <-s.done:
	case <-time.After(3 * time.Second):
		t.Fatal("context cancellation did not reap child")
	}
	if err := s.Close(); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(root); err != nil {
		t.Fatalf("caller-owned data removed: %v", err)
	}
}

func TestCloseKillsAndReapsUnresponsiveChild(t *testing.T) {
	options := testOptions()
	options.Host = "ignore-interrupt"
	s, err := NewServerWithOptions(options)
	if err != nil {
		t.Fatal(err)
	}
	if err := s.Close(); err == nil || !strings.Contains(err.Error(), "shutdown exceeded") {
		t.Fatalf("forced termination was not reported: %v", err)
	}
	select {
	case <-s.done:
	default:
		t.Fatal("Close returned before child was reaped")
	}
}

func TestUnsupportedFixtureFailsBeforeProcessCreation(t *testing.T) {
	options := testOptions()
	options.Executable = "must-not-run"
	options.InitialObjects = []Object{{ObjectAttrs: ObjectAttrs{BucketName: "bucket", Name: "object", Etag: "unsupported"}}}
	if _, err := NewServerWithOptions(options); !errors.Is(err, ErrUnsupportedFixture) {
		t.Fatal(fmt.Errorf("fixture identity silently accepted: %w", err))
	}
}

func TestMemoryAndEventOptionsReachTheRuntimeExactly(t *testing.T) {
	for _, notify := range []EventNotificationOptions{{Finalize: true, Delete: true, MetadataUpdate: true, Archive: true}, {}} {
		options := testOptions()
		options.MemoryMaxBytes = 2 << 20
		options.MemoryCatalogMaxBytes = 4 << 20
		options.EventOptions = EventManagerOptions{ProjectID: "local-project", TopicName: "events", Bucket: "selected-bucket", ObjectPrefix: "prefix/", NotifyOn: notify}
		options.PubSubEmulatorHost = "127.0.0.1:8085"
		s, err := NewServerWithOptions(options)
		if err != nil {
			t.Fatal(err)
		}
		args := make(map[string]string)
		for index := 2; index+1 < len(s.cmd.Args); index += 2 {
			args[s.cmd.Args[index]] = s.cmd.Args[index+1]
		}
		want := map[string]string{"--backend": "memory", "--memory-max-bytes": "2097152", "--memory-catalog-max-bytes": "4194304", "--event-pubsub-project-id": "local-project", "--event-pubsub-topic": "events", "--event-bucket": "selected-bucket", "--event-object-prefix": "prefix/", "--pubsub-emulator-host": "127.0.0.1:8085", "--event-list": "finalize,delete,metadataUpdate,archive"}
		if notify == (EventNotificationOptions{}) {
			want["--event-list"] = "none"
		}
		for flag, value := range want {
			if args[flag] != value {
				t.Errorf("%s = %q, want %q", flag, args[flag], value)
			}
		}
		if _, exists := args["--data-dir"]; exists {
			t.Error("default memory backend received a disk directory")
		}
		if err := s.Close(); err != nil {
			t.Fatal(err)
		}
	}
	options := testOptions()
	options.Executable = "must-not-run"
	options.EventOptions = EventManagerOptions{ObjectPrefix: "ignored-is-forbidden"}
	if _, err := NewServerWithOptions(options); err == nil || !strings.Contains(err.Error(), "EventOptions requires") {
		t.Fatalf("incomplete event options: %v", err)
	}
}

func TestNoListenerPreservesExplicitExternalURL(t *testing.T) {
	if runtime.GOOS == "windows" {
		return
	}
	options := testOptions()
	options.NoListener = true
	options.ExternalURL = "https://advertised.invalid"
	s, err := NewServerWithOptions(options)
	if err != nil {
		t.Fatal(err)
	}
	defer s.Stop()
	if s.URL() != options.ExternalURL {
		t.Fatalf("explicit source URL changed: %q", s.URL())
	}
	response, err := s.HTTPClient().Get(s.URL() + "/value")
	if err != nil {
		t.Fatal(err)
	}
	_ = response.Body.Close()
	if response.Header.Get("X-Test-Host") != "advertised.invalid" {
		t.Fatal("advertised Host was rewritten")
	}
}
