package gcsmimic_test

import (
	"bytes"
	"context"
	"crypto/md5"
	"crypto/rand"
	"errors"
	"fmt"
	"hash/crc32"
	"io"
	"net/http"
	"net/url"
	"testing"
	"time"

	"cloud.google.com/go/storage"
	"google.golang.org/api/googleapi"
)

func requireObjectStatus(t *testing.T, err error, code int) {
	t.Helper()
	var apiError *googleapi.Error
	if !errors.As(err, &apiError) || apiError.Code != code {
		t.Fatalf("want googleapi.Error %d, got %v", code, err)
	}
}

func requireObjectMissing(t *testing.T, ctx context.Context, object *storage.ObjectHandle) {
	t.Helper()
	if _, err := object.Attrs(ctx); !errors.Is(err, storage.ErrObjectNotExist) {
		t.Fatalf("missing metadata: want ErrObjectNotExist, got %v", err)
	}
	reader, err := object.NewReader(ctx)
	if reader != nil {
		if closeErr := reader.Close(); closeErr != nil {
			t.Errorf("close unexpected reader: %v", closeErr)
		}
	}
	if !errors.Is(err, storage.ErrObjectNotExist) {
		t.Fatalf("missing read: want ErrObjectNotExist, got %v", err)
	}
}

func readObject(t *testing.T, ctx context.Context, object *storage.ObjectHandle, want []byte, generation int64) {
	t.Helper()
	reader, err := object.NewReader(ctx)
	if err != nil {
		t.Fatal(err)
	}
	got, readErr := io.ReadAll(io.LimitReader(reader, int64(len(want))+1))
	closeErr := reader.Close()
	if readErr != nil || closeErr != nil {
		t.Fatalf("read=%v, close=%v", readErr, closeErr)
	}
	if !bytes.Equal(got, want) || reader.Attrs.Size != int64(len(want)) || reader.Attrs.Generation != generation {
		t.Fatalf("download: bytes=%x, attrs=%+v; want bytes=%x, generation=%d", got, reader.Attrs, want, generation)
	}
}

