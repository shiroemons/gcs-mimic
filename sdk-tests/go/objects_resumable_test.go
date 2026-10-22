package gcsmimic_test

import (
	"bytes"
	"context"
	"crypto/md5"
	"crypto/rand"
	"encoding/base64"
	"encoding/binary"
	"errors"
	"fmt"
	"hash/crc32"
	"io"
	"maps"
	"net/http"
	"sync"
	"testing"
	"time"

	"cloud.google.com/go/storage"
	"google.golang.org/api/option"
)

const resumableChunkSize = 256 << 10

func resumableBucket(t *testing.T, ctx context.Context, client *storage.Client, project string) *storage.BucketHandle {
	t.Helper()
	var nonce [8]byte
	if _, err := rand.Read(nonce[:]); err != nil {
		t.Fatal(err)
	}
	bucket := client.Bucket(fmt.Sprintf("sdk-go-resumable-%x", nonce))
	if err := bucket.Create(ctx, project, nil); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		cleanupCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		if err := bucket.Delete(cleanupCtx); err != nil {
			t.Errorf("cleanup bucket: %v", err)
		}
	})
	return bucket
}

func resumableObject(t *testing.T, bucket *storage.BucketHandle, name string) *storage.ObjectHandle {
	t.Helper()
	object := bucket.Object(name)
	t.Cleanup(func() {
		ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		if err := object.Delete(ctx); err != nil && !errors.Is(err, storage.ErrObjectNotExist) {
			t.Errorf("cleanup object: %v", err)
		}
	})
	return object
}

func requireResumableObject(t *testing.T, ctx context.Context, object *storage.ObjectHandle, writer *storage.Writer, payload []byte) {
	t.Helper()
	attrs, err := object.Attrs(ctx)
	if err != nil {
		t.Fatal(err)
	}
	md5sum := md5.Sum(payload)
	crc := crc32.Checksum(payload, crc32.MakeTable(crc32.Castagnoli))
	for _, got := range []*storage.ObjectAttrs{writer.Attrs(), attrs} {
		if got == nil || got.Bucket != object.BucketName() || got.Name != object.ObjectName() ||
			got.Size != int64(len(payload)) || got.Generation <= 0 || got.Metageneration != 1 ||
			got.ContentType != writer.ContentType || !maps.Equal(got.Metadata, writer.Metadata) ||
			got.CRC32C != crc || !bytes.Equal(got.MD5, md5sum[:]) {
			t.Fatalf("resumable attrs: %+v; want size=%d, CRC32C=%08x, MD5=%x", got, len(payload), crc, md5sum)
		}
	}
	readObject(t, ctx, object, payload, attrs.Generation)
}

