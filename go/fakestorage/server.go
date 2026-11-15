// Package fakestorage provides a subprocess facade for selected fake-gcs-server Go APIs, for
// tests that migrate from fake-gcs-server and keep its Go API. New tests should use the
// gcsmimic package, whose API is gcs-mimic's own.
// The Rust executable owns storage and HTTP semantics. NoListener uses a private
// Unix socket without opening a TCP listener; this is not in-process execution.
package fakestorage

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httputil"
	"net/url"
	"os"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"sync"
	"time"

	"cloud.google.com/go/storage"
	"github.com/shiroemons/gcs-mimic/go/internal/mimicproc"
	"google.golang.org/api/option"
)

// Options map the supported source options to the Rust runtime.
type Options struct {
	InitialObjects        []Object
	StorageRoot           string // Native gcs-mimic data, never an old fake-gcs-server backend.
	Seed                  string
	Scheme                string
	Host                  string
	Port                  uint16
	NoListener            bool // Private HTTP-over-Unix transport; unsupported on Windows.
	ExternalURL           string
	PublicHost            string
	AllowedCORSHeaders    []string
	Writer                io.Writer
	BucketsLocation       string
	CertificateLocation   string
	PrivateKeyLocation    string
	Executable            string // Defaults to GCSM_BINARY, then gcs-mimic on PATH.
	EventOptions          EventManagerOptions
	PubSubEmulatorHost    string // Explicit local Pub/Sub endpoint; otherwise PUBSUB_EMULATOR_HOST.
	MemoryMaxBytes        uint64 // Zero uses the bounded Rust memory backend default.
	MemoryCatalogMaxBytes uint64
}

type EventNotificationOptions struct {
	Finalize, Delete, MetadataUpdate, Archive bool
}

type EventManagerOptions struct {
	ProjectID, TopicName, Bucket, ObjectPrefix string
	NotifyOn                                   EventNotificationOptions
}

// Server owns one child process and an isolated memory or caller-owned disk catalog.
type Server struct {
	proc       *mimicproc.Process
	endpoint   *url.URL
	publicURL  string
	url        string
	socketDir  string
	socketPath string
	client     *http.Client
	closeOnce  sync.Once
	closeErr   error
}

func NewServer(objects []Object) *Server {
	s, err := NewServerWithOptions(Options{InitialObjects: objects})
	if err != nil {
		panic(err)
	}
	return s
}

func NewServerWithHostPort(objects []Object, host string, port uint16) (*Server, error) {
	return NewServerWithOptions(Options{InitialObjects: objects, Host: host, Port: port})
}

func NewServerWithOptions(options Options) (*Server, error) {
	return NewServerWithContext(context.Background(), options)
}

// NewServerWithContext bounds startup and stops/reaps the child when ctx is canceled.
func NewServerWithContext(ctx context.Context, options Options) (*Server, error) {
	if err := validateOptions(options); err != nil {
		return nil, err
	}
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	s := &Server{}
	root := options.StorageRoot
	if options.NoListener {
		var err error
		s.socketDir, err = os.MkdirTemp("", "gm-")
		if err != nil {
			return nil, fmt.Errorf("create private Unix socket directory: %w", err)
		}
		s.socketPath = filepath.Join(s.socketDir, "s")
	}
	if err := s.start(ctx, options, root); err != nil {
		return nil, errors.Join(err, s.Close())
	}
	go func() {
		select {
		case <-ctx.Done():
			_ = s.Close() // Close retains its error for the caller's subsequent Close.
		case <-s.proc.Done():
		}
	}()
	for _, object := range options.InitialObjects {
		if object.Retention != nil {
			if err := s.ensureFixtureBucket(ctx, object.ObjectAttrs); err != nil {
				return nil, errors.Join(ctx.Err(), fmt.Errorf("initial fixture bucket: %w", err), s.Close())
			}
		}
	}
	for _, object := range options.InitialObjects {
		if err := s.createObjectStreaming(ctx, object.StreamingObject(), initialFixture); err != nil {
			return nil, errors.Join(ctx.Err(), fmt.Errorf("initial object: %w", err), s.Close())
		}
	}
	return s, nil
}

func validateOptions(options Options) error {
	if err := mimicproc.CheckRenamedBinary(options.Executable, "Options.Executable"); err != nil {
		return err
	}
	if options.Seed != "" && len(options.InitialObjects) != 0 {
		return errors.New("provide either Seed or InitialObjects")
	}
	if options.Scheme != "" && options.Scheme != "http" && options.Scheme != "https" {
		return errors.New("Scheme must be http or https")
	}
	if options.NoListener && (options.Host != "" || options.Port != 0) {
		return errors.New("NoListener uses a private Unix socket; Host and Port are unavailable")
	}
	if options.NoListener && runtime.GOOS == "windows" {
		return errors.New("NoListener requires Unix sockets and is unsupported on Windows; no TCP fallback is used")
	}
	if options.NoListener && (options.CertificateLocation != "" || options.PrivateKeyLocation != "") {
		return errors.New("NoListener uses private HTTP-over-Unix; TLS certificate options are unavailable")
	}
	if options.StorageRoot != "" && (options.MemoryMaxBytes != 0 || options.MemoryCatalogMaxBytes != 0) {
		return errors.New("memory budgets require an empty StorageRoot (the memory backend)")
	}
	if event := options.EventOptions; event != (EventManagerOptions{}) && (event.ProjectID == "" || event.TopicName == "") {
		return errors.New("EventOptions requires ProjectID and TopicName; no event setting is ignored")
	}
	if len(options.AllowedCORSHeaders) != 0 {
		return errors.New("AllowedCORSHeaders is unsupported; configure bucket CORS through the SDK")
	}
	if (options.CertificateLocation == "") != (options.PrivateKeyLocation == "") {
		return errors.New("provide both CertificateLocation and PrivateKeyLocation")
	}
	for _, object := range options.InitialObjects {
		if err := validateFixture(object.ObjectAttrs); err != nil {
			return err
		}
	}
	return nil
}

