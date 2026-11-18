package gcsmimic_test

import (
	"bytes"
	"context"
	"crypto/tls"
	"crypto/x509"
	"errors"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"slices"
	"strings"
	"testing"
	"time"

	"cloud.google.com/go/storage"
	"golang.org/x/oauth2"
	"google.golang.org/api/option"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/credentials"
	"google.golang.org/grpc/status"
)

// grpcTLSEndpoint returns the loopback gRPC endpoint named by variable and the CA that signs the
// listener's certificate, or skips the test when scripts/sdk-go.sh did not provide them.
func grpcTLSEndpoint(t *testing.T, variable string) (string, *x509.CertPool) {
	t.Helper()
	endpoint := os.Getenv(variable)
	caFile := os.Getenv("GCSM_GRPC_CA_CERT")
	if endpoint == "" || caFile == "" {
		t.Skipf("%s or GCSM_GRPC_CA_CERT is unset", variable)
	}
	host, _, err := net.SplitHostPort(endpoint)
	if err != nil || net.ParseIP(host) == nil || !net.ParseIP(host).IsLoopback() {
		t.Fatalf("gRPC TLS endpoint must be a loopback IP:PORT: %q", endpoint)
	}
	config, err := emulatorTLSConfig(caFile)
	if err != nil {
		t.Fatal(err)
	}
	return endpoint, config.RootCAs
}

// newGRPCTLSClient builds the official gRPC client the way an application reaches Cloud Storage
// over TLS, not through STORAGE_EMULATOR_HOST_GRPC: the pinned SDK turns that variable into a
// plaintext channel without credentials. GOOGLE_CLOUD_DISABLE_DIRECT_PATH keeps the client from
// trying DirectPath, whose GCE detection may query the metadata server, and client metrics stay
// off so nothing is exported to Cloud Monitoring.
func newGRPCTLSClient(ctx context.Context, t *testing.T, endpoint string, roots *x509.CertPool, auth option.ClientOption, extra ...option.ClientOption) *storage.Client {
	t.Helper()
	t.Setenv("STORAGE_EMULATOR_HOST_GRPC", "")
	t.Setenv("GOOGLE_CLOUD_DISABLE_DIRECT_PATH", "true")
	transport := credentials.NewTLS(&tls.Config{MinVersion: tls.VersionTLS12, RootCAs: roots})
	options := append([]option.ClientOption{
		option.WithEndpoint(endpoint),
		option.WithGRPCDialOption(grpc.WithTransportCredentials(transport)),
		auth,
		storage.WithDisabledClientMetrics(),
	}, extra...)
	client, err := storage.NewGRPCClient(ctx, options...)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { client.Close() })
	return client
}

// grpcWrite writes payload with the SDK's default resumable Writer, the gRPC write path the
// emulator supports.
func grpcWrite(ctx context.Context, object *storage.ObjectHandle, payload []byte) (*storage.ObjectAttrs, error) {
	writer := object.NewWriter(ctx)
	writer.ContentType = "application/octet-stream"
	_, writeErr := writer.Write(payload)
	if closeErr := writer.Close(); closeErr != nil {
		return nil, closeErr
	}
	return writer.Attrs(), writeErr
}

