package gcsmimic_test

import (
	"bytes"
	"context"
	"crypto/rand"
	"encoding/json"
	"fmt"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"slices"
	"strings"
	"testing"
	"time"

	"cloud.google.com/go/storage"
)

// scripts/sdk-go.sh starts a third server for local development: strict mode with the built-in
// test signer, signed_host_port=lenient and a trusted listener whose requests without credentials
// act as localAppPrincipal. Its identity file grants that identity roles/storage.admin and the test
// signer roles/storage.objectUser on strictProject.
const (
	localAppPrincipal = "local-app@example.invalid"
	testSignerID      = "test-signer@gcs-mimic.invalid"
)

// trustedSetup is a bucket that a client without credentials created through the trusted listener,
// so the trusted principal owns it. The bucket keeps the default policy, which grants nothing to
// allUsers.
type trustedSetup struct {
	ctx        context.Context
	httpClient *http.Client
	bucket     *storage.BucketHandle
	main       *url.URL
	trusted    *url.URL
}

func newTrustedSetup(t *testing.T, label string) trustedSetup {
	t.Helper()
	rawMain, rawTrusted := os.Getenv("GCS_MIMIC_LOCAL_STRICT_ENDPOINT"), os.Getenv("GCS_MIMIC_TRUSTED_ENDPOINT")
	if rawMain == "" || rawTrusted == "" {
		t.Skip("GCS_MIMIC_LOCAL_STRICT_ENDPOINT or GCS_MIMIC_TRUSTED_ENDPOINT is unset; scripts/sdk-go.sh starts a strict server with the test signer, signed_host_port=lenient and a trusted listener")
	}
	mainURL, err := emulatorEndpoint(rawMain, os.Getenv("GCS_MIMIC_ALLOWED_HOSTS"))
	if err != nil {
		t.Fatal(err)
	}
	ctx, client, httpClient, trustedURL := newEmulatorHostClient(t, rawTrusted)
	var nonce [8]byte
	if _, err := rand.Read(nonce[:]); err != nil {
		t.Fatal(err)
	}
	bucket := client.Bucket(fmt.Sprintf("sdk-go-trusted-%s-%x", label, nonce))
	if err := bucket.Create(ctx, strictProject, nil); err != nil {
		t.Fatalf("create bucket without credentials on the trusted listener: %v", err)
	}
	removeBucketAfterTest(t, bucket)
	return trustedSetup{ctx: ctx, httpClient: httpClient, bucket: bucket, main: mainURL, trusted: trustedURL}
}

// testSignerKey runs `gcs-mimic test-signer credentials`, the service account file that binary and
// image users get without a repository checkout, and returns its signer and private key.
func testSignerKey(t *testing.T) (string, []byte) {
	t.Helper()
	binary := os.Getenv("GCS_MIMIC_BINARY")
	if binary == "" {
		t.Skip("GCS_MIMIC_BINARY is unset; scripts/sdk-go.sh exports the emulator binary that prints the test signer credentials")
	}
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	output, err := exec.CommandContext(ctx, binary, "test-signer", "credentials").Output()
	if err != nil {
		t.Fatalf("gcs-mimic test-signer credentials: %v", err)
	}
	var credentials struct {
		Type        string `json:"type"`
		ClientEmail string `json:"client_email"`
		PrivateKey  string `json:"private_key"`
	}
	if err := json.Unmarshal(output, &credentials); err != nil {
		t.Fatalf("decode test signer credentials: %v", err)
	}
	if credentials.Type != "service_account" || credentials.ClientEmail != testSignerID || !strings.Contains(credentials.PrivateKey, "PRIVATE KEY") {
		t.Fatalf("unexpected test signer credentials: type=%q, client_email=%q", credentials.Type, credentials.ClientEmail)
	}
	return credentials.ClientEmail, []byte(credentials.PrivateKey)
}

// requestStatus sends one request and checks its status; a 403 must be SignatureDoesNotMatch.
func requestStatus(t *testing.T, ctx context.Context, client *http.Client, method, target, contentType, body string, want int) []byte {
	t.Helper()
	headers := http.Header{}
	if contentType != "" {
		headers.Set("Content-Type", contentType)
	}
	_, data := migrationHTTP(t, ctx, client, method, target, body, headers, want)
	if want == http.StatusForbidden && !bytes.Contains(data, []byte("<Code>SignatureDoesNotMatch</Code>")) {
		t.Fatalf("%s: unexpected XML error: %s", method, data)
	}
	return data
}