// RSM-SDK-GO-001 and RSM-SDK-GO-002 keep real resumable and small multipart paths visible.
func TestObjectResumableWriter(t *testing.T) {
	ctx, _, httpClient, _ := newEmulatorClient(t)
	for _, tc := range []struct {
		name      string
		chunkSize int
		size      int
		stream    bool
	}{
		{"default chunk size resumable", -1, (16 << 20) + 37, false},
		{"default chunk size small multipart", -1, 2*resumableChunkSize + 37, false},
		{"multiple 256 KiB chunks", resumableChunkSize, 2*resumableChunkSize + 37, false},
		{"empty final chunk", resumableChunkSize, 2 * resumableChunkSize, false},
		{"empty object", resumableChunkSize, 0, false},
		{"unknown length stream", resumableChunkSize, 2*resumableChunkSize + 37, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			ctx, cancel := context.WithCancel(ctx)
			defer cancel()
			client, log := recordedResumableClient(t, ctx, httpClient, false)
			bucket := resumableBucket(t, ctx, client, "sdk-resumable")
			object := resumableObject(t, bucket, "dir/日本語 +%?.bin")
			payload := bytes.Repeat([]byte("\x00\xffstream\r\n"), tc.size/10+1)[:tc.size]
			writer := object.If(storage.Conditions{DoesNotExist: true}).NewWriter(ctx)
			if tc.chunkSize >= 0 {
				writer.ChunkSize = tc.chunkSize
			}
			writer.ContentType = "application/x-resumable-test"
			writer.Metadata = map[string]string{"team": "sdk", "unicode": "雪", "empty": ""}
			if tc.chunkSize == resumableChunkSize && !tc.stream {
				sum := md5.Sum(payload)
				writer.MD5 = sum[:]
				writer.SendCRC32C = true
				writer.CRC32C = crc32.Checksum(payload, crc32.MakeTable(crc32.Castagnoli))
			}
			if tc.stream {
				// Hide length and WriterTo so copying uses only a bounded streaming buffer.
				_, err := io.CopyBuffer(writer, struct{ io.Reader }{bytes.NewReader(payload)}, make([]byte, 17<<10))
				if err != nil {
					t.Fatal(err)
				}
			} else if _, err := writer.Write(payload); err != nil {
				t.Fatal(err)
			}
			requireObjectMissing(t, ctx, object)
			if err := writer.Close(); err != nil {
				t.Fatal(err)
			}
			requireResumableObject(t, ctx, object, writer, payload)
			log.mu.Lock()
			defer log.mu.Unlock()
			wantChunks := 3
			if tc.chunkSize < 0 {
				wantChunks = 2
				if tc.size < 16<<20 {
					wantChunks = 0
				}
			} else if tc.size == 0 {
				wantChunks = 0
			}
			if len(log.requests) != wantChunks {
				t.Fatalf("chunk requests=%d; want %d", len(log.requests), wantChunks)
			}
			for _, request := range log.requests {
				if request.Method != http.MethodPost || request.URL.String() != log.location {
					t.Fatalf("chunk did not POST to returned Location: method=%s, same location=%v", request.Method, request.URL.String() == log.location)
				}
			}
		})
	}
}

// RSM-SDK-GO-002 also rejects invalid supplied hashes without replacing live data.
func TestObjectResumableChecksumFailurePreservesObject(t *testing.T) {
	ctx, client, _, _ := newEmulatorClient(t)
	bucket := resumableBucket(t, ctx, client, "sdk-resumable")
	payload := bytes.Repeat([]byte("checksum\x00\xff"), 60000)
	for _, algorithm := range []string{"CRC32C", "MD5"} {
		for _, existing := range []bool{false, true} {
			t.Run(fmt.Sprintf("%s existing=%v", algorithm, existing), func(t *testing.T) {
				ctx, cancel := context.WithCancel(ctx)
				defer cancel()
				object := resumableObject(t, bucket, fmt.Sprintf("bad-%s-%v", algorithm, existing))
				condition := storage.Conditions{DoesNotExist: true}
				original := []byte("previous generation survives a bad checksum")
				if existing {
					attrs, err := uploadDefaultObject(ctx, object.If(condition), original)
					if err != nil {
						t.Fatal(err)
					}
					condition = storage.Conditions{GenerationMatch: attrs.Generation}
				}
				writer := object.If(condition).NewWriter(ctx)
				writer.ChunkSize = resumableChunkSize
				if algorithm == "CRC32C" {
					writer.SendCRC32C = true
					writer.CRC32C = crc32.Checksum(payload, crc32.MakeTable(crc32.Castagnoli)) ^ 1
				} else {
					sum := md5.Sum(payload)
					writer.MD5 = append([]byte(nil), sum[:]...)
					writer.MD5[0] ^= 1
				}
				if _, err := writer.Write(payload); err != nil {
					requireObjectStatus(t, err, http.StatusBadRequest)
				}
				requireObjectStatus(t, writer.Close(), http.StatusBadRequest)
				if existing {
					readObject(t, ctx, object, original, condition.GenerationMatch)
				} else {
					requireObjectMissing(t, ctx, object)
				}
			})
		}
	}
}

