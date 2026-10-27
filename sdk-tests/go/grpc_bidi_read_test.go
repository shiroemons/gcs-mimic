package gcsmimic_test

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"hash/crc32"
	"io"
	"net"
	"os"
	"testing"
	"time"

	"cloud.google.com/go/storage"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/metadata"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/reflect/protoreflect"
	"google.golang.org/protobuf/reflect/protoregistry"
)

func TestGRPCBidiRead(t *testing.T) {
	if os.Getenv("STORAGE_EMULATOR_HOST_GRPC") == "" {
		t.Skip("STORAGE_EMULATOR_HOST_GRPC is unset")
	}
	ctx, jsonClient, _, _ := newEmulatorClient(t)
	bucket := resumableBucket(t, ctx, jsonClient, "bidi-read")
	object := resumableObject(t, bucket, "dir/object")
	payload := bytes.Repeat([]byte("0123456789abcdef"), 20000)
	if _, err := strictUpload(ctx, object, payload); err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithTimeout(ctx, 15*time.Second)
	defer cancel()
	client, err := storage.NewGRPCClient(ctx, storage.WithGRPCBidiReads())
	if err != nil {
		t.Fatal(err)
	}
	defer client.Close()
	downloader, err := client.Bucket(bucket.BucketName()).Object(object.ObjectName()).NewMultiRangeDownloader(ctx)
	if err != nil {
		t.Fatal(err)
	}
	defer downloader.Close()
	ranges := [][2]int64{{0, 150000}, {123, 101000}, {-7, 0}, {-999999, 9}, {int64(len(payload)), 0}}
	outputs := make([]bytes.Buffer, len(ranges))
	errors := make(chan error, len(ranges))
	for i, r := range ranges {
		downloader.Add(&outputs[i], r[0], r[1], func(_, _ int64, err error) { errors <- err })
	}
	downloader.Wait()
	for range ranges {
		if err := <-errors; err != nil {
			t.Fatal(err)
		}
	}
	if err := downloader.Close(); err != nil {
		t.Fatal(err)
	}
	for i, r := range ranges {
		start := r[0]
		if start < 0 {
			start = max(0, int64(len(payload))+start)
		}
		end := int64(len(payload))
		if r[1] > 0 {
			end = min(end, start+r[1])
		}
		if !bytes.Equal(outputs[i].Bytes(), payload[start:end]) {
			t.Fatalf("range %v returned %d bytes", r, outputs[i].Len())
		}
	}
}

type bidiVerifiedWriter struct {
	want    []byte
	written int
}

func (w *bidiVerifiedWriter) Write(data []byte) (int, error) {
	if len(data) > len(w.want)-w.written || !bytes.Equal(data, w.want[w.written:w.written+len(data)]) {
		return 0, fmt.Errorf("range bytes differ at offset %d", w.written)
	}
	w.written += len(data)
	return len(data), nil
}

func TestGRPCBidiReadManyRanges(t *testing.T) {
	if os.Getenv("STORAGE_EMULATOR_HOST_GRPC") == "" {
		t.Skip("STORAGE_EMULATOR_HOST_GRPC is unset")
	}
	ctx, jsonClient, _, _ := newEmulatorClient(t)
	bucket := resumableBucket(t, ctx, jsonClient, "bidi-many-ranges")
	object := resumableObject(t, bucket, "many-ranges")
	payload := bytes.Repeat([]byte("0123456789abcdef"), 10000)
	if _, err := strictUpload(ctx, object, payload); err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithTimeout(ctx, 15*time.Second)
	defer cancel()
	client, err := storage.NewGRPCClient(ctx, storage.WithGRPCBidiReads())
	if err != nil {
		t.Fatal(err)
	}
	defer client.Close()
	downloader, err := client.Bucket(bucket.BucketName()).Object(object.ObjectName()).NewMultiRangeDownloader(ctx)
	if err != nil {
		t.Fatal(err)
	}
	const count = 600
	writers := make([]bidiVerifiedWriter, count)
	completed := make(chan error, count)
	finished := make(chan error, 1)
	go func() {
		for i := range writers {
			offset := i % 1000
			writers[i].want = payload[offset : offset+65537]
			downloader.Add(&writers[i], int64(offset), 65537, func(_, length int64, err error) {
				if err == nil && length != 65537 {
					err = fmt.Errorf("received %d bytes", length)
				}
				completed <- err
			})
		}
		downloader.Wait()
		for range count {
			if err := <-completed; err != nil {
				finished <- err
				return
			}
		}
		finished <- downloader.Close()
	}()
	select {
	case err := <-finished:
		if err != nil {
			t.Fatal(err)
		}
	case <-ctx.Done():
		t.Fatalf("Add/Wait did not complete before the deadline: %v", ctx.Err())
	}
	for _, writer := range writers {
		if writer.written != len(writer.want) {
			t.Fatal("range incomplete")
		}
	}
}

