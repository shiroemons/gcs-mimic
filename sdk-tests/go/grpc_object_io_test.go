package gcsmimic_test

import (
	"bytes"
	"context"
	"io"
	"net"
	"os"
	"testing"
	"time"

	"cloud.google.com/go/storage"
	"google.golang.org/api/iterator"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/metadata"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/reflect/protoreflect"
	"google.golang.org/protobuf/reflect/protoregistry"
	"google.golang.org/protobuf/types/dynamicpb"
)

func TestGRPCObjectIO(t *testing.T) {
	if os.Getenv("STORAGE_EMULATOR_HOST_GRPC") == "" {
		t.Skip("STORAGE_EMULATOR_HOST_GRPC is unset")
	}
	ctx, jsonClient, _, _ := newEmulatorClient(t)
	bucket := resumableBucket(t, ctx, jsonClient, "grpc-object-io")
	ctx, cancel := context.WithTimeout(ctx, 30*time.Second)
	defer cancel()
	client, err := storage.NewGRPCClient(ctx)
	if err != nil {
		t.Fatal(err)
	}
	defer client.Close()
	grpcBucket := client.Bucket(bucket.BucketName())
	jsonWriter := bucket.Object("json").NewWriter(ctx)
	if _, err := jsonWriter.Write([]byte("from-json")); err != nil {
		t.Fatal(err)
	}
	if err := jsonWriter.Close(); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		cleanup, stop := context.WithTimeout(context.Background(), 5*time.Second)
		defer stop()
		_ = bucket.Object("json").Delete(cleanup)
	})
	jsonReader, err := grpcBucket.Object("json").NewReader(ctx)
	if err != nil {
		t.Fatal(err)
	}
	jsonBytes, err := io.ReadAll(jsonReader)
	jsonReader.Close()
	if err != nil || string(jsonBytes) != "from-json" {
		t.Fatalf("JSON to gRPC=%q error=%v", jsonBytes, err)
	}
	for _, size := range []int{5, 5 << 20} {
		name := "small"
		if size > 5 {
			name = "dir/large"
		}
		payload := bytes.Repeat([]byte("x"), size)
		object := grpcBucket.Object(name)
		t.Cleanup(func() {
			cleanup, stop := context.WithTimeout(context.Background(), 5*time.Second)
			defer stop()
			_ = bucket.Object(name).Delete(cleanup)
		})
		writer := object.NewWriter(ctx)
		writer.ContentType = "application/octet-stream"
		writer.Metadata = map[string]string{"transport": "grpc"}
		if _, err := writer.Write(payload); err != nil {
			t.Fatal(err)
		}
		if err := writer.Close(); err != nil {
			t.Fatal(err)
		}
		attrs, err := object.Attrs(ctx)
		if err != nil || attrs.Size != int64(size) || attrs.Metadata["transport"] != "grpc" {
			t.Fatalf("attrs=%v error=%v", attrs, err)
		}
		reader, err := object.NewReader(ctx)
		if err != nil {
			t.Fatal(err)
		}
		got, err := io.ReadAll(reader)
		reader.Close()
		if err != nil || !bytes.Equal(got, payload) {
			t.Fatalf("gRPC bytes=%d error=%v", len(got), err)
		}
		jsonReader, err := bucket.Object(name).NewReader(ctx)
		if err != nil {
			t.Fatal(err)
		}
		got, err = io.ReadAll(jsonReader)
		jsonReader.Close()
		if err != nil || !bytes.Equal(got, payload) {
			t.Fatalf("JSON bytes=%d error=%v", len(got), err)
		}
		ranged, err := object.NewRangeReader(ctx, 1, 2)
		if err != nil {
			t.Fatal(err)
		}
		got, err = io.ReadAll(ranged)
		ranged.Close()
		if err != nil || string(got) != "xx" {
			t.Fatalf("range=%q error=%v", got, err)
		}
	}
	it := grpcBucket.Objects(ctx, &storage.Query{Prefix: "dir/", Delimiter: "/"})
	attrs, err := it.Next()
	if err != nil || attrs.Name != "dir/large" {
		t.Fatalf("list=%v error=%v", attrs, err)
	}
	if _, err = it.Next(); err != iterator.Done {
		t.Fatalf("end=%v", err)
	}
	it = grpcBucket.Objects(ctx, &storage.Query{Delimiter: "/"})
	var prefix bool
	for {
		attrs, err = it.Next()
		if err == iterator.Done {
			break
		}
		if err != nil {
			t.Fatal(err)
		}
		prefix = prefix || attrs.Prefix == "dir/"
	}
	if !prefix {
		t.Fatal("missing dir/ prefix")
	}
	pager := iterator.NewPager(grpcBucket.Objects(ctx, nil), 1, "")
	var pages int
	for token := "first"; token != ""; {
		var objects []*storage.ObjectAttrs
		var err error
		token, err = pager.NextPage(&objects)
		if err != nil || len(objects) != 1 {
			t.Fatalf("page=%v error=%v", objects, err)
		}
		pages++
	}
	if pages != 3 {
		t.Fatalf("pages=%d", pages)
	}
	for _, name := range []string{"small", "dir/large"} {
		if err := grpcBucket.Object(name).Delete(ctx); err != nil {
			t.Fatal(err)
		}
	}
}

