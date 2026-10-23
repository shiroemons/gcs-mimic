package gcsmimic_test

import (
	"bytes"
	"context"
	"crypto"
	"crypto/rand"
	"crypto/rsa"
	"crypto/sha256"
	"crypto/x509"
	"encoding/hex"
	"encoding/pem"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"os"
	"sort"
	"strings"
	"testing"
	"time"

	"cloud.google.com/go/storage"
)

// The key is deliberately public test material; signing never calls IAM or ADC.
func TestV4RSASignedDownload(t *testing.T) {
	ctx, bucket, attrs, payload := strictFixture(t, "signed")
	endpoint, err := emulatorEndpoint(os.Getenv("GCS_MIMIC_STRICT_ENDPOINT"), os.Getenv("GCS_MIMIC_ALLOWED_HOSTS"))
	if err != nil {
		t.Fatal(err)
	}
	key, err := os.ReadFile("../../fixtures/signing/test-only-private.pem")
	if err != nil {
		t.Fatal(err)
	}
	transport := &http.Transport{TLSClientConfig: trustedEmulatorTLS(t), Proxy: nil, MaxConnsPerHost: 2, ResponseHeaderTimeout: 5 * time.Second}
	t.Cleanup(transport.CloseIdleConnections)
	client := &http.Client{Transport: emulatorTransport{endpoint: endpoint, base: transport}, Timeout: 10 * time.Second,
		CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}
	block, _ := pem.Decode(key)
	if block == nil {
		t.Fatal("missing fixture key PEM")
	}
	parsed, err := x509.ParsePKCS8PrivateKey(block.Bytes)
	if err != nil {
		t.Fatal(err)
	}
	privateKey, ok := parsed.(*rsa.PrivateKey)
	if !ok {
		t.Fatal("fixture key is not RSA")
	}
	sign := func(method, name string, query url.Values, headers []string) string {
		t.Helper()
		var sdkMessage []byte
		signed, err := storage.SignedURL(bucket.BucketName(), name, &storage.SignedURLOptions{
			Scheme: storage.SigningSchemeV4, GoogleAccessID: "local-viewer@example.invalid",
			SignBytes: func(message []byte) ([]byte, error) {
				sdkMessage = append([]byte(nil), message...)
				digest := sha256.Sum256(message)
				return rsa.SignPKCS1v15(rand.Reader, privateKey, crypto.SHA256, digest[:])
			},
			Method: method, Expires: time.Now().Add(5 * time.Minute), Hostname: endpoint.Host,
			Insecure: endpoint.Scheme == "http", QueryParameters: query, Headers: headers,
		})
		if err != nil {
			t.Fatal(err)
		}
		// storage/v1.70.0 uses URL.Hostname() in canonical headers, dropping non-default ports.
		// Keep the server strict and cross-check with an independent Go canonical request that
		// changes only that SDK defect. The unmodified SDK signature is rejected below.
		u, err := url.Parse(signed)
		if err != nil {
			t.Fatal(err)
		}
		q := u.Query()
		q.Del("X-Goog-Signature")
		canonicalHeaders := append([]string{"host:" + u.Host}, headers...)
		sort.Strings(canonicalHeaders)
		payloadHash := "UNSIGNED-PAYLOAD"
		for _, h := range headers {
			if strings.HasPrefix(h, "x-goog-content-sha256:") {
				payloadHash = strings.TrimPrefix(h, "x-goog-content-sha256:")
			}
		}
		canonical := method + "\n" + u.EscapedPath() + "\n" + strings.ReplaceAll(q.Encode(), "+", "%20") + "\n" +
			strings.Join(canonicalHeaders, "\n") + "\n\n" + q.Get("X-Goog-SignedHeaders") + "\n" + payloadHash
		hash := sha256.Sum256([]byte(canonical))
		_, scope, ok := strings.Cut(q.Get("X-Goog-Credential"), "/")
		if !ok {
			t.Fatal("missing credential scope")
		}
		toSign := "GOOG4-RSA-SHA256\n" + q.Get("X-Goog-Date") + "\n" + scope + "\n" + hex.EncodeToString(hash[:])
		portlessCanonical := strings.Replace(canonical, "host:"+u.Host+"\n", "host:"+u.Hostname()+"\n", 1)
		portlessHash := sha256.Sum256([]byte(portlessCanonical))
		portlessMessage := "GOOG4-RSA-SHA256\n" + q.Get("X-Goog-Date") + "\n" + scope + "\n" + hex.EncodeToString(portlessHash[:])
		if !bytes.Equal(sdkMessage, []byte(portlessMessage)) {
			t.Fatalf("independent canonical request differs from SDK beyond its host-port defect: got %q, want %q", sdkMessage, portlessMessage)
		}
		digest := sha256.Sum256([]byte(toSign))
		signature, err := rsa.SignPKCS1v15(rand.Reader, privateKey, crypto.SHA256, digest[:])
		if err != nil {
			t.Fatal(err)
		}
		q.Set("X-Goog-Signature", hex.EncodeToString(signature))
		u.RawQuery = q.Encode()
		return u.String()
	}
	request := func(method, signed, host, rangeHeader string, wantStatus int, want []byte) {
		t.Helper()
		req, err := http.NewRequestWithContext(ctx, method, signed, nil)
		if err != nil {
			t.Fatal(err)
		}
		req.Header.Set("x-required", "one")
		if strings.Contains(req.URL.Query().Get("X-Goog-SignedHeaders"), "x-goog-content-sha256") {
			req.Header.Set("x-goog-content-sha256", "UNSIGNED-PAYLOAD")
		}
		if host != "" {
			req.Host = host
		}
		if rangeHeader != "" {
			req.Header.Set("Range", rangeHeader)
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
		if res.StatusCode != wantStatus {
			t.Fatalf("want %d, got %d: %s", wantStatus, res.StatusCode, body)
		}
		if want != nil && !bytes.Equal(body, want) {
			t.Fatalf("body %q, want %q", body, want)
		}
		if wantStatus == http.StatusForbidden && method != "HEAD" && !strings.Contains(string(body), "<Code>SignatureDoesNotMatch</Code>") {
			t.Fatalf("unexpected XML error: %s", body)
		}
	}
	query := url.Values{"generation": {fmt.Sprint(attrs.Generation)}}
	unmodified, err := storage.SignedURL(bucket.BucketName(), attrs.Name, &storage.SignedURLOptions{
		Scheme: storage.SigningSchemeV4, GoogleAccessID: "local-viewer@example.invalid", PrivateKey: key,
		Method: "GET", Expires: time.Now().Add(5 * time.Minute), Hostname: endpoint.Host, Insecure: endpoint.Scheme == "http",
	})
	if err != nil {
		t.Fatal(err)
	}
	request("GET", unmodified, "", "", http.StatusForbidden, nil)
	// A guarded client-side dialer maps a default-port authority to the isolated ephemeral
	// listener, proving unmodified SDK signing without opening privileged or external ports.
	portless := *endpoint
	portless.Host = endpoint.Hostname()
	if strings.Contains(portless.Host, ":") {
		portless.Host = "[" + portless.Host + "]"
	}
	defaultTransport := transport.Clone()
	defaultTransport.DialContext = func(ctx context.Context, network, address string) (net.Conn, error) {
		port := "80"
		if endpoint.Scheme == "https" {
			port = "443"
		}
		if address != net.JoinHostPort(endpoint.Hostname(), port) {
			return nil, fmt.Errorf("refusing unexpected dial target %s", address)
		}
		return (&net.Dialer{Timeout: 5 * time.Second}).DialContext(ctx, network, endpoint.Host)
	}
	t.Cleanup(defaultTransport.CloseIdleConnections)
	defaultClient := *client
	defaultClient.Transport = emulatorTransport{endpoint: &portless, base: defaultTransport}
	defaultURL, err := storage.SignedURL(bucket.BucketName(), attrs.Name, &storage.SignedURLOptions{
		Scheme: storage.SigningSchemeV4, GoogleAccessID: "local-viewer@example.invalid", PrivateKey: key,
		Method: "GET", Expires: time.Now().Add(5 * time.Minute), Hostname: portless.Host, Insecure: endpoint.Scheme == "http",
	})
	if err != nil {
		t.Fatal(err)
	}
	requestClient := client
	client = &defaultClient
	request("GET", defaultURL, "", "", http.StatusOK, payload)
	request("GET", defaultURL+"&tampered=1", "", "", http.StatusForbidden, nil)
	t.Run("ResponseOverrides", func(t *testing.T) {
		for _, method := range []string{"GET", "HEAD"} {
			signed, err := storage.SignedURL(bucket.BucketName(), attrs.Name, &storage.SignedURLOptions{
				Scheme: storage.SigningSchemeV4, GoogleAccessID: "local-viewer@example.invalid", PrivateKey: key,
				Method: method, Expires: time.Now().Add(5 * time.Minute), Hostname: portless.Host, Insecure: endpoint.Scheme == "http",
				QueryParameters: url.Values{"response-content-disposition": {`attachment; filename="Go + file.csv"`}, "response-content-type": {"text/csv"}},
			})
			if err != nil {
				t.Fatal(err)
			}
			req, err := http.NewRequestWithContext(ctx, method, signed, nil)
			if err != nil {
				t.Fatal(err)
			}
			res, err := defaultClient.Do(req)
			if err != nil {
				t.Fatal(err)
			}
			body, readErr := io.ReadAll(io.LimitReader(res.Body, 64*1024))
			closeErr := res.Body.Close()
			if readErr != nil || closeErr != nil {
				t.Fatalf("read/close: %v / %v", readErr, closeErr)
			}
			if res.StatusCode != 200 || res.Header.Get("Content-Disposition") != `attachment; filename="Go + file.csv"` || res.Header.Get("Content-Type") != "text/csv" {
				t.Fatalf("override response: %d %v %s", res.StatusCode, res.Header, body)
			}
			if method == "GET" && !bytes.Equal(body, payload) || method == "HEAD" && len(body) != 0 {
				t.Fatalf("unexpected body %q", body)
			}
			request(method, strings.Replace(signed, "text%2Fcsv", "text%2Fhtml", 1), "", "", http.StatusForbidden, nil)
		}
	})
	client = requestClient
	signed := sign("GET", attrs.Name, query, []string{"x-required:one"})
	request("GET", signed, "", "", http.StatusOK, payload)
	request("GET", signed, "", "bytes=1-4", http.StatusPartialContent, payload[1:5])
	request("HEAD", sign("HEAD", attrs.Name, query, nil), "", "", http.StatusOK, []byte{})
	request("GET", signed+"&added=1", "", "", http.StatusForbidden, nil)
	queryNames := sign("GET", attrs.Name, url.Values{"a": {"a b"}, "+": {"+"}, "é": {"日"}}, nil)
	request("GET", queryNames, "", "", http.StatusNotImplemented, nil)
	request("GET", queryNames+"&tampered=1", "", "", http.StatusForbidden, nil)
	request("GET", strings.Replace(signed, "generation="+fmt.Sprint(attrs.Generation), "generation=1", 1), "", "", http.StatusForbidden, nil)
	request("GET", signed, "wrong.invalid:4443", "", http.StatusForbidden, nil)
	request("GET", signed, endpoint.Host+"0", "", http.StatusForbidden, nil)
	request("HEAD", signed, "", "", http.StatusForbidden, []byte{})
	request("GET", sign("GET", attrs.Name, nil, []string{"x-goog-content-sha256:UNSIGNED-PAYLOAD"}), "", "", http.StatusOK, payload)
	for _, name := range []string{"é日/a b+%?#", "a//b/", "a/.../b"} {
		if _, err := strictUpload(ctx, bucket.Object(name), payload); err != nil {
			t.Fatal(err)
		}
		request("GET", sign("GET", name, nil, nil), "", "", http.StatusOK, payload)
	}

	t.Run("SignedUpload", func(t *testing.T) {
		name := "signed-upload.txt"
		sdkURL := func(method, host string) string {
			t.Helper()
			signed, err := storage.SignedURL(bucket.BucketName(), name, &storage.SignedURLOptions{
				Scheme: storage.SigningSchemeV4, GoogleAccessID: "local-admin@example.invalid", PrivateKey: key,
				Method: method, ContentType: "text/plain", Expires: time.Now().Add(5 * time.Minute),
				Hostname: host, Insecure: endpoint.Scheme == "http",
			})
			if err != nil {
				t.Fatal(err)
			}
			return signed
		}
		put := sdkURL("PUT", portless.Host)
		get := sdkURL("GET", portless.Host)
		call := func(method, signed, contentType string, want int) []byte {
			t.Helper()
			var input io.Reader
			if method == "PUT" {
				input = strings.NewReader("signed private bytes")
			}
			req, err := http.NewRequestWithContext(ctx, method, signed, input)
			if err != nil {
				t.Fatal(err)
			}
			if contentType != "" {
				req.Header.Set("Content-Type", contentType)
			}
			res, err := defaultClient.Do(req)
			if err != nil {
				t.Fatal(err)
			}
			body, readErr := io.ReadAll(io.LimitReader(res.Body, 64*1024))
			closeErr := res.Body.Close()
			if readErr != nil || closeErr != nil {
				t.Fatalf("read/close: %v / %v", readErr, closeErr)
			}
			if res.StatusCode != want {
				t.Fatalf("want %d, got %d: %s", want, res.StatusCode, body)
			}
			if want == http.StatusForbidden && !bytes.Contains(body, []byte("<Code>SignatureDoesNotMatch</Code>")) {
				t.Fatalf("unexpected XML error: %s", body)
			}
			return body
		}
		call("PUT", put, "text/plain", http.StatusOK)
		call("PUT", put, "changed", http.StatusForbidden)
		call("PUT", put, "", http.StatusForbidden)
		call("PUT", put+"&tampered=1", "text/plain", http.StatusForbidden)
		call("PUT", strings.Replace(put, "/signed-upload.txt?", "/other.txt?", 1), "text/plain", http.StatusForbidden)
		call("GET", put, "text/plain", http.StatusForbidden)
		call("PUT", get, "text/plain", http.StatusForbidden)
		if body := call("GET", get, "text/plain", http.StatusOK); string(body) != "signed private bytes" {
			t.Fatalf("unexpected bytes: %q", body)
		}
		before, err := bucket.Object(name).Attrs(ctx)
		if err != nil {
			t.Fatal(err)
		}
		digest := sha256.Sum256([]byte("signed private bytes"))
		digestHeader := hex.EncodeToString(digest[:])
		digestURL, err := storage.SignedURL(bucket.BucketName(), name, &storage.SignedURLOptions{
			Scheme: storage.SigningSchemeV4, GoogleAccessID: "local-admin@example.invalid", PrivateKey: key,
			Method: "PUT", ContentType: "text/plain", Headers: []string{"x-goog-content-sha256:" + digestHeader},
			Expires: time.Now().Add(5 * time.Minute), Hostname: portless.Host, Insecure: endpoint.Scheme == "http",
		})
		if err != nil {
			t.Fatal(err)
		}
		// The pinned SDK substitutes this header into the payload line; query signing requires UNSIGNED-PAYLOAD.
		for _, input := range []string{"signed private bytes", "changed bytes"} {
			req, err := http.NewRequestWithContext(ctx, "PUT", digestURL, strings.NewReader(input))
			if err != nil {
				t.Fatal(err)
			}
			req.Header.Set("Content-Type", "text/plain")
			req.Header.Set("x-goog-content-sha256", digestHeader)
			res, err := defaultClient.Do(req)
			if err != nil {
				t.Fatal(err)
			}
			body, readErr := io.ReadAll(io.LimitReader(res.Body, 64*1024))
			closeErr := res.Body.Close()
			if readErr != nil || closeErr != nil {
				t.Fatalf("read/close: %v / %v", readErr, closeErr)
			}
			if res.StatusCode != http.StatusForbidden || !bytes.Contains(body, []byte("<Code>SignatureDoesNotMatch</Code>")) {
				t.Fatalf("digest substitution returned %d: %s", res.StatusCode, body)
			}
			requireStrictObjectUnchanged(t, ctx, bucket, before, []byte("signed private bytes"))
		}
		unsigned, err := http.NewRequestWithContext(ctx, "GET", portless.String()+"/"+bucket.BucketName()+"/"+name, nil)
		if err != nil {
			t.Fatal(err)
		}
		res, err := defaultClient.Do(unsigned)
		if err != nil {
			t.Fatal(err)
		}
		res.Body.Close()
		if res.StatusCode != http.StatusUnauthorized {
			t.Fatalf("private object returned %d", res.StatusCode)
		}
		explicit, err := http.NewRequestWithContext(ctx, "PUT", sdkURL("PUT", endpoint.Host), strings.NewReader("rejected"))
		if err != nil {
			t.Fatal(err)
		}
		explicit.Header.Set("Content-Type", "text/plain")
		res, err = client.Do(explicit)
		if err != nil {
			t.Fatal(err)
		}
		res.Body.Close()
		if res.StatusCode != http.StatusForbidden {
			t.Fatalf("SDK port omission returned %d", res.StatusCode)
		}
	})
}