func writeResumablePrefix(t *testing.T, ctx context.Context, writer *storage.Writer) {
	t.Helper()
	acknowledged := make(chan struct{}, 1)
	writer.ChunkSize = resumableChunkSize
	writer.ProgressFunc = func(n int64) {
		if n == resumableChunkSize {
			select {
			case acknowledged <- struct{}{}:
			default:
			}
		}
	}
	if _, err := writer.Write(bytes.Repeat([]byte("x"), resumableChunkSize+1)); err != nil {
		t.Fatal(err)
	}
	select {
	case <-acknowledged:
	case <-ctx.Done():
		t.Fatalf("first chunk was not acknowledged: %v", ctx.Err())
	}
}

func uploadDefaultObject(ctx context.Context, object *storage.ObjectHandle, payload []byte) (*storage.ObjectAttrs, error) {
	writer := object.NewWriter(ctx)
	writer.ContentType = "application/octet-stream"
	_, writeErr := writer.Write(payload)
	if err := writer.Close(); err != nil {
		return nil, err
	}
	return writer.Attrs(), writeErr
}

// RSM-SDK-GO-005 rechecks initiation conditions after a concurrent publication.
func TestObjectResumableConditionsAtFinalization(t *testing.T) {
	ctx, client, _, _ := newEmulatorClient(t)
	bucket := resumableBucket(t, ctx, client, "sdk-resumable")
	for _, existing := range []bool{false, true} {
		t.Run(fmt.Sprintf("existing=%v", existing), func(t *testing.T) {
			object := resumableObject(t, bucket, fmt.Sprintf("condition-%v", existing))
			condition := storage.Conditions{DoesNotExist: true}
			if existing {
				attrs, err := uploadDefaultObject(ctx, object.If(condition), []byte("original"))
				if err != nil {
					t.Fatal(err)
				}
				condition = storage.Conditions{GenerationMatch: attrs.Generation}
			}
			uploadCtx, cancel := context.WithCancel(ctx)
			defer cancel()
			writer := object.If(condition).NewWriter(uploadCtx)
			writeResumablePrefix(t, ctx, writer)
			if existing {
				readObject(t, ctx, object, []byte("original"), condition.GenerationMatch)
			} else {
				requireObjectMissing(t, ctx, object)
			}
			winner := []byte("concurrent winner")
			attrs, err := uploadDefaultObject(ctx, object.If(condition), winner)
			if err != nil {
				t.Fatal(err)
			}
			requireObjectStatus(t, writer.Close(), http.StatusPreconditionFailed)
			readObject(t, ctx, object, winner, attrs.Generation)
			replacement := bytes.Repeat([]byte("conditional replacement"), 25000)
			matched := object.If(storage.Conditions{GenerationMatch: attrs.Generation}).NewWriter(ctx)
			matched.ChunkSize = resumableChunkSize
			matched.ContentType = "application/x-conditional-resumable"
			if _, err := matched.Write(replacement); err != nil {
				t.Fatal(err)
			}
			readObject(t, ctx, object, winner, attrs.Generation)
			if err := matched.Close(); err != nil {
				t.Fatal(err)
			}
			requireResumableObject(t, ctx, object, matched, replacement)
			if matched.Attrs().Generation == attrs.Generation {
				t.Fatal("successful overwrite did not create a new generation")
			}
		})
	}
}

