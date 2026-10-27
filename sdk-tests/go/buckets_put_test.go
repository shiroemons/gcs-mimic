package gcsmimic_test

import (
	"context"
	"crypto/rand"
	"errors"
	"fmt"
	"maps"
	"net"
	"net/http"
	"os"
	"reflect"
	"testing"
	"time"

	"cloud.google.com/go/storage"
	"golang.org/x/oauth2"
	"google.golang.org/api/iterator"
	"google.golang.org/api/option"
	raw "google.golang.org/api/storage/v1"
)

func TestBucketMetadataReplace(t *testing.T) {
	ctx, client, httpClient, endpoint := newEmulatorClient(t)
	api, err := raw.NewService(ctx, option.WithHTTPClient(httpClient), option.WithoutAuthentication(),
		option.WithEndpoint(endpoint.String()+"/storage/v1/"))
	if err != nil {
		t.Fatal(err)
	}
	var nonce [8]byte
	if _, err := rand.Read(nonce[:]); err != nil {
		t.Fatal(err)
	}
	bucket := client.Bucket(fmt.Sprintf("sdk-go-bucket-replace-%x", nonce))
	if err := bucket.Create(ctx, "sdk-bucket-replace", nil); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		cleanupCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		objects := bucket.Objects(cleanupCtx, &storage.Query{Versions: true})
		for {
			attrs, err := objects.Next()
			if errors.Is(err, iterator.Done) {
				break
			}
			if err != nil {
				t.Errorf("cleanup listing: %v", err)
				return
			}
			if err := bucket.Object(attrs.Name).Generation(attrs.Generation).Delete(cleanupCtx); err != nil {
				t.Errorf("cleanup generation: %v", err)
			}
		}
		if err := bucket.Delete(cleanupCtx); err != nil {
			t.Errorf("cleanup bucket: %v", err)
		}
	})
	payload := []byte("PUT preserves existing object\x00\xff")
	object, err := strictUpload(ctx, bucket.Object("kept.bin"), payload)
	if err != nil {
		t.Fatal(err)
	}
	original, err := bucket.Attrs(ctx)
	if err != nil {
		t.Fatal(err)
	}
	first, err := api.Buckets.Update(bucket.BucketName(), &raw.Bucket{
		Labels: map[string]string{"keep": "one", "remove": "two"}, StorageClass: "NEARLINE",
		Versioning: &raw.BucketVersioning{Enabled: true}, DefaultEventBasedHold: true,
	}).IfMetagenerationMatch(original.MetaGeneration).Context(ctx).Do()
	if err != nil {
		t.Fatal(err)
	}
	firstAttrs := requireBucketReplacement(t, ctx, bucket, original, first)
	if !firstAttrs.VersioningEnabled || !firstAttrs.DefaultEventBasedHold || firstAttrs.StorageClass != "NEARLINE" ||
		!maps.Equal(firstAttrs.Labels, map[string]string{"keep": "one", "remove": "two"}) {
		t.Fatalf("initial replacement: %+v", firstAttrs)
	}
	pap, err := api.Buckets.Update(bucket.BucketName(), &raw.Bucket{
		IamConfiguration: &raw.BucketIamConfiguration{PublicAccessPrevention: "enforced"},
	}).Context(ctx).Do()
	if err != nil {
		t.Fatal(err)
	}
	firstAttrs = requireBucketReplacement(t, ctx, bucket, firstAttrs, pap)
	if !firstAttrs.VersioningEnabled || !maps.Equal(firstAttrs.Labels, first.Labels) ||
		firstAttrs.PublicAccessPrevention != storage.PublicAccessPreventionEnforced {
		t.Fatalf("PUT must preserve omitted labels/versioning while changing PAP: %+v", firstAttrs)
	}
	patch := storage.BucketAttrsToUpdate{}
	patch.SetLabel("add", "three")
	merged, err := bucket.Update(ctx, patch)
	if err != nil {
		t.Fatal(err)
	}
	if !maps.Equal(merged.Labels, map[string]string{"keep": "one", "remove": "two", "add": "three"}) ||
		merged.StorageClass != "NEARLINE" || !merged.VersioningEnabled || !merged.DefaultEventBasedHold {
		t.Fatalf("PATCH must preserve omitted metadata: %+v", merged)
	}
	requireBucketUpdate(t, ctx, bucket, firstAttrs, merged)
	second, err := api.Buckets.Update(bucket.BucketName(), &raw.Bucket{Labels: map[string]string{"only": "replacement"}}).
		IfMetagenerationNotMatch(first.Metageneration).Context(ctx).Do()
	if err != nil {
		t.Fatal(err)
	}
	secondAttrs := requireBucketReplacement(t, ctx, bucket, merged, second)
	if !maps.Equal(secondAttrs.Labels, map[string]string{"only": "replacement"}) || secondAttrs.StorageClass != "NEARLINE" ||
		!secondAttrs.VersioningEnabled || !secondAttrs.DefaultEventBasedHold {
		t.Fatalf("PUT must replace labels and preserve omitted settings: %+v", secondAttrs)
	}
	_, err = api.Buckets.Update(bucket.BucketName(), &raw.Bucket{}).
		IfMetagenerationMatch(merged.MetaGeneration).Context(ctx).Do()
	requireStrictError(t, err, http.StatusPreconditionFailed, "conditionNotMet")
	_, err = api.Buckets.Update(bucket.BucketName(), &raw.Bucket{}).
		IfMetagenerationNotMatch(second.Metageneration).Context(ctx).Do()
	requireStrictError(t, err, http.StatusPreconditionFailed, "conditionNotMet")
	unchanged, err := bucket.Attrs(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if unchanged.MetaGeneration != secondAttrs.MetaGeneration || unchanged.Etag != secondAttrs.Etag ||
		!unchanged.Updated.Equal(secondAttrs.Updated) || !maps.Equal(unchanged.Labels, secondAttrs.Labels) ||
		unchanged.StorageClass != secondAttrs.StorageClass || !unchanged.VersioningEnabled || !unchanged.DefaultEventBasedHold {
		t.Fatalf("failed PUT conditions changed metadata: %+v", unchanged)
	}
	last, err := api.Buckets.Update(bucket.BucketName(), &raw.Bucket{}).
		IfMetagenerationMatch(second.Metageneration).Context(ctx).Do()
	if err != nil {
		t.Fatal(err)
	}
	lastAttrs := requireBucketReplacement(t, ctx, bucket, secondAttrs, last)
	if !maps.Equal(lastAttrs.Labels, secondAttrs.Labels) || lastAttrs.StorageClass != "NEARLINE" || !lastAttrs.VersioningEnabled || !lastAttrs.DefaultEventBasedHold {
		t.Fatalf("empty PUT must preserve modeled settings: %+v", lastAttrs)
	}
	requireStrictObjectUnchanged(t, ctx, bucket, object, payload)
}