// TestGRPCTLS writes, reads and lists through the official client over TLS on the default
// grpc_scheme=auto listener, including a bidi read and an appendable write, and checks that JSON
// sees what the TLS calls wrote.
func TestGRPCTLS(t *testing.T) {
	endpoint, roots := grpcTLSEndpoint(t, "GCSM_GRPC_TLS_ENDPOINT")
	ctx, jsonClient, _, _ := newEmulatorClient(t)
	bucket := resumableBucket(t, ctx, jsonClient, "grpc-tls")
	ctx, cancel := context.WithTimeout(ctx, 20*time.Second)
	defer cancel()
	client := newGRPCTLSClient(ctx, t, endpoint, roots, option.WithoutAuthentication())
	grpcBucket := client.Bucket(bucket.BucketName())
	payload := bytes.Repeat([]byte("over TLS "), 4096)
	resumableObject(t, bucket, "tls/object")
	written, err := grpcWrite(ctx, grpcBucket.Object("tls/object"), payload)
	if err != nil || written.Size != int64(len(payload)) {
		t.Fatalf("WriteObject over TLS: %+v %v", written, err)
	}
	got, err := readAll(ctx, grpcBucket.Object("tls/object"))
	if err != nil || !bytes.Equal(got, payload) {
		t.Fatalf("ReadObject over TLS: %d bytes, %v", len(got), err)
	}
	names, err := listObjectNames(ctx, grpcBucket)
	if err != nil || !slices.Contains(names, "tls/object") {
		t.Fatalf("ListObjects over TLS: %v %v", names, err)
	}
	attrs, err := bucket.Object("tls/object").Attrs(ctx)
	if err != nil || attrs.Generation != written.Generation {
		t.Fatalf("JSON attrs of the TLS write: %+v %v", attrs, err)
	}

	bidi := newGRPCTLSClient(ctx, t, endpoint, roots, option.WithoutAuthentication(), storage.WithGRPCBidiReads())
	downloader, err := bidi.Bucket(bucket.BucketName()).Object("tls/object").NewMultiRangeDownloader(ctx)
	if err != nil {
		t.Fatal(err)
	}
	var tail bytes.Buffer
	done := make(chan error, 1)
	downloader.Add(&tail, 9, 18, func(_, _ int64, err error) { done <- err })
	downloader.Wait()
	if err := <-done; err != nil || !bytes.Equal(tail.Bytes(), payload[9:27]) {
		t.Fatalf("BidiReadObject over TLS: %q %v", tail.String(), err)
	}
	if err := downloader.Close(); err != nil {
		t.Fatal(err)
	}

	resumableObject(t, bucket, "tls/appendable")
	writer := grpcBucket.Object("tls/appendable").NewWriter(ctx)
	writer.Append = true
	writer.FinalizeOnClose = true
	if err := appendWrite(ctx, writer, []byte("appended over TLS")); err != nil {
		t.Fatal(err)
	}
	appended, err := readAll(ctx, bucket.Object("tls/appendable"))
	if err != nil || string(appended) != "appended over TLS" {
		t.Fatalf("JSON read of the appendable TLS write: %q %v", appended, err)
	}
}

// TestGRPCTLSStrict shows that official call credentials flow over TLS: a static token source
// authenticates as the strict profile's administrator, and a client without credentials is
// UNAUTHENTICATED.
func TestGRPCTLSStrict(t *testing.T) {
	endpoint, roots := grpcTLSEndpoint(t, "GCSM_STRICT_GRPC_TLS_ENDPOINT")
	ctx, jsonClient := newStrictClient(t, strictAdminToken)
	bucket := resumableBucket(t, ctx, jsonClient, strictProject)
	ctx, cancel := context.WithTimeout(ctx, 15*time.Second)
	defer cancel()
	admin := newGRPCTLSClient(ctx, t, endpoint, roots,
		option.WithTokenSource(oauth2.StaticTokenSource(&oauth2.Token{AccessToken: strictAdminToken})))
	object := admin.Bucket(bucket.BucketName()).Object("private")
	resumableObject(t, bucket, "private")
	if _, err := grpcWrite(ctx, object, []byte("private")); err != nil {
		t.Fatal(err)
	}
	got, err := readAll(ctx, object)
	if err != nil || string(got) != "private" {
		t.Fatalf("token-source read over TLS: %q %v", got, err)
	}
	anonymous := newGRPCTLSClient(ctx, t, endpoint, roots, option.WithoutAuthentication())
	_, err = anonymous.Bucket(bucket.BucketName()).Object("private").Attrs(ctx)
	if status.Code(err) != codes.Unauthenticated {
		t.Fatalf("anonymous call over TLS must be UNAUTHENTICATED: %v", err)
	}
}

// TestGRPCTLSUntrustedCA checks that the client verifies the listener: trusting an unrelated CA
// fails the handshake instead of reaching the emulator.
func TestGRPCTLSUntrustedCA(t *testing.T) {
	endpoint, _ := grpcTLSEndpoint(t, "GCSM_GRPC_TLS_ENDPOINT")
	other := httptest.NewTLSServer(http.NotFoundHandler())
	defer other.Close()
	roots := x509.NewCertPool()
	roots.AddCert(other.Certificate())
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	client := newGRPCTLSClient(ctx, t, endpoint, roots, option.WithoutAuthentication())
	bucket := client.Bucket("grpc-tls-untrusted").Retryer(storage.WithPolicy(storage.RetryNever))
	_, err := bucket.Attrs(ctx)
	if err == nil || errors.Is(err, storage.ErrBucketNotExist) || status.Code(err) != codes.Unavailable ||
		!strings.Contains(err.Error(), "certificate") {
		t.Fatalf("an untrusted server certificate must fail the handshake: %v", err)
	}
}
