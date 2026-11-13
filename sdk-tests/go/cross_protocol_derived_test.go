package gcsmimic_test

import (
	"bytes"
	"context"
	"encoding/json"
	"hash/crc32"
	"net/http"
	"reflect"
	"testing"
	"time"

	"cloud.google.com/go/storage"
	pb "google.golang.org/genproto/googleapis/storage/v1"
	"google.golang.org/protobuf/types/known/wrapperspb"
)

// Objects derived through one transport (rewrite, copy, compose, move) and objects written or
// changed through legacy v1 must look the same through every other transport. The
// per-operation rows verify each operation on its own transport; this matrix checks that the
// result is one shared state.

// comparable drops the empty-versus-absent distinction that the transports encode differently
// for a missing MD5 or an empty body.
func (v crossView) comparable() crossView {
	if len(v.MD5) == 0 {
		v.MD5 = nil
	}
	if len(v.Body) == 0 {
		v.Body = nil
	}
	return v
}

// requireAgreement takes the JSON view as the reference, checks its bytes and CRC32C against
// the expected body, and requires every other transport to observe the same generation state.
func (e *crossEnv) requireAgreement(label, name string, body []byte) crossView {
	e.t.Helper()
	reference, err := e.viewJSON(name, 0)
	if err != nil {
		e.t.Fatalf("%s: JSON view of %s: %v", label, name, err)
	}
	if !bytes.Equal(reference.Body, body) || reference.Size != int64(len(body)) ||
		reference.CRC32C != crc32.Checksum(body, crc32.MakeTable(crc32.Castagnoli)) {
		e.t.Fatalf("%s: JSON view of %s is %+v, want body %q", label, name, reference, body)
	}
	for _, observer := range crossObservers {
		got, err := observer.view(e, name, 0)
		if err != nil {
			e.t.Fatalf("%s: %s view of %s: %v", label, observer.name, name, err)
		}
		want := reference
		if observer.metadataOnly {
			want.Body = nil
		}
		// An XML GET always sends a Content-Type header, so an object without a stored content
		// type is served as application/octet-stream there.
		if observer.name == "xml" && want.ContentType == "" {
			want.ContentType = "application/octet-stream"
		}
		if !reflect.DeepEqual(got.comparable(), want.comparable()) {
			e.t.Fatalf("%s: %s view of %s\n got %+v\nwant %+v", label, observer.name, name, got, want)
		}
	}
	return reference
}

// postJSON sends a JSON API request whose resource body is encoded from value.
func (e *crossEnv) postJSON(path string, value any) {
	e.t.Helper()
	body, err := json.Marshal(value)
	if err != nil {
		e.t.Fatal(err)
	}
	if _, _, err := e.send(http.MethodPost, path, nil, http.Header{"Content-Type": {"application/json"}}, body); err != nil {
		e.t.Fatalf("POST %s: %v", path, err)
	}
}

