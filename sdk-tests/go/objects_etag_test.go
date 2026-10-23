package gcsmimic_test

import (
	"context"
	"crypto/rand"
	"errors"
	"fmt"
	"io"
	"net/http"
	"testing"
	"time"

	"cloud.google.com/go/storage"
	"google.golang.org/api/googleapi"
	"google.golang.org/api/option"
	raw "google.golang.org/api/storage/v1"
)

func TestReadETagValidators(t *testing.T) {
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
	bucket := client.Bucket(fmt.Sprintf("sdk-go-etag-%x", nonce))
	if err := bucket.Create(ctx, "sdk-etag", nil); err != nil {
		t.Fatal(err)
	}
	object := bucket.Object("etag.txt")
	t.Cleanup(func() {
		cleanupCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		if err := object.Delete(cleanupCtx); err != nil && !errors.Is(err, storage.ErrObjectNotExist) {
			t.Errorf("cleanup object: %v", err)
		}
		if err := bucket.Delete(cleanupCtx); err != nil {
			t.Errorf("cleanup bucket: %v", err)
		}
	})
	if _, err := strictUpload(ctx, object, []byte("0123456789")); err != nil {
		t.Fatal(err)
	}
	metadata, err := api.Objects.Get(bucket.BucketName(), object.ObjectName()).Context(ctx).Do()
	if err != nil {
		t.Fatal(err)
	}
	if metadata.Etag == "" || metadata.ServerResponse.Header.Get("ETag") != metadata.Etag {
		t.Fatalf("metadata ETag: resource=%q headers=%v", metadata.Etag, metadata.ServerResponse.Header)
	}
	_, err = api.Objects.Get(bucket.BucketName(), object.ObjectName()).IfNoneMatch(metadata.Etag).Context(ctx).Do()
	if !googleapi.IsNotModified(err) {
		t.Fatalf("conditional metadata GET: %v", err)
	}
	var apiError *googleapi.Error
	if !errors.As(err, &apiError) || apiError.Header.Get("ETag") != metadata.Etag {
		t.Fatalf("304 validator: %v", err)
	}
	_, err = api.Objects.Get(bucket.BucketName(), object.ObjectName()).IfGenerationMatch(metadata.Generation).
		IfNoneMatch(metadata.Etag).Context(ctx).Do()
	if !googleapi.IsNotModified(err) {
		t.Fatalf("combined conditional metadata GET: %v", err)
	}
	_, err = api.Objects.Get(bucket.BucketName(), object.ObjectName()).IfGenerationMatch(0).
		IfNoneMatch(metadata.Etag).Context(ctx).Do()
	requireStrictError(t, err, http.StatusPreconditionFailed, "conditionNotMet")
	call := api.Objects.Get(bucket.BucketName(), object.ObjectName()).Generation(metadata.Generation).
		IfMetagenerationMatch(metadata.Metageneration).Context(ctx)
	call.Header().Set("If-Match", metadata.Etag)
	call.Header().Set("Range", "bytes=2-5")
	response, err := call.Download()
	if err != nil {
		t.Fatal(err)
	}
	body, readErr := io.ReadAll(io.LimitReader(response.Body, 1024))
	closeErr := response.Body.Close()
	if response.StatusCode != http.StatusPartialContent || response.Header.Get("ETag") != metadata.Etag ||
		string(body) != "2345" || readErr != nil || closeErr != nil {
		t.Fatalf("conditional range: status=%d headers=%v body=%q read=%v close=%v", response.StatusCode, response.Header, body, readErr, closeErr)
	}
	stale := api.Objects.Get(bucket.BucketName(), object.ObjectName()).Context(ctx)
	stale.Header().Set("If-Match", `"stale"`)
	_, err = stale.Do()
	requireStrictError(t, err, http.StatusPreconditionFailed, "conditionNotMet")
	if _, err := object.Update(ctx, storage.ObjectAttrsToUpdate{ContentType: "text/plain"}); err != nil {
		t.Fatal(err)
	}
	changed, err := api.Objects.Get(bucket.BucketName(), object.ObjectName()).IfNoneMatch(metadata.Etag).Context(ctx).Do()
	if err != nil || changed == nil || changed.Etag == metadata.Etag {
		t.Fatalf("changed metadata: result=%+v error=%v", changed, err)
	}
	bucketMetadata, err := api.Buckets.Get(bucket.BucketName()).Context(ctx).Do()
	if err != nil {
		t.Fatal(err)
	}
	_, err = api.Buckets.Get(bucket.BucketName()).IfNoneMatch(bucketMetadata.Etag).Context(ctx).Do()
	if !googleapi.IsNotModified(err) {
		t.Fatalf("conditional bucket GET: %v", err)
	}
	policy, err := api.Buckets.GetIamPolicy(bucket.BucketName()).Context(ctx).Do()
	if err != nil {
		t.Fatal(err)
	}
	policyTag := policy.ServerResponse.Header.Get("ETag")
	if policyTag == "" {
		t.Fatal("policy GET has no local HTTP validator")
	}
	policyRead := api.Buckets.GetIamPolicy(bucket.BucketName()).Fields("version").Context(ctx)
	policyRead.Header().Set("If-None-Match", policyTag)
	_, err = policyRead.Do()
	if !googleapi.IsNotModified(err) {
		t.Fatalf("conditional policy GET: %v", err)
	}
	policyRead.Header().Del("If-None-Match")
	policyRead.Header().Set("If-Match", `"stale"`)
	_, err = policyRead.Do()
	requireStrictError(t, err, http.StatusPreconditionFailed, "conditionNotMet")
}
