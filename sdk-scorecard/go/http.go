package main

import (
	"crypto/tls"
	"crypto/x509"
	"fmt"
	"net/http"
	"net/url"
	"os"
	"time"
)

type localTransport struct {
	origin *url.URL
	base   http.RoundTripper
}

func (t localTransport) RoundTrip(r *http.Request) (*http.Response, error) {
	if r.URL.Scheme != t.origin.Scheme || r.URL.Host != t.origin.Host || r.URL.User != nil {
		return nil, fmt.Errorf("request escaped emulator")
	}
	return t.base.RoundTrip(r)
}

func localClient(origin *url.URL, caFile string) (*http.Client, error) {
	if (origin.Scheme != "http" && origin.Scheme != "https") || origin.User != nil ||
		origin.Path != "" || origin.RawQuery != "" || origin.Fragment != "" ||
		(origin.Hostname() != "127.0.0.1" && origin.Hostname() != "localhost" && origin.Hostname() != "::1") {
		return nil, fmt.Errorf("loopback HTTP(S) origin required")
	}
	t := http.DefaultTransport.(*http.Transport).Clone()
	t.Proxy = nil
	t.TLSClientConfig = &tls.Config{MinVersion: tls.VersionTLS12}
	if caFile != "" {
		pem, err := os.ReadFile(caFile)
		if err != nil {
			return nil, err
		}
		roots := x509.NewCertPool()
		if !roots.AppendCertsFromPEM(pem) {
			return nil, fmt.Errorf("invalid scorecard CA")
		}
		t.TLSClientConfig.RootCAs = roots
	}
	return &http.Client{Transport: localTransport{origin, t}, Timeout: 20 * time.Second,
		CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}, nil
}
