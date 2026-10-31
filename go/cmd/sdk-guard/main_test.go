package main

import (
	"io"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"testing"
)

func TestProxyRejectsCloudMetadataChangedOriginAndConnect(t *testing.T) {
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) { _, _ = w.Write([]byte("local")) }))
	defer upstream.Close()
	origin, err := localOrigin(upstream.URL)
	if err != nil {
		t.Fatal(err)
	}
	guard := &originProxy{origin: origin, client: &http.Transport{Proxy: nil}, slots: make(chan struct{}, 1)}
	defer guard.client.CloseIdleConnections()
	proxy := httptest.NewServer(guard)
	defer proxy.Close()
	proxyURL, _ := url.Parse(proxy.URL)
	transport := &http.Transport{Proxy: http.ProxyURL(proxyURL)}
	defer transport.CloseIdleConnections()
	client := &http.Client{Transport: transport}
	for _, target := range []string{upstream.URL, "http://metadata.google.internal/", "http://storage.googleapis.com/bucket", "http://127.0.0.1:1/upload"} {
		response, err := client.Get(target)
		if err != nil {
			t.Fatal(err)
		}
		_, readErr := io.Copy(io.Discard, response.Body)
		closeErr := response.Body.Close()
		want := 403
		if target == upstream.URL {
			want = 200
		}
		if response.StatusCode != want || readErr != nil || closeErr != nil {
			t.Fatalf("%s: %d %v %v", target, response.StatusCode, readErr, closeErr)
		}
	}
	if _, err := client.Get("https://storage.googleapis.com/"); err == nil {
		t.Fatal("CONNECT was accepted")
	}
	if guard.blocked.Load() != 4 || guard.requests.Load() != 1 {
		t.Fatal("guard did not see every request")
	}
}

func TestChildEnvironmentCannotBypassProxyOrDiscoverCredentials(t *testing.T) {
	t.Setenv("NO_PROXY", "*")
	t.Setenv("GOOGLE_APPLICATION_CREDENTIALS", "must-not-read")
	t.Setenv("STORAGE_EMULATOR_HOST", "external.invalid")
	t.Setenv("GCS_MIMIC_GRPC_ENDPOINT", "must-not-inherit")
	env := strings.Join(guardedEnvironment("http://127.0.0.1:4443", "http://127.0.0.1:1234"), "\n")
	for _, forbidden := range []string{"NO_PROXY=*", "must-not-read", "external.invalid", "must-not-inherit"} {
		if strings.Contains(env, forbidden) {
			t.Fatalf("ambient config retained: %s", forbidden)
		}
	}
}
