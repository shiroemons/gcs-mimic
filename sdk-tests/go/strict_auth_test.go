package gcsmimic_test

import (
	"bytes"
	"context"
	"crypto/rand"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"net/http"
	"os"
	"slices"
	"strings"
	"testing"
	"time"

	"cloud.google.com/go/storage"
	"golang.org/x/oauth2"
	"google.golang.org/api/googleapi"
	"google.golang.org/api/iterator"
	"google.golang.org/api/option"
)

// These values come from config/identities.json, a local-only fixture registry. The tokens are
// deliberately public emulator test values, not Google credentials.
const (
	strictProject      = "local-project"
	strictAdminToken   = "local-admin-token"
	strictViewerToken  = "local-viewer-token"
	strictCreatorToken = "local-creator-token"
)

// newStrictClient returns a client for the strict-mode emulator. An empty token creates an
// anonymous client. The endpoint is always explicit and STORAGE_EMULATOR_HOST is cleared because
// the pinned SDK forces WithoutAuthentication whenever that variable is set, which would drop the
// bearer token. The SDK ignores credential options when an HTTP client is supplied, so the guarded
// transport attaches the same static token itself; WithTokenSource still keeps the SDK from
// attempting Application Default Credentials or metadata-server discovery.
func newStrictClient(t *testing.T, token string) (context.Context, *storage.Client) {
	t.Helper()
	ctx, client, _ := newStrictClientWithReadMode(t, token, true)
	return ctx, client
}

func newStrictClientWithReadMode(t *testing.T, token string, jsonReads bool) (context.Context, *storage.Client, *http.Client) {
	t.Helper()
	raw := os.Getenv("GCS_MIMIC_STRICT_ENDPOINT")
	if raw == "" {
		t.Skip("GCS_MIMIC_STRICT_ENDPOINT is unset; strict suite requires an explicit local emulator started with --auth-mode strict")
	}
	u, err := emulatorEndpoint(raw, os.Getenv("GCS_MIMIC_ALLOWED_HOSTS"))
	if err != nil {
		t.Fatal(err)
	}
	t.Setenv("STORAGE_EMULATOR_HOST", "")
	transport := &http.Transport{
		Proxy:                 nil,
		MaxConnsPerHost:       2,
		MaxIdleConns:          2,
		IdleConnTimeout:       5 * time.Second,
		ResponseHeaderTimeout: 5 * time.Second,
		DialContext: func(ctx context.Context, network, address string) (net.Conn, error) {
			host, port, err := net.SplitHostPort(address)
			if err != nil {
				return nil, err
			}
			// Resolve localhost ourselves so hosts-file changes cannot send tests off-machine.
			if host == "localhost" {
				address = net.JoinHostPort("127.0.0.1", port)
			}
			return (&net.Dialer{Timeout: 5 * time.Second}).DialContext(ctx, network, address)
		},
	}
	t.Cleanup(transport.CloseIdleConnections)
	var roundTripper http.RoundTripper = emulatorTransport{endpoint: u, base: transport}
	opts := []option.ClientOption{
		storage.WithDisabledClientMetrics(),
		option.WithEndpoint(strings.TrimSuffix(u.String(), "/") + "/storage/v1/"),
	}
	if jsonReads {
		opts = append(opts, storage.WithJSONReads())
	}
	if token == "" {
		opts = append(opts, option.WithoutAuthentication())
	} else {
		source := oauth2.StaticTokenSource(&oauth2.Token{AccessToken: token, TokenType: "Bearer"})
		roundTripper = &oauth2.Transport{Source: source, Base: roundTripper}
		opts = append(opts, option.WithTokenSource(source))
	}
	httpClient := &http.Client{
		Transport:     roundTripper,
		Timeout:       10 * time.Second,
		CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse },
	}
	opts = append(opts, option.WithHTTPClient(httpClient))
	ctx, cancel := context.WithTimeout(context.Background(), 45*time.Second)
	t.Cleanup(cancel)
	client, err := storage.NewClient(ctx, opts...)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := client.Close(); err != nil {
			t.Errorf("close client: %v", err)
		}
	})
	return ctx, client, httpClient
}