func (s *Server) start(ctx context.Context, options Options, root string) error {
	host := options.Host
	if host == "" {
		host = "127.0.0.1"
	}
	scheme := options.Scheme
	if scheme == "" {
		scheme = "https"
	}
	publicHost := options.PublicHost
	if publicHost == "" {
		publicHost = "storage.googleapis.com"
	}
	s.publicURL = scheme + "://" + publicHost
	var args []string
	for _, budget := range []struct {
		flag  string
		value uint64
	}{{"--memory-max-bytes", options.MemoryMaxBytes}, {"--memory-catalog-max-bytes", options.MemoryCatalogMaxBytes}} {
		if budget.value != 0 {
			args = append(args, budget.flag, strconv.FormatUint(budget.value, 10))
		}
	}
	if options.NoListener && options.ExternalURL == "" {
		args = append(args, "--api-base-url", "http://localhost")
	}
	if options.EventOptions != (EventManagerOptions{}) {
		event := options.EventOptions
		var selected []string
		for _, candidate := range []struct {
			name    string
			enabled bool
		}{{"finalize", event.NotifyOn.Finalize}, {"delete", event.NotifyOn.Delete}, {"metadataUpdate", event.NotifyOn.MetadataUpdate}, {"archive", event.NotifyOn.Archive}} {
			if candidate.enabled {
				selected = append(selected, candidate.name)
			}
		}
		eventList := strings.Join(selected, ",")
		if eventList == "" {
			eventList = "none"
		}
		args = append(args, "--event-pubsub-project-id", event.ProjectID, "--event-pubsub-topic", event.TopicName,
			"--event-bucket", event.Bucket, "--event-object-prefix", event.ObjectPrefix, "--event-list", eventList)
	}
	for _, field := range [][2]string{{"--seed-dir", options.Seed}, {"--api-base-url", options.ExternalURL},
		{"--public-base-url", s.publicURL}, {"--default-location", options.BucketsLocation},
		{"--tls-cert", options.CertificateLocation}, {"--tls-key", options.PrivateKeyLocation}} {
		if field[1] != "" {
			args = append(args, field[0], field[1])
		}
	}
	if options.PubSubEmulatorHost != "" {
		args = append(args, "--pubsub-emulator-host", options.PubSubEmulatorHost)
	}
	proc, err := mimicproc.Start(ctx, mimicproc.Spec{
		Product:    "fakestorage",
		Executable: mimicproc.Executable(options.Executable),
		Serve: mimicproc.Serve{CompatProfile: "fake-gcs-server", Project: "local-project",
			Listen:     net.JoinHostPort(host, strconv.Itoa(int(options.Port))),
			SocketPath: s.socketPath, Scheme: scheme, DataDir: root},
		Extra:           args,
		CertificateFile: options.CertificateLocation,
		Output:          options.Writer,
	})
	if err != nil {
		return err
	}
	s.proc = proc
	s.endpoint = proc.Endpoint()
	s.url = options.ExternalURL
	if s.url == "" && !options.NoListener {
		s.url = s.endpoint.String()
	}
	s.client = &http.Client{Transport: proc.RoundTripper(s.allowedURL), Timeout: 30 * time.Second}
	return nil
}

// Close gracefully stops, forcibly terminates on timeout, reaps, and removes only owned data.
func (s *Server) Close() error {
	s.closeOnce.Do(func() {
		if s.proc != nil {
			s.closeErr = s.proc.Stop()
		}
		if s.socketDir != "" {
			s.closeErr = errors.Join(s.closeErr, os.RemoveAll(s.socketDir))
		}
	})
	return s.closeErr
}

func (s *Server) Stop() {
	if err := s.Close(); err != nil {
		panic(err)
	}
}

func (s *Server) URL() string              { return s.url }
func (s *Server) PublicURL() string        { return s.publicURL }
func (s *Server) HTTPClient() *http.Client { return s.client }

func (s *Server) HTTPHandler() http.Handler {
	proxy := &httputil.ReverseProxy{Rewrite: func(request *httputil.ProxyRequest) {
		request.SetURL(s.endpoint)
		request.Out.Host = request.In.Host
	}, Transport: s.proc.Transport()}
	return http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		select {
		case <-s.proc.Closing():
			http.Error(writer, "gcs-mimic facade is closed", http.StatusServiceUnavailable)
		default:
			proxy.ServeHTTP(writer, request)
		}
	})
}

func (s *Server) Client() *storage.Client {
	client, err := storage.NewClient(context.Background(), storage.WithJSONReads(), storage.WithDisabledClientMetrics(),
		option.WithEndpoint(s.endpoint.String()+"/storage/v1/"), option.WithHTTPClient(s.HTTPClient()), option.WithoutAuthentication())
	if err != nil {
		panic(err)
	}
	return client
}
