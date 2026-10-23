package gcsmimic_test

import (
	"bytes"
	"context"
	"encoding/base64"
	"io"
	"maps"
	"net/http"
	"strings"
	"testing"

	"cloud.google.com/go/storage"
	"google.golang.org/api/option"
	raw "google.golang.org/api/storage/v1"
)

func TestObjectCopyJSON(t *testing.T) {
	ctx, client, httpClient, endpoint := newEmulatorClient(t)
	bucket := resumableBucket(t, ctx, client, "sdk-copy")
	source := resumableObject(t, bucket, "source/猫%2F")
	target := resumableObject(t, bucket, "destination/猫%2F")
	payload := bytes.Repeat([]byte("copy bytes\x00\xff"), 16000)
	writer := source.NewWriter(ctx)
	writer.ChunkSize = 0
	writer.ContentType = "text/plain"
	writer.Metadata = map[string]string{"source": "value"}
	if _, err := writer.Write(payload); err != nil {
		t.Fatal(err)
	}
	if err := writer.Close(); err != nil {
		t.Fatal(err)
	}
	api, err := raw.NewService(ctx, option.WithHTTPClient(httpClient), option.WithoutAuthentication(), option.WithEndpoint(endpoint.String()+"/storage/v1/"))
	if err != nil {
		t.Fatal(err)
	}
	original, err := source.Attrs(ctx)
	if err != nil {
		t.Fatal(err)
	}
	copyMetadata := &raw.Object{ContentType: writer.ContentType, Metadata: writer.Metadata}
	copied, err := api.Objects.Copy(bucket.BucketName(), source.ObjectName(), bucket.BucketName(), target.ObjectName(), nil).
		SourceGeneration(original.Generation).IfSourceGenerationMatch(original.Generation).IfSourceMetagenerationMatch(original.Metageneration).
		IfGenerationMatch(0).Projection("full").Context(ctx).Do()
	if err != nil {
		t.Fatal(err)
	}
	if copied.Name != target.ObjectName() || copied.Generation == original.Generation || copied.Metageneration != 1 || copied.Size != uint64(len(payload)) || !maps.Equal(copied.Metadata, writer.Metadata) || copied.Md5Hash != base64.StdEncoding.EncodeToString(original.MD5) {
		t.Fatalf("copied metadata: %+v", copied)
	}
	reader, err := target.NewReader(ctx)
	if err != nil {
		t.Fatal(err)
	}
	got, readErr := io.ReadAll(io.LimitReader(reader, int64(len(payload)+1)))
	closeErr := reader.Close()
	if readErr != nil || closeErr != nil || !bytes.Equal(got, payload) {
		t.Fatalf("copy bytes: read=%v close=%v length=%d", readErr, closeErr, len(got))
	}
	_, err = api.Objects.Copy(bucket.BucketName(), source.ObjectName(), bucket.BucketName(), target.ObjectName(), copyMetadata).IfGenerationMatch(0).Context(ctx).Do()
	requireStrictError(t, err, http.StatusPreconditionFailed, "conditionNotMet")
	replaced, err := api.Objects.Copy(bucket.BucketName(), source.ObjectName(), bucket.BucketName(), target.ObjectName(), &raw.Object{
		ContentType: "application/json", Metadata: map[string]string{"replacement": "value"},
	}).IfGenerationMatch(copied.Generation).Fields("name,contentType,metadata,metageneration").Context(ctx).Do()
	if err != nil {
		t.Fatal(err)
	}
	if replaced.ContentType != "application/json" || replaced.Metageneration != 1 || !maps.Equal(replaced.Metadata, map[string]string{"replacement": "value"}) || replaced.Generation != 0 {
		t.Fatalf("replacement metadata: %+v", replaced)
	}
}

type copierRewriteTransport struct {
	t      *testing.T
	called bool
}

func (transport *copierRewriteTransport) RoundTrip(request *http.Request) (*http.Response, error) {
	transport.called = true
	if request.Method != http.MethodPost || !strings.Contains(request.URL.Path, "/rewriteTo/") || request.URL.Query().Get("projection") != "full" {
		transport.t.Fatalf("CopierFrom request: %s %s", request.Method, request.URL)
	}
	return &http.Response{StatusCode: 200, Header: http.Header{"Content-Type": {"application/json"}}, Body: io.NopCloser(strings.NewReader(`{"done":true,"objectSize":"0","totalBytesRewritten":"0","resource":{"bucket":"bucket","name":"destination","generation":"2","metageneration":"1","size":"0"}}`)), Request: request}, nil
}

func TestObjectCopyHighLevelCopierUsesRewrite(t *testing.T) {
	ctx := context.Background()
	transport := &copierRewriteTransport{t: t}
	client, err := storage.NewClient(ctx, option.WithEndpoint("http://localhost/storage/v1/"), option.WithoutAuthentication(), option.WithHTTPClient(&http.Client{Transport: transport}))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := client.Close(); err != nil {
			t.Error(err)
		}
	})
	_, err = client.Bucket("bucket").Object("destination").CopierFrom(client.Bucket("bucket").Object("source")).Run(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if !transport.called {
		t.Fatal("CopierFrom made no rewrite request")
	}
}
