package gcsmimic_test

import (
	"bytes"
	"context"
	"crypto/x509"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"os"
	"strings"
	"testing"
	"time"

	"cloud.google.com/go/storage"
)

func TestV4RSAVirtualHostedURLs(t *testing.T) {
	ctx, bucket, attrs, payload := strictFixture(t, "virtual")
	endpoint, err := emulatorEndpoint(os.Getenv("GCS_MIMIC_STRICT_ENDPOINT"), os.Getenv("GCS_MIMIC_ALLOWED_HOSTS"))
	if err != nil {
		t.Fatal(err)
	}
	key, err := os.ReadFile("../../fixtures/signing/test-only-private.pem")
	if err != nil {
		t.Fatal(err)
	}
	const domain = "storage.example.invalid"
	host := bucket.BucketName() + "." + domain
	port := "80"
	if endpoint.Scheme == "https" {
		port = "443"
	}
	transport := &http.Transport{
		TLSClientConfig: trustedEmulatorTLS(t), Proxy: nil, MaxConnsPerHost: 2,
		ResponseHeaderTimeout: 5 * time.Second,
		DialContext: func(ctx context.Context, network, address string) (net.Conn, error) {
			if address != net.JoinHostPort(host, port) && address != net.JoinHostPort(host, endpoint.Port()) {
				return nil, fmt.Errorf("refusing unexpected virtual-host dial target %s", address)
			}
			return (&net.Dialer{Timeout: 5 * time.Second}).DialContext(ctx, network, endpoint.Host)
		},
	}
	t.Cleanup(transport.CloseIdleConnections)
	origin := &url.URL{Scheme: endpoint.Scheme, Host: host}
	client := &http.Client{Transport: emulatorTransport{endpoint: origin, base: transport}, Timeout: 10 * time.Second,
		CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}
	sign := func(method, name, hostname string, style storage.URLStyle) string {
		t.Helper()
		signed, err := storage.SignedURL(bucket.BucketName(), name, &storage.SignedURLOptions{
			Scheme: storage.SigningSchemeV4, GoogleAccessID: "local-admin@example.invalid", PrivateKey: key,
			Method: method, Expires: time.Now().Add(5 * time.Minute), Hostname: hostname,
			Style: style, Insecure: endpoint.Scheme == "http",
		})
		if err != nil {
			t.Fatal(err)
		}
		return signed
	}
	get := sign("GET", attrs.Name, domain, storage.VirtualHostedStyle())
	req, err := http.NewRequestWithContext(ctx, "GET", get, nil)
	if err != nil {
		t.Fatal(err)
	}
	res, err := client.Do(req)
	if endpoint.Scheme == "https" && os.Getenv("GCS_MIMIC_TLS_CERT") == "" {
		var mismatch x509.HostnameError
		if !errors.As(err, &mismatch) {
			if res != nil {
				res.Body.Close()
			}
			t.Fatalf("embedded certificate must reject virtual-host SAN: %v", err)
		}
		t.Log("embedded certificate correctly rejects virtual hosts; use a supplied SAN certificate for HTTPS virtual-host transfers")
		return
	}
	if err != nil {
		t.Fatal(err)
	}
	body, readErr := io.ReadAll(io.LimitReader(res.Body, 64*1024))
	closeErr := res.Body.Close()
	if readErr != nil || closeErr != nil || res.StatusCode != http.StatusOK || !bytes.Equal(body, payload) {
		t.Fatalf("virtual GET: %d %q; read/close %v / %v", res.StatusCode, body, readErr, closeErr)
	}
	call := func(method, signed, input string, want int) []byte {
		t.Helper()
		req, err := http.NewRequestWithContext(ctx, method, signed, strings.NewReader(input))
		if err != nil {
			t.Fatal(err)
		}
		res, err := client.Do(req)
		if err != nil {
			t.Fatal(err)
		}
		body, readErr := io.ReadAll(io.LimitReader(res.Body, 64*1024))
		closeErr := res.Body.Close()
		if readErr != nil || closeErr != nil || res.StatusCode != want {
			t.Fatalf("%s: %d %q; read/close %v / %v", method, res.StatusCode, body, readErr, closeErr)
		}
		if want == http.StatusForbidden && !strings.Contains(string(body), "<Code>SignatureDoesNotMatch</Code>") {
			t.Fatalf("unexpected XML error: %s", body)
		}
		return body
	}
	call("GET", get+"&tampered=1", "", http.StatusForbidden)
	call("HEAD", sign("HEAD", attrs.Name, domain, storage.VirtualHostedStyle()), "", http.StatusOK)
	for _, name := range []string{"virtual-put", "é日/a b+%?#", "a//b/"} {
		call("PUT", sign("PUT", name, domain, storage.VirtualHostedStyle()), "virtual bytes", http.StatusOK)
		if got := call("GET", sign("GET", name, "", storage.BucketBoundHostname(host)), "", http.StatusOK); string(got) != "virtual bytes" {
			t.Fatalf("bucket-bound read: %q", got)
		}
	}
	// SPEC-GO-SIGNER-PORT: the SDK drops explicit ports only in its canonical Host.
	explicit := sign("GET", attrs.Name, net.JoinHostPort(domain, endpoint.Port()), storage.VirtualHostedStyle())
	explicitOrigin := &url.URL{Scheme: endpoint.Scheme, Host: net.JoinHostPort(host, endpoint.Port())}
	client.Transport = emulatorTransport{endpoint: explicitOrigin, base: transport}
	call("GET", explicit, "", http.StatusForbidden)
}
