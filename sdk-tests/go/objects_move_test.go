package gcsmimic_test

import (
	"errors"
	"io"
	"net/http"
	"testing"

	"cloud.google.com/go/storage"
)

func TestObjectMove(t *testing.T) {
	ctx, client, _, _ := newEmulatorClient(t)
	bucket := resumableBucket(t, ctx, client, "sdk-move")
	source := resumableObject(t, bucket, "source/猫%2F")
	target := resumableObject(t, bucket, "destination/猫%2F")
	writer := source.NewWriter(ctx)
	if _, err := writer.Write([]byte("move bytes")); err != nil {
		t.Fatal(err)
	}
	if err := writer.Close(); err != nil {
		t.Fatal(err)
	}
	original, err := source.Attrs(ctx)
	if err != nil {
		t.Fatal(err)
	}
	_, err = source.If(storage.Conditions{GenerationMatch: original.Generation + 1}).Move(ctx, storage.MoveObjectDestination{Object: target.ObjectName()})
	requireStrictError(t, err, http.StatusPreconditionFailed, "conditionNotMet")
	moved, err := source.Move(ctx, storage.MoveObjectDestination{Object: target.ObjectName()})
	if err != nil {
		t.Fatal(err)
	}
	if moved.Name != target.ObjectName() || moved.Generation == original.Generation || moved.Metageneration != 1 {
		t.Fatalf("moved: %+v", moved)
	}
	if _, err := source.Attrs(ctx); !errors.Is(err, storage.ErrObjectNotExist) {
		t.Fatalf("source still exists: %v", err)
	}
	reader, err := target.NewReader(ctx)
	if err != nil {
		t.Fatal(err)
	}
	data, readErr := io.ReadAll(reader)
	closeErr := reader.Close()
	if readErr != nil || closeErr != nil || string(data) != "move bytes" {
		t.Fatalf("moved bytes: %q, %v, %v", data, readErr, closeErr)
	}
}