func TestGRPCObjectIOStrict(t *testing.T) {
	endpoint := os.Getenv("GCS_MIMIC_STRICT_GRPC_ENDPOINT")
	if endpoint == "" {
		t.Skip("GCS_MIMIC_STRICT_GRPC_ENDPOINT is unset")
	}
	ctx, jsonClient := newStrictClient(t, strictAdminToken)
	bucket := resumableBucket(t, ctx, jsonClient, strictProject)
	t.Setenv("STORAGE_EMULATOR_HOST_GRPC", endpoint)
	ctx, cancel := context.WithTimeout(ctx, 15*time.Second)
	defer cancel()
	client, err := storage.NewGRPCClient(ctx)
	if err != nil {
		t.Fatal(err)
	}
	defer client.Close()
	object := client.Bucket(bucket.BucketName()).Object("private")
	admin := metadata.AppendToOutgoingContext(ctx, "authorization", "Bearer "+strictAdminToken)
	writer := object.NewWriter(admin)
	if _, err := writer.Write([]byte("private")); err != nil {
		t.Fatal(err)
	}
	if err := writer.Close(); err != nil {
		t.Fatal(err)
	}
	defer object.Delete(admin)
	if _, err := object.Attrs(ctx); err == nil {
		t.Fatal("anonymous read succeeded")
	}
	reader := metadata.AppendToOutgoingContext(ctx, "authorization", "Bearer "+strictViewerToken)
	if _, err := object.Attrs(reader); err != nil {
		t.Fatal(err)
	}
	denied := object.NewWriter(reader)
	_, writeErr := denied.Write([]byte("denied"))
	closeErr := denied.Close()
	if writeErr == nil && closeErr == nil {
		t.Fatal("viewer write succeeded")
	}
}

func TestGRPCObjectIODeletePolicies(t *testing.T) {
	if os.Getenv("STORAGE_EMULATOR_HOST_GRPC") == "" {
		t.Skip("STORAGE_EMULATOR_HOST_GRPC is unset")
	}
	ctx, bucket := versioningBucket(t, &storage.BucketAttrs{})
	ctx, cancel := context.WithTimeout(ctx, 15*time.Second)
	defer cancel()
	client, err := storage.NewGRPCClient(ctx)
	if err != nil {
		t.Fatal(err)
	}
	defer client.Close()
	object := client.Bucket(bucket.BucketName()).Object("protected")
	writer := object.NewWriter(ctx)
	writer.TemporaryHold = true
	if _, err := writer.Write([]byte("held")); err != nil {
		t.Fatal(err)
	}
	if err := writer.Close(); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		cleanup, stop := context.WithTimeout(context.Background(), 5*time.Second)
		defer stop()
		_, _ = bucket.Object("protected").Update(cleanup, storage.ObjectAttrsToUpdate{TemporaryHold: false})
		_, _ = bucket.Update(cleanup, storage.BucketAttrsToUpdate{RetentionPolicy: &storage.RetentionPolicy{}})
	})
	if err := object.Delete(ctx); status.Code(err) != codes.PermissionDenied {
		t.Fatalf("held delete=%v", err)
	}
	if _, err := bucket.Object("protected").Update(ctx, storage.ObjectAttrsToUpdate{TemporaryHold: false}); err != nil {
		t.Fatal(err)
	}
	if _, err := bucket.Update(ctx, storage.BucketAttrsToUpdate{RetentionPolicy: &storage.RetentionPolicy{RetentionPeriod: time.Hour}}); err != nil {
		t.Fatal(err)
	}
	if err := object.Delete(ctx); status.Code(err) != codes.PermissionDenied {
		t.Fatalf("retained delete=%v", err)
	}
	if _, err := bucket.Update(ctx, storage.BucketAttrsToUpdate{RetentionPolicy: &storage.RetentionPolicy{}, SoftDeletePolicy: &storage.SoftDeletePolicy{RetentionDuration: 7 * 24 * time.Hour}}); err != nil {
		t.Fatal(err)
	}
	if err := object.Generation(writer.Attrs().Generation).Delete(ctx); err != nil {
		t.Fatal(err)
	}
	retained, err := bucket.Object("protected").Generation(writer.Attrs().Generation).SoftDeleted().Attrs(ctx)
	if err != nil || retained.SoftDeleteTime.IsZero() {
		t.Fatalf("soft deleted=%v error=%v", retained, err)
	}
}