// requireStrictError checks the status code and the first error reason. JSON media reads return
// only the raw body in googleapi.Error, so the documented envelope is decoded when needed.
func requireStrictError(t *testing.T, err error, code int, reason string) {
	t.Helper()
	var apiError *googleapi.Error
	if !errors.As(err, &apiError) || apiError.Code != code {
		t.Fatalf("want googleapi.Error %d %s, got %v", code, reason, err)
	}
	items := apiError.Errors
	if len(items) == 0 {
		var envelope struct {
			Error struct {
				Errors []googleapi.ErrorItem `json:"errors"`
			} `json:"error"`
		}
		if decodeErr := json.Unmarshal([]byte(apiError.Body), &envelope); decodeErr != nil {
			t.Fatalf("decode error body %q: %v", apiError.Body, decodeErr)
		}
		items = envelope.Error.Errors
	}
	if len(items) == 0 || items[0].Reason != reason {
		t.Fatalf("want reason %q, got %+v (body %q)", reason, items, apiError.Body)
	}
}

func strictUpload(ctx context.Context, object *storage.ObjectHandle, payload []byte) (*storage.ObjectAttrs, error) {
	writer := object.NewWriter(ctx)
	writer.ChunkSize = 0
	writer.ContentType = "application/octet-stream"
	_, writeErr := writer.Write(payload)
	if closeErr := writer.Close(); closeErr != nil {
		return nil, closeErr
	}
	if writeErr != nil {
		return nil, writeErr
	}
	return writer.Attrs(), nil
}

func listObjectNames(ctx context.Context, bucket *storage.BucketHandle) ([]string, error) {
	it := bucket.Objects(ctx, nil)
	var names []string
	for len(names) < 100 {
		attrs, err := it.Next()
		if err == iterator.Done {
			return names, nil
		}
		if err != nil {
			return nil, err
		}
		names = append(names, attrs.Name)
	}
	return nil, errors.New("object listing did not terminate")
}

// strictFixture creates an admin-owned bucket in the fixture project holding one object, and
// removes both with the admin client after the test.
func strictFixture(t *testing.T, label string) (context.Context, *storage.BucketHandle, *storage.ObjectAttrs, []byte) {
	t.Helper()
	ctx, admin := newStrictClient(t, strictAdminToken)
	var nonce [8]byte
	if _, err := rand.Read(nonce[:]); err != nil {
		t.Fatal(err)
	}
	bucket := admin.Bucket(fmt.Sprintf("sdk-go-strict-%s-%x", label, nonce))
	if err := bucket.Create(ctx, strictProject, nil); err != nil {
		t.Fatalf("admin create bucket: %v", err)
	}
	t.Cleanup(func() {
		cleanupCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		names, err := listObjectNames(cleanupCtx, bucket)
		if err != nil {
			t.Errorf("cleanup list: %v", err)
		}
		for _, name := range names {
			if err := bucket.Object(name).Delete(cleanupCtx); err != nil && !errors.Is(err, storage.ErrObjectNotExist) {
				t.Errorf("cleanup object %s: %v", name, err)
			}
		}
		if err := bucket.Delete(cleanupCtx); err != nil && !errors.Is(err, storage.ErrBucketNotExist) {
			t.Errorf("cleanup bucket: %v", err)
		}
	})
	payload := []byte("strict fixture\x00\xff")
	attrs, err := strictUpload(ctx, bucket.Object("dir/fixture.bin"), payload)
	if err != nil {
		t.Fatalf("admin upload: %v", err)
	}
	return ctx, bucket, attrs, payload
}