// SPEC-GO-SIGNER-PORT: the pinned SDK drops the explicit port from the Host it signs. Under
// signed_host_port=lenient its unmodified URLs for host:port pass; every other signed part stays
// bound.
func TestBuiltInSignerURLsPassUnderLenientHostPort(t *testing.T) {
	setup := newTrustedSetup(t, "signed")
	signer, key := testSignerKey(t)
	ctx, bucket, endpoint := setup.ctx, setup.bucket, setup.main
	payload := []byte("lenient fixture\x00\xff")
	if _, err := strictUpload(ctx, bucket.Object("dir/fixture.bin"), payload); err != nil {
		t.Fatalf("upload fixture through the trusted listener: %v", err)
	}
	client := emulatorHTTPClient(t, endpoint)
	sign := func(method, name, contentType string) string {
		t.Helper()
		signed, err := storage.SignedURL(bucket.BucketName(), name, &storage.SignedURLOptions{
			Scheme: storage.SigningSchemeV4, GoogleAccessID: signer, PrivateKey: key,
			Method: method, ContentType: contentType, Expires: time.Now().Add(5 * time.Minute),
			Hostname: endpoint.Host, Insecure: endpoint.Scheme == "http",
		})
		if err != nil {
			t.Fatal(err)
		}
		// The URL keeps the port; only the Host the SDK signs lacks it.
		if u, err := url.Parse(signed); err != nil || u.Host != endpoint.Host {
			t.Fatalf("SDK URL %q does not name %s: %v", signed, endpoint.Host, err)
		}
		return signed
	}
	get := sign("GET", "dir/fixture.bin", "")
	if got := requestStatus(t, ctx, client, "GET", get, "", "", http.StatusOK); !bytes.Equal(got, payload) {
		t.Fatalf("signed GET: %q, want %q", got, payload)
	}
	requestStatus(t, ctx, client, "GET", get+"&tampered=1", "", "", http.StatusForbidden)

	const name, uploaded = "dir/signed-upload.txt", "lenient signed bytes"
	put := sign("PUT", name, "text/plain")
	requestStatus(t, ctx, client, "PUT", put, "text/plain", uploaded, http.StatusOK)
	requestStatus(t, ctx, client, "PUT", put, "text/html", "changed", http.StatusForbidden)
	requestStatus(t, ctx, client, "PUT", put+"&tampered=1", "text/plain", "changed", http.StatusForbidden)
	requestStatus(t, ctx, client, "GET", put, "text/plain", "", http.StatusForbidden)
	if got := requestStatus(t, ctx, client, "GET", sign("GET", name, ""), "", "", http.StatusOK); string(got) != uploaded {
		t.Fatalf("signed GET after PUT: %q, want %q", got, uploaded)
	}
	attrs, err := bucket.Object(name).Attrs(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if attrs.ContentType != "text/plain" || attrs.Size != int64(len(uploaded)) || attrs.Owner != "user-"+testSignerID {
		t.Fatalf("signed upload attrs: content type %q, size %d, owner %q", attrs.ContentType, attrs.Size, attrs.Owner)
	}

	t.Run("ExactServerNamesTheSetting", func(t *testing.T) {
		// The exact server registers the same published key for local-admin@example.invalid.
		ctx, bucket, fixture, _ := strictFixture(t, "exact-port")
		exact, err := emulatorEndpoint(os.Getenv("GCS_MIMIC_STRICT_ENDPOINT"), os.Getenv("GCS_MIMIC_ALLOWED_HOSTS"))
		if err != nil {
			t.Fatal(err)
		}
		signed, err := storage.SignedURL(bucket.BucketName(), fixture.Name, &storage.SignedURLOptions{
			Scheme: storage.SigningSchemeV4, GoogleAccessID: "local-admin@example.invalid", PrivateKey: key,
			Method: "GET", Expires: time.Now().Add(5 * time.Minute), Hostname: exact.Host, Insecure: exact.Scheme == "http",
		})
		if err != nil {
			t.Fatal(err)
		}
		body := requestStatus(t, ctx, emulatorHTTPClient(t, exact), "GET", signed, "", "", http.StatusForbidden)
		if !bytes.Contains(body, []byte("set signed_host_port=lenient to accept it")) {
			t.Fatalf("exact rejection does not name signed_host_port=lenient: %s", body)
		}
	})
}

// DESIGN-101: an unmodified client with STORAGE_EMULATOR_HOST and no credentials acts as the
// trusted principal on the trusted listener, while the main listener keeps it anonymous.
func TestTrustedListenerServesTheEmulatorHostClient(t *testing.T) {
	setup := newTrustedSetup(t, "client")
	ctx, bucket := setup.ctx, setup.bucket
	policy, err := bucket.IAM().Policy(ctx)
	if err != nil {
		t.Fatalf("bucket policy: %v", err)
	}
	for _, role := range policy.Roles() {
		for _, member := range policy.Members(role) {
			if member == "allUsers" || member == "allAuthenticatedUsers" {
				t.Fatalf("bucket grants %s to %s", role, member)
			}
		}
	}
	object := bucket.Object("dir/trusted.bin")
	payload := []byte("trusted listener\x00\xff")
	written, err := strictUpload(ctx, object, payload)
	if err != nil {
		t.Fatalf("upload: %v", err)
	}
	if written.Owner != "user-"+localAppPrincipal {
		t.Fatalf("owner %q, want the trusted principal", written.Owner)
	}
	// The client keeps the SDK's default XML media reads.
	readObject(t, ctx, object, payload, written.Generation)

	anonymous := emulatorHTTPClient(t, setup.main)
	for _, target := range []string{
		setup.main.String() + "/storage/v1/b/" + bucket.BucketName() + "/o/" + url.PathEscape(object.ObjectName()),
		setup.main.String() + "/storage/v1/b/" + bucket.BucketName() + "/o/" + url.PathEscape(object.ObjectName()) + "?alt=media",
		setup.main.String() + "/" + bucket.BucketName() + "/" + object.ObjectName(),
	} {
		migrationHTTP(t, ctx, anonymous, "GET", target, "", http.Header{}, http.StatusUnauthorized)
	}

	resumableClient, log := recordedResumableClient(t, ctx, setup.httpClient, false)
	resumable := resumableClient.Bucket(bucket.BucketName()).Object("dir/resumable.bin")
	large := bytes.Repeat([]byte("trusted\x00\xff"), (2*resumableChunkSize+37)/9+1)[:2*resumableChunkSize+37]
	writer := resumable.If(storage.Conditions{DoesNotExist: true}).NewWriter(ctx)
	writer.ChunkSize = resumableChunkSize
	writer.ContentType = "application/x-trusted-resumable"
	if _, err := writer.Write(large); err != nil {
		t.Fatal(err)
	}
	if err := writer.Close(); err != nil {
		t.Fatalf("resumable upload: %v", err)
	}
	log.mu.Lock()
	location, chunks := log.location, len(log.requests)
	sameLocation := true
	for _, request := range log.requests {
		sameLocation = sameLocation && request.URL.String() == location
	}
	log.mu.Unlock()
	if !strings.HasPrefix(location, setup.trusted.String()+"/upload/storage/v1/") || chunks != 3 || !sameLocation {
		t.Fatalf("resumable Location %q with %d chunks (all to it: %v); want three chunks on %s", location, chunks, sameLocation, setup.trusted)
	}
	attrs, err := bucket.Object(resumable.ObjectName()).Attrs(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if attrs.Owner != "user-"+localAppPrincipal || attrs.Size != int64(len(large)) || attrs.Generation != writer.Attrs().Generation {
		t.Fatalf("resumable object: owner %q, size %d, generation %d", attrs.Owner, attrs.Size, attrs.Generation)
	}
	readObject(t, ctx, bucket.Object(resumable.ObjectName()), large, attrs.Generation)

	names, err := listObjectNames(ctx, bucket)
	if err != nil || !slices.Equal(names, []string{resumable.ObjectName(), object.ObjectName()}) {
		t.Fatalf("list objects: %v, %v", names, err)
	}
	for _, name := range names {
		target := bucket.Object(name)
		if err := target.Delete(ctx); err != nil {
			t.Fatalf("delete %s: %v", name, err)
		}
		requireObjectMissing(t, ctx, target)
	}
}
