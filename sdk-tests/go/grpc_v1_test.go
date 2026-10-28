package gcsmimic_test

import (
	"context"
	"hash/crc32"
	"io"
	"net"
	"os"
	"testing"
	"time"

	pb "google.golang.org/genproto/googleapis/storage/v1"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/metadata"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/types/known/wrapperspb"
)

func TestGRPCV1Legacy(t *testing.T) {
	endpoint := os.Getenv("STORAGE_EMULATOR_HOST_GRPC")
	if endpoint == "" {
		t.Skip("STORAGE_EMULATOR_HOST_GRPC is unset")
	}
	host, _, err := net.SplitHostPort(endpoint)
	if err != nil || net.ParseIP(host) == nil || !net.ParseIP(host).IsLoopback() {
		t.Fatal("v1 test requires a loopback gRPC endpoint")
	}
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()
	conn, err := grpc.NewClient(endpoint, grpc.WithTransportCredentials(insecure.NewCredentials()))
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	client := pb.NewStorageClient(conn)
	name := "grpc-v1-legacy"
	if _, err = client.InsertBucket(ctx, &pb.InsertBucketRequest{Project: "local-project", Bucket: &pb.Bucket{Name: name, Labels: map[string]string{"old": "remove"}}}); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		cleanup, stop := context.WithTimeout(context.Background(), 2*time.Second)
		defer stop()
		_, _ = client.DeleteBucket(cleanup, &pb.DeleteBucketRequest{Bucket: name})
	})
	if _, err = client.GetBucket(ctx, &pb.GetBucketRequest{Bucket: name}); err != nil {
		t.Fatal(err)
	}
	if _, err = client.ListBuckets(ctx, &pb.ListBucketsRequest{Project: "local-project", Prefix: name}); err != nil {
		t.Fatal(err)
	}
	// Reference-emulator clients often omit the project; the listing then spans every project.
	other := name + "-other"
	if _, err = client.InsertBucket(ctx, &pb.InsertBucketRequest{Project: "other-project", Bucket: &pb.Bucket{Name: other}}); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		cleanup, stop := context.WithTimeout(context.Background(), 2*time.Second)
		defer stop()
		_, _ = client.DeleteBucket(cleanup, &pb.DeleteBucketRequest{Bucket: other})
	})
	var listed []string
	for token := ""; ; {
		page, err := client.ListBuckets(ctx, &pb.ListBucketsRequest{Prefix: name, MaxResults: 1, PageToken: token})
		if err != nil {
			t.Fatal(err)
		}
		for _, item := range page.Items {
			listed = append(listed, item.Name)
		}
		if token = page.NextPageToken; token == "" {
			break
		}
	}
	if len(listed) != 2 || listed[0] != name || listed[1] != other {
		t.Fatalf("project-free listing=%v", listed)
	}
	bucketAttrs, err := client.GetBucket(ctx, &pb.GetBucketRequest{Bucket: name})
	if err != nil {
		t.Fatal(err)
	}
	bucketAttrs.DefaultEventBasedHold = true
	bucketAttrs.Labels = map[string]string{"replacement": "yes"}
	if _, err = client.UpdateBucket(ctx, &pb.UpdateBucketRequest{Bucket: name, Metadata: bucketAttrs}); err != nil {
		t.Fatal(err)
	}
	bucketAttrs, err = client.GetBucket(ctx, &pb.GetBucketRequest{Bucket: name})
	if err != nil || !bucketAttrs.GetDefaultEventBasedHold() || len(bucketAttrs.Labels) != 1 || bucketAttrs.Labels["replacement"] != "yes" {
		t.Fatalf("bucket update=%v error=%v", bucketAttrs, err)
	}
	bucketAttrs.Id = "changed-output"
	if _, err = client.UpdateBucket(ctx, &pb.UpdateBucketRequest{Bucket: name, Metadata: bucketAttrs}); status.Code(err) != codes.Unimplemented {
		t.Fatalf("changed bucket output=%v", err)
	}
	if _, err = client.UpdateBucket(ctx, &pb.UpdateBucketRequest{Bucket: name, Metadata: &pb.Bucket{DefaultEventBasedHold: false}}); err != nil {
		t.Fatal(err)
	}
	stream, err := client.InsertObject(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if err = stream.Send(&pb.InsertObjectRequest{FirstMessage: &pb.InsertObjectRequest_InsertObjectSpec{InsertObjectSpec: &pb.InsertObjectSpec{Resource: &pb.Object{Bucket: name, Name: "source"}}}, Data: &pb.InsertObjectRequest_ChecksummedData{ChecksummedData: &pb.ChecksummedData{Content: []byte("hello")}}}); err != nil {
		t.Fatal(err)
	}
	if err = stream.Send(&pb.InsertObjectRequest{WriteOffset: 5, Data: &pb.InsertObjectRequest_ChecksummedData{ChecksummedData: &pb.ChecksummedData{Content: []byte(" world"), Crc32C: wrapperspb.UInt32(crc32.Checksum([]byte(" world"), crc32.MakeTable(crc32.Castagnoli)))}}, ObjectChecksums: &pb.ObjectChecksums{Md5Hash: "5eb63bbbe01eeed093cb22bb8f5acdc3"}, FinishWrite: true}); err != nil {
		t.Fatal(err)
	}
	if _, err = stream.CloseAndRecv(); err != nil {
		t.Fatal(err)
	}
	_, jsonClient, _, _ := newEmulatorClient(t)
	jsonWriter := jsonClient.Bucket(name).Object("json").NewWriter(ctx)
	if _, err = jsonWriter.Write([]byte("from-json")); err != nil {
		t.Fatal(err)
	}
	if err = jsonWriter.Close(); err != nil {
		t.Fatal(err)
	}
	jsonObject, err := client.GetObject(ctx, &pb.GetObjectRequest{Bucket: name, Object: "json"})
	if err != nil || jsonObject.GetSize() != 9 {
		t.Fatalf("JSON to v1=%v error=%v", jsonObject, err)
	}
	reader, err := jsonClient.Bucket(name).Object("source").NewReader(ctx)
	if err != nil {
		t.Fatal(err)
	}
	data, err := io.ReadAll(reader)
	reader.Close()
	if err != nil || string(data) != "hello world" {
		t.Fatalf("JSON read=%q error=%v", data, err)
	}
	fetched, err := client.GetObject(ctx, &pb.GetObjectRequest{Bucket: name, Object: "source"})
	if err != nil {
		t.Fatal(err)
	}
	fetched.ContentType = "text/plain"
	fetched.Metadata = map[string]string{"a": "b"}
	if _, err = client.UpdateObject(ctx, &pb.UpdateObjectRequest{Bucket: name, Object: "source", Metadata: fetched}); err != nil {
		t.Fatal(err)
	}
	fetched, err = client.GetObject(ctx, &pb.GetObjectRequest{Bucket: name, Object: "source"})
	if err != nil {
		t.Fatal(err)
	}
	fetched.Generation++
	if _, err = client.UpdateObject(ctx, &pb.UpdateObjectRequest{Bucket: name, Object: "source", Metadata: fetched}); status.Code(err) != codes.Unimplemented {
		t.Fatalf("changed object output=%v", err)
	}
	if _, err = client.PatchObject(ctx, &pb.PatchObjectRequest{Bucket: name, Object: "source", Metadata: &pb.Object{Metadata: map[string]string{"c": "d"}}}); err != nil {
		t.Fatal(err)
	}
	attrs, err := jsonClient.Bucket(name).Object("source").Attrs(ctx)
	if err != nil || attrs.ContentType != "text/plain" || attrs.Metadata["a"] != "b" || attrs.Metadata["c"] != "d" {
		t.Fatalf("v1 update to JSON=%v error=%v", attrs, err)
	}
	if _, err = client.ComposeObject(ctx, &pb.ComposeObjectRequest{DestinationBucket: name, DestinationObject: "composed", SourceObjects: []*pb.ComposeObjectRequest_SourceObjects{{Name: "source"}, {Name: "source"}}}); err != nil {
		t.Fatal(err)
	}
	reader, err = jsonClient.Bucket(name).Object("composed").NewReader(ctx)
	if err != nil {
		t.Fatal(err)
	}
	data, err = io.ReadAll(reader)
	reader.Close()
	if err != nil || string(data) != "hello worldhello world" {
		t.Fatalf("compose=%q error=%v", data, err)
	}
	list, err := client.ListObjects(ctx, &pb.ListObjectsRequest{Bucket: name, MaxResults: 1})
	if err != nil || len(list.GetItems()) != 1 || list.GetNextPageToken() == "" {
		t.Fatalf("list=%v error=%v", list, err)
	}
	next, err := client.ListObjects(ctx, &pb.ListObjectsRequest{Bucket: name, MaxResults: 1, PageToken: list.NextPageToken})
	if err != nil || len(next.GetItems()) != 1 || next.Items[0].Name == list.Items[0].Name {
		t.Fatalf("next page=%v error=%v", next, err)
	}
	for _, object := range []string{"source", "composed", "json"} {
		if _, err = client.DeleteObject(ctx, &pb.DeleteObjectRequest{Bucket: name, Object: object}); err != nil {
			t.Fatal(err)
		}
	}
	if _, err = client.DeleteBucket(ctx, &pb.DeleteBucketRequest{Bucket: name}); err != nil {
		t.Fatal(err)
	}
	_, err = client.CopyObject(ctx, &pb.CopyObjectRequest{})
	if status.Code(err) != codes.Unimplemented || len(status.Convert(err).Details()) == 0 {
		t.Fatalf("unsupported=%v", err)
	}
	supported := map[string]bool{"GetBucket": true, "DeleteBucket": true, "InsertBucket": true, "UpdateBucket": true, "ListBuckets": true, "InsertObject": true, "GetObject": true, "DeleteObject": true, "UpdateObject": true, "PatchObject": true, "ComposeObject": true, "ListObjects": true}
	methods := pb.File_google_storage_v1_storage_proto.Services().ByName("Storage").Methods()
	for i := 0; i < methods.Len(); i++ {
		method := string(methods.Get(i).Name())
		if supported[method] {
			continue
		}
		deadline, stop := context.WithTimeout(ctx, 2*time.Second)
		err = conn.Invoke(deadline, "/google.storage.v1.Storage/"+method, &pb.GetBucketRequest{}, &pb.Bucket{})
		stop()
		if status.Code(err) != codes.Unimplemented || len(status.Convert(err).Details()) == 0 {
			t.Errorf("%s=%v", method, err)
		}
	}
	deadline, stop := context.WithTimeout(ctx, 2*time.Second)
	defer stop()
	unsupportedStream, err := conn.NewStream(deadline, &grpc.StreamDesc{ServerStreams: true}, "/google.storage.v1.Storage/GetObjectMedia")
	if err != nil {
		t.Fatal(err)
	}
	if err = unsupportedStream.RecvMsg(&pb.GetObjectMediaResponse{}); status.Code(err) != codes.Unimplemented || len(status.Convert(err).Details()) == 0 {
		t.Fatalf("stream without input=%v", err)
	}
}