// requireStrictObjectUnchanged reads the object as the admin and checks its generation and bytes.
func requireStrictObjectUnchanged(t *testing.T, ctx context.Context, bucket *storage.BucketHandle, want *storage.ObjectAttrs, payload []byte) {
	t.Helper()
	object := bucket.Object(want.Name)
	attrs, err := object.Attrs(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if attrs.Generation != want.Generation || attrs.Metageneration != want.Metageneration || len(attrs.Metadata) != len(want.Metadata) {
		t.Fatalf("object changed: got %+v, want %+v", attrs, want)
	}
	readObject(t, ctx, object, payload, want.Generation)
}

func TestStrictAuthAdminLifecycle(t *testing.T) {
	ctx, client := newStrictClient(t, strictAdminToken)
	var nonce [8]byte
	if _, err := rand.Read(nonce[:]); err != nil {
		t.Fatal(err)
	}
	name := fmt.Sprintf("sdk-go-strict-admin-%x", nonce)
	bucket := client.Bucket(name)
	if err := bucket.Create(ctx, strictProject, nil); err != nil {
		t.Fatalf("create bucket: %v", err)
	}
	bucketDeleted := false
	object := bucket.Object("dir/admin object.bin")
	t.Cleanup(func() {
		if bucketDeleted {
			return
		}
		cleanupCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		if err := object.Delete(cleanupCtx); err != nil && !errors.Is(err, storage.ErrObjectNotExist) {
			t.Errorf("cleanup object: %v", err)
		}
		if err := bucket.Delete(cleanupCtx); err != nil {
			t.Errorf("cleanup bucket: %v", err)
		}
	})
	t.Logf("strict endpoint; admin principal from the local fixture registry; project=%s", strictProject)
	bucketAttrs, err := bucket.Attrs(ctx)
	if err != nil {
		t.Fatalf("get bucket: %v", err)
	}
	if bucketAttrs.Name != name || bucketAttrs.MetaGeneration != 1 {
		t.Fatalf("unexpected bucket: %+v", bucketAttrs)
	}
	buckets := client.Buckets(ctx, strictProject)
	buckets.Prefix = name
	listed, err := buckets.Next()
	if err != nil || listed.Name != name {
		t.Fatalf("list buckets: got %+v, %v", listed, err)
	}

	payload := bytes.Repeat([]byte("strict\x00\xff"), 64)
	written, err := strictUpload(ctx, object, payload)
	if err != nil {
		t.Fatalf("upload: %v", err)
	}
	attrs, err := object.Attrs(ctx)
	if err != nil {
		t.Fatalf("object metadata: %v", err)
	}
	if attrs.Generation != written.Generation || attrs.Size != int64(len(payload)) || attrs.Metageneration != 1 {
		t.Fatalf("object metadata: %+v, written %+v", attrs, written)
	}
	readObject(t, ctx, object, payload, attrs.Generation)
	names, err := listObjectNames(ctx, bucket)
	if err != nil || !slices.Equal(names, []string{object.ObjectName()}) {
		t.Fatalf("list objects: got %v, %v", names, err)
	}
	updated, err := object.Update(ctx, storage.ObjectAttrsToUpdate{Metadata: map[string]string{"principal": "admin"}})
	if err != nil {
		t.Fatalf("update: %v", err)
	}
	if updated.Generation != attrs.Generation || updated.Metageneration != 2 || updated.Metadata["principal"] != "admin" {
		t.Fatalf("update result: %+v", updated)
	}
	readObject(t, ctx, object, payload, attrs.Generation)
	if err := object.Delete(ctx); err != nil {
		t.Fatalf("delete object: %v", err)
	}
	requireObjectMissing(t, ctx, object)
	if err := bucket.Delete(ctx); err != nil {
		t.Fatalf("delete bucket: %v", err)
	}
	bucketDeleted = true
	if _, err := bucket.Attrs(ctx); !errors.Is(err, storage.ErrBucketNotExist) {
		t.Fatalf("get after delete: want ErrBucketNotExist, got %v", err)
	}
}

func TestStrictAuthRejectsMissingAndInvalidCredentials(t *testing.T) {
	ctx, adminBucket, fixture, payload := strictFixture(t, "unauth")
	for _, tc := range []struct {
		name, token, reason string
	}{
		{"anonymous", "", "required"},
		{"unregistered token", "not-a-registered-token", "authError"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			callerCtx, caller := newStrictClient(t, tc.token)
			bucket := caller.Bucket(adminBucket.BucketName())
			object := bucket.Object(fixture.Name)
			_, err := object.Attrs(callerCtx)
			requireStrictError(t, err, http.StatusUnauthorized, tc.reason)
			reader, err := object.NewReader(callerCtx)
			if reader != nil {
				if closeErr := reader.Close(); closeErr != nil {
					t.Errorf("close unexpected reader: %v", closeErr)
				}
			}
			requireStrictError(t, err, http.StatusUnauthorized, tc.reason)
			_, err = listObjectNames(callerCtx, bucket)
			requireStrictError(t, err, http.StatusUnauthorized, tc.reason)
		})
	}
	requireStrictObjectUnchanged(t, ctx, adminBucket, fixture, payload)
}

