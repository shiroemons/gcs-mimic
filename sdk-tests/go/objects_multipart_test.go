package gcsmimic_test

import (
	"bytes"
	"context"
	"crypto/md5"
	"crypto/rand"
	"errors"
	"fmt"
	"hash/crc32"
	"net/http"
	"reflect"
	"testing"
	"time"

	"cloud.google.com/go/storage"
)

func TestObjectMultipartWriter(t *testing.T) {
	ctx, client, _, _ := newEmulatorClient(t)
	var nonce [8]byte
	if _, err := rand.Read(nonce[:]); err != nil {
		t.Fatal(err)
	}
	bucket := client.Bucket(fmt.Sprintf("sdk-go-multipart-%x", nonce))
	if err := bucket.Create(ctx, "sdk-multipart", nil); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		cleanupCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		if err := bucket.Delete(cleanupCtx); err != nil {
			t.Errorf("cleanup bucket: %v", err)
		}
	})
	for _, payload := range [][]byte{nil, bytes.Repeat([]byte("multipart\x00\xff\r\n"), 12000)} {
		t.Run(fmt.Sprintf("%d bytes", len(payload)), func(t *testing.T) {
			object := bucket.Object("dir/日本語 +%?" + fmt.Sprint(len(payload)))
			t.Cleanup(func() {
				cleanupCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
				defer cancel()
				if err := object.Delete(cleanupCtx); err != nil && !errors.Is(err, storage.ErrObjectNotExist) {
					t.Errorf("cleanup object: %v", err)
				}
			})
			want := storage.ObjectAttrs{
				Name:        object.ObjectName(),
				ContentType: "application/x-multipart-test", CacheControl: "private, max-age=60",
				ContentDisposition: `attachment; filename="example.bin"`, ContentLanguage: "en",
				Metadata: map[string]string{"team": "sdk", "unicode": "雪", "empty": ""},
			}
			sum := md5.Sum(payload)
			want.MD5 = sum[:]
			want.CRC32C = crc32.Checksum(payload, crc32.MakeTable(crc32.Castagnoli))
			writer := object.If(storage.Conditions{DoesNotExist: true}).NewWriter(ctx)
			writer.ChunkSize = 0
			writer.ObjectAttrs = want
			writer.SendCRC32C = true
			if _, err := writer.Write(payload); err != nil {
				t.Fatal(err)
			}
			if err := writer.Close(); err != nil {
				t.Fatal(err)
			}
			attrs, err := object.Attrs(ctx)
			if err != nil {
				t.Fatal(err)
			}
			for _, got := range []*storage.ObjectAttrs{writer.Attrs(), attrs} {
				if got.Bucket != bucket.BucketName() || got.Name != object.ObjectName() || got.Size != int64(len(payload)) ||
					got.Generation <= 0 || got.Metageneration != 1 || got.ContentType != want.ContentType ||
					got.CacheControl != want.CacheControl || got.ContentDisposition != want.ContentDisposition ||
					got.ContentLanguage != want.ContentLanguage || !reflect.DeepEqual(got.Metadata, want.Metadata) ||
					got.CRC32C != want.CRC32C || !bytes.Equal(got.MD5, want.MD5) {
					t.Fatalf("multipart metadata: %+v; want %+v", got, want)
				}
			}
			readObject(t, ctx, object, payload, attrs.Generation)
			for _, condition := range []storage.Conditions{
				{DoesNotExist: true},
				{GenerationMatch: attrs.Generation, MetagenerationMatch: attrs.Metageneration + 1},
			} {
				retry := object.If(condition).NewWriter(ctx)
				retry.ChunkSize = 0
				if _, err := retry.Write(payload); err != nil {
					requireObjectStatus(t, err, http.StatusPreconditionFailed)
				}
				requireObjectStatus(t, retry.Close(), http.StatusPreconditionFailed)
			}
			readObject(t, ctx, object, payload, attrs.Generation)
			replacement := []byte("conditional multipart replacement")
			overwrite := object.If(storage.Conditions{
				GenerationMatch: attrs.Generation, MetagenerationMatch: attrs.Metageneration,
			}).NewWriter(ctx)
			overwrite.ChunkSize = 0
			if _, err := overwrite.Write(replacement); err != nil {
				t.Fatal(err)
			}
			if err := overwrite.Close(); err != nil {
				t.Fatal(err)
			}
			updated, err := object.Attrs(ctx)
			if err != nil {
				t.Fatal(err)
			}
			if updated.Generation == attrs.Generation || updated.Metageneration != 1 || len(updated.Metadata) != 0 {
				t.Fatalf("replacement metadata: %+v; previous generation=%d", updated, attrs.Generation)
			}
			stale := object.If(storage.Conditions{GenerationMatch: attrs.Generation}).NewWriter(ctx)
			stale.ChunkSize = 0
			if _, err := stale.Write(payload); err != nil {
				requireObjectStatus(t, err, http.StatusPreconditionFailed)
			}
			requireObjectStatus(t, stale.Close(), http.StatusPreconditionFailed)
			readObject(t, ctx, object, replacement, updated.Generation)
		})
	}
	t.Run("checksum mismatch does not publish", func(t *testing.T) {
		for _, algorithm := range []string{"CRC32C", "MD5"} {
			t.Run(algorithm, func(t *testing.T) {
				object := bucket.Object("bad-" + algorithm)
				t.Cleanup(func() {
					cleanupCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
					defer cancel()
					if err := object.Delete(cleanupCtx); err != nil && !errors.Is(err, storage.ErrObjectNotExist) {
						t.Errorf("cleanup object: %v", err)
					}
				})
				writer := object.NewWriter(ctx)
				writer.ChunkSize = 0
				if algorithm == "CRC32C" {
					writer.SendCRC32C = true
					writer.CRC32C = 1
				} else {
					writer.MD5 = make([]byte, md5.Size)
				}
				if _, err := writer.Write([]byte("must not be published")); err != nil {
					requireObjectStatus(t, err, http.StatusBadRequest)
				}
				requireObjectStatus(t, writer.Close(), http.StatusBadRequest)
				requireObjectMissing(t, ctx, object)
			})
		}
	})
}