func TestGRPCV1LegacySafety(t *testing.T) {
	endpoint := os.Getenv("STORAGE_EMULATOR_HOST_GRPC")
	if endpoint == "" {
		t.Skip("STORAGE_EMULATOR_HOST_GRPC is unset")
	}
	ctx, jsonClient, _, _ := newEmulatorClient(t)
	bucket := resumableBucket(t, ctx, jsonClient, "grpc-v1-safety")
	ctx, cancel := context.WithTimeout(ctx, 20*time.Second)
	defer cancel()
	conn, err := grpc.NewClient(endpoint, grpc.WithTransportCredentials(insecure.NewCredentials()))
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	client := pb.NewStorageClient(conn)
	if _, err = client.GetBucket(ctx, &pb.GetBucketRequest{Bucket: bucket.BucketName()}); err != nil {
		t.Fatal(err)
	}
	for _, tc := range []struct {
		name     string
		offset   int64
		finish   bool
		crc      *wrapperspb.UInt32Value
		checksum *pb.ObjectChecksums
	}{
		{name: "offset", offset: 1, finish: true},
		{name: "unfinished"},
		{name: "crc", finish: true, crc: wrapperspb.UInt32(0)},
		{name: "md5", finish: true, checksum: &pb.ObjectChecksums{Md5Hash: "00000000000000000000000000000000"}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			stream, err := client.InsertObject(ctx)
			if err != nil {
				t.Fatal(err)
			}
			if err = stream.Send(&pb.InsertObjectRequest{FirstMessage: &pb.InsertObjectRequest_InsertObjectSpec{InsertObjectSpec: &pb.InsertObjectSpec{Resource: &pb.Object{Bucket: bucket.BucketName(), Name: tc.name}}}, WriteOffset: tc.offset, FinishWrite: tc.finish, ObjectChecksums: tc.checksum, Data: &pb.InsertObjectRequest_ChecksummedData{ChecksummedData: &pb.ChecksummedData{Content: []byte("body"), Crc32C: tc.crc}}}); err != nil {
				t.Fatal(err)
			}
			_, err = stream.CloseAndRecv()
			if status.Code(err) != codes.InvalidArgument {
				t.Fatalf("bad upload=%v", err)
			}
			if _, err = client.GetObject(ctx, &pb.GetObjectRequest{Bucket: bucket.BucketName(), Object: tc.name}); status.Code(err) != codes.NotFound {
				t.Fatalf("failed upload published: %v", err)
			}
		})
	}
	writer := bucket.Object("existing").NewWriter(ctx)
	if _, err = writer.Write([]byte("original")); err != nil {
		t.Fatal(err)
	}
	if err = writer.Close(); err != nil {
		t.Fatal(err)
	}
	defer bucket.Object("existing").Delete(ctx)
	writer = bucket.Object("held").NewWriter(ctx)
	writer.TemporaryHold = true
	if _, err = writer.Write([]byte("held")); err != nil {
		t.Fatal(err)
	}
	if err = writer.Close(); err != nil {
		t.Fatal(err)
	}
	if _, err = client.UpdateObject(ctx, &pb.UpdateObjectRequest{Bucket: bucket.BucketName(), Object: "held", Metadata: &pb.Object{TemporaryHold: false}}); err != nil {
		t.Fatal(err)
	}
	held, err := bucket.Object("held").Attrs(ctx)
	if err != nil || held.TemporaryHold {
		t.Fatalf("clear temporary hold=%v error=%v", held, err)
	}
	if err = bucket.Object("held").Delete(ctx); err != nil {
		t.Fatal(err)
	}
	_, err = client.DeleteObject(ctx, &pb.DeleteObjectRequest{Bucket: bucket.BucketName(), Object: "existing", IfGenerationMatch: wrapperspb.Int64(0)})
	if status.Code(err) != codes.FailedPrecondition {
		t.Fatalf("delete precondition=%v", err)
	}
	_, err = client.PatchObject(ctx, &pb.PatchObjectRequest{Bucket: bucket.BucketName(), Object: "existing", IfGenerationMatch: wrapperspb.Int64(0), Metadata: &pb.Object{ContentType: "text/plain"}})
	if status.Code(err) != codes.FailedPrecondition {
		t.Fatalf("patch precondition=%v", err)
	}
	_, err = client.GetObject(ctx, &pb.GetObjectRequest{Bucket: bucket.BucketName(), Object: "existing", CommonRequestParams: &pb.CommonRequestParams{UserProject: "billing"}})
	if status.Code(err) != codes.Unimplemented || len(status.Convert(err).Details()) == 0 {
		t.Fatalf("billing=%v", err)
	}
}

