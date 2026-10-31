package main

import (
	"context"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"net/url"
	"os"
	"strings"
	"testing"
	"time"
)

const rpcPath = "/google.storage.v2.Storage/CreateBucket"

func h2Server(handler http.Handler) *httptest.Server {
	server := httptest.NewUnstartedServer(handler)
	server.Config.Protocols = new(http.Protocols)
	server.Config.Protocols.SetHTTP1(true)
	server.Config.Protocols.SetUnencryptedHTTP2(true)
	server.Config.HTTP2 = boundedHTTP2()
	server.Start()
	return server
}

func frontdoor(t *testing.T, handler http.Handler) (*originProxy, *httptest.Server) {
	t.Helper()
	upstream := h2Server(handler)
	t.Cleanup(upstream.Close)
	grpc, err := localOrigin(upstream.URL)
	if err != nil {
		t.Fatal(err)
	}
	httpBackend := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.ProtoMajor != 1 {
			t.Error("HTTP backend received an HTTP2 request")
		}
		_, _ = io.WriteString(w, "http")
	}))
	t.Cleanup(httpBackend.Close)
	origin, err := localOrigin(httpBackend.URL)
	if err != nil {
		t.Fatal(err)
	}
	guard := &originProxy{origin: origin, grpc: grpc, client: fixedTransport(origin),
		grpcClient: fixedTransport(grpc), slots: make(chan struct{}, 1)}
	guard.grpcClient.Protocols = new(http.Protocols)
	guard.grpcClient.Protocols.SetUnencryptedHTTP2(true)
	guard.grpcClient.HTTP2 = boundedHTTP2()
	t.Cleanup(guard.client.CloseIdleConnections)
	t.Cleanup(guard.grpcClient.CloseIdleConnections)
	server := httptest.NewUnstartedServer(guard)
	guard.front, err = localOrigin("http://" + server.Listener.Addr().String())
	if err != nil {
		t.Fatal(err)
	}
	server.Config.Protocols = new(http.Protocols)
	server.Config.Protocols.SetHTTP1(true)
	server.Config.Protocols.SetUnencryptedHTTP2(true)
	server.Config.HTTP2 = boundedHTTP2()
	server.Start()
	t.Cleanup(server.Close)
	return guard, server
}

func h2Client() *http.Transport {
	transport := &http.Transport{Proxy: nil, Protocols: new(http.Protocols)}
	transport.Protocols.SetUnencryptedHTTP2(true)
	return transport
}

func TestFrontdoorPreservesHTTP1AndBackendSessionURLs(t *testing.T) {
	guard, server := frontdoor(t, http.HandlerFunc(func(http.ResponseWriter, *http.Request) {
		t.Error("HTTP request reached gRPC backend")
	}))
	proxyURL, _ := url.Parse(server.URL)
	transport := &http.Transport{Proxy: http.ProxyURL(proxyURL)}
	defer transport.CloseIdleConnections()
	client := &http.Client{Transport: transport, Timeout: 5 * time.Second}
	for _, origin := range []string{server.URL, guard.origin.String()} {
		response, err := client.Get(origin + "/upload/storage/v1/session?offset=0")
		if err != nil {
			t.Fatal(err)
		}
		body, err := io.ReadAll(io.LimitReader(response.Body, 16))
		response.Body.Close()
		if err != nil || string(body) != "http" || response.StatusCode != http.StatusOK {
			t.Fatalf("HTTP backend/session route failed: %q %v", body, err)
		}
	}
	if guard.requests.Load() != 2 || guard.grpcRequests.Load() != 0 {
		t.Fatal("HTTP requests not counted")
	}
}

