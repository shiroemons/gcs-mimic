package gcsmimic_test

import (
	"bytes"
	"context"
	"crypto/rand"
	"fmt"
	"slices"
	"testing"
	"time"
)

const bucketReaderMember = "user:local-bucket-reader@example.invalid"

func TestStrictAuthBucketIAM(t *testing.T) {
	ctx, bucket, attrs, payload := strictFixture(t, "iam")
	_, other, otherAttrs, _ := strictFixture(t, "iam-other")
	_, reader := newStrictClient(t, "local-bucket-reader-token")
	_, viewer := newStrictClient(t, strictViewerToken)
	readerBucket := reader.Bucket(bucket.BucketName())
	policy, err := bucket.IAM().Policy(ctx)
	if err != nil {
		t.Fatalf("admin get default policy: %v", err)
	}
	if len(policy.InternalProto.Etag) == 0 || len(policy.Roles()) != 2 ||
		!slices.Equal(slices.Sorted(slices.Values(policy.Members("roles/storage.legacyBucketOwner"))),
			[]string{"projectEditor:" + strictProject, "projectOwner:" + strictProject}) ||
		!slices.Equal(policy.Members("roles/storage.legacyBucketReader"), []string{"projectViewer:" + strictProject}) {
		t.Fatalf("unexpected default policy: %v", policy.InternalProto)
	}
	_, err = readerBucket.Object(attrs.Name).Attrs(ctx)
	requireStrictError(t, err, 403, "forbidden")
	_, err = listObjectNames(ctx, readerBucket)
	requireStrictError(t, err, 403, "forbidden")
	stale, err := bucket.IAM().Policy(ctx)
	if err != nil {
		t.Fatal(err)
	}
	policy.Add(bucketReaderMember, "roles/storage.objectViewer")
	if err := bucket.IAM().SetPolicy(ctx, policy); err != nil {
		t.Fatalf("admin grant bucket-only viewer: %v", err)
	}
	granted, err := bucket.IAM().Policy(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if !granted.HasRole(bucketReaderMember, "roles/storage.objectViewer") ||
		bytes.Equal(granted.InternalProto.Etag, stale.InternalProto.Etag) {
		t.Fatalf("grant or fresh etag missing: %v", granted.InternalProto)
	}
	requireStrictError(t, bucket.IAM().SetPolicy(ctx, stale), 412, "conditionNotMet")

	readObject(t, ctx, readerBucket.Object(attrs.Name), payload, attrs.Generation)
	names, err := listObjectNames(ctx, readerBucket)
	if err != nil || !slices.Equal(names, []string{attrs.Name}) {
		t.Fatalf("bucket-only list: names=%v, error=%v", names, err)
	}
	_, err = reader.Bucket(other.BucketName()).Object(otherAttrs.Name).Attrs(ctx)
	requireStrictError(t, err, 403, "forbidden")
	_, err = reader.Bucket(other.BucketName()).Object(otherAttrs.Name).NewReader(ctx)
	requireStrictError(t, err, 403, "forbidden")
	_, err = listObjectNames(ctx, reader.Bucket(other.BucketName()))
	requireStrictError(t, err, 403, "forbidden")
	_, err = strictUpload(ctx, readerBucket.Object("denied.txt"), []byte("denied"))
	requireStrictError(t, err, 403, "forbidden")
	requireObjectMissing(t, ctx, bucket.Object("denied.txt"))
	_, err = viewer.Bucket(bucket.BucketName()).IAM().Policy(ctx)
	requireStrictError(t, err, 403, "forbidden")
	requireStrictError(t, viewer.Bucket(bucket.BucketName()).IAM().SetPolicy(ctx, granted), 403, "forbidden")
	_, err = readerBucket.IAM().Policy(ctx)
	requireStrictError(t, err, 403, "forbidden")
	requireStrictError(t, readerBucket.IAM().SetPolicy(ctx, granted), 403, "forbidden")

	granted.Remove(bucketReaderMember, "roles/storage.objectViewer")
	if err := bucket.IAM().SetPolicy(ctx, granted); err != nil {
		t.Fatalf("admin revoke: %v", err)
	}
	revoked, err := bucket.IAM().Policy(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if revoked.HasRole(bucketReaderMember, "roles/storage.objectViewer") ||
		bytes.Equal(revoked.InternalProto.Etag, granted.InternalProto.Etag) {
		t.Fatalf("revocation or fresh etag missing: %v", revoked.InternalProto)
	}
	_, err = readerBucket.Object(attrs.Name).Attrs(ctx)
	requireStrictError(t, err, 403, "forbidden")
	_, err = readerBucket.Object(attrs.Name).NewReader(ctx)
	requireStrictError(t, err, 403, "forbidden")
	_, err = listObjectNames(ctx, readerBucket)
	requireStrictError(t, err, 403, "forbidden")
	requireStrictObjectUnchanged(t, ctx, bucket, attrs, payload)
}

func TestBucketIAMPermissive(t *testing.T) {
	ctx, client, _, _ := newEmulatorClient(t)
	var nonce [8]byte
	if _, err := rand.Read(nonce[:]); err != nil {
		t.Fatal(err)
	}
	bucket := client.Bucket(fmt.Sprintf("sdk-go-iam-%x", nonce))
	if err := bucket.Create(ctx, "local-project", nil); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		cleanupCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		if err := bucket.Delete(cleanupCtx); err != nil {
			t.Errorf("cleanup bucket: %v", err)
		}
	})
	policy, err := bucket.IAM().Policy(ctx)
	if err != nil {
		t.Fatal(err)
	}
	policy.Add(bucketReaderMember, "roles/storage.objectViewer")
	if err := bucket.IAM().SetPolicy(ctx, policy); err != nil {
		t.Fatal(err)
	}
	got, err := bucket.IAM().Policy(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if !got.HasRole(bucketReaderMember, "roles/storage.objectViewer") ||
		len(got.InternalProto.Etag) == 0 || bytes.Equal(got.InternalProto.Etag, policy.InternalProto.Etag) {
		t.Fatalf("policy round trip: %v", got.InternalProto)
	}
	requireStrictError(t, bucket.IAM().SetPolicy(ctx, policy), 412, "conditionNotMet")
}
