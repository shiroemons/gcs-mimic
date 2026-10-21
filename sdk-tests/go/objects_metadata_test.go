package gcsmimic_test

import (
	"bytes"
	"context"
	"crypto/rand"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"testing"
	"time"

	"cloud.google.com/go/storage"
)

func TestObjectMetadataUpdate(t *testing.T) {
	ctx, client, httpClient, endpoint := newEmulatorClient(t)
	var nonce [8]byte
	if _, err := rand.Read(nonce[:]); err != nil {
		t.Fatal(err)
	}
	bucket := client.Bucket(fmt.Sprintf("sdk-go-metadata-%x", nonce))
	if err := bucket.Create(ctx, "sdk-metadata", nil); err != nil {
		t.Fatal(err)
	}
	object := bucket.Object("dir/metadata.txt")
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
	uploadURL := *endpoint
	uploadURL.Path = "/upload/storage/v1/b/" + bucket.BucketName() + "/o"
	uploadURL.RawQuery = url.Values{"uploadType": {"media"}, "name": {object.ObjectName()}}.Encode()
	request, err := http.NewRequestWithContext(ctx, http.MethodPost, uploadURL.String(), bytes.NewBufferString("metadata payload"))
	if err != nil {
		t.Fatal(err)
	}
	request.Header.Set("Content-Type", "application/octet-stream")
	response, err := httpClient.Do(request)
	if err != nil {
		t.Fatal(err)
	}
	body, readErr := io.ReadAll(io.LimitReader(response.Body, 64<<10))
	closeErr := response.Body.Close()
	if response.StatusCode != 200 || readErr != nil || closeErr != nil {
		t.Fatalf("upload: status=%d read=%v close=%v body=%s", response.StatusCode, readErr, closeErr, body)
	}
	original, err := object.Attrs(ctx)
	if err != nil {
		t.Fatal(err)
	}
	first, err := object.Update(ctx, storage.ObjectAttrsToUpdate{
		ContentType: "text/plain", ContentLanguage: "en", ContentDisposition: "attachment", CacheControl: "private, max-age=60",
		Metadata: map[string]string{"keep": "one", "empty": "two"},
	})
	if err != nil {
		t.Fatal(err)
	}
	if first.Generation != original.Generation || first.Metageneration != original.Metageneration+1 || first.Etag == original.Etag || first.ContentType != "text/plain" {
		t.Fatalf("first update: %+v", first)
	}
	conditioned := object.If(storage.Conditions{MetagenerationMatch: first.Metageneration})
	second, err := conditioned.Update(ctx, storage.ObjectAttrsToUpdate{Metadata: map[string]string{"empty": "", "add": "three"}})
	if err != nil {
		t.Fatal(err)
	}
	if second.Metadata["keep"] != "one" || second.Metadata["add"] != "three" {
		t.Fatalf("merge: %+v", second.Metadata)
	}
	// The pinned HTTP SDK sends an empty string unchanged; only null deletes a JSON map key.
	if value, ok := second.Metadata["empty"]; !ok || value != "" {
		t.Fatalf("empty metadata value: %+v", second.Metadata)
	}
	_, err = conditioned.Update(ctx, storage.ObjectAttrsToUpdate{ContentType: "application/should-not-commit"})
	requireObjectStatus(t, err, http.StatusPreconditionFailed)
	attrs, err := object.Attrs(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if attrs.Metageneration != second.Metageneration || attrs.ContentType != "text/plain" || attrs.ContentLanguage != "en" || attrs.CacheControl != "private, max-age=60" {
		t.Fatalf("read metadata: %+v", attrs)
	}
	reader, err := object.NewReader(ctx)
	if err != nil {
		t.Fatal(err)
	}
	download, readErr := io.ReadAll(io.LimitReader(reader, 1024))
	closeErr = reader.Close()
	if readErr != nil || closeErr != nil || string(download) != "metadata payload" || reader.Attrs.ContentType != "text/plain" {
		t.Fatalf("download: read=%v close=%v body=%q attrs=%+v", readErr, closeErr, download, reader.Attrs)
	}
	cleared, err := object.Update(ctx, storage.ObjectAttrsToUpdate{ContentType: "", ContentLanguage: "", Metadata: map[string]string{}})
	if err != nil {
		t.Fatal(err)
	}
	if len(cleared.Metadata) != 0 || cleared.ContentType != "" || cleared.ContentLanguage != "" {
		t.Fatalf("clear with empty SDK values: %+v", cleared)
	}
}