func TestObjectMediaLifecycle(t *testing.T) {
	ctx, client, httpClient, endpoint := newEmulatorClient(t)
	var nonce [8]byte
	if _, err := rand.Read(nonce[:]); err != nil {
		t.Fatal(err)
	}
	bucket := client.Bucket(fmt.Sprintf("sdk-go-objects-%x", nonce))
	if err := bucket.Create(ctx, fmt.Sprintf("sdk-project-%x", nonce), nil); err != nil {
		t.Fatal(err)
	}
	deleted := false
	t.Cleanup(func() {
		if !deleted {
			cleanupCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
			defer cancel()
			if err := bucket.Delete(cleanupCtx); err != nil {
				t.Errorf("cleanup bucket: %v", err)
			}
		}
	})
	cleanupObject := func(t *testing.T, object *storage.ObjectHandle) {
		t.Helper()
		t.Cleanup(func() {
			cleanupCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
			defer cancel()
			if err := object.Delete(cleanupCtx); err != nil && !errors.Is(err, storage.ErrObjectNotExist) {
				t.Errorf("cleanup object %q: %v", object.ObjectName(), err)
			}
		})
	}
	for _, name := range []string{"dir/file.bin", "dir%2Ffile.bin", "space name.bin", "日本語/雪 +?#%.bin"} {
		t.Run(name, func(t *testing.T) {
			object := bucket.Object(name)
			cleanupObject(t, object)
			payload := []byte("media round trip\x00\xff\n" + name)
			const contentType = "application/x-gcs-mimic-sdk-test"
			uploadURL := *endpoint
			uploadURL.Path = "/upload/storage/v1/b/" + bucket.BucketName() + "/o"
			uploadURL.RawQuery = url.Values{"uploadType": {"media"}, "name": {name}, "ifGenerationMatch": {"0"}}.Encode()
			// Keep raw media coverage independent of the SDK Writer's multipart transport.
			request, err := http.NewRequestWithContext(ctx, http.MethodPost, uploadURL.String(), bytes.NewReader(payload))
			if err != nil {
				t.Fatal(err)
			}
			request.Header.Set("Content-Type", contentType)
			response, err := httpClient.Do(request)
			if err != nil {
				t.Fatal(err)
			}
			body, readErr := io.ReadAll(io.LimitReader(response.Body, 64<<10))
			closeErr := response.Body.Close()
			if readErr != nil || closeErr != nil || response.StatusCode != http.StatusOK {
				t.Fatalf("media upload: status=%d, read=%v, close=%v, body=%s", response.StatusCode, readErr, closeErr, body)
			}
			attrs, err := object.Attrs(ctx)
			if err != nil {
				t.Fatal(err)
			}
			md5sum := md5.Sum(payload)
			crc := crc32.Checksum(payload, crc32.MakeTable(crc32.Castagnoli))
			if attrs.Bucket != bucket.BucketName() || attrs.Name != name || attrs.Size != int64(len(payload)) ||
				attrs.ContentType != contentType || attrs.Generation <= 0 || attrs.Metageneration != 1 ||
				attrs.CRC32C != crc || !bytes.Equal(attrs.MD5, md5sum[:]) {
				t.Fatalf("unexpected object metadata: %+v; want size=%d, CRC32C=%08x, MD5=%x", attrs, len(payload), crc, md5sum)
			}
			readObject(t, ctx, object, payload, attrs.Generation)
			readObject(t, ctx, object.Generation(attrs.Generation), payload, attrs.Generation)
			requireObjectMissing(t, ctx, object.Generation(attrs.Generation+1))
			matched := object.If(storage.Conditions{GenerationMatch: attrs.Generation, MetagenerationMatch: attrs.Metageneration})
			if _, err := matched.Attrs(ctx); err != nil {
				t.Fatalf("matching metadata preconditions: %v", err)
			}
			readObject(t, ctx, matched, payload, attrs.Generation)
			for _, condition := range []storage.Conditions{
				{GenerationNotMatch: attrs.Generation},
				{MetagenerationNotMatch: attrs.Metageneration},
			} {
				conditional := object.If(condition)
				_, err := conditional.Attrs(ctx)
				requireObjectStatus(t, err, http.StatusNotModified)
				reader, err := conditional.NewReader(ctx)
				if reader != nil {
					if closeErr := reader.Close(); closeErr != nil {
						t.Errorf("close unexpected reader: %v", closeErr)
					}
				}
				requireObjectStatus(t, err, http.StatusNotModified)
			}
			for _, condition := range []storage.Conditions{
				{GenerationMatch: attrs.Generation + 1},
				{DoesNotExist: true},
				{MetagenerationMatch: attrs.Metageneration + 1},
			} {
				conditional := object.If(condition)
				_, err := conditional.Attrs(ctx)
				requireObjectStatus(t, err, http.StatusPreconditionFailed)
				reader, err := conditional.NewReader(ctx)
				if reader != nil {
					if closeErr := reader.Close(); closeErr != nil {
						t.Errorf("close unexpected reader: %v", closeErr)
					}
				}
				requireObjectStatus(t, err, http.StatusPreconditionFailed)
				requireObjectStatus(t, conditional.Delete(ctx), http.StatusPreconditionFailed)
			}
			readObject(t, ctx, object, payload, attrs.Generation)
			requireObjectStatus(t, bucket.Delete(ctx), http.StatusConflict)
			if err := object.Generation(attrs.Generation).If(storage.Conditions{
				GenerationMatch: attrs.Generation, MetagenerationMatch: attrs.Metageneration,
			}).Delete(ctx); err != nil {
				t.Fatalf("conditional generation delete: %v", err)
			}
			requireObjectMissing(t, ctx, object)
		})
	}
	t.Run("unsupported SDK resumable writer", func(t *testing.T) {
		object := bucket.Object("unsupported-writer")
		cleanupObject(t, object)
		writer := object.NewWriter(ctx)
		writer.ChunkSize = 256 << 10
		// Exceed one chunk so the pinned SDK cannot choose single-request multipart.
		_, writeErr := writer.Write(bytes.Repeat([]byte("x"), writer.ChunkSize+1))
		closeErr := writer.Close()
		if writeErr != nil {
			requireObjectStatus(t, writeErr, http.StatusNotImplemented)
		}
		requireObjectStatus(t, closeErr, http.StatusNotImplemented)
		var apiError *googleapi.Error
		if !errors.As(closeErr, &apiError) || apiError.Message == "" || len(apiError.Errors) == 0 || apiError.Errors[0].Reason != "notImplemented" {
			t.Fatalf("resumable rejection must explain unsupported semantics: %v", closeErr)
		}
		requireObjectMissing(t, ctx, object)
	})
	if err := bucket.Delete(ctx); err != nil {
		t.Fatalf("delete empty bucket: %v", err)
	}
	deleted = true
}