func TestFrontdoorStreamsHTTP2AndPreservesTrailers(t *testing.T) {
	guard, server := frontdoor(t, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.ProtoMajor != 2 || r.URL.Path != rpcPath {
			t.Error("RPC did not use the fixed h2c backend")
		}
		w.Header().Set("Content-Type", "application/grpc")
		w.Header().Set("Trailer", "Grpc-Status, Grpc-Message")
		first := make([]byte, 5)
		if _, err := io.ReadFull(r.Body, first); err != nil {
			return
		}
		if _, err := w.Write(first); err != nil {
			return
		}
		w.(http.Flusher).Flush()
		if _, err := io.Copy(w, r.Body); err != nil {
			return
		}
		w.Header().Set("Grpc-Status", "7")
		w.Header().Set("Grpc-Message", "denied")
	}))
	transport := h2Client()
	defer transport.CloseIdleConnections()
	reader, writer := io.Pipe()
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	request, err := http.NewRequestWithContext(ctx, http.MethodPost, server.URL+rpcPath, reader)
	if err != nil {
		t.Fatal(err)
	}
	request.Header.Set("Content-Type", "application/grpc+proto")
	responseCh := make(chan *http.Response, 1)
	errorCh := make(chan error, 1)
	go func() {
		response, err := transport.RoundTrip(request)
		if err != nil {
			errorCh <- err
			return
		}
		responseCh <- response
	}()
	if _, err := writer.Write([]byte("first")); err != nil {
		t.Fatal(err)
	}
	var response *http.Response
	select {
	case response = <-responseCh:
	case err := <-errorCh:
		t.Fatal(err)
	case <-ctx.Done():
		t.Fatal(ctx.Err())
	}
	defer response.Body.Close()
	first := make([]byte, 5)
	if _, err := io.ReadFull(response.Body, first); err != nil || string(first) != "first" {
		t.Fatalf("response was buffered until request EOF: %q %v", first, err)
	}
	if _, err := writer.Write([]byte("last")); err != nil {
		t.Fatal(err)
	}
	if err := writer.Close(); err != nil {
		t.Fatal(err)
	}
	body, err := io.ReadAll(io.LimitReader(response.Body, 16))
	if err != nil || string(body) != "last" || response.ProtoMajor != 2 ||
		response.Trailer.Get("Grpc-Status") != "7" || response.Trailer.Get("Grpc-Message") != "denied" {
		t.Fatalf("body=%q trailers=%v error=%v", body, response.Trailer, err)
	}
	if guard.requests.Load() != 1 || guard.grpcRequests.Load() != 1 {
		t.Fatal("RPC was not counted")
	}
}

func TestFrontdoorRejectsForeignOriginsAndUnsupportedRPCs(t *testing.T) {
	guard, server := frontdoor(t, http.HandlerFunc(func(http.ResponseWriter, *http.Request) {
		t.Error("rejected request reached backend")
	}))
	for _, target := range []string{
		"http://storage.googleapis.com" + rpcPath,
		"http://metadata.google.internal" + rpcPath,
		"http://127.0.0.1:1" + rpcPath,
		"http://user@" + guard.front.Host + rpcPath,
		guard.grpc.String() + rpcPath,
		"https://" + guard.front.Host + rpcPath,
		server.URL + "/other.Service/Call",
	} {
		request := httptest.NewRequest(http.MethodPost, target, nil)
		request.Header.Set("Content-Type", "application/grpc")
		response := httptest.NewRecorder()
		guard.ServeHTTP(response, request)
		if response.Code != http.StatusForbidden {
			t.Fatalf("accepted %s: %d", target, response.Code)
		}
	}
	for _, host := range []string{"storage.googleapis.com", guard.origin.Host, "127.0.0.1:1"} {
		request := httptest.NewRequest(http.MethodPost, rpcPath, nil)
		request.Host = host
		request.Header.Set("Content-Type", "application/grpc")
		response := httptest.NewRecorder()
		guard.ServeHTTP(response, request)
		if response.Code != http.StatusForbidden {
			t.Fatalf("accepted relative host %s", host)
		}
	}
	request := httptest.NewRequest(http.MethodConnect, server.URL, nil)
	response := httptest.NewRecorder()
	guard.ServeHTTP(response, request)
	if response.Code != http.StatusForbidden || guard.blocked.Load() != 11 || guard.requests.Load() != 0 {
		t.Fatalf("blocked=%d admitted=%d", guard.blocked.Load(), guard.requests.Load())
	}
}

func TestFrontdoorAdmissionAndConnectionLimit(t *testing.T) {
	guard, server := frontdoor(t, http.HandlerFunc(func(http.ResponseWriter, *http.Request) {
		t.Error("exhausted admission reached backend")
	}))
	guard.slots <- struct{}{}
	response := httptest.NewRecorder()
	guard.ServeHTTP(response, httptest.NewRequest(http.MethodGet, server.URL+"/storage/v1/b", nil))
	if response.Code != http.StatusServiceUnavailable || guard.requests.Load() != 0 {
		t.Fatal("unbounded admission")
	}
	<-guard.slots
	hook := connectionLimit(1)
	first, peer := net.Pipe()
	defer first.Close()
	defer peer.Close()
	hook(first, http.StateNew)
	second, rejected := net.Pipe()
	defer second.Close()
	defer rejected.Close()
	hook(second, http.StateNew)
	if _, err := rejected.Read(make([]byte, 1)); err != io.EOF {
		t.Fatalf("excess connection retained: %v", err)
	}
	hook(first, http.StateClosed)
	third, accepted := net.Pipe()
	defer third.Close()
	defer accepted.Close()
	hook(third, http.StateNew)
	if err := third.SetDeadline(time.Now().Add(time.Second)); err != nil {
		t.Fatal("connection slot was not released", err)
	}
}