// The SDK registers its pinned internal Storage protos; reflection lets wire tests use
// that exact schema without copying it or importing a Go internal package.
func grpcIOMessage(t *testing.T, name string, fields map[string]any) *dynamicpb.Message {
	t.Helper()
	descriptor, err := protoregistry.GlobalFiles.FindDescriptorByName(protoreflect.FullName("google.storage.v2." + name))
	if err != nil {
		t.Fatal(err)
	}
	message := dynamicpb.NewMessage(descriptor.(protoreflect.MessageDescriptor))
	for key, value := range fields {
		field := message.Descriptor().Fields().ByName(protoreflect.Name(key))
		if field == nil {
			t.Fatalf("unknown %s.%s", name, key)
		}
		var reflected protoreflect.Value
		switch value := value.(type) {
		case string:
			reflected = protoreflect.ValueOfString(value)
		case int64:
			reflected = protoreflect.ValueOfInt64(value)
		case uint32:
			reflected = protoreflect.ValueOfUint32(value)
		case bool:
			reflected = protoreflect.ValueOfBool(value)
		case []byte:
			reflected = protoreflect.ValueOfBytes(value)
		case proto.Message:
			reflected = protoreflect.ValueOfMessage(value.ProtoReflect())
		default:
			t.Fatalf("unsupported wire value %T", value)
		}
		message.Set(field, reflected)
	}
	return message
}

func grpcIOField(message *dynamicpb.Message, name string) protoreflect.Value {
	return message.Get(message.Descriptor().Fields().ByName(protoreflect.Name(name)))
}