func TestStrictAuthObjectViewer(t *testing.T) {
	ctx, adminBucket, fixture, payload := strictFixture(t, "viewer")
	viewerCtx, viewer := newStrictClient(t, strictViewerToken)
	bucket := viewer.Bucket(adminBucket.BucketName())
	object := bucket.Object(fixture.Name)

	attrs, err := object.Attrs(viewerCtx)
	if err != nil || attrs.Generation != fixture.Generation {
		t.Fatalf("viewer metadata: got %+v, %v", attrs, err)
	}
	readObject(t, viewerCtx, object, payload, fixture.Generation)
	names, err := listObjectNames(viewerCtx, bucket)
	if err != nil || !slices.Equal(names, []string{fixture.Name}) {
		t.Fatalf("viewer list: got %v, %v", names, err)
	}

	_, err = strictUpload(viewerCtx, bucket.Object("dir/viewer-new.bin"), []byte("viewer"))
	requireStrictError(t, err, http.StatusForbidden, "forbidden")
	_, err = object.Update(viewerCtx, storage.ObjectAttrsToUpdate{Metadata: map[string]string{"principal": "viewer"}})
	requireStrictError(t, err, http.StatusForbidden, "forbidden")
	requireStrictError(t, object.Delete(viewerCtx), http.StatusForbidden, "forbidden")
	// objectViewer lacks storage.buckets.get, so bucket metadata stays hidden.
	_, err = bucket.Attrs(viewerCtx)
	requireStrictError(t, err, http.StatusForbidden, "forbidden")

	if _, err := adminBucket.Object("dir/viewer-new.bin").Attrs(ctx); !errors.Is(err, storage.ErrObjectNotExist) {
		t.Fatalf("rejected viewer upload published an object: %v", err)
	}
	requireStrictObjectUnchanged(t, ctx, adminBucket, fixture, payload)
}

func TestStrictAuthObjectCreator(t *testing.T) {
	ctx, adminBucket, _, _ := strictFixture(t, "creator")
	creatorCtx, creator := newStrictClient(t, strictCreatorToken)
	bucket := creator.Bucket(adminBucket.BucketName())
	object := bucket.Object("dir/creator.bin")

	original := []byte("creator original")
	created, err := strictUpload(creatorCtx, object, original)
	if err != nil {
		t.Fatalf("creator create: %v", err)
	}
	if created.Name != object.ObjectName() || created.Generation <= 0 || created.Size != int64(len(original)) {
		t.Fatalf("creator create result: %+v", created)
	}
	// objectCreator lacks storage.objects.delete, which an overwrite of a live object requires.
	_, err = strictUpload(creatorCtx, object, []byte("creator overwrite"))
	requireStrictError(t, err, http.StatusForbidden, "forbidden")
	requireStrictObjectUnchanged(t, ctx, adminBucket, created, original)

	_, err = object.Attrs(creatorCtx)
	requireStrictError(t, err, http.StatusForbidden, "forbidden")
	reader, err := object.NewReader(creatorCtx)
	if reader != nil {
		if closeErr := reader.Close(); closeErr != nil {
			t.Errorf("close unexpected reader: %v", closeErr)
		}
	}
	requireStrictError(t, err, http.StatusForbidden, "forbidden")
}
