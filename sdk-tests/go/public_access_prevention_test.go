package gcsmimic_test

import (
	"bytes"
	"context"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"strings"
	"testing"
	"time"

	"cloud.google.com/go/storage"
)

func TestPublicAccessPrevention(t *testing.T) {
	ctx, bucket, object, payload := strictFixture(t, "pap")
	attrs, err := bucket.Attrs(ctx)
	if err != nil || attrs.PublicAccessPrevention != storage.PublicAccessPreventionInherited {
		t.Fatalf("default PAP: attrs=%+v error=%v", attrs, err)
	}
	policy, err := bucket.IAM().Policy(ctx)
	if err != nil {
		t.Fatal(err)
	}
	policy.Add("allUsers", "roles/storage.objectViewer")
	if err := bucket.IAM().SetPolicy(ctx, policy); err != nil {
		t.Fatal(err)
	}
	_, anonymous := newStrictClient(t, "")
	readObject(t, ctx, anonymous.Bucket(bucket.BucketName()).Object(object.Name), payload, object.Generation)
	attrs, err = bucket.If(storage.BucketConditions{MetagenerationMatch: attrs.MetaGeneration}).Update(ctx,
		storage.BucketAttrsToUpdate{PublicAccessPrevention: storage.PublicAccessPreventionEnforced})
	if err != nil || attrs.PublicAccessPrevention != storage.PublicAccessPreventionEnforced {
		t.Fatalf("set PAP: attrs=%+v error=%v", attrs, err)
	}
	got, err := bucket.Attrs(ctx)
	if err != nil || got.PublicAccessPrevention != storage.PublicAccessPreventionEnforced {
		t.Fatalf("get PAP: attrs=%+v error=%v", got, err)
	}
	_, err = anonymous.Bucket(bucket.BucketName()).Object(object.Name).NewReader(ctx)
	requireStrictError(t, err, 401, "required")
	stored, err := bucket.IAM().Policy(ctx)
	if err != nil || !stored.HasRole("allUsers", "roles/storage.objectViewer") {
		t.Fatalf("stored public binding: policy=%v error=%v", stored, err)
	}
	requireStrictError(t, bucket.IAM().SetPolicy(ctx, stored), 412, "conditionNotMet")

	endpoint, err := emulatorEndpoint(os.Getenv("GCS_MIMIC_STRICT_ENDPOINT"), os.Getenv("GCS_MIMIC_ALLOWED_HOSTS"))
	if err != nil {
		t.Fatal(err)
	}
	key, err := os.ReadFile("../../fixtures/signing/test-only-private.pem")
	if err != nil {
		t.Fatal(err)
	}
	// The pinned Go signer omits non-default ports; bind a final default-port URL and guard its local dial mapping.
	portless := *endpoint
	portless.Host = endpoint.Hostname()
	if strings.Contains(portless.Host, ":") {
		portless.Host = "[" + portless.Host + "]"
	}
	transport := &http.Transport{TLSClientConfig: trustedEmulatorTLS(t), Proxy: nil, ResponseHeaderTimeout: 5 * time.Second,
		DialContext: func(ctx context.Context, network, address string) (net.Conn, error) {
			port := "80"
			if endpoint.Scheme == "https" {
				port = "443"
			}
			if address != net.JoinHostPort(endpoint.Hostname(), port) {
				return nil, fmt.Errorf("refusing unexpected dial target %s", address)
			}
			return (&net.Dialer{Timeout: 5 * time.Second}).DialContext(ctx, network, endpoint.Host)
		}}
	t.Cleanup(transport.CloseIdleConnections)
	client := &http.Client{Transport: emulatorTransport{endpoint: &portless, base: transport}, Timeout: 10 * time.Second,
		CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}
	for _, method := range []string{"GET", "PUT"} {
		name := object.Name
		var body io.Reader
		if method == "PUT" {
			name = "pap-signed-put"
			body = bytes.NewReader(payload)
		}
		signed, err := storage.SignedURL(bucket.BucketName(), name, &storage.SignedURLOptions{
			Scheme: storage.SigningSchemeV4, GoogleAccessID: "local-admin@example.invalid", PrivateKey: key,
			Method: method, Expires: time.Now().Add(5 * time.Minute), Hostname: portless.Host, Insecure: endpoint.Scheme == "http"})
		if err != nil {
			t.Fatal(err)
		}
		req, err := http.NewRequestWithContext(ctx, method, signed, body)
		if err != nil {
			t.Fatal(err)
		}
		res, err := client.Do(req)
		if err != nil {
			t.Fatal(err)
		}
		got, readErr := io.ReadAll(io.LimitReader(res.Body, 64*1024))
		closeErr := res.Body.Close()
		if res.StatusCode != 200 || readErr != nil || closeErr != nil {
			t.Fatalf("signed %s: status=%d body=%s read=%v close=%v", method, res.StatusCode, got, readErr, closeErr)
		}
		if method == "GET" && !bytes.Equal(got, payload) {
			t.Fatalf("signed bytes: %q", got)
		}
	}
	uploaded, err := bucket.Object("pap-signed-put").Attrs(ctx)
	if err != nil {
		t.Fatal(err)
	}
	readObject(t, ctx, bucket.Object("pap-signed-put"), payload, uploaded.Generation)
	attrs, err = bucket.Update(ctx, storage.BucketAttrsToUpdate{PublicAccessPrevention: storage.PublicAccessPreventionInherited})
	if err != nil || attrs.PublicAccessPrevention != storage.PublicAccessPreventionInherited {
		t.Fatalf("disable PAP: attrs=%+v error=%v", attrs, err)
	}
	readObject(t, ctx, anonymous.Bucket(bucket.BucketName()).Object(object.Name), payload, object.Generation)

	created := bucket.BucketName() + "-enforced"
	_, admin := newStrictClient(t, strictAdminToken)
	other := admin.Bucket(created)
	if err := other.Create(ctx, strictProject, &storage.BucketAttrs{PublicAccessPrevention: storage.PublicAccessPreventionEnforced}); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := other.Delete(ctx); err != nil {
			t.Errorf("delete enforced bucket: %v", err)
		}
	})
	attrs, err = other.Attrs(ctx)
	if err != nil || attrs.PublicAccessPrevention != storage.PublicAccessPreventionEnforced {
		t.Fatalf("insert PAP: attrs=%+v error=%v", attrs, err)
	}
}
