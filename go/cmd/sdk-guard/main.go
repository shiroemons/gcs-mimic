// sdk-guard runs SDK harnesses behind a test-only origin-restricted frontdoor.
package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"net"
	"net/http"
	"net/http/httptest"
	"net/http/httputil"
	"net/url"
	"os"
	"os/exec"
	"os/signal"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"time"
)

type originProxy struct {
	origin       *url.URL
	front        *url.URL
	grpc         *url.URL
	client       *http.Transport
	grpcClient   *http.Transport
	slots        chan struct{}
	requests     atomic.Int64
	blocked      atomic.Int64
	grpcRequests atomic.Int64
}

func localOrigin(raw string) (*url.URL, error) {
	u, err := url.Parse(raw)
	if err != nil || u.Scheme != "http" || u.User != nil || strings.ContainsAny(raw, "?#") ||
		(u.Path != "" && u.Path != "/") || u.Port() == "" ||
		(u.Hostname() != "127.0.0.1" && u.Hostname() != "::1" && u.Hostname() != "localhost") {
		return nil, errors.New("endpoint must be a bare loopback HTTP origin with an explicit port")
	}
	port, err := strconv.Atoi(u.Port())
	if err != nil || port < 1 || port > 65535 {
		return nil, errors.New("endpoint port must be between 1 and 65535")
	}
	u.Path = ""
	return u, nil
}

func (p *originProxy) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	allowed := r.URL.Scheme == p.origin.Scheme && r.URL.Host == p.origin.Host && r.Host == p.origin.Host
	if p.front != nil {
		allowed = allowed || (r.Host == p.front.Host &&
			((r.URL.Scheme == "http" && r.URL.Host == p.front.Host) ||
				(r.URL.Scheme == "" && r.URL.Host == "")))
	}
	contentType := strings.Split(r.Header.Get("Content-Type"), ";")[0]
	grpc := contentType == "application/grpc" || strings.HasPrefix(contentType, "application/grpc+")
	if r.Method == http.MethodConnect || !allowed || r.URL.User != nil || r.URL.Opaque != "" ||
		r.Header.Get("Upgrade") != "" || (grpc && (p.grpc == nil || r.Method != http.MethodPost ||
		(!strings.HasPrefix(r.URL.Path, "/google.storage.v1.Storage/") &&
			!strings.HasPrefix(r.URL.Path, "/google.storage.v2.Storage/")))) {
		p.blocked.Add(1)
		http.Error(w, "request escaped emulator origin", http.StatusForbidden)
		return
	}
	select {
	case p.slots <- struct{}{}:
		defer func() { <-p.slots }()
	default:
		http.Error(w, "SDK guard concurrency exhausted", http.StatusServiceUnavailable)
		return
	}
	p.requests.Add(1)
	target, transport := p.origin, p.client
	if grpc {
		p.grpcRequests.Add(1)
		target, transport = p.grpc, p.grpcClient
	}
	proxy := &httputil.ReverseProxy{
		Rewrite: func(request *httputil.ProxyRequest) {
			request.Out.URL.Scheme, request.Out.URL.Host = target.Scheme, target.Host
			request.Out.Host = target.Host
			request.Out.Header.Del("Proxy-Authorization")
			request.Out.Header.Del("Proxy-Connection")
		},
		Transport:     transport,
		FlushInterval: -1,
		ErrorHandler: func(w http.ResponseWriter, _ *http.Request, _ error) {
			http.Error(w, "local emulator unavailable", http.StatusBadGateway)
		},
	}
	proxy.ServeHTTP(w, r)
}

func guardedEnvironment(origin, proxy string) []string {
	var env []string
	for _, value := range os.Environ() {
		name, _, _ := strings.Cut(value, "=")
		lower := strings.ToLower(name)
		if strings.HasSuffix(lower, "_proxy") || strings.HasPrefix(name, "GOOGLE_") ||
			strings.HasPrefix(name, "GCLOUD_") || strings.HasPrefix(name, "STORAGE_EMULATOR_") ||
			strings.HasPrefix(name, "GCS_MIMIC_ENDPOINT") || name == "GCS_MIMIC_EGRESS_PROXY" ||
			name == "GCS_MIMIC_GRPC_ENDPOINT" {
			continue
		}
		env = append(env, value)
	}
	return append(env, "HTTP_PROXY="+proxy, "http_proxy="+proxy, "HTTPS_PROXY="+proxy, "https_proxy="+proxy,
		"ALL_PROXY="+proxy, "all_proxy="+proxy, "NO_PROXY=", "no_proxy=", "GCE_METADATA_DISABLED=true",
		"GCS_MIMIC_ENDPOINT="+origin, "GCS_MIMIC_EGRESS_PROXY="+proxy)
}

func boundedHTTP2() *http.HTTP2Config {
	return &http.HTTP2Config{MaxConcurrentStreams: 8, StrictMaxConcurrentRequests: true,
		MaxReadFrameSize: 16 << 10, MaxDecoderHeaderTableSize: 4096, MaxEncoderHeaderTableSize: 4096,
		MaxReceiveBufferPerConnection: 256 << 10, MaxReceiveBufferPerStream: 64 << 10,
		SendPingTimeout: 15 * time.Second, PingTimeout: 5 * time.Second, WriteByteTimeout: 15 * time.Second}
}