// RSM-SDK-GO-005 isolates metadata changes from generation changes at publication.
func TestObjectResumableMetagenerationAtFinalization(t *testing.T) {
	ctx, client, _, _ := newEmulatorClient(t)
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	bucket := resumableBucket(t, ctx, client, "sdk-resumable")
	object := resumableObject(t, bucket, "metageneration")
	original := []byte("unchanged bytes during metadata update")
	attrs, err := uploadDefaultObject(ctx, object.If(storage.Conditions{DoesNotExist: true}), original)
	if err != nil {
		t.Fatal(err)
	}
	writer := object.If(storage.Conditions{GenerationMatch: attrs.Generation, MetagenerationMatch: attrs.Metageneration}).NewWriter(ctx)
	writeResumablePrefix(t, ctx, writer)
	updated, err := object.Update(ctx, storage.ObjectAttrsToUpdate{Metadata: map[string]string{"winner": "metadata update"}})
	if err != nil {
		t.Fatal(err)
	}
	if updated.Generation != attrs.Generation || updated.Metageneration <= attrs.Metageneration {
		t.Fatalf("metadata update: generation=%d, metageneration=%d", updated.Generation, updated.Metageneration)
	}
	requireObjectStatus(t, writer.Close(), http.StatusPreconditionFailed)
	readObject(t, ctx, object, original, attrs.Generation)
	current, err := object.Attrs(ctx)
	if err != nil || current.Metageneration != updated.Metageneration || !maps.Equal(current.Metadata, updated.Metadata) {
		t.Fatalf("failed upload changed metadata: attrs=%+v, err=%v", current, err)
	}
	payload := bytes.Repeat([]byte("matched metadata\x00\xff"), 40000)
	matched := object.If(storage.Conditions{GenerationMatch: current.Generation, MetagenerationMatch: current.Metageneration}).NewWriter(ctx)
	matched.ChunkSize = resumableChunkSize
	matched.ContentType = "application/x-metageneration-resumable"
	if _, err := matched.Write(payload); err != nil {
		t.Fatal(err)
	}
	if err := matched.Close(); err != nil {
		t.Fatal(err)
	}
	requireResumableObject(t, ctx, object, matched, payload)
}

// RSM-SDK-GO-004 distinguishes local cancellation from explicit session cancellation.
func TestObjectResumableContextCancellationDoesNotPublish(t *testing.T) {
	ctx, _, httpClient, _ := newEmulatorClient(t)
	client, log := recordedResumableClient(t, ctx, httpClient, false)
	bucket := resumableBucket(t, ctx, client, "sdk-resumable")
	object := resumableObject(t, bucket, "cancelled")
	uploadCtx, cancel := context.WithCancel(ctx)
	defer cancel()
	writer := object.If(storage.Conditions{DoesNotExist: true}).NewWriter(uploadCtx)
	writeResumablePrefix(t, ctx, writer)
	requireObjectMissing(t, ctx, object)
	cancel()
	if err := writer.Close(); !errors.Is(err, context.Canceled) {
		t.Fatalf("cancelled upload: want context.Canceled, got %v", err)
	}
	requireObjectMissing(t, ctx, object)
	log.mu.Lock()
	location, deletes := log.location, log.deletes
	log.mu.Unlock()
	if location == "" || deletes != 0 {
		t.Fatalf("cancelled Writer: session present=%v, DELETE count=%d", location != "", deletes)
	}
	request, err := http.NewRequestWithContext(ctx, http.MethodPut, location, nil)
	if err != nil {
		t.Fatal(err)
	}
	request.Header.Set("Content-Range", "bytes */*")
	response, err := httpClient.Do(request)
	if err != nil {
		t.Fatal(err)
	}
	closeErr := response.Body.Close()
	if closeErr != nil || response.StatusCode != 308 || response.Header.Get("Range") != "bytes=0-262143" {
		t.Fatalf("retained session: status=%d, Range=%q, close=%v", response.StatusCode, response.Header.Get("Range"), closeErr)
	}
	request, err = http.NewRequestWithContext(ctx, http.MethodDelete, location, nil)
	if err != nil {
		t.Fatal(err)
	}
	response, err = httpClient.Do(request)
	if err != nil {
		t.Fatal(err)
	}
	if closeErr := response.Body.Close(); closeErr != nil || response.StatusCode != 499 {
		t.Fatalf("explicit session cancellation: status=%d, close=%v", response.StatusCode, closeErr)
	}
	requireObjectMissing(t, ctx, object)
}

type interruptedChunkTransport struct {
	base         http.RoundTripper
	mu           sync.Mutex
	requests     []*http.Request
	dropped      bool
	dropResponse bool
	location     string
	deletes      int
}