func requireBucketReplacement(t *testing.T, ctx context.Context, bucket *storage.BucketHandle, before *storage.BucketAttrs, replaced *raw.Bucket) *storage.BucketAttrs {
	t.Helper()
	got, err := bucket.Attrs(ctx)
	if err != nil {
		t.Fatal(err)
	}
	updated, err := time.Parse(time.RFC3339Nano, replaced.Updated)
	if err != nil {
		t.Fatal(err)
	}
	if got.Name != replaced.Name || got.MetaGeneration != replaced.Metageneration || got.Etag != replaced.Etag ||
		got.StorageClass != replaced.StorageClass || !maps.Equal(got.Labels, replaced.Labels) ||
		got.VersioningEnabled != (replaced.Versioning != nil && replaced.Versioning.Enabled) ||
		got.DefaultEventBasedHold != replaced.DefaultEventBasedHold || !got.Updated.Equal(updated) {
		t.Fatalf("Attrs differs from PUT response: got=%+v response=%+v", got, replaced)
	}
	requireBucketUpdate(t, ctx, bucket, before, got)
	return got
}

func TestStrictAuthBucketMetadataReplace(t *testing.T) {
	ctx, bucket, object, payload := strictFixture(t, "bucket-replace")
	seed := storage.BucketAttrsToUpdate{}
	seed.SetLabel("remove", "old")
	before, err := bucket.Update(ctx, seed)
	if err != nil {
		t.Fatal(err)
	}
	policy, err := bucket.IAM().Policy(ctx)
	if err != nil {
		t.Fatal(err)
	}
	api := newStrictBucketService(t, ctx, strictViewerToken)
	body := &raw.Bucket{Labels: map[string]string{"owner": "admin"}, StorageClass: "COLDLINE"}
	_, err = api.Buckets.Update(bucket.BucketName(), body).Context(ctx).Do()
	requireStrictError(t, err, http.StatusForbidden, "forbidden")
	after, err := bucket.Attrs(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if after.MetaGeneration != before.MetaGeneration || after.Etag != before.Etag ||
		!after.Updated.Equal(before.Updated) || !maps.Equal(after.Labels, before.Labels) || after.StorageClass != before.StorageClass {
		t.Fatalf("denied PUT changed metadata: before=%+v after=%+v", before, after)
	}
	api = newStrictBucketService(t, ctx, strictAdminToken)
	updated, err := api.Buckets.Update(bucket.BucketName(), body).IfMetagenerationMatch(before.MetaGeneration).Context(ctx).Do()
	if err != nil {
		t.Fatalf("admin PUT: %v", err)
	}
	attrs := requireBucketReplacement(t, ctx, bucket, before, updated)
	if !maps.Equal(attrs.Labels, body.Labels) || attrs.StorageClass != body.StorageClass {
		t.Fatalf("admin replacement: %+v", attrs)
	}
	afterPolicy, err := bucket.IAM().Policy(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if string(afterPolicy.InternalProto.Etag) != string(policy.InternalProto.Etag) {
		t.Fatal("PUT changed IAM policy etag")
	}
	if !reflect.DeepEqual(afterPolicy.InternalProto.Bindings, policy.InternalProto.Bindings) {
		t.Fatal("PUT changed IAM policy bindings")
	}
	requireStrictObjectUnchanged(t, ctx, bucket, object, payload)
}

func newStrictBucketService(t *testing.T, ctx context.Context, token string) *raw.Service {
	t.Helper()
	endpoint, err := emulatorEndpoint(os.Getenv("GCS_MIMIC_STRICT_ENDPOINT"), os.Getenv("GCS_MIMIC_ALLOWED_HOSTS"))
	if err != nil {
		t.Fatal(err)
	}
	transport := &http.Transport{
		TLSClientConfig: trustedEmulatorTLS(t),
		Proxy:           nil, MaxConnsPerHost: 2, MaxIdleConns: 2,
		IdleConnTimeout: 5 * time.Second, ResponseHeaderTimeout: 5 * time.Second,
		DialContext: func(ctx context.Context, network, address string) (net.Conn, error) {
			host, port, err := net.SplitHostPort(address)
			if err != nil {
				return nil, err
			}
			// Match the shared harness so hosts-file changes cannot redirect localhost off-machine.
			if host == "localhost" {
				address = net.JoinHostPort("127.0.0.1", port)
			}
			return (&net.Dialer{Timeout: 5 * time.Second}).DialContext(ctx, network, address)
		},
	}
	t.Cleanup(transport.CloseIdleConnections)
	httpClient := &http.Client{
		Transport: &oauth2.Transport{
			Source: oauth2.StaticTokenSource(&oauth2.Token{AccessToken: token, TokenType: "Bearer"}),
			Base:   emulatorTransport{endpoint: endpoint, base: transport},
		},
		Timeout:       10 * time.Second,
		CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse },
	}
	api, err := raw.NewService(ctx, option.WithHTTPClient(httpClient), option.WithoutAuthentication(),
		option.WithEndpoint(endpoint.String()+"/storage/v1/"))
	if err != nil {
		t.Fatal(err)
	}
	return api
}