func TestGuardChild(t *testing.T) {
	mode := os.Getenv("SDK_GUARD_TEST_CHILD")
	if mode == "" {
		return
	}
	endpoint := os.Getenv("GCS_MIMIC_ENDPOINT")
	transport := h2Client()
	defer transport.CloseIdleConnections()
	target := endpoint + rpcPath
	if mode == "blocked" {
		target = "http://storage.googleapis.com" + rpcPath
	}
	request, err := http.NewRequest(http.MethodPost, target, strings.NewReader("frame"))
	if err != nil {
		t.Fatal(err)
	}
	request.Header.Set("Content-Type", "application/grpc")
	request.Header.Set("X-Guard-Test-Front", endpoint)
	if mode == "blocked" {
		proxy, _ := url.Parse(os.Getenv("HTTP_PROXY"))
		transport = &http.Transport{Proxy: http.ProxyURL(proxy)}
		defer transport.CloseIdleConnections()
	}
	response, err := transport.RoundTrip(request)
	if err != nil {
		t.Fatal(err)
	}
	defer response.Body.Close()
	if _, err := io.Copy(io.Discard, response.Body); err != nil {
		t.Fatal(err)
	}
	if mode == "failure" {
		os.Exit(7)
	}
}

func TestRunCountsGRPCOnlyAndClosesFrontdoorAfterChildFailure(t *testing.T) {
	for _, mode := range []string{"success", "failure"} {
		t.Run(mode, func(t *testing.T) {
			t.Setenv("SDK_GUARD_TEST_CHILD", mode)
			front := make(chan string, 1)
			upstream := h2Server(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				front <- r.Header.Get("X-Guard-Test-Front")
				w.Header().Set("Content-Type", "application/grpc")
				w.Header().Set("Grpc-Status", "0")
			}))
			defer upstream.Close()
			err := run(context.Background(), "http://127.0.0.1:1", upstream.URL,
				[]string{os.Args[0], "-test.run=^TestGuardChild$"})
			if (err != nil) != (mode == "failure") {
				t.Fatalf("run result: %v", err)
			}
			select {
			case endpoint := <-front:
				origin, err := localOrigin(endpoint)
				if err != nil {
					t.Fatal(err)
				}
				conn, err := net.DialTimeout("tcp", origin.Host, time.Second)
				if err == nil {
					conn.Close()
					t.Fatal("frontdoor listener leaked")
				}
			default:
				t.Fatal("no gRPC request traversed the frontdoor")
			}
		})
	}
}

func TestRunBlockedRequestFailsEvenWhenChildSucceeds(t *testing.T) {
	t.Setenv("SDK_GUARD_TEST_CHILD", "blocked")
	upstream := httptest.NewServer(http.NotFoundHandler())
	defer upstream.Close()
	err := run(context.Background(), upstream.URL, "http://127.0.0.1:1", []string{os.Args[0], "-test.run=^TestGuardChild$"})
	if err == nil || !strings.Contains(err.Error(), "blocked 1") {
		t.Fatalf("blocked request did not fail run: %v", err)
	}
}

func TestRunCancellationClosesFrontdoorAndUpstream(t *testing.T) {
	t.Setenv("SDK_GUARD_TEST_CHILD", "cancel")
	started := make(chan string, 1)
	stopped := make(chan struct{})
	upstream := h2Server(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		started <- r.Header.Get("X-Guard-Test-Front")
		<-r.Context().Done()
		close(stopped)
	}))
	defer upstream.Close()
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	done := make(chan error, 1)
	go func() {
		done <- run(ctx, "http://127.0.0.1:1", upstream.URL, []string{os.Args[0], "-test.run=^TestGuardChild$"})
	}()
	var endpoint string
	select {
	case endpoint = <-started:
	case <-time.After(5 * time.Second):
		t.Fatal("child did not connect")
	}
	cancel()
	select {
	case err := <-done:
		if err == nil {
			t.Fatal("cancelled child succeeded")
		}
	case <-time.After(5 * time.Second):
		t.Fatal("run did not stop")
	}
	select {
	case <-stopped:
	case <-time.After(5 * time.Second):
		t.Fatal("upstream request leaked")
	}
	origin, err := localOrigin(endpoint)
	if err != nil {
		t.Fatal(err)
	}
	conn, err := net.DialTimeout("tcp", origin.Host, time.Second)
	if err == nil {
		conn.Close()
		t.Fatal("frontdoor listener leaked")
	}
}

func TestOriginRejectsMalformedPortAndEmptyOriginQuery(t *testing.T) {
	for _, input := range []string{"http://127.0.0.1:0", "http://127.0.0.1:65536", "http://127.0.0.1:1234?", "http://127.0.0.1:1234#"} {
		if _, err := localOrigin(input); err == nil {
			t.Fatalf("accepted %s", input)
		}
	}
	if err := run(context.Background(), "http://127.0.0.1:1234", "http://127.0.0.1:1234", []string{"unused"}); err == nil {
		t.Fatal("same backend origins accepted")
	}
	transport := fixedTransport(&url.URL{Scheme: "http", Host: "127.0.0.1:1234"})
	_, err := transport.DialContext(context.Background(), "tcp", "127.0.0.1:4321")
	if err == nil {
		t.Fatal("foreign upstream dial accepted")
	}
}