func (l *interruptedChunkTransport) RoundTrip(r *http.Request) (*http.Response, error) {
	if r.Header.Get("Content-Range") == "" {
		response, err := l.base.RoundTrip(r)
		l.mu.Lock()
		if r.Method == http.MethodDelete && r.URL.String() == l.location {
			l.deletes++
		}
		if err == nil && r.Method == http.MethodPost && r.URL.Query().Get("uploadType") == "resumable" {
			l.location = response.Header.Get("Location")
		}
		l.mu.Unlock()
		return response, err
	}
	l.mu.Lock()
	if len(l.requests) == 16 {
		l.mu.Unlock()
		return nil, errors.New("chunk request limit exceeded")
	}
	l.requests = append(l.requests, r.Clone(r.Context()))
	l.mu.Unlock()
	response, err := l.base.RoundTrip(r)
	if err != nil {
		return response, err
	}
	l.mu.Lock()
	drop := l.dropResponse && !l.dropped && response.StatusCode == http.StatusOK && response.Header.Get("X-Http-Status-Code-Override") == "308"
	if drop {
		l.dropped = true
	}
	l.mu.Unlock()
	if drop {
		// Lose one acknowledgement after persistence; the SDK must resend this chunk.
		_, readErr := io.Copy(io.Discard, io.LimitReader(response.Body, 64<<10))
		closeErr := response.Body.Close()
		if readErr != nil || closeErr != nil {
			return nil, fmt.Errorf("discard chunk acknowledgement: read=%v, close=%v", readErr, closeErr)
		}
		return nil, io.ErrUnexpectedEOF
	}
	return response, nil
}

func recordedResumableClient(t *testing.T, ctx context.Context, httpClient *http.Client, dropResponse bool) (*storage.Client, *interruptedChunkTransport) {
	t.Helper()
	log := &interruptedChunkTransport{base: httpClient.Transport, dropResponse: dropResponse}
	guarded := *httpClient
	guarded.Transport = log
	// The SDK clones its HTTP client, so install the fault before constructing it.
	client, err := storage.NewClient(ctx, storage.WithJSONReads(), storage.WithDisabledClientMetrics(),
		option.WithoutAuthentication(), option.WithHTTPClient(&guarded))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := client.Close(); err != nil {
			t.Errorf("close client: %v", err)
		}
	})
	return client, log
}

// RSM-SDK-GO-003 exercises the pinned SDK's whole-chunk retry after a lost acknowledgement.
func TestObjectResumableRetriesLostChunkResponse(t *testing.T) {
	ctx, _, httpClient, _ := newEmulatorClient(t)
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	client, log := recordedResumableClient(t, ctx, httpClient, true)
	bucket := resumableBucket(t, ctx, client, "sdk-resumable")
	object := resumableObject(t, bucket, "retry")
	payload := bytes.Repeat([]byte("retry\x00\xff"), 80000)
	writer := object.If(storage.Conditions{DoesNotExist: true}).NewWriter(ctx)
	writer.ChunkSize = resumableChunkSize
	writer.ChunkRetryDeadline = 5 * time.Second
	writer.ContentType = "application/x-resumable-retry"
	if _, err := writer.Write(payload); err != nil {
		t.Fatal(err)
	}
	if err := writer.Close(); err != nil {
		t.Fatal(err)
	}
	requireResumableObject(t, ctx, object, writer, payload)
	log.mu.Lock()
	defer log.mu.Unlock()
	if !log.dropped || len(log.requests) != 4 {
		t.Fatalf("dropped=%v, chunk attempts=%d; want one dropped acknowledgement and four attempts", log.dropped, len(log.requests))
	}
	wantRanges := []string{"bytes 0-262143/*", "bytes 0-262143/*", "bytes 262144-524287/*", fmt.Sprintf("bytes 524288-%d/%d", len(payload)-1, len(payload))}
	for i, request := range log.requests {
		if request.Method != http.MethodPost || request.Header.Get("Content-Range") != wantRanges[i] ||
			request.Header.Get("X-GUploader-No-308") != "yes" || request.URL.String() != log.requests[0].URL.String() {
			t.Fatalf("chunk %d: method=%s, range=%q, no-308=%q, same location=%v", i, request.Method,
				request.Header.Get("Content-Range"), request.Header.Get("X-GUploader-No-308"), request.URL.String() == log.requests[0].URL.String())
		}
	}
	if token := log.requests[0].Header.Get("X-Goog-Gcs-Idempotency-Token"); token == "" || token != log.requests[1].Header.Get("X-Goog-Gcs-Idempotency-Token") {
		t.Fatal("retried chunk did not preserve its invocation token")
	}
	var crc [4]byte
	binary.BigEndian.PutUint32(crc[:], crc32.Checksum(payload, crc32.MakeTable(crc32.Castagnoli)))
	wantHash := "crc32c=" + base64.StdEncoding.EncodeToString(crc[:])
	if got := log.requests[3].Header.Get("X-Goog-Hash"); got != wantHash {
		t.Fatalf("final checksum=%q; want %q", got, wantHash)
	}
}