func bidiMessage(t *testing.T, name string, value any) proto.Message {
	t.Helper()
	typ, err := protoregistry.GlobalTypes.FindMessageByName(protoreflect.FullName("google.storage.v2." + name))
	if err != nil {
		t.Fatal(err)
	}
	message := typ.New().Interface()
	if value != nil {
		data, err := json.Marshal(value)
		if err != nil {
			t.Fatal(err)
		}
		if err := protojson.Unmarshal(data, message); err != nil {
			t.Fatal(err)
		}
	}
	return message
}

func bidiStream(t *testing.T, ctx context.Context, endpoint string) grpc.ClientStream {
	t.Helper()
	host, _, err := net.SplitHostPort(endpoint)
	if err != nil || net.ParseIP(host) == nil || !net.ParseIP(host).IsLoopback() {
		t.Fatal("gRPC endpoint must be loopback IP:PORT")
	}
	conn, err := grpc.NewClient(endpoint, grpc.WithTransportCredentials(insecure.NewCredentials()))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { conn.Close() })
	stream, err := conn.NewStream(ctx, &grpc.StreamDesc{ClientStreams: true, ServerStreams: true}, "/google.storage.v2.Storage/BidiReadObject")
	if err != nil {
		t.Fatal(err)
	}
	return stream
}

func bidiSend(t *testing.T, stream grpc.ClientStream, value any) {
	t.Helper()
	if err := stream.SendMsg(bidiMessage(t, "BidiReadObjectRequest", value)); err != nil {
		t.Fatal(err)
	}
}

type bidiResponse struct {
	Metadata         *struct{ Generation string }
	ReadHandle       struct{ Handle []byte }
	ObjectDataRanges []struct {
		ChecksummedData struct {
			Content []byte
			CRC32C  uint32
		}
		ReadRange struct {
			ReadID     string
			ReadOffset string
			ReadLength string
		}
		RangeEnd bool
	}
}

func bidiReceive(t *testing.T, stream grpc.ClientStream) (bidiResponse, error) {
	t.Helper()
	message := bidiMessage(t, "BidiReadObjectResponse", nil)
	var response bidiResponse
	if err := stream.RecvMsg(message); err != nil {
		return response, err
	}
	data, err := protojson.Marshal(message)
	if err != nil {
		t.Fatal(err)
	}
	if err := json.Unmarshal(data, &response); err != nil {
		t.Fatal(err)
	}
	return response, nil
}

