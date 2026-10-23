package gcsmimic_test

import (
	"testing"

	"cloud.google.com/go/storage"
	"google.golang.org/api/iterator"
	"google.golang.org/api/option"
	raw "google.golang.org/api/storage/v1"
)

func TestPartialResponseFields(t *testing.T) {
	ctx, client, httpClient, endpoint := newEmulatorClient(t)
	bucket := resumableBucket(t, ctx, client, "sdk-fields")
	object := resumableObject(t, bucket, "selected")
	writer := object.NewWriter(ctx)
	writer.ChunkSize = 0
	if _, err := writer.Write([]byte("data")); err != nil {
		t.Fatal(err)
	}
	if err := writer.Close(); err != nil {
		t.Fatal(err)
	}
	api, err := raw.NewService(ctx, option.WithHTTPClient(httpClient), option.WithoutAuthentication(),
		option.WithEndpoint(endpoint.String()+"/storage/v1/"))
	if err != nil {
		t.Fatal(err)
	}
	got, err := api.Buckets.Get(bucket.BucketName()).Fields("name").Context(ctx).Do()
	if err != nil {
		t.Fatal(err)
	}
	if got.Name != bucket.BucketName() || got.Kind != "" || got.Metageneration != 0 {
		t.Fatalf("selected bucket: %+v", got)
	}
	query := &storage.Query{}
	if err := query.SetAttrSelection([]string{"Name", "Size"}); err != nil {
		t.Fatal(err)
	}
	it := bucket.Objects(ctx, query)
	attrs, err := it.Next()
	if err != nil {
		t.Fatal(err)
	}
	if attrs.Name != "selected" || attrs.Size != 4 || attrs.ContentType != "" || attrs.Generation != 0 {
		t.Fatalf("selected object attrs: %+v", attrs)
	}
	if _, err := it.Next(); err != iterator.Done {
		t.Fatalf("iterator end: %v", err)
	}
}