// RSM-SDK-GO-005 retains strict-admin success beside creator permission races.
func TestStrictAuthResumableWriter(t *testing.T) {
	ctx, client := newStrictClient(t, strictAdminToken)
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	bucket := resumableBucket(t, ctx, client, strictProject)
	object := resumableObject(t, bucket, "strict-resumable")
	payload := bytes.Repeat([]byte("strict\x00\xff"), 70000)
	writer := object.If(storage.Conditions{DoesNotExist: true}).NewWriter(ctx)
	writer.ChunkSize = resumableChunkSize
	writer.ContentType = "application/x-strict-resumable"
	if _, err := writer.Write(payload); err != nil {
		t.Fatal(err)
	}
	if err := writer.Close(); err != nil {
		t.Fatal(err)
	}
	requireResumableObject(t, ctx, object, writer, payload)
}

// RSM-SDK-GO-005 rechecks overwrite permission using the initiating creator identity.
func TestStrictAuthResumableCreatorOverwriteRace(t *testing.T) {
	ctx, admin := newStrictClient(t, strictAdminToken)
	_, creator := newStrictClient(t, strictCreatorToken)
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	bucket := resumableBucket(t, ctx, admin, strictProject)
	object := resumableObject(t, bucket, "creator-created")
	creatorObject := creator.Bucket(bucket.BucketName()).Object(object.ObjectName())
	payload := bytes.Repeat([]byte("creator\x00\xff"), 70000)
	created := creatorObject.NewWriter(ctx)
	created.ChunkSize = resumableChunkSize
	created.ContentType = "application/x-creator-resumable"
	if _, err := created.Write(payload); err != nil {
		t.Fatal(err)
	}
	if err := created.Close(); err != nil {
		t.Fatal(err)
	}
	requireResumableObject(t, ctx, object, created, payload)
	denied := creatorObject.NewWriter(ctx)
	denied.ChunkSize = resumableChunkSize
	if _, err := denied.Write(payload); err != nil {
		requireObjectStatus(t, err, http.StatusForbidden)
	}
	requireObjectStatus(t, denied.Close(), http.StatusForbidden)
	readObject(t, ctx, object, payload, created.Attrs().Generation)
	object = resumableObject(t, bucket, "creator-race")
	creatorObject = creator.Bucket(bucket.BucketName()).Object(object.ObjectName())
	racing := creatorObject.NewWriter(ctx)
	writeResumablePrefix(t, ctx, racing)
	winner := []byte("admin wins the concurrent create")
	attrs, err := uploadDefaultObject(ctx, object.If(storage.Conditions{DoesNotExist: true}), winner)
	if err != nil {
		t.Fatal(err)
	}
	requireObjectStatus(t, racing.Close(), http.StatusForbidden)
	readObject(t, ctx, object, winner, attrs.Generation)
}