func TestGRPCBidiReadWire(t *testing.T) {
	endpoint := os.Getenv("STORAGE_EMULATOR_HOST_GRPC")
	if endpoint == "" {
		t.Skip("STORAGE_EMULATOR_HOST_GRPC is unset")
	}
	ctx, jsonClient, _, _ := newEmulatorClient(t)
	ctx, cancel := context.WithTimeout(ctx, 20*time.Second)
	defer cancel()
	bucket := resumableBucket(t, ctx, jsonClient, "bidi-wire")
	object := resumableObject(t, bucket, "object")
	payload := bytes.Repeat([]byte("abcdefghijk"), 17000)
	attrs, err := strictUpload(ctx, object, payload)
	if err != nil {
		t.Fatal(err)
	}
	spec := map[string]any{"bucket": "projects/_/buckets/" + bucket.BucketName(), "object": object.ObjectName()}
	stream := bidiStream(t, ctx, endpoint)
	bidiSend(t, stream, map[string]any{"readObjectSpec": spec})
	first, err := bidiReceive(t, stream)
	if err != nil || first.Metadata == nil || len(first.ReadHandle.Handle) == 0 {
		t.Fatalf("initial metadata/handle: %+v %v", first, err)
	}
	bidiSend(t, stream, map[string]any{"readRanges": []any{
		map[string]any{"readId": 1, "readOffset": 0, "readLength": 140000},
		map[string]any{"readId": 2, "readOffset": 111, "readLength": 150000},
	}})
	contents := map[string][]byte{}
	ended := map[string]bool{}
	for len(ended) < 2 {
		response, err := bidiReceive(t, stream)
		if err != nil {
			t.Fatal(err)
		}
		if response.Metadata != nil {
			t.Fatal("metadata repeated")
		}
		for _, part := range response.ObjectDataRanges {
			data := part.ChecksummedData.Content
			if len(data) > 65536 || crc32.Checksum(data, crc32.MakeTable(crc32.Castagnoli)) != part.ChecksummedData.CRC32C {
				t.Fatal("invalid bounded chunk or CRC32C")
			}
			id := part.ReadRange.ReadID
			contents[id] = append(contents[id], data...)
			if part.RangeEnd {
				ended[id] = true
			}
		}
	}
	if !bytes.Equal(contents["1"], payload[:140000]) || !bytes.Equal(contents["2"], payload[111:150111]) {
		t.Fatal("interleaved range data mismatch")
	}
	if _, err := strictUpload(ctx, object, []byte("replacement")); err != nil {
		t.Fatal(err)
	}
	// A later range and a reconnect must both retain the generation selected before overwrite.
	bidiSend(t, stream, map[string]any{"readRanges": []any{map[string]any{"readId": 3, "readOffset": -5}}})
	response, err := bidiReceive(t, stream)
	if err != nil || len(response.ObjectDataRanges) != 1 || !response.ObjectDataRanges[0].RangeEnd || !bytes.Equal(response.ObjectDataRanges[0].ChecksummedData.Content, payload[len(payload)-5:]) {
		t.Fatalf("later range: %+v %v", response, err)
	}
	stream.CloseSend()
	if _, err := bidiReceive(t, stream); err != io.EOF {
		t.Fatalf("expected EOF: %v", err)
	}
	resumed := bidiStream(t, ctx, endpoint)
	resumeSpec := map[string]any{"bucket": spec["bucket"], "object": spec["object"], "generation": attrs.Generation, "readHandle": map[string]any{"handle": first.ReadHandle.Handle}}
	bidiSend(t, resumed, map[string]any{"readObjectSpec": resumeSpec, "readRanges": []any{map[string]any{"readId": 9, "readLength": 11}}})
	initial, err := bidiReceive(t, resumed)
	if err != nil || initial.Metadata != nil {
		t.Fatalf("handle reconnect metadata: %+v %v", initial, err)
	}
	response, err = bidiReceive(t, resumed)
	if err != nil || !bytes.Equal(response.ObjectDataRanges[0].ChecksummedData.Content, payload[:11]) {
		t.Fatalf("handle generation changed: %+v %v", response, err)
	}
	resumed.CloseSend()
	oversized := make([]any, 101)
	for i := range oversized {
		oversized[i] = map[string]any{"readId": i}
	}
	for _, tc := range []struct {
		name   string
		change map[string]any
		ranges []any
		want   codes.Code
	}{
		{"missing", map[string]any{"object": "absent"}, nil, codes.NotFound},
		{"generation", map[string]any{"generation": attrs.Generation}, nil, codes.NotFound},
		{"generation-match", map[string]any{"ifGenerationMatch": attrs.Generation}, nil, codes.FailedPrecondition},
		{"generation-not-match", map[string]any{"ifGenerationNotMatch": 0}, nil, codes.OK},
		{"meta-match", map[string]any{"ifMetagenerationMatch": 999}, nil, codes.FailedPrecondition},
		{"meta-not-match", map[string]any{"ifMetagenerationNotMatch": 1}, nil, codes.FailedPrecondition},
		{"out-of-range", nil, []any{map[string]any{"readId": 1, "readOffset": 999}}, codes.OutOfRange},
		{"negative-length", nil, []any{map[string]any{"readId": 1, "readLength": -1}}, codes.OutOfRange},
		{"too-many-in-one-message", nil, oversized, codes.InvalidArgument},
		{"unsupported", map[string]any{"routingToken": "route"}, nil, codes.Unimplemented},
	} {
		t.Run(tc.name, func(t *testing.T) {
			requestSpec := map[string]any{"bucket": spec["bucket"], "object": spec["object"]}
			for key, value := range tc.change {
				requestSpec[key] = value
			}
			s := bidiStream(t, ctx, endpoint)
			bidiSend(t, s, map[string]any{"readObjectSpec": requestSpec, "readRanges": tc.ranges})
			s.CloseSend()
			_, err := bidiReceive(t, s)
			if status.Code(err) != tc.want {
				t.Fatalf("want %v: %v", tc.want, err)
			}
			if tc.want == codes.InvalidArgument && storage.ShouldRetry(err) {
				t.Fatal("invalid batch must not retry")
			}
			if tc.want == codes.OutOfRange && len(status.Convert(err).Details()) == 0 {
				t.Fatal("missing per-range error details")
			}
		})
	}
}

