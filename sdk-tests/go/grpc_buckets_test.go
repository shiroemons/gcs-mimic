package gcsmimic_test

import (
	"context"
	"fmt"
	"net"
	"os"
	"testing"
	"time"

	"cloud.google.com/go/storage"
	"google.golang.org/api/iterator"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/metadata"
	"google.golang.org/grpc/status"
)

func TestGRPCBuckets(t *testing.T) {
	if os.Getenv("STORAGE_EMULATOR_HOST_GRPC") == "" {
		t.Skip("STORAGE_EMULATOR_HOST_GRPC is unset")
	}
	host, _, err := net.SplitHostPort(os.Getenv("STORAGE_EMULATOR_HOST_GRPC"))
	if err != nil || net.ParseIP(host) == nil || !net.ParseIP(host).IsLoopback() {
		t.Fatal("gRPC endpoint must be a loopback IP:PORT")
	}
	ctx, jsonClient, _, _ := newEmulatorClient(t)
	ctx, cancel := context.WithTimeout(ctx, 15*time.Second)
	defer cancel()
	client, err := storage.NewGRPCClient(ctx)
	if err != nil {
		t.Fatal(err)
	}
	defer client.Close()
	prefix := fmt.Sprintf("grpc-buckets-%d-", time.Now().UnixNano())
	jsonBucket := jsonClient.Bucket(prefix + "json")
	if err := jsonBucket.Create(ctx, "sdk-project", nil); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		cleanup, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		_ = jsonBucket.Delete(cleanup)
	})
	attrs, err := client.Bucket(jsonBucket.BucketName()).Attrs(ctx)
	if err != nil || attrs.Name != jsonBucket.BucketName() {
		t.Fatalf("JSON to gRPC: %v, %v", attrs, err)
	}
	bucket := client.Bucket(prefix + "grpc")
	if err := bucket.Create(ctx, "sdk-project", nil); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		cleanup, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		_ = jsonClient.Bucket(bucket.BucketName()).Delete(cleanup)
	})
	attrs, err = jsonClient.Bucket(bucket.BucketName()).Attrs(ctx)
	if err != nil || attrs.Name != bucket.BucketName() {
		t.Fatalf("gRPC to JSON: %v, %v", attrs, err)
	}
	it := client.Buckets(ctx, "sdk-project")
	it.Prefix = prefix
	it.PageInfo().MaxSize = 1
	seen := map[string]bool{}
	for {
		attrs, err := it.Next()
		if err == iterator.Done {
			break
		}
		if err != nil {
			t.Fatal(err)
		}
		seen[attrs.Name] = true
	}
	if len(seen) != 2 || !seen[bucket.BucketName()] || !seen[jsonBucket.BucketName()] {
		t.Fatalf("paginated buckets: %v", seen)
	}
	attrs, err = bucket.Attrs(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := bucket.UserProject("").Attrs(ctx); err != nil {
		t.Fatalf("default empty UserProject: %v", err)
	}
	if _, err := bucket.UserProject("billing-project").Attrs(ctx); status.Code(err) != codes.Unimplemented || len(status.Convert(err).Details()) != 1 {
		t.Fatalf("unsupported billing metadata: %v", err)
	}
	_, billingErr := jsonClient.Bucket(bucket.BucketName()).UserProject("billing-project").Attrs(ctx)
	requireStrictError(t, billingErr, 400, "badRequest")
	if _, err := bucket.If(storage.BucketConditions{MetagenerationMatch: attrs.MetaGeneration + 1}).Attrs(ctx); status.Code(err) != codes.FailedPrecondition {
		t.Fatalf("stale Get condition: %v", err)
	}
	if err := bucket.If(storage.BucketConditions{MetagenerationNotMatch: attrs.MetaGeneration}).Delete(ctx); status.Code(err) != codes.FailedPrecondition {
		t.Fatalf("Delete not-match condition: %v", err)
	}
	if err := bucket.Create(ctx, "sdk-project", nil); status.Code(err) != codes.AlreadyExists {
		t.Fatalf("duplicate Create: %v", err)
	}
	object := jsonClient.Bucket(bucket.BucketName()).Object("nonempty")
	writer := object.NewWriter(ctx)
	if _, err := writer.Write([]byte("hello")); err != nil {
		t.Fatal(err)
	}
	if err := writer.Close(); err != nil {
		t.Fatal(err)
	}
	if err := bucket.Delete(ctx); status.Code(err) != codes.FailedPrecondition {
		t.Fatalf("nonempty Delete: %v", err)
	}
	if err := object.Delete(ctx); err != nil {
		t.Fatal(err)
	}
	if err := bucket.Delete(ctx); err != nil {
		t.Fatal(err)
	}
}

