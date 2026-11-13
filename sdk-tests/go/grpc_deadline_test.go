package gcsmimic_test

import (
	"bytes"
	"context"
	"errors"
	"io"
	"net"
	"os"
	"testing"
	"time"

	"cloud.google.com/go/storage"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/types/dynamicpb"
)

// gRPC clients enforce deadlines and cancellation by resetting the stream, so these cases cut
// each call shape short and check what the emulator leaves behind on the same connection: no
// published partial object, no held writer, a resumable session that resumes, and a connection
// that keeps serving.
func TestGRPCDeadlineAndCancellation(t *testing.T) {
	endpoint := os.Getenv("STORAGE_EMULATOR_HOST_GRPC")
	if endpoint == "" {
		t.Skip("STORAGE_EMULATOR_HOST_GRPC is unset")
	}
	host, _, err := net.SplitHostPort(endpoint)
	if err != nil || net.ParseIP(host) == nil || !net.ParseIP(host).IsLoopback() {
		t.Fatal("gRPC endpoint must be loopback IP:PORT")
	}
	ctx, jsonClient, _, _ := newEmulatorClient(t)
	bucket := resumableBucket(t, ctx, jsonClient, "grpc-deadline")
	ctx, cancel := context.WithTimeout(ctx, 60*time.Second)
	defer cancel()
	// A fixed 64 KiB window disables grpc-go's dynamic window growth, so a reader that stops
	// reading leaves the server blocked on flow control instead of buffering the whole object.
	conn, err := grpc.NewClient(endpoint,
		grpc.WithTransportCredentials(insecure.NewCredentials()),
		grpc.WithInitialWindowSize(64<<10),
		grpc.WithInitialConnWindowSize(64<<10))
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()

	msg := func(name string, fields map[string]any) *dynamicpb.Message { return grpcIOMessage(t, name, fields) }
	bucketName := "projects/_/buckets/" + bucket.BucketName()
	spec := func(name string) *dynamicpb.Message {
		resource := msg("Object", map[string]any{"bucket": bucketName, "name": name})
		return msg("WriteObjectSpec", map[string]any{"resource": resource, "if_generation_match": int64(0)})
	}
	data := func(content []byte) *dynamicpb.Message {
		return msg("ChecksummedData", map[string]any{"content": content})
	}
	open := func(callCtx context.Context, method string, desc grpc.StreamDesc) grpc.ClientStream {
		t.Helper()
		stream, err := conn.NewStream(callCtx, &desc, "/google.storage.v2.Storage/"+method)
		if err != nil {
			t.Fatal(err)
		}
		return stream
	}
	// failure drains whatever arrived before the reset and returns the call's final status.
	failure := func(stream grpc.ClientStream, response string) codes.Code {
		for {
			if err := stream.RecvMsg(msg(response, nil)); err != nil {
				return status.Code(err)
			}
		}
	}
	read := func(name string) []byte {
		t.Helper()
		reader, err := bucket.Object(name).NewReader(ctx)
		if err != nil {
			t.Fatalf("read %s: %v", name, err)
		}
		defer reader.Close()
		content, err := io.ReadAll(reader)
		if err != nil {
			t.Fatalf("read %s: %v", name, err)
		}
		return content
	}
	// serving proves that an abandoned call left the shared connection usable.
	serving := func(name string) {
		t.Helper()
		request := msg("GetObjectRequest", map[string]any{"bucket": bucketName, "object": name})
		if err := conn.Invoke(ctx, "/google.storage.v2.Storage/GetObject", request, msg("Object", nil)); err != nil {
			t.Fatalf("GetObject after an abandoned call: %v", err)
		}
	}
	names := []string{"deadline-write", "cancelled-resumable", "large"}
	t.Cleanup(func() {
		cleanup, stop := context.WithTimeout(context.Background(), 5*time.Second)
		defer stop()
		for _, name := range names {
			_ = bucket.Object(name).Delete(cleanup)
		}
	})

	large := bytes.Repeat([]byte("0123456789abcdef"), 512<<10)
	writer := bucket.Object("large").NewWriter(ctx)
	if _, err := writer.Write(large); err != nil {
		t.Fatal(err)
	}
	if err := writer.Close(); err != nil {
		t.Fatal(err)
	}

	// WriteObject: an expired deadline publishes nothing and leaves no writer behind.
	{
		callCtx, stop := context.WithTimeout(ctx, 300*time.Millisecond)
		defer stop()
		stream := open(callCtx, "WriteObject", grpc.StreamDesc{ClientStreams: true})
		if err := stream.SendMsg(msg("WriteObjectRequest", map[string]any{"write_object_spec": spec("deadline-write"), "checksummed_data": data([]byte("partial"))})); err != nil {
			t.Fatal(err)
		}
		<-callCtx.Done()
		if code := failure(stream, "WriteObjectResponse"); code != codes.DeadlineExceeded {
			t.Fatalf("expired WriteObject=%v", code)
		}
		if _, err := bucket.Object("deadline-write").Attrs(ctx); !errors.Is(err, storage.ErrObjectNotExist) {
			t.Fatalf("expired WriteObject published an object: %v", err)
		}
		// if_generation_match=0 succeeds only while nothing was published under the name.
		stream = open(ctx, "WriteObject", grpc.StreamDesc{ClientStreams: true})
		stream.SendMsg(msg("WriteObjectRequest", map[string]any{"write_object_spec": spec("deadline-write"), "checksummed_data": data([]byte("complete")), "finish_write": true}))
		stream.CloseSend()
		if err := stream.RecvMsg(msg("WriteObjectResponse", nil)); err != nil {
			t.Fatalf("write after the expired call: %v", err)
		}
		if got := read("deadline-write"); string(got) != "complete" {
			t.Fatalf("content=%q", got)
		}
		serving("deadline-write")
	}

	// BidiWriteObject: cancellation keeps the resumable session, which resumes and completes.
	{
		started := msg("StartResumableWriteResponse", nil)
		if err := conn.Invoke(ctx, "/google.storage.v2.Storage/StartResumableWrite", msg("StartResumableWriteRequest", map[string]any{"write_object_spec": spec("cancelled-resumable")}), started); err != nil {
			t.Fatal(err)
		}
		upload := grpcIOField(started, "upload_id").String()
		callCtx, stop := context.WithCancel(ctx)
		stream := open(callCtx, "BidiWriteObject", grpc.StreamDesc{ClientStreams: true, ServerStreams: true})
		if err := stream.SendMsg(msg("BidiWriteObjectRequest", map[string]any{"upload_id": upload, "checksummed_data": data([]byte("abc")), "state_lookup": true})); err != nil {
			t.Fatal(err)
		}
		persisted := msg("BidiWriteObjectResponse", nil)
		if err := stream.RecvMsg(persisted); err != nil || grpcIOField(persisted, "persisted_size").Int() != 3 {
			t.Fatalf("persisted=%v error=%v", persisted, err)
		}
		// Cancel only after the acknowledgement. Bytes sent without one may still commit after a
		// later QueryWriteStatus, because the server finishes a message it had already received.
		stop()
		if code := failure(stream, "BidiWriteObjectResponse"); code != codes.Canceled {
			t.Fatalf("cancelled BidiWriteObject=%v", code)
		}
		if _, err := bucket.Object("cancelled-resumable").Attrs(ctx); !errors.Is(err, storage.ErrObjectNotExist) {
			t.Fatalf("cancelled upload published an object: %v", err)
		}
		queried := msg("QueryWriteStatusResponse", nil)
		if err := conn.Invoke(ctx, "/google.storage.v2.Storage/QueryWriteStatus", msg("QueryWriteStatusRequest", map[string]any{"upload_id": upload}), queried); err != nil {
			t.Fatalf("query after cancellation: %v", err)
		}
		if offset := grpcIOField(queried, "persisted_size").Int(); offset != 3 {
			t.Fatalf("persisted after cancellation=%d", offset)
		}
		content := []byte("abcdef")
		stream = open(ctx, "BidiWriteObject", grpc.StreamDesc{ClientStreams: true, ServerStreams: true})
		stream.SendMsg(msg("BidiWriteObjectRequest", map[string]any{"upload_id": upload, "write_offset": int64(3), "checksummed_data": data(content[3:]), "finish_write": true}))
		stream.CloseSend()
		if err := stream.RecvMsg(msg("BidiWriteObjectResponse", nil)); err != nil {
			t.Fatalf("resume after cancellation: %v", err)
		}
		if got := read("cancelled-resumable"); !bytes.Equal(got, content) {
			t.Fatalf("content=%q", got)
		}
		serving("cancelled-resumable")
	}

	// ReadObject: an expired deadline ends a server stream that the reader stopped draining.
	{
		callCtx, stop := context.WithTimeout(ctx, 300*time.Millisecond)
		defer stop()
		stream := open(callCtx, "ReadObject", grpc.StreamDesc{ServerStreams: true})
		if err := stream.SendMsg(msg("ReadObjectRequest", map[string]any{"bucket": bucketName, "object": "large"})); err != nil {
			t.Fatal(err)
		}
		stream.CloseSend()
		if err := stream.RecvMsg(msg("ReadObjectResponse", nil)); err != nil {
			t.Fatalf("first ReadObject message: %v", err)
		}
		<-callCtx.Done()
		if code := failure(stream, "ReadObjectResponse"); code != codes.DeadlineExceeded {
			t.Fatalf("expired ReadObject=%v", code)
		}
		serving("large")
	}

	// BidiReadObject: cancellation ends a bidirectional read mid-range.
	{
		callCtx, stop := context.WithCancel(ctx)
		stream := open(callCtx, "BidiReadObject", grpc.StreamDesc{ClientStreams: true, ServerStreams: true})
		request := bidiMessage(t, "BidiReadObjectRequest", map[string]any{
			"readObjectSpec": map[string]any{"bucket": bucketName, "object": "large"},
			"readRanges":     []any{map[string]any{"readId": 1, "readOffset": 0, "readLength": 0}},
		})
		if err := stream.SendMsg(request); err != nil {
			t.Fatal(err)
		}
		if err := stream.RecvMsg(bidiMessage(t, "BidiReadObjectResponse", nil)); err != nil {
			t.Fatalf("first BidiReadObject message: %v", err)
		}
		stop()
		if code := failure(stream, "BidiReadObjectResponse"); code != codes.Canceled {
			t.Fatalf("cancelled BidiReadObject=%v", code)
		}
		serving("large")
	}
}