func TestCrossProtocolDerivedObjects(t *testing.T) {
	grpcEndpoint := crossGRPCEndpoint(t, "STORAGE_EMULATOR_HOST_GRPC")
	ctx, jsonClient, httpClient, endpoint := newEmulatorClient(t)
	bucket := resumableBucket(t, ctx, jsonClient, "cross-derived")
	ctx, cancel := context.WithTimeout(ctx, 30*time.Second)
	defer cancel()
	env := newCrossEnv(t, ctx, httpClient, endpoint, grpcEndpoint, bucket.BucketName())
	objects := env.sdk.Bucket(env.bucket)
	t.Cleanup(func() {
		cleanup, stop := context.WithTimeout(context.Background(), 5*time.Second)
		defer stop()
		for _, name := range []string{"src-a", "src-b", "rewritten-json", "copied-grpc", "copied-xml", "composed-json", "composed-grpc", "composed-v1", "inserted-v1", "moved-json", "moved-grpc"} {
			_ = bucket.Object(name).Delete(cleanup)
		}
	})

	alpha, beta := []byte("alpha"), []byte("beta")
	if _, err := env.writeJSON("src-a", alpha, nil); err != nil {
		t.Fatal(err)
	}
	if _, err := env.writeXML("src-b", beta, nil); err != nil {
		t.Fatal(err)
	}

	// Rewrite and copy through each transport keep the source bytes and metadata.
	env.postJSON(env.jsonPath("src-a")+"/rewriteTo/b/"+env.bucket+"/o/rewritten-json", map[string]any{})
	env.requireAgreement("JSON rewrite", "rewritten-json", alpha)
	if _, err := objects.Object("copied-grpc").CopierFrom(objects.Object("src-b")).Run(ctx); err != nil {
		t.Fatalf("gRPC rewrite: %v", err)
	}
	env.requireAgreement("gRPC rewrite", "copied-grpc", beta)
	if _, _, err := env.send(http.MethodPut, env.xmlPath("copied-xml"), nil, http.Header{"X-Goog-Copy-Source": {"/" + env.bucket + "/src-a"}}, nil); err != nil {
		t.Fatalf("XML copy: %v", err)
	}
	env.requireAgreement("XML copy", "copied-xml", alpha)

	// Compose through each transport concatenates in the requested order.
	env.postJSON(env.jsonPath("composed-json")+"/compose", map[string]any{
		"sourceObjects": []map[string]string{{"name": "src-a"}, {"name": "src-b"}},
		"destination":   map[string]string{"contentType": crossContentType},
	})
	env.requireAgreement("JSON compose", "composed-json", append(append([]byte{}, alpha...), beta...))
	composer := objects.Object("composed-grpc").ComposerFrom(objects.Object("src-b"), objects.Object("src-a"))
	composer.ContentType = crossContentType
	if _, err := composer.Run(ctx); err != nil {
		t.Fatalf("gRPC compose: %v", err)
	}
	env.requireAgreement("gRPC compose", "composed-grpc", append(append([]byte{}, beta...), alpha...))
	if _, err := env.v1.ComposeObject(ctx, &pb.ComposeObjectRequest{DestinationBucket: env.bucket, DestinationObject: "composed-v1", SourceObjects: []*pb.ComposeObjectRequest_SourceObjects{{Name: "src-a"}, {Name: "src-a"}}}); err != nil {
		t.Fatalf("v1 compose: %v", err)
	}
	env.requireAgreement("v1 compose", "composed-v1", append(append([]byte{}, alpha...), alpha...))

	// Legacy v1 writes and metadata changes are shared state too.
	stream, err := env.v1.InsertObject(ctx)
	if err != nil {
		t.Fatal(err)
	}
	legacy := []byte("legacy")
	if err := stream.Send(&pb.InsertObjectRequest{
		FirstMessage: &pb.InsertObjectRequest_InsertObjectSpec{InsertObjectSpec: &pb.InsertObjectSpec{Resource: &pb.Object{Bucket: env.bucket, Name: "inserted-v1"}}},
		Data:         &pb.InsertObjectRequest_ChecksummedData{ChecksummedData: &pb.ChecksummedData{Content: legacy}},
		FinishWrite:  true,
	}); err != nil {
		t.Fatal(err)
	}
	if _, err := stream.CloseAndRecv(); err != nil {
		t.Fatalf("v1 insert: %v", err)
	}
	inserted := env.requireAgreement("v1 insert", "inserted-v1", legacy)
	if _, err := env.v1.PatchObject(ctx, &pb.PatchObjectRequest{Bucket: env.bucket, Object: "copied-xml", IfMetagenerationMatch: wrapperspb.Int64(1), Metadata: &pb.Object{Metadata: map[string]string{"patched": "v1"}}}); err != nil {
		t.Fatalf("v1 patch: %v", err)
	}
	patched := env.requireAgreement("v1 patch", "copied-xml", alpha)
	if patched.Metageneration != 2 || patched.Metadata["patched"] != "v1" {
		t.Fatalf("v1 patch is not visible: %+v", patched)
	}

	// A move publishes the destination and removes the source for every transport.
	env.postJSON(env.jsonPath("rewritten-json")+"/moveTo/o/moved-json", map[string]any{})
	env.requireAgreement("JSON move", "moved-json", alpha)
	env.requireAbsent("JSON move source", "rewritten-json", 0)
	if _, err := objects.Object("copied-grpc").Move(ctx, storage.MoveObjectDestination{Object: "moved-grpc"}); err != nil {
		t.Fatalf("gRPC move: %v", err)
	}
	env.requireAgreement("gRPC move", "moved-grpc", beta)
	env.requireAbsent("gRPC move source", "copied-grpc", 0)

	listing := map[string][]int64{}
	for _, name := range []string{"src-a", "src-b", "copied-xml", "composed-json", "composed-grpc", "composed-v1", "inserted-v1", "moved-json", "moved-grpc"} {
		view, err := env.viewJSON(name, 0)
		if err != nil {
			t.Fatal(err)
		}
		listing[name] = []int64{view.Generation}
	}
	if listing["inserted-v1"][0] != inserted.Generation {
		t.Fatalf("inserted generation changed: %v", listing["inserted-v1"])
	}
	env.requireListings("after derived writes", false, listing)
}