func TestGRPCBidiReadStrict(t *testing.T) {
	endpoint := os.Getenv("GCS_MIMIC_STRICT_GRPC_ENDPOINT")
	if endpoint == "" {
		t.Skip("GCS_MIMIC_STRICT_GRPC_ENDPOINT is unset")
	}
	ctx, client := newStrictClient(t, strictAdminToken)
	bucket := resumableBucket(t, ctx, client, strictProject)
	object := resumableObject(t, bucket, "private")
	if _, err := strictUpload(ctx, object, []byte("private data")); err != nil {
		t.Fatal(err)
	}
	var handle []byte
	for _, token := range []string{strictAdminToken, strictViewerToken, strictCreatorToken, ""} {
		ctx, cancel := context.WithTimeout(ctx, 5*time.Second)
		if token != "" {
			ctx = metadata.AppendToOutgoingContext(ctx, "authorization", "Bearer "+token)
		}
		s := bidiStream(t, ctx, endpoint)
		spec := map[string]any{"bucket": "projects/_/buckets/" + bucket.BucketName(), "object": "private"}
		if handle != nil {
			spec["readHandle"] = map[string]any{"handle": handle}
		}
		bidiSend(t, s, map[string]any{"readObjectSpec": spec, "readRanges": []any{map[string]any{"readId": 1}}})
		s.CloseSend()
		response, err := bidiReceive(t, s)
		want := codes.OK
		if token == strictCreatorToken {
			want = codes.PermissionDenied
		}
		if token == "" {
			want = codes.Unauthenticated
		}
		if status.Code(err) != want {
			t.Fatalf("token %q: want %v got %v", token, want, err)
		}
		if want == codes.OK {
			handle = response.ReadHandle.Handle
			response, err = bidiReceive(t, s)
			if err != nil || string(response.ObjectDataRanges[0].ChecksummedData.Content) != "private data" {
				t.Fatalf("strict read: %+v %v", response, err)
			}
		}
		cancel()
	}
}
