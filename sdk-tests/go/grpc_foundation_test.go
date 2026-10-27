package gcsmimic_test

import (
	"context"
	"errors"
	"net"
	"os"
	"strings"
	"testing"
	"time"

	"cloud.google.com/go/storage"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/metadata"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/types/known/emptypb"
)

func TestGRPCFoundationFailsFast(t *testing.T) {
	endpoint := os.Getenv("STORAGE_EMULATOR_HOST_GRPC")
	if endpoint == "" {
		t.Skip("STORAGE_EMULATOR_HOST_GRPC is unset")
	}
	host, _, err := net.SplitHostPort(endpoint)
	if err != nil || net.ParseIP(host) == nil || !net.ParseIP(host).IsLoopback() {
		t.Fatalf("gRPC endpoint must be a loopback IP:PORT: %q", endpoint)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 8*time.Second)
	defer cancel()
	client, err := storage.NewGRPCClient(ctx)
	if err != nil {
		t.Fatal(err)
	}
	defer client.Close()
	// The SDK requires this option for its multi-range API, even on the gRPC transport.
	bidiClient, err := storage.NewGRPCClient(ctx, storage.WithGRPCBidiReads())
	if err != nil {
		t.Fatal(err)
	}
	defer bidiClient.Close()
	bucket := client.Bucket("grpc-foundation")
	for _, tc := range []struct {
		name string
		call func(context.Context) error
	}{
		{"RapidWriter", func(ctx context.Context) error {
			writer := bucket.Object("object").NewWriter(ctx)
			writer.Append = true
			_, writeErr := writer.Write([]byte("hello"))
			closeErr := writer.Close()
			if closeErr != nil {
				return closeErr
			}
			return writeErr
		}},
		{"MultiRangeDownloader", func(ctx context.Context) error {
			downloader, err := bidiClient.Bucket("grpc-foundation").Object("object").NewMultiRangeDownloader(ctx)
			if downloader != nil {
				downloader.Close()
			}
			return err
		}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			deadline, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			defer cancel()
			start := time.Now()
			err := tc.call(deadline)
			if status.Code(err) != codes.Unimplemented || storage.ShouldRetry(err) || deadline.Err() != nil || time.Since(start) >= 2*time.Second {
				t.Fatalf("expected immediate non-retryable UNIMPLEMENTED, got %T: %v (elapsed %v)", err, err, time.Since(start))
			}
			if errors.Is(err, storage.ErrBucketNotExist) || errors.Is(err, storage.ErrObjectNotExist) || !strings.Contains(err.Error(), "gcs-mimic") || !strings.Contains(err.Error(), "JSON API") {
				t.Fatalf("unsupported error must identify the emulator and remedy: %v", err)
			}
		})
	}
}

func TestGRPCFoundationWireStatus(t *testing.T) {
	endpoint := os.Getenv("STORAGE_EMULATOR_HOST_GRPC")
	if endpoint == "" {
		t.Skip("STORAGE_EMULATOR_HOST_GRPC is unset")
	}
	host, _, err := net.SplitHostPort(endpoint)
	if err != nil || net.ParseIP(host) == nil || !net.ParseIP(host).IsLoopback() {
		t.Fatal("gRPC endpoint must be loopback IP:PORT")
	}
	conn, err := grpc.NewClient(endpoint, grpc.WithTransportCredentials(insecure.NewCredentials()))
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	for _, method := range []string{
		"CancelResumableWrite", "BidiReadObject", "UnknownMethod",
		"other.Service/UnknownMethod",
	} {
		t.Run(method, func(t *testing.T) {
			ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
			defer cancel()
			path := "/google.storage.v2.Storage/" + method
			if strings.Contains(method, "/") {
				path = "/" + method
			}
			var err error
			if method == "WriteObject" || method == "BidiReadObject" || method == "BidiWriteObject" {
				stream, streamErr := conn.NewStream(ctx, &grpc.StreamDesc{ClientStreams: true, ServerStreams: true}, path)
				if streamErr != nil {
					t.Fatal(streamErr)
				}
				// No first message or EOF: unsupported streaming methods must reject from metadata alone.
				err = stream.RecvMsg(&emptypb.Empty{})
			} else {
				err = conn.Invoke(ctx, path, &emptypb.Empty{}, &emptypb.Empty{})
			}
			st := status.Convert(err)
			if st.Code() != codes.Unimplemented || ctx.Err() != nil || storage.ShouldRetry(err) {
				t.Fatalf("expected non-retryable UNIMPLEMENTED: %v", err)
			}
			if len(st.Details()) != 1 {
				t.Fatalf("missing google.rpc.Status details: %v", st)
			}
			info, ok := st.Details()[0].(interface {
				GetReason() string
				GetDomain() string
				GetMetadata() map[string]string
			})
			if !ok || st.Proto().Details[0].TypeUrl != "type.googleapis.com/google.rpc.ErrorInfo" || info.GetReason() != "badRequest" || info.GetDomain() != "global" || info.GetMetadata()["service"] != "gcs-mimic" || !strings.Contains(info.GetMetadata()["remedy"], "JSON API") {
				t.Fatalf("non-actionable ErrorInfo: %v", st.Details())
			}
		})
	}
}

func TestGRPCFoundationStrictAuth(t *testing.T) {
	endpoint := os.Getenv("GCS_MIMIC_STRICT_GRPC_ENDPOINT")
	if endpoint == "" {
		t.Skip("GCS_MIMIC_STRICT_GRPC_ENDPOINT is unset")
	}
	host, _, err := net.SplitHostPort(endpoint)
	if err != nil || net.ParseIP(host) == nil || !net.ParseIP(host).IsLoopback() {
		t.Fatal("strict gRPC endpoint must be loopback IP:PORT")
	}
	conn, err := grpc.NewClient(endpoint, grpc.WithTransportCredentials(insecure.NewCredentials()))
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	for _, token := range []string{"", "Basic garbage", "Bearer unknown", "Bearer local-admin-token"} {
		ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
		if token != "" {
			ctx = metadata.AppendToOutgoingContext(ctx, "authorization", token)
		}
		err := conn.Invoke(ctx, "/google.storage.v2.Storage/CancelResumableWrite", &emptypb.Empty{}, &emptypb.Empty{})
		cancel()
		want := codes.Unauthenticated
		if token == "Bearer local-admin-token" {
			want = codes.Unimplemented
		}
		if status.Code(err) != want {
			t.Errorf("credential %q: expected %v, got %v", token, want, err)
		}
	}
}
