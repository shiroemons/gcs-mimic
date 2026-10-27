package gcsmimic_test

import (
	"context"
	"crypto/rand"
	"errors"
	"fmt"
	"hash/crc32"
	"net/http"
	"testing"
	"time"

	"cloud.google.com/go/storage"
)

func TestSDKObjectDefaults(t *testing.T) {
	ctx, client, _, _ := newEmulatorClient(t)
	bucket := resumableBucket(t, ctx, client, "sdk-defaults")
	source := resumableObject(t, bucket, "extensionless")
	writer := source.NewWriter(ctx)
	if _, err := writer.Write([]byte("default bytes")); err != nil {
		t.Fatal(err)
	}
	if err := writer.Close(); err != nil {
		t.Fatal(err)
	}
	attrs, err := source.Attrs(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if attrs.ContentType != "text/plain; charset=utf-8" {
		t.Fatalf("default upload content type: %q", attrs.ContentType)
	}
	destination := resumableObject(t, bucket, "composed")
	composed, err := destination.ComposerFrom(source, source).Run(ctx)
	if err != nil {
		t.Fatal(err)
	}
	readObject(t, ctx, destination, []byte("default bytesdefault bytes"), composed.Generation)
	readObject(t, ctx, source, []byte("default bytes"), attrs.Generation)
}

func TestObjectCompose(t *testing.T) {
	ctx, client, _, _ := newEmulatorClient(t)
	var nonce [8]byte
	if _, err := rand.Read(nonce[:]); err != nil {
		t.Fatal(err)
	}
	bucket := client.Bucket(fmt.Sprintf("sdk-go-compose-%x", nonce))
	if err := bucket.Create(ctx, "sdk-compose", nil); err != nil {
		t.Fatal(err)
	}
	a, b, destination := bucket.Object("dir/雪-a"), bucket.Object("b"), bucket.Object("composite")
	t.Cleanup(func() {
		cleanupCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		for _, object := range []*storage.ObjectHandle{a, b, destination} {
			if err := object.Delete(cleanupCtx); err != nil && !errors.Is(err, storage.ErrObjectNotExist) {
				t.Errorf("cleanup %s: %v", object.ObjectName(), err)
			}
		}
		if err := bucket.Delete(cleanupCtx); err != nil {
			t.Errorf("cleanup bucket: %v", err)
		}
	})
	first, err := strictUpload(ctx, a, []byte("first\x00"))
	if err != nil {
		t.Fatal(err)
	}
	second, err := strictUpload(ctx, b, []byte("second\xff"))
	if err != nil {
		t.Fatal(err)
	}
	composer := destination.If(storage.Conditions{DoesNotExist: true}).ComposerFrom(
		b.Generation(second.Generation),
		a.If(storage.Conditions{GenerationMatch: first.Generation}),
		b,
	)
	composer.ContentType = "application/x-composite"
	composer.Metadata = map[string]string{"source": "compose"}
	// The JSON composite guide ignores supplied hashes; the SDK comment differs.
	composer.SendCRC32C = true
	composer.CRC32C = 0
	attrs, err := composer.Run(ctx)
	if err != nil {
		t.Fatal(err)
	}
	want := []byte("second\xfffirst\x00second\xff")
	if attrs.ComponentCount != 3 || attrs.CRC32C != crc32.Checksum(want, crc32.MakeTable(crc32.Castagnoli)) || len(attrs.MD5) != 0 ||
		attrs.ContentType != composer.ContentType || attrs.Metadata["source"] != "compose" {
		t.Fatalf("composite metadata: %+v", attrs)
	}
	readObject(t, ctx, destination, want, attrs.Generation)
	_, err = composer.Run(ctx)
	requireObjectStatus(t, err, http.StatusPreconditionFailed)
	readObject(t, ctx, destination, want, attrs.Generation)
	badSource := destination.ComposerFrom(a.If(storage.Conditions{GenerationMatch: first.Generation + 1}))
	_, err = badSource.Run(ctx)
	requireObjectStatus(t, err, http.StatusPreconditionFailed)
	readObject(t, ctx, destination, want, attrs.Generation)
	readObject(t, ctx, a, []byte("first\x00"), first.Generation)
	readObject(t, ctx, b, []byte("second\xff"), second.Generation)

	appendComposer := destination.If(storage.Conditions{GenerationMatch: attrs.Generation}).ComposerFrom(destination, a, a, b)
	appendComposer.DeleteSourceObjects = true
	appended, err := appendComposer.Run(ctx)
	if err != nil {
		t.Fatal(err)
	}
	want = append(want, []byte("first\x00first\x00second\xff")...)
	if appended.ComponentCount != 6 || len(appended.MD5) != 0 {
		t.Fatalf("append metadata: %+v", appended)
	}
	readObject(t, ctx, destination, want, appended.Generation)
	requireObjectMissing(t, ctx, a)
	requireObjectMissing(t, ctx, b)
}

func TestObjectComposeStrictPermissions(t *testing.T) {
	ctx, admin := newStrictClient(t, strictAdminToken)
	_, viewer := newStrictClient(t, strictViewerToken)
	_, creator := newStrictClient(t, strictCreatorToken)
	var nonce [8]byte
	if _, err := rand.Read(nonce[:]); err != nil {
		t.Fatal(err)
	}
	name := fmt.Sprintf("sdk-go-compose-strict-%x", nonce)
	bucket := admin.Bucket(name)
	if err := bucket.Create(ctx, strictProject, nil); err != nil {
		t.Fatal(err)
	}
	source, destination := bucket.Object("source"), bucket.Object("destination")
	t.Cleanup(func() {
		cleanupCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		for _, object := range []*storage.ObjectHandle{source, destination} {
			if err := object.Delete(cleanupCtx); err != nil && !errors.Is(err, storage.ErrObjectNotExist) {
				t.Errorf("cleanup object: %v", err)
			}
		}
		if err := bucket.Delete(cleanupCtx); err != nil {
			t.Errorf("cleanup bucket: %v", err)
		}
	})
	if _, err := strictUpload(ctx, source, []byte("strict compose")); err != nil {
		t.Fatal(err)
	}
	for _, client := range []*storage.Client{viewer, creator} {
		_, err := client.Bucket(name).Object("destination").ComposerFrom(client.Bucket(name).Object("source")).Run(ctx)
		requireStrictError(t, err, http.StatusForbidden, "forbidden")
	}
	composer := destination.If(storage.Conditions{DoesNotExist: true}).ComposerFrom(source)
	attrs, err := composer.Run(ctx)
	if err != nil {
		t.Fatal(err)
	}
	readObject(t, ctx, destination, []byte("strict compose"), attrs.Generation)
	if attrs.ComponentCount != 1 || len(attrs.MD5) != 0 {
		t.Fatalf("one-source composite: %+v", attrs)
	}
}