func TestGRPCBucketsStrict(t *testing.T) {
	endpoint := os.Getenv("GCS_MIMIC_STRICT_GRPC_ENDPOINT")
	if endpoint == "" {
		t.Skip("GCS_MIMIC_STRICT_GRPC_ENDPOINT is unset")
	}
	host, _, err := net.SplitHostPort(endpoint)
	if err != nil || net.ParseIP(host) == nil || !net.ParseIP(host).IsLoopback() {
		t.Fatal("strict gRPC endpoint must be loopback IP:PORT")
	}
	t.Setenv("STORAGE_EMULATOR_HOST_GRPC", endpoint)
	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()
	client, err := storage.NewGRPCClient(ctx)
	if err != nil {
		t.Fatal(err)
	}
	defer client.Close()
	admin := metadata.AppendToOutgoingContext(ctx, "authorization", "Bearer local-admin-token")
	viewer := metadata.AppendToOutgoingContext(ctx, "authorization", "Bearer local-viewer-token")
	bucket := client.Bucket(fmt.Sprintf("grpc-strict-%d", time.Now().UnixNano()))
	if err := bucket.Create(admin, "local-project", nil); err != nil {
		t.Fatal(err)
	}
	defer bucket.Delete(admin)
	for _, tc := range []struct {
		ctx  context.Context
		code codes.Code
	}{{ctx, codes.Unauthenticated}, {viewer, codes.PermissionDenied}} {
		if _, err := bucket.Attrs(tc.ctx); status.Code(err) != tc.code {
			t.Errorf("Attrs: want %v, got %v", tc.code, err)
		}
		if err := bucket.Delete(tc.ctx); status.Code(err) != tc.code {
			t.Errorf("Delete: want %v, got %v", tc.code, err)
		}
		if err := bucket.Create(tc.ctx, "local-project", nil); status.Code(err) != tc.code {
			t.Errorf("Create: want %v, got %v", tc.code, err)
		}
		if _, err := client.Buckets(tc.ctx, "local-project").Next(); status.Code(err) != tc.code {
			t.Errorf("List: want %v, got %v", tc.code, err)
		}
	}
	if _, err := bucket.Attrs(admin); err != nil {
		t.Fatal(err)
	}
	_, jsonAdmin := newStrictClient(t, strictAdminToken)
	_, jsonAnonymous := newStrictClient(t, "")
	jsonBucket := jsonAdmin.Bucket(bucket.BucketName())
	policy, err := jsonBucket.IAM().Policy(ctx)
	if err != nil {
		t.Fatal(err)
	}
	policy.Add("allUsers", "roles/storage.admin")
	if err := jsonBucket.IAM().SetPolicy(ctx, policy); err != nil {
		t.Fatal(err)
	}
	if _, err := jsonAnonymous.Bucket(bucket.BucketName()).Attrs(ctx); err != nil {
		t.Fatalf("anonymous JSON public bucket: %v", err)
	}
	if _, err := bucket.Attrs(ctx); err != nil {
		t.Fatalf("anonymous gRPC public bucket: %v", err)
	}
	if err := bucket.Delete(ctx); err != nil {
		t.Fatalf("anonymous gRPC public delete: %v", err)
	}
}
