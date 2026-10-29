package main

import (
	"encoding/pem"
	"net/http"
	"net/http/httptest"
	"net/url"
	"os"
	"path/filepath"
	"testing"
)

func TestLocalClientTrustAndOrigin(t *testing.T) {
	server := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) { w.WriteHeader(204) }))
	defer server.Close()
	origin, _ := url.Parse(server.URL)
	ca := filepath.Join(t.TempDir(), "ca.pem")
	if err := os.WriteFile(ca, pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: server.Certificate().Raw}), 0600); err != nil {
		t.Fatal(err)
	}
	for _, file := range []string{"", ca} {
		client, err := localClient(origin, file)
		if err != nil {
			t.Fatal(err)
		}
		response, err := client.Get(server.URL)
		if response != nil {
			response.Body.Close()
		}
		if (err == nil) != (file == ca) {
			t.Fatalf("unexpected CA trust result: %v", err)
		}
		if _, err := client.Get("https://storage.googleapis.com"); err == nil {
			t.Fatal("external origin accepted")
		}
		if file == ca {
			transport := client.Transport.(localTransport).base.(*http.Transport)
			transport.CloseIdleConnections()
			transport.TLSClientConfig.ServerName = "wrong.invalid"
			if _, err := client.Get(server.URL); err == nil {
				t.Fatal("wrong SAN accepted")
			}
		}
	}
}
