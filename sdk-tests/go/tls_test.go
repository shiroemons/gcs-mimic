package gcsmimic_test

import (
	"crypto/tls"
	"encoding/pem"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"testing"
)

func TestHTTPSCertificateTrust(t *testing.T) {
	server := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusNoContent)
	}))
	defer server.Close()
	ca := filepath.Join(t.TempDir(), "ca.pem")
	if err := os.WriteFile(ca, pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: server.Certificate().Raw}), 0600); err != nil {
		t.Fatal(err)
	}
	for _, tc := range []struct {
		name, ca string
		trusted  bool
	}{{"untrusted", "", false}, {"explicit CA", ca, true}} {
		t.Run(tc.name, func(t *testing.T) {
			config, err := emulatorTLSConfig(tc.ca)
			if err != nil {
				t.Fatal(err)
			}
			transport := &http.Transport{TLSClientConfig: config}
			defer transport.CloseIdleConnections()
			response, err := (&http.Client{Transport: transport}).Get(server.URL)
			if response != nil {
				response.Body.Close()
			}
			if (err == nil) != tc.trusted {
				t.Fatalf("trusted=%t, error=%v", tc.trusted, err)
			}
			if tc.trusted && (response.TLS == nil || response.TLS.Version < tls.VersionTLS12 || len(response.TLS.VerifiedChains) == 0) {
				t.Fatal("TLS connection was not verified")
			}
		})
	}
	if err := os.WriteFile(ca, []byte("not a certificate"), 0600); err != nil {
		t.Fatal(err)
	}
	if _, err := emulatorTLSConfig(ca); err == nil {
		t.Fatal("malformed CA accepted")
	}
	if _, err := emulatorTLSConfig(ca + ".missing"); err == nil {
		t.Fatal("missing CA accepted")
	}
}
