package gcsmimic_test

import (
	"bytes"
	"context"
	"crypto/rand"
	"fmt"
	"net/http"
	"strings"
	"sync"
	"testing"
	"time"

	"cloud.google.com/go/storage"
	"google.golang.org/api/option"
)

type rewriteRequestLog struct {
	base   http.RoundTripper
	mu     sync.Mutex
	tokens []string
}

func (l *rewriteRequestLog) RoundTrip(r *http.Request) (*http.Response, error) {
	if strings.Contains(r.URL.EscapedPath(), "/rewriteTo/") {
		r = r.Clone(r.Context())
		u := *r.URL
		q := u.Query()
		// The SDK's testing-only byte maximum is private; keep the real Copier loop.
		q.Set("maxBytesRewrittenPerCall", "1048576")
		u.RawQuery = q.Encode()
		r.URL = &u
		l.mu.Lock()
		l.tokens = append(l.tokens, q.Get("rewriteToken"))
		l.mu.Unlock()
	}
	return l.base.RoundTrip(r)
}

func rewriteBucket(t *testing.T, ctx context.Context, client *storage.Client, location string) *storage.BucketHandle {
	t.Helper()
	var nonce [8]byte
	if _, err := rand.Read(nonce[:]); err != nil {
		t.Fatal(err)
	}
	bucket := client.Bucket(fmt.Sprintf("sdk-go-rewrite-%x", nonce))
	if err := bucket.Create(ctx, "sdk-rewrite", &storage.BucketAttrs{Location: location}); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		cleanupCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		if err := bucket.Delete(cleanupCtx); err != nil {
			t.Errorf("cleanup rewrite bucket: %v", err)
		}
	})
	return bucket
}

func TestObjectRewriteCopier(t *testing.T) {
	ctx, client, httpClient, _ := newEmulatorClient(t)
	for _, tc := range []struct {
		name     string
		location string
		size     int
		calls    int
	}{
		{"one call same location", "US", (2 << 20) + 17, 1},
		{"multiple calls across locations", "EU", (2 << 20) + 17, 3},
		{"exact byte boundary", "EU", 2 << 20, 2},
		{"empty object", "EU", 0, 1},
	} {
		t.Run(tc.name, func(t *testing.T) {
			log := &rewriteRequestLog{base: httpClient.Transport}
			recordedHTTP := *httpClient
			recordedHTTP.Transport = log
			recorded, err := storage.NewClient(ctx, storage.WithJSONReads(), storage.WithDisabledClientMetrics(),
				option.WithoutAuthentication(), option.WithHTTPClient(&recordedHTTP))
			if err != nil {
				t.Fatal(err)
			}
			t.Cleanup(func() {
				if err := recorded.Close(); err != nil {
					t.Errorf("close rewrite client: %v", err)
				}
			})
			sourceBucket := rewriteBucket(t, ctx, client, "US")
			destinationBucket := rewriteBucket(t, ctx, client, tc.location)
			source := resumableObject(t, sourceBucket, "source/日本 +%.bin")
			destination := resumableObject(t, destinationBucket, "destination/雪 +%.bin")
			payload := bytes.Repeat([]byte{0, 255, 17, 42}, (tc.size+3)/4)[:tc.size]
			writer := source.NewWriter(ctx)
			writer.ChunkSize = 0
			writer.ContentType = "application/x-rewrite-source"
			writer.Metadata = map[string]string{"source": "preserved"}
			if _, err := writer.Write(payload); err != nil {
				t.Fatal(err)
			}
			if err := writer.Close(); err != nil {
				t.Fatal(err)
			}
			copier := recorded.Bucket(destinationBucket.BucketName()).Object(destination.ObjectName()).
				If(storage.Conditions{DoesNotExist: true}).CopierFrom(source.Generation(writer.Attrs().Generation))
			copier.ContentType = "application/x-rewrite-destination"
			copier.Metadata = map[string]string{"destination": "雪"}
			var progress []uint64
			copier.ProgressFunc = func(copied, total uint64) {
				if total != uint64(len(payload)) || copied > total ||
					(len(progress) > 0 && copied <= progress[len(progress)-1]) {
					t.Errorf("invalid rewrite progress: copied=%d total=%d previous=%v", copied, total, progress)
				}
				progress = append(progress, copied)
			}
			attrs, err := copier.Run(ctx)
			if err != nil {
				t.Fatal(err)
			}
			if attrs.Size != int64(len(payload)) || attrs.ContentType != copier.ContentType ||
				attrs.Metadata["destination"] != "雪" || len(attrs.Metadata) != 1 ||
				attrs.CRC32C != writer.Attrs().CRC32C || !bytes.Equal(attrs.MD5, writer.Attrs().MD5) ||
				copier.RewriteToken != "" || len(progress) != tc.calls {
				t.Fatalf("rewrite attrs=%+v token=%q progress=%v", attrs, copier.RewriteToken, progress)
			}
			readObject(t, ctx, destination, payload, attrs.Generation)
			readObject(t, ctx, source, payload, writer.Attrs().Generation)
			log.mu.Lock()
			tokens := append([]string(nil), log.tokens...)
			log.mu.Unlock()
			if len(tokens) != tc.calls || tokens[0] != "" {
				t.Fatalf("rewrite request tokens=%v; want %d calls", tokens, tc.calls)
			}
			for _, token := range tokens[1:] {
				if token == "" {
					t.Fatal("Copier continued without a rewrite token")
				}
			}
			_, err = destination.If(storage.Conditions{DoesNotExist: true}).CopierFrom(source).Run(ctx)
			requireObjectStatus(t, err, http.StatusPreconditionFailed)
			readObject(t, ctx, destination, payload, attrs.Generation)
		})
	}
}