func TestGRPCV1LegacyStrict(t *testing.T) {
	endpoint := os.Getenv("GCS_MIMIC_STRICT_GRPC_ENDPOINT")
	if endpoint == "" {
		t.Skip("GCS_MIMIC_STRICT_GRPC_ENDPOINT is unset")
	}
	ctx, jsonClient := newStrictClient(t, strictAdminToken)
	bucket := resumableBucket(t, ctx, jsonClient, strictProject)
	ctx, cancel := context.WithTimeout(ctx, 15*time.Second)
	defer cancel()
	conn, err := grpc.NewClient(endpoint, grpc.WithTransportCredentials(insecure.NewCredentials()))
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	client := pb.NewStorageClient(conn)
	admin := metadata.AppendToOutgoingContext(ctx, "authorization", "Bearer "+strictAdminToken)
	viewer := metadata.AppendToOutgoingContext(ctx, "authorization", "Bearer "+strictViewerToken)
	writer := bucket.Object("private").NewWriter(ctx)
	if _, err = writer.Write([]byte("private")); err != nil {
		t.Fatal(err)
	}
	if err = writer.Close(); err != nil {
		t.Fatal(err)
	}
	defer bucket.Object("private").Delete(ctx)
	for _, callCtx := range []context.Context{admin, viewer} {
		if _, err = client.GetObject(callCtx, &pb.GetObjectRequest{Bucket: bucket.BucketName(), Object: "private"}); err != nil {
			t.Fatal(err)
		}
	}
	if _, err = client.GetObject(ctx, &pb.GetObjectRequest{Bucket: bucket.BucketName(), Object: "private"}); err == nil {
		t.Fatal("anonymous read succeeded")
	}
	// Omitting the project lists only projects where the caller holds storage.buckets.list.
	listed, err := client.ListBuckets(admin, &pb.ListBucketsRequest{Prefix: bucket.BucketName()})
	if err != nil || len(listed.Items) != 1 || listed.Items[0].Name != bucket.BucketName() {
		t.Fatalf("admin project-free listing=%v error=%v", listed, err)
	}
	if _, err = client.ListBuckets(viewer, &pb.ListBucketsRequest{}); status.Code(err) != codes.PermissionDenied {
		t.Fatalf("viewer project-free listing=%v", err)
	}
	if _, err = client.ListBuckets(ctx, &pb.ListBucketsRequest{}); status.Code(err) != codes.Unauthenticated {
		t.Fatalf("anonymous project-free listing=%v", err)
	}
	if _, err = client.DeleteObject(viewer, &pb.DeleteObjectRequest{Bucket: bucket.BucketName(), Object: "private"}); status.Code(err) != codes.PermissionDenied {
		t.Fatalf("viewer delete=%v", err)
	}
	if _, err = client.UpdateBucket(viewer, &pb.UpdateBucketRequest{Bucket: bucket.BucketName(), Metadata: &pb.Bucket{DefaultEventBasedHold: true}}); status.Code(err) != codes.PermissionDenied {
		t.Fatalf("viewer bucket update=%v", err)
	}
	stream, err := client.InsertObject(viewer)
	if err != nil {
		t.Fatal(err)
	}
	_ = stream.Send(&pb.InsertObjectRequest{FirstMessage: &pb.InsertObjectRequest_InsertObjectSpec{InsertObjectSpec: &pb.InsertObjectSpec{Resource: &pb.Object{Bucket: bucket.BucketName(), Name: "denied"}}}, FinishWrite: true})
	if _, err = stream.CloseAndRecv(); status.Code(err) != codes.PermissionDenied {
		t.Fatalf("viewer upload=%v", err)
	}
}
