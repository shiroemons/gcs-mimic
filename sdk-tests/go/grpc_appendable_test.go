package gcsmimic_test

import (
	"bytes"
	"context"
	"errors"
	"io"
	"os"
	"testing"
	"time"

	"cloud.google.com/go/storage"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
)

func appendWrite(ctx context.Context, writer *storage.Writer, payload []byte) error {
	_, writeErr := writer.Write(payload)
	if closeErr := writer.Close(); closeErr != nil {
		return closeErr
	}
	return writeErr
}

func readAll(ctx context.Context, object *storage.ObjectHandle) ([]byte, error) {
	reader, err := object.NewReader(ctx)
	if err != nil {
		return nil, err
	}
	defer reader.Close()
	return io.ReadAll(reader)
}

// TestGRPCAppendable covers the ordinary Go Writer.Append flow: an unfinalized appendable
// object is readable over gRPC, a takeover resumes at the persisted offset, and
// FinalizeOnClose publishes it with the same generation to every protocol.
func TestGRPCAppendable(t *testing.T) {
	if os.Getenv("STORAGE_EMULATOR_HOST_GRPC") == "" {
		t.Skip("STORAGE_EMULATOR_HOST_GRPC is unset")
	}
	ctx, jsonClient, _, _ := newEmulatorClient(t)
	bucket := resumableBucket(t, ctx, jsonClient, "appendable")
	ctx, cancel := context.WithTimeout(ctx, 20*time.Second)
	defer cancel()
	client, err := storage.NewGRPCClient(ctx)
	if err != nil {
		t.Fatal(err)
	}
	defer client.Close()
	bidiClient, err := storage.NewGRPCClient(ctx, storage.WithGRPCBidiReads())
	if err != nil {
		t.Fatal(err)
	}
	defer bidiClient.Close()
	grpcBucket := client.Bucket(bucket.BucketName())
	object := grpcBucket.Object("log")
	resumableObject(t, bucket, "log")

	writer := object.NewWriter(ctx)
	writer.Append = true
	writer.ContentType = "text/plain"
	if err := appendWrite(ctx, writer, []byte("hello")); err != nil {
		t.Fatal(err)
	}
	created := writer.Attrs()
	if created == nil || created.Generation <= 0 || created.Size != 5 || !created.Finalized.IsZero() || created.MD5 != nil {
		t.Fatalf("unfinalized writer attrs: %+v", created)
	}
	got, err := readAll(ctx, object)
	if err != nil || !bytes.Equal(got, []byte("hello")) {
		t.Fatalf("ReadObject of unfinalized object: %q %v", got, err)
	}
	attrs, err := object.Attrs(ctx)
	if err != nil || attrs.Generation != created.Generation || attrs.Size != 5 || attrs.MD5 != nil || attrs.ContentType != "text/plain" {
		t.Fatalf("GetObject of unfinalized object: %+v %v", attrs, err)
	}
	downloader, err := bidiClient.Bucket(bucket.BucketName()).Object("log").NewMultiRangeDownloader(ctx)
	if err != nil {
		t.Fatal(err)
	}
	var tail bytes.Buffer
	done := make(chan error, 1)
	downloader.Add(&tail, 1, 0, func(_, _ int64, err error) { done <- err })
	downloader.Wait()
	if err := <-done; err != nil || tail.String() != "ello" {
		t.Fatalf("BidiReadObject of unfinalized object: %q %v", tail.String(), err)
	}
	if err := downloader.Close(); err != nil {
		t.Fatal(err)
	}
	// JSON and XML see the object only after finalization.
	if _, err := bucket.Object("log").Attrs(ctx); !errors.Is(err, storage.ErrObjectNotExist) {
		t.Fatalf("JSON must not list unfinalized appendable objects: %v", err)
	}

	resumed, offset, err := object.Generation(created.Generation).NewWriterFromAppendableObject(ctx, &storage.AppendableWriterOpts{FinalizeOnClose: true})
	if err != nil || offset != 5 {
		t.Fatalf("takeover offset %d: %v", offset, err)
	}
	if err := appendWrite(ctx, resumed, []byte(" world")); err != nil {
		t.Fatal(err)
	}
	if final := resumed.Attrs(); final == nil || final.Generation != created.Generation || final.Size != 11 {
		t.Fatalf("finalized writer attrs: %+v", final)
	}
	jsonAttrs, err := bucket.Object("log").Attrs(ctx)
	if err != nil || jsonAttrs.Generation != created.Generation || jsonAttrs.Size != 11 {
		t.Fatalf("JSON attrs after finalization: %+v %v", jsonAttrs, err)
	}
	got, err = readAll(ctx, bucket.Object("log"))
	if err != nil || string(got) != "hello world" {
		t.Fatalf("JSON read after finalization: %q %v", got, err)
	}
	if _, _, err := object.Generation(created.Generation).NewWriterFromAppendableObject(ctx, nil); status.Code(err) != codes.FailedPrecondition {
		t.Fatalf("takeover of a finalized object must fail: %v", err)
	}
}

// TestGRPCAppendableSingleWriter checks that a takeover cuts off the previous writer.
func TestGRPCAppendableSingleWriter(t *testing.T) {
	if os.Getenv("STORAGE_EMULATOR_HOST_GRPC") == "" {
		t.Skip("STORAGE_EMULATOR_HOST_GRPC is unset")
	}
	ctx, jsonClient, _, _ := newEmulatorClient(t)
	bucket := resumableBucket(t, ctx, jsonClient, "appendable")
	ctx, cancel := context.WithTimeout(ctx, 20*time.Second)
	defer cancel()
	client, err := storage.NewGRPCClient(ctx)
	if err != nil {
		t.Fatal(err)
	}
	defer client.Close()
	object := client.Bucket(bucket.BucketName()).Object("single")
	resumableObject(t, bucket, "single")
	writer := object.NewWriter(ctx)
	writer.Append = true
	if err := appendWrite(ctx, writer, []byte("base")); err != nil {
		t.Fatal(err)
	}
	generation := writer.Attrs().Generation
	first, _, err := object.Generation(generation).NewWriterFromAppendableObject(ctx, nil)
	if err != nil {
		t.Fatal(err)
	}
	second, offset, err := object.Generation(generation).NewWriterFromAppendableObject(ctx, &storage.AppendableWriterOpts{FinalizeOnClose: true})
	if err != nil || offset != 4 {
		t.Fatalf("second takeover offset %d: %v", offset, err)
	}
	if err := appendWrite(ctx, first, []byte("-stale")); status.Code(err) != codes.FailedPrecondition {
		t.Fatalf("superseded writer must fail: %v", err)
	}
	if err := appendWrite(ctx, second, []byte("-current")); err != nil {
		t.Fatal(err)
	}
	got, err := readAll(ctx, object)
	if err != nil || string(got) != "base-current" {
		t.Fatalf("content after takeover: %q %v", got, err)
	}
}
