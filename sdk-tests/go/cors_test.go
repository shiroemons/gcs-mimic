package gcsmimic_test

import (
	"context"
	"crypto/rand"
	"fmt"
	"io"
	"net/http"
	"reflect"
	"testing"
	"time"

	"cloud.google.com/go/storage"
	"google.golang.org/api/option"
	raw "google.golang.org/api/storage/v1"
)

func TestBucketCORS(t *testing.T) {
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
	name := fmt.Sprintf("sdk-go-cors-%x", nonce)
	rules := []*raw.BucketCors{{Origin: []string{"https://app.example"}, Method: []string{"GET", "HEAD"}, ResponseHeader: []string{"ETag"}, MaxAgeSeconds: 120}}
	if _, err := api.Buckets.Insert("sdk-cors", &raw.Bucket{Name: name, Cors: rules}).Context(ctx).Do(); err != nil {
		t.Fatal(err)
	}
	bucket := client.Bucket(name)
	t.Cleanup(func() {
		cleanupCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		if err := bucket.Delete(cleanupCtx); err != nil {
			t.Errorf("cleanup bucket: %v", err)
		}
	})
	before, err := bucket.Attrs(ctx)
	if err != nil {
		t.Fatal(err)
	}
	want := []storage.CORS{{Origins: []string{"https://app.example"}, Methods: []string{"GET", "HEAD"}, ResponseHeaders: []string{"ETag"}, MaxAge: 120 * time.Second}}
	if !reflect.DeepEqual(before.CORS, want) {
		t.Fatalf("created CORS: got=%+v want=%+v", before.CORS, want)
	}
	want = []storage.CORS{{Origins: []string{"*"}, Methods: []string{"*", "OPTIONS"}, ResponseHeaders: []string{"ETag", "X-Example"}, MaxAge: 300 * time.Second}}
	updated, err := bucket.If(storage.BucketConditions{MetagenerationMatch: before.MetaGeneration}).Update(ctx, storage.BucketAttrsToUpdate{CORS: want})
	if err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(updated.CORS, want) {
		t.Fatalf("updated CORS: got=%+v want=%+v", updated.CORS, want)
	}
	requireBucketUpdate(t, ctx, bucket, before, updated)
	_, err = bucket.If(storage.BucketConditions{MetagenerationMatch: before.MetaGeneration}).Update(ctx, storage.BucketAttrsToUpdate{CORS: []storage.CORS{}})
	requireObjectStatus(t, err, http.StatusPreconditionFailed)
	label := storage.BucketAttrsToUpdate{}
	label.SetLabel("keep", "cors")
	got, err := bucket.Update(ctx, label)
	if err != nil || !reflect.DeepEqual(got.CORS, want) {
		t.Fatalf("omitted CORS must preserve configuration: attrs=%+v err=%v", got, err)
	}
	cleared, err := bucket.Update(ctx, storage.BucketAttrsToUpdate{CORS: []storage.CORS{}})
	if err != nil || len(cleared.CORS) != 0 {
		t.Fatalf("empty CORS must clear configuration: attrs=%+v err=%v", cleared, err)
	}
	rules = []*raw.BucketCors{{Origin: []string{"https://*.example.com:443"}, Method: []string{"GET", "HEAD"}, ResponseHeader: []string{"ETag"}}}
	replaced, err := api.Buckets.Update(name, &raw.Bucket{Cors: rules}).IfMetagenerationMatch(cleared.MetaGeneration).Context(ctx).Do()
	if err != nil {
		t.Fatal(err)
	}
	attrs := requireBucketReplacement(t, ctx, bucket, cleared, replaced)
	if len(attrs.CORS) != 1 || !reflect.DeepEqual(attrs.CORS[0].Origins, rules[0].Origin) || attrs.CORS[0].MaxAge != 0 {
		t.Fatalf("replacement CORS/default field round trip: %+v", attrs.CORS)
	}
	request, err := http.NewRequestWithContext(ctx, http.MethodOptions, endpoint.String()+"/"+name+"/file", nil)
	if err != nil {
		t.Fatal(err)
	}
	request.Header.Set("Origin", "https://dev.example.com")
	request.Header.Set("Access-Control-Request-Method", "GET")
	request.Header.Set("Access-Control-Request-Headers", "etag")
	response, err := httpClient.Do(request)
	if err != nil {
		t.Fatal(err)
	}
	defer response.Body.Close()
	if _, err := io.Copy(io.Discard, response.Body); err != nil {
		t.Fatal(err)
	}
	if response.StatusCode != http.StatusOK || response.Header.Get("Access-Control-Allow-Origin") != "https://dev.example.com" ||
		response.Header.Get("Access-Control-Max-Age") != "3600" || response.Header.Get("Access-Control-Allow-Credentials") != "" {
		t.Fatalf("XML preflight from SDK-configured bucket: status=%s headers=%v", response.Status, response.Header)
	}
	if _, err := api.Buckets.Update(name, &raw.Bucket{}).Context(ctx).Do(); err != nil {
		t.Fatal(err)
	}
	attrs, err = bucket.Attrs(ctx)
	if err != nil || len(attrs.CORS) != 0 {
		t.Fatalf("omitted PUT CORS must clear configuration: attrs=%+v err=%v", attrs, err)
	}
}

func TestStrictAuthBucketCORS(t *testing.T) {
	ctx, bucket, object, payload := strictFixture(t, "cors")
	_, viewer := newStrictClient(t, strictViewerToken)
	before, err := bucket.Attrs(ctx)
	if err != nil {
		t.Fatal(err)
	}
	want := []storage.CORS{{Origins: []string{"https://app.example"}, Methods: []string{"GET"}, ResponseHeaders: []string{"Authorization"}, MaxAge: time.Hour}}
	_, err = viewer.Bucket(bucket.BucketName()).Update(ctx, storage.BucketAttrsToUpdate{CORS: want})
	requireStrictError(t, err, http.StatusForbidden, "forbidden")
	after, err := bucket.Attrs(ctx)
	if err != nil || after.MetaGeneration != before.MetaGeneration || !reflect.DeepEqual(after.CORS, before.CORS) {
		t.Fatalf("denied CORS update changed bucket: attrs=%+v err=%v", after, err)
	}
	updated, err := bucket.If(storage.BucketConditions{MetagenerationMatch: before.MetaGeneration}).Update(ctx, storage.BucketAttrsToUpdate{CORS: want})
	if err != nil || !reflect.DeepEqual(updated.CORS, want) {
		t.Fatalf("admin CORS update: attrs=%+v err=%v", updated, err)
	}
	requireBucketUpdate(t, ctx, bucket, before, updated)
	requireStrictObjectUnchanged(t, ctx, bucket, object, payload)
}
