package gcsmimic_test

import (
	"context"
	"crypto/rand"
	"fmt"
	"maps"
	"net/http"
	"os"
	"testing"
	"time"

	"cloud.google.com/go/storage"
	"golang.org/x/oauth2"
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
		if err := bucket.Delete(cleanupCtx); err != nil {
			t.Errorf("cleanup bucket: %v", err)
		}
	})
	payload := []byte("PUT preserves existing object\x00\xff")
	object, err := strictUpload(ctx, bucket.Object("kept.bin"), payload)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		cleanupCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		if err := bucket.Object(object.Name).Delete(cleanupCtx); err != nil {
			t.Errorf("cleanup object: %v", err)
		}
	})
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
	if !maps.Equal(secondAttrs.Labels, map[string]string{"only": "replacement"}) || secondAttrs.StorageClass != "STANDARD" ||
		secondAttrs.VersioningEnabled || secondAttrs.DefaultEventBasedHold {
		t.Fatalf("PUT must replace labels and reset omitted settings: %+v", secondAttrs)
	}
	_, err = api.Buckets.Update(bucket.BucketName(), &raw.Bucket{}).
		IfMetagenerationMatch(merged.MetaGeneration).Context(ctx).Do()
	requireObjectStatus(t, err, http.StatusPreconditionFailed)
	_, err = api.Buckets.Update(bucket.BucketName(), &raw.Bucket{}).
		IfMetagenerationNotMatch(second.Metageneration).Context(ctx).Do()
	requireObjectStatus(t, err, http.StatusPreconditionFailed)
	unchanged, err := bucket.Attrs(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if unchanged.MetaGeneration != secondAttrs.MetaGeneration || unchanged.Etag != secondAttrs.Etag ||
		!unchanged.Updated.Equal(secondAttrs.Updated) || !maps.Equal(unchanged.Labels, secondAttrs.Labels) ||
		unchanged.StorageClass != secondAttrs.StorageClass || unchanged.VersioningEnabled || unchanged.DefaultEventBasedHold {
		t.Fatalf("failed PUT conditions changed metadata: %+v", unchanged)
	}
	last, err := api.Buckets.Update(bucket.BucketName(), &raw.Bucket{}).
		IfMetagenerationMatch(second.Metageneration).Context(ctx).Do()
	if err != nil {
		t.Fatal(err)
	}
	lastAttrs := requireBucketReplacement(t, ctx, bucket, secondAttrs, last)
	if len(lastAttrs.Labels) != 0 || lastAttrs.StorageClass != "STANDARD" || lastAttrs.VersioningEnabled || lastAttrs.DefaultEventBasedHold {
		t.Fatalf("empty PUT must restore modeled defaults: %+v", lastAttrs)
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
	before, err := bucket.Attrs(ctx)
	if err != nil {
		t.Fatal(err)
	}
	policy, err := bucket.IAM().Policy(ctx)
	if err != nil {
		t.Fatal(err)
	}
	// Reuse the origin-guarded HTTP harness; explicit static tokens never trigger ADC discovery.
	t.Setenv("GCS_MIMIC_ENDPOINT", os.Getenv("GCS_MIMIC_STRICT_ENDPOINT"))
	_, _, httpClient, endpoint := newEmulatorClient(t)
	source := oauth2.StaticTokenSource(&oauth2.Token{AccessToken: strictViewerToken, TokenType: "Bearer"})
	httpClient.Transport = &oauth2.Transport{Source: source, Base: httpClient.Transport}
	api, err := raw.NewService(ctx, option.WithHTTPClient(httpClient), option.WithoutAuthentication(),
		option.WithEndpoint(endpoint.String()+"/storage/v1/"))
	if err != nil {
		t.Fatal(err)
	}
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
	source = oauth2.StaticTokenSource(&oauth2.Token{AccessToken: strictAdminToken, TokenType: "Bearer"})
	httpClient.Transport.(*oauth2.Transport).Source = source
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
	requireStrictObjectUnchanged(t, ctx, bucket, object, payload)
}