func fixedTransport(origin *url.URL) *http.Transport {
	dialer := &net.Dialer{Timeout: 5 * time.Second, KeepAlive: 15 * time.Second}
	return &http.Transport{Proxy: nil, MaxConnsPerHost: 8, MaxIdleConns: 8, MaxIdleConnsPerHost: 8,
		IdleConnTimeout: 15 * time.Second, ResponseHeaderTimeout: 15 * time.Second,
		MaxResponseHeaderBytes: 32 << 10, DisableCompression: true,
		DialContext: func(ctx context.Context, network, address string) (net.Conn, error) {
			if network != "tcp" || address != origin.Host {
				return nil, errors.New("upstream dial escaped fixed emulator origin")
			}
			return dialer.DialContext(ctx, network, origin.Host)
		}}
}

func connectionLimit(limit int) func(net.Conn, http.ConnState) {
	var mu sync.Mutex
	connections := make(map[net.Conn]struct{})
	return func(conn net.Conn, state http.ConnState) {
		mu.Lock()
		defer mu.Unlock()
		if state == http.StateNew {
			if len(connections) >= limit {
				_ = conn.Close()
			} else {
				connections[conn] = struct{}{}
			}
		} else if state == http.StateClosed {
			delete(connections, conn)
		}
	}
}

func run(ctx context.Context, endpoint, grpcEndpoint string, command []string) error {
	origin, err := localOrigin(endpoint)
	if err != nil {
		return err
	}
	if len(command) == 0 {
		return errors.New("provide a compiled SDK harness executable after --")
	}
	ctx, cancel := context.WithTimeout(ctx, 2*time.Minute)
	defer cancel()
	transport := fixedTransport(origin)
	defer transport.CloseIdleConnections()
	guard := &originProxy{origin: origin, client: transport, slots: make(chan struct{}, 8)}
	if grpcEndpoint != "" {
		guard.grpc, err = localOrigin(grpcEndpoint)
		if err != nil {
			return err
		}
		if guard.grpc.Host == origin.Host {
			return errors.New("gRPC and HTTP backends must be distinct origins")
		}
		guard.grpcClient = fixedTransport(guard.grpc)
		guard.grpcClient.Protocols = new(http.Protocols)
		guard.grpcClient.Protocols.SetUnencryptedHTTP2(true)
		guard.grpcClient.HTTP2 = boundedHTTP2()
		defer guard.grpcClient.CloseIdleConnections()
	}
	proxy := httptest.NewUnstartedServer(guard)
	defer proxy.Close()
	proxy.Config.MaxHeaderBytes = 32 << 10
	proxy.Config.ReadHeaderTimeout = 5 * time.Second
	proxy.Config.ReadTimeout = 60 * time.Second
	proxy.Config.WriteTimeout = 60 * time.Second
	proxy.Config.IdleTimeout = 15 * time.Second
	proxy.Config.ConnState = connectionLimit(16)
	proxy.Config.BaseContext = func(net.Listener) context.Context { return ctx }
	if guard.grpc != nil {
		proxy.Config.Protocols = new(http.Protocols)
		proxy.Config.Protocols.SetHTTP1(true)
		proxy.Config.Protocols.SetUnencryptedHTTP2(true)
		proxy.Config.HTTP2 = boundedHTTP2()
		guard.front, err = localOrigin("http://" + proxy.Listener.Addr().String())
		if err != nil {
			return err
		}
	}
	proxy.Start()
	defer func() { cancel(); _ = proxy.Config.Close() }()
	childOrigin := origin.String()
	if guard.front != nil {
		childOrigin = guard.front.String()
	}
	child := exec.CommandContext(ctx, command[0], command[1:]...)
	child.Env = guardedEnvironment(childOrigin, proxy.URL)
	child.Stdout, child.Stderr = os.Stdout, os.Stderr
	err = child.Run()
	fmt.Fprintf(os.Stderr, "SDK guard: HTTP=%d gRPC=%d blocked=%d\n", guard.requests.Load()-guard.grpcRequests.Load(), guard.grpcRequests.Load(), guard.blocked.Load())
	if guard.blocked.Load() != 0 {
		err = errors.Join(err, fmt.Errorf("SDK guard blocked %d out-of-origin requests", guard.blocked.Load()))
	}
	if err == nil && guard.requests.Load() == 0 {
		return errors.New("SDK bypassed the required origin proxy")
	}
	return err
}

func main() {
	endpoint := flag.String("endpoint", "", "bare loopback HTTP emulator origin")
	grpcEndpoint := flag.String("grpc-endpoint", "", "optional distinct bare loopback HTTP origin for gRPC")
	flag.Parse()
	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt)
	defer cancel()
	if err := run(ctx, *endpoint, *grpcEndpoint, flag.Args()); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
