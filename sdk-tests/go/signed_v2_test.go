package gcsmimic_test

import (
	"bytes"
	"crypto/md5"
	"encoding/base64"
	"io"
	"net/http"
	"os"
	"strings"
	"testing"
	"time"

	"cloud.google.com/go/storage"
)

func TestV2RSASignedTransfers(t *testing.T) {
	ctx, bucket, attrs, payload := strictFixture(t, "v2")
	endpoint, err := emulatorEndpoint(os.Getenv("GCS_MIMIC_STRICT_ENDPOINT"), os.Getenv("GCS_MIMIC_ALLOWED_HOSTS"))
	if err != nil {
		t.Fatal(err)
	}
	// The V2 SDK signer always emits HTTPS, including when Insecure is true.
	if endpoint.Scheme != "https" {
		t.Skip("Go V2 SignedURL requires GCS_MIMIC_SDK_SCHEME=https; URLs must remain unchanged")
	}
	key, err := os.ReadFile("../../fixtures/signing/test-only-private.pem")
	if err != nil {
		t.Fatal(err)
	}
	transport := &http.Transport{TLSClientConfig: trustedEmulatorTLS(t), Proxy: nil, ResponseHeaderTimeout: 5 * time.Second}
	t.Cleanup(transport.CloseIdleConnections)
	client := &http.Client{Transport: emulatorTransport{endpoint: endpoint, base: transport}, Timeout: 10 * time.Second,
		CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}
	sign := func(method, name, signer, contentType, checksum string) string {
		t.Helper()
		u, err := storage.SignedURL(bucket.BucketName(), name, &storage.SignedURLOptions{
			Scheme: storage.SigningSchemeV2, GoogleAccessID: signer, PrivateKey: key,
			Method: method, Expires: time.Now().Add(5 * time.Minute), Hostname: endpoint.Host,
			Insecure: endpoint.Scheme == "http", ContentType: contentType, MD5: checksum,
		})
		if err != nil {
			t.Fatal(err)
		}
		return u
	}
	call := func(method, signed, contentType, checksum string, input []byte, status int, code string) []byte {
		t.Helper()
		req, err := http.NewRequestWithContext(ctx, method, signed, bytes.NewReader(input))
		if err != nil {
			t.Fatal(err)
		}
		if contentType != "" {
			req.Header.Set("Content-Type", contentType)
		}
		if checksum != "" {
			req.Header.Set("Content-MD5", checksum)
		}
		res, err := client.Do(req)
		if err != nil {
			t.Fatal(err)
		}
		body, err := io.ReadAll(io.LimitReader(res.Body, 64*1024))
		closeErr := res.Body.Close()
		if err != nil || closeErr != nil {
			t.Fatalf("read/close: %v / %v", err, closeErr)
		}
		if res.StatusCode != status || (code != "" && !strings.Contains(string(body), "<Code>"+code+"</Code>")) {
			t.Fatalf("want %d %s, got %d: %s", status, code, res.StatusCode, body)
		}
		return body
	}
	get := sign("GET", attrs.Name, "local-admin@example.invalid", "", "")
	if got := call("GET", get, "", "", nil, 200, ""); !bytes.Equal(got, payload) {
		t.Fatalf("download %q", got)
	}
	call("GET", strings.Replace(get, "/dir/", "/changed/", 1), "", "", nil, 403, "SignatureDoesNotMatch")
	call("GET", sign("GET", attrs.Name, "unknown@example.invalid", "", ""), "", "", nil, 403, "InvalidAccessKeyId")
	call("PUT", sign("PUT", "denied.txt", "local-viewer@example.invalid", "", ""), "", "", payload, 403, "AccessDenied")
	digest := md5.Sum(payload)
	checksum := base64.StdEncoding.EncodeToString(digest[:])
	put := sign("PUT", "v2-upload.txt", "local-admin@example.invalid", "text/plain", checksum)
	call("PUT", put, "changed", checksum, payload, 403, "SignatureDoesNotMatch")
	call("PUT", put, "", checksum, payload, 403, "SignatureDoesNotMatch")
	call("PUT", put, "text/plain", "", payload, 403, "SignatureDoesNotMatch")
	call("PUT", put, "text/plain", checksum, []byte("wrong body"), 400, "BadDigest")
	call("GET", put, "text/plain", checksum, nil, 403, "SignatureDoesNotMatch")
	call("PUT", put, "text/plain", checksum, payload, 200, "")
	get = sign("GET", "v2-upload.txt", "local-admin@example.invalid", "", "")
	if got := call("GET", get, "", "", nil, 200, ""); !bytes.Equal(got, payload) {
		t.Fatalf("upload readback %q", got)
	}
}
