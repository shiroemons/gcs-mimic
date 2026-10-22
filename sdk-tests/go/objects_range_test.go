package gcsmimic_test

import (
	"bytes"
	"context"
	"crypto/rand"
	"errors"
	"fmt"
	"io"
	"net/http"
	"testing"
	"time"

	"cloud.google.com/go/storage"
)

func TestObjectRangeReader(t *testing.T) {
	ctx, client, _, _ := newEmulatorClient(t)
	var nonce [8]byte
	if _, err := rand.Read(nonce[:]); err != nil {
		t.Fatal(err)
	}
	bucket := client.Bucket(fmt.Sprintf("sdk-go-range-%x", nonce))
	if err := bucket.Create(ctx, "sdk-range", nil); err != nil {
		t.Fatal(err)
	}
	object := bucket.Object("dir/range 日本語.bin")
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
	payload := []byte("0123456789")
	writer := object.NewWriter(ctx)
	writer.ChunkSize = 0
	if _, err := writer.Write(payload); err != nil {
		t.Fatal(err)
	}
	if err := writer.Close(); err != nil {
		t.Fatal(err)
	}
	generation := writer.Attrs().Generation
	for _, tc := range []struct {
		name           string
		offset, length int64
		want           string
	}{
		{"closed", 2, 4, "2345"},
		{"open ended", 6, -1, "6789"},
		{"suffix", -3, -1, "789"},
		{"clamped", 8, 100, "89"},
		{"large suffix", -100, -1, "0123456789"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			reader, err := object.Generation(generation).NewRangeReader(ctx, tc.offset, tc.length)
			if err != nil {
				t.Fatal(err)
			}
			got, readErr := io.ReadAll(io.LimitReader(reader, int64(len(payload))+1))
			closeErr := reader.Close()
			if readErr != nil || closeErr != nil || !bytes.Equal(got, []byte(tc.want)) {
				t.Fatalf("range: got=%q want=%q read=%v close=%v", got, tc.want, readErr, closeErr)
			}
			if reader.Attrs.Size != int64(len(payload)) || reader.Attrs.Generation != generation {
				t.Fatalf("range attrs: %+v", reader.Attrs)
			}
		})
	}
	for _, offset := range []int64{10, 100} {
		reader, err := object.NewRangeReader(ctx, offset, -1)
		if reader != nil {
			if closeErr := reader.Close(); closeErr != nil {
				t.Errorf("close unexpected reader: %v", closeErr)
			}
		}
		requireObjectStatus(t, err, http.StatusRequestedRangeNotSatisfiable)
	}
	_, err := object.If(storage.Conditions{GenerationMatch: generation + 1}).NewRangeReader(ctx, 2, 4)
	requireObjectStatus(t, err, http.StatusPreconditionFailed)
}