func TestGRPCObjectIOWire(t *testing.T) {
	endpoint := os.Getenv("STORAGE_EMULATOR_HOST_GRPC")
	if endpoint == "" {
		t.Skip("STORAGE_EMULATOR_HOST_GRPC is unset")
	}
	host, _, err := net.SplitHostPort(endpoint)
	if err != nil || net.ParseIP(host) == nil || !net.ParseIP(host).IsLoopback() {
		t.Fatal("gRPC endpoint must be loopback IP:PORT")
	}
	ctx, jsonClient, _, _ := newEmulatorClient(t)
	bucket := resumableBucket(t, ctx, jsonClient, "grpc-wire")
	ctx, cancel := context.WithTimeout(ctx, 15*time.Second)
	defer cancel()
	conn, err := grpc.NewClient(endpoint, grpc.WithTransportCredentials(insecure.NewCredentials()))
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	msg := func(name string, fields map[string]any) *dynamicpb.Message { return grpcIOMessage(t, name, fields) }
	invoke := func(method string, request, response *dynamicpb.Message) error {
		return conn.Invoke(ctx, "/google.storage.v2.Storage/"+method, request, response)
	}
	resource := func(name string) *dynamicpb.Message {
		return msg("Object", map[string]any{"bucket": "projects/_/buckets/" + bucket.BucketName(), "name": name})
	}
	spec := func(name string) *dynamicpb.Message {
		return msg("WriteObjectSpec", map[string]any{"resource": resource(name), "if_generation_match": int64(0)})
	}
	start := msg("StartResumableWriteResponse", nil)
	if err := invoke("StartResumableWrite", msg("StartResumableWriteRequest", map[string]any{"write_object_spec": spec("resumed")}), start); err != nil {
		t.Fatal(err)
	}
	upload := grpcIOField(start, "upload_id").String()
	t.Cleanup(func() {
		cleanup, stop := context.WithTimeout(context.Background(), 5*time.Second)
		defer stop()
		_ = bucket.Object("resumed").Delete(cleanup)
		_ = bucket.Object("streamed").Delete(cleanup)
	})
	open := func(method string) grpc.ClientStream {
		stream, err := conn.NewStream(ctx, &grpc.StreamDesc{ClientStreams: true, ServerStreams: method == "BidiWriteObject"}, "/google.storage.v2.Storage/"+method)
		if err != nil {
			t.Fatal(err)
		}
		return stream
	}
	data := func(content string) *dynamicpb.Message {
		return msg("ChecksummedData", map[string]any{"content": []byte(content)})
	}
	stream := open("BidiWriteObject")
	if err := stream.SendMsg(msg("BidiWriteObjectRequest", map[string]any{"upload_id": upload, "checksummed_data": data("abc"), "state_lookup": true})); err != nil {
		t.Fatal(err)
	}
	persisted := msg("BidiWriteObjectResponse", nil)
	if err := stream.RecvMsg(persisted); err != nil {
		t.Fatal(err)
	}
	if grpcIOField(persisted, "persisted_size").Int() != 3 {
		t.Fatalf("persisted=%v", persisted)
	}
	stream.CloseSend()
	query := func() *dynamicpb.Message {
		response := msg("QueryWriteStatusResponse", nil)
		if err := invoke("QueryWriteStatus", msg("QueryWriteStatusRequest", map[string]any{"upload_id": upload}), response); err != nil {
			t.Fatal(err)
		}
		return response
	}
	if grpcIOField(query(), "persisted_size").Int() != 3 {
		t.Fatal("durable offset is not 3")
	}
	wrong := open("BidiWriteObject")
	wrong.SendMsg(msg("BidiWriteObjectRequest", map[string]any{"upload_id": upload, "write_offset": int64(2), "finish_write": true}))
	wrong.CloseSend()
	if err := wrong.RecvMsg(msg("BidiWriteObjectResponse", nil)); status.Code(err) != codes.OutOfRange {
		t.Fatalf("wrong offset=%v", err)
	}
	stream = open("BidiWriteObject")
	stream.SendMsg(msg("BidiWriteObjectRequest", map[string]any{"upload_id": upload, "write_offset": int64(3), "checksummed_data": data("def"), "finish_write": true}))
	stream.CloseSend()
	if err := stream.RecvMsg(msg("BidiWriteObjectResponse", nil)); err != nil {
		t.Fatal(err)
	}
	if grpcIOField(query(), "resource").Message().Get(resource("x").Descriptor().Fields().ByName("size")).Int() != 6 {
		t.Fatal("completed size is not 6")
	}
	stream = open("WriteObject")
	stream.SendMsg(msg("WriteObjectRequest", map[string]any{"write_object_spec": spec("streamed"), "checksummed_data": data("hello")}))
	stream.SendMsg(msg("WriteObjectRequest", map[string]any{"write_offset": int64(5), "checksummed_data": data(" world"), "finish_write": true}))
	stream.CloseSend()
	response := msg("WriteObjectResponse", nil)
	if err := stream.RecvMsg(response); err != nil {
		t.Fatal(err)
	}
	for _, test := range []struct {
		name     string
		checksum *dynamicpb.Message
	}{
		{"bad-crc", msg("ObjectChecksums", map[string]any{"crc32c": uint32(1)})},
		{"bad-md5", msg("ObjectChecksums", map[string]any{"md5_hash": make([]byte, 16)})},
	} {
		stream = open("WriteObject")
		stream.SendMsg(msg("WriteObjectRequest", map[string]any{"write_object_spec": spec(test.name), "checksummed_data": data("bad"), "object_checksums": test.checksum, "finish_write": true}))
		stream.CloseSend()
		if err := stream.RecvMsg(msg("WriteObjectResponse", nil)); status.Code(err) != codes.InvalidArgument {
			t.Fatalf("checksum=%v", err)
		}
		if _, err := bucket.Object(test.name).Attrs(ctx); err == nil {
			t.Fatal("checksum failure published an object")
		}
	}
	stream, err = conn.NewStream(ctx, &grpc.StreamDesc{ServerStreams: true}, "/google.storage.v2.Storage/ReadObject")
	if err != nil {
		t.Fatal(err)
	}
	stream.SendMsg(msg("ReadObjectRequest", map[string]any{"bucket": "projects/_/buckets/" + bucket.BucketName(), "object": "resumed", "read_offset": int64(-4), "read_limit": int64(2)}))
	stream.CloseSend()
	read := msg("ReadObjectResponse", nil)
	if err := stream.RecvMsg(read); err != nil {
		t.Fatal(err)
	}
	content := grpcIOField(read, "checksummed_data").Message()
	if string(content.Get(content.Descriptor().Fields().ByName("content")).Bytes()) != "cd" {
		t.Fatalf("range=%v", read)
	}
	for _, test := range []struct {
		name     string
		checksum *dynamicpb.Message
		atStart  bool
	}{
		{"resumed-bad-crc", msg("ObjectChecksums", map[string]any{"crc32c": uint32(1)}), true},
		{"resumed-bad-md5", msg("ObjectChecksums", map[string]any{"md5_hash": make([]byte, 16)}), true},
		{"resumed-final-crc", msg("ObjectChecksums", map[string]any{"crc32c": uint32(1)}), false},
	} {
		fields := map[string]any{"write_object_spec": spec(test.name)}
		if test.atStart {
			fields["object_checksums"] = test.checksum
		}
		started := msg("StartResumableWriteResponse", nil)
		if err := invoke("StartResumableWrite", msg("StartResumableWriteRequest", fields), started); err != nil {
			t.Fatal(err)
		}
		id := grpcIOField(started, "upload_id").String()
		stream = open("BidiWriteObject")
		stream.SendMsg(msg("BidiWriteObjectRequest", map[string]any{"upload_id": id, "checksummed_data": data("abc"), "object_checksums": test.checksum}))
		stream.CloseSend()
		if err := stream.RecvMsg(msg("BidiWriteObjectResponse", nil)); status.Code(err) != codes.Unimplemented {
			t.Fatalf("non-final resumable checksum=%v", err)
		}
		state := msg("QueryWriteStatusResponse", nil)
		if err := invoke("QueryWriteStatus", msg("QueryWriteStatusRequest", map[string]any{"upload_id": id}), state); err != nil || grpcIOField(state, "persisted_size").Int() != 0 {
			t.Fatalf("rejected checksum advanced offset: %v %v", state, err)
		}
		stream = open("BidiWriteObject")
		stream.SendMsg(msg("BidiWriteObjectRequest", map[string]any{"upload_id": id, "checksummed_data": data("abc"), "state_lookup": true}))
		if err := stream.RecvMsg(msg("BidiWriteObjectResponse", nil)); err != nil {
			t.Fatal(err)
		}
		stream.CloseSend()
		if err := invoke("QueryWriteStatus", msg("QueryWriteStatusRequest", map[string]any{"upload_id": id}), state); err != nil || grpcIOField(state, "persisted_size").Int() != 3 {
			t.Fatalf("prefix status=%v %v", state, err)
		}
		stream = open("BidiWriteObject")
		final := map[string]any{"upload_id": id, "write_offset": int64(3), "checksummed_data": data("def"), "finish_write": true}
		if !test.atStart {
			final["object_checksums"] = test.checksum
		}
		stream.SendMsg(msg("BidiWriteObjectRequest", final))
		stream.CloseSend()
		if err := stream.RecvMsg(msg("BidiWriteObjectResponse", nil)); status.Code(err) != codes.InvalidArgument {
			t.Fatalf("resumed checksum mismatch=%v", err)
		}
		if _, err := bucket.Object(test.name).Attrs(ctx); err == nil {
			t.Fatal("resumed checksum failure published an object")
		}
		if err := invoke("QueryWriteStatus", msg("QueryWriteStatusRequest", map[string]any{"upload_id": id}), msg("QueryWriteStatusResponse", nil)); status.Code(err) != codes.NotFound {
			t.Fatalf("failed session remains usable: %v", err)
		}
	}
}
