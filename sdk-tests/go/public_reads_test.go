package gcsmimic_test

import (
	"bytes"
	"context"
	"crypto/rand"
	"encoding/xml"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"strconv"
	"sync"
	"testing"
	"time"

	"cloud.google.com/go/storage"
	"google.golang.org/api/googleapi"
	"google.golang.org/api/option"
)

type readRequestLog struct {
	base     http.RoundTripper
	mu       sync.Mutex
	requests []*http.Request
}

func (l *readRequestLog) RoundTrip(r *http.Request) (*http.Response, error) {
	l.mu.Lock()
	l.requests = append(l.requests, r.Clone(r.Context()))
	l.mu.Unlock()
	return l.base.RoundTrip(r)
}

func (l *readRequestLog) last(t *testing.T, bucket, object, method string, jsonReads bool) *http.Request {
	t.Helper()
	l.mu.Lock()
	defer l.mu.Unlock()
	if len(l.requests) == 0 {
		t.Fatal("SDK sent no read request")
	}
	r := l.requests[len(l.requests)-1]
	want := "/" + bucket + "/" + url.PathEscape(object)
	if jsonReads {
		want = "/storage/v1/b/" + bucket + "/o/" + url.PathEscape(object)
	}
	if r.Method != method || r.URL.EscapedPath() != want || (jsonReads && r.URL.Query().Get("alt") != "media") {
		t.Fatalf("read request: %s %s; want %s %s (JSON=%v)", r.Method, r.URL, method, want, jsonReads)
	}
	return r
}

func strictReadClient(t *testing.T, token string, jsonReads bool) (*storage.Client, *readRequestLog) {
	t.Helper()
	log := &readRequestLog{}
	_, client := newStrictClientWithReadMode(t, token, jsonReads, log)
	client.SetRetry(storage.WithPolicy(storage.RetryNever))
	t.Cleanup(func() {
		if t.Failed() {
			log.mu.Lock()
			defer log.mu.Unlock()
			for _, request := range log.requests {
				headers := request.Header.Clone()
				headers.Del("Authorization")
				t.Logf("read request: %s %s; headers=%v", request.Method, request.URL, headers)
			}
		}
	})
	return client, log
}

func requireReadBytes(t *testing.T, reader *storage.Reader, want []byte, attrs *storage.ObjectAttrs, cacheControl string) {
	t.Helper()
	got, readErr := io.ReadAll(io.LimitReader(reader, int64(len(want))+1))
	closeErr := reader.Close()
	if readErr != nil || closeErr != nil || !bytes.Equal(got, want) {
		t.Fatalf("download: bytes=%x, read=%v, close=%v; want %x", got, readErr, closeErr, want)
	}
	if reader.Attrs.Generation != attrs.Generation || reader.Attrs.Metageneration != attrs.Metageneration ||
		reader.Attrs.Size != attrs.Size || reader.Attrs.ContentType != attrs.ContentType || reader.Attrs.CacheControl != cacheControl {
		t.Fatalf("reader attrs=%+v; want generation=%d, metageneration=%d, size=%d, type=%q, cache=%q",
			reader.Attrs, attrs.Generation, attrs.Metageneration, attrs.Size, attrs.ContentType, cacheControl)
	}
}

func requireReadDenied(t *testing.T, ctx context.Context, object *storage.ObjectHandle, code int) error {
	t.Helper()
	reader, err := object.NewReader(ctx)
	if reader != nil {
		if closeErr := reader.Close(); closeErr != nil {
			t.Errorf("close unexpected reader: %v", closeErr)
		}
	}
	requireObjectStatus(t, err, code)
	return err
}

func TestXMLReadsStrict(t *testing.T) {
	ctx, bucket, attrs, payload := strictFixture(t, "xml")
	client, log := strictReadClient(t, strictAdminToken, false)
	object := client.Bucket(bucket.BucketName()).Object(attrs.Name)
	for _, tc := range []struct {
		name           string
		object         *storage.ObjectHandle
		offset, length int64
		want           []byte
		method         string
	}{
		{"whole", object, 0, -1, payload, http.MethodGet},
		{"range", object, 2, 5, payload[2:7], http.MethodGet},
		{"generation", object.Generation(attrs.Generation), 0, -1, payload, http.MethodGet},
		{"matching conditions", object.If(storage.Conditions{GenerationMatch: attrs.Generation, MetagenerationMatch: attrs.Metageneration}), 0, -1, payload, http.MethodGet},
		{"head", object, 0, 0, nil, http.MethodHead},
	} {
		t.Run(tc.name, func(t *testing.T) {
			reader, err := tc.object.NewRangeReader(ctx, tc.offset, tc.length)
			if err != nil {
				t.Fatal(err)
			}
			requireReadBytes(t, reader, tc.want, attrs, "private, max-age=0")
			r := log.last(t, bucket.BucketName(), attrs.Name, tc.method, false)
			switch tc.name {
			case "range":
				if r.Header.Get("Range") != "bytes=2-6" || reader.Attrs.StartOffset != 2 {
					t.Fatalf("range request=%v, attrs=%+v", r.Header, reader.Attrs)
				}
			case "generation":
				if r.URL.Query().Get("generation") != strconv.FormatInt(attrs.Generation, 10) {
					t.Fatalf("generation query missing: %s", r.URL)
				}
			case "matching conditions":
				if r.Header.Get("X-Goog-If-Generation-Match") != strconv.FormatInt(attrs.Generation, 10) ||
					r.Header.Get("X-Goog-If-Metageneration-Match") != strconv.FormatInt(attrs.Metageneration, 10) {
					t.Fatalf("XML conditions missing: %v", r.Header)
				}
			}
		})
	}
	for _, condition := range []storage.Conditions{
		{GenerationMatch: attrs.Generation + 1}, {MetagenerationMatch: attrs.Metageneration + 1},
	} {
		requireReadDenied(t, ctx, object.If(condition), http.StatusPreconditionFailed)
		log.last(t, bucket.BucketName(), attrs.Name, http.MethodGet, false)
	}
}

func TestPublicReadsGrantAndRevoke(t *testing.T) {
	if os.Getenv("GCS_MIMIC_STRICT_ENDPOINT") == "" {
		t.Skip("GCS_MIMIC_STRICT_ENDPOINT is unset; public reads require an explicit strict emulator")
	}
	for _, jsonReads := range []bool{false, true} {
		t.Run(fmt.Sprintf("JSON=%v", jsonReads), func(t *testing.T) {
			ctx, bucket, attrs, payload := strictFixture(t, "public")
			anonymous, log := strictReadClient(t, "", jsonReads)
			object := anonymous.Bucket(bucket.BucketName()).Object(attrs.Name)
			requireReadDenied(t, ctx, object, http.StatusUnauthorized)
			log.last(t, bucket.BucketName(), attrs.Name, http.MethodGet, jsonReads)
			policy, err := bucket.IAM().Policy(ctx)
			if err != nil {
				t.Fatal(err)
			}
			policy.Add("allUsers", "roles/storage.objectViewer")
			if err := bucket.IAM().SetPolicy(ctx, policy); err != nil {
				t.Fatal(err)
			}
			reader, err := object.NewReader(ctx)
			if err != nil {
				t.Fatal(err)
			}
			requireReadBytes(t, reader, payload, attrs, "public, max-age=3600")
			log.last(t, bucket.BucketName(), attrs.Name, http.MethodGet, jsonReads)
			policy, err = bucket.IAM().Policy(ctx)
			if err != nil {
				t.Fatal(err)
			}
			policy.Remove("allUsers", "roles/storage.objectViewer")
			if err := bucket.IAM().SetPolicy(ctx, policy); err != nil {
				t.Fatal(err)
			}
			requireReadDenied(t, ctx, object, http.StatusUnauthorized)
			log.last(t, bucket.BucketName(), attrs.Name, http.MethodGet, jsonReads)
		})
	}
}

func TestPublicReadsAllAuthenticatedUsers(t *testing.T) {
	ctx, bucket, attrs, payload := strictFixture(t, "authenticated")
	policy, err := bucket.IAM().Policy(ctx)
	if err != nil {
		t.Fatal(err)
	}
	policy.Add("allAuthenticatedUsers", "roles/storage.objectViewer")
	if err := bucket.IAM().SetPolicy(ctx, policy); err != nil {
		t.Fatal(err)
	}
	for _, jsonReads := range []bool{false, true} {
		t.Run(fmt.Sprintf("JSON=%v", jsonReads), func(t *testing.T) {
			for _, tc := range []struct {
				name, token string
				code        int
			}{
				{"registered without project roles", "local-bucket-reader-token", http.StatusOK},
				{"anonymous", "", http.StatusUnauthorized},
				{"unregistered", "unregistered-public-reader-token", http.StatusUnauthorized},
			} {
				t.Run(tc.name, func(t *testing.T) {
					client, log := strictReadClient(t, tc.token, jsonReads)
					object := client.Bucket(bucket.BucketName()).Object(attrs.Name)
					if tc.code == http.StatusOK {
						reader, err := object.NewReader(ctx)
						if err != nil {
							t.Fatal(err)
						}
						cacheControl := "private, max-age=0"
						if jsonReads {
							cacheControl = "no-cache"
						}
						requireReadBytes(t, reader, payload, attrs, cacheControl)
					} else {
						code := tc.code
						if tc.name == "unregistered" && !jsonReads {
							code = http.StatusBadRequest
						}
						err := requireReadDenied(t, ctx, object, code)
						if tc.name == "unregistered" {
							if jsonReads {
								requireStrictError(t, err, code, "authError")
							} else {
								var apiError *googleapi.Error
								var body struct{ Code string }
								if !errors.As(err, &apiError) || xml.Unmarshal([]byte(apiError.Body), &body) != nil || body.Code != "InvalidAuthentication" {
									t.Fatalf("want XML InvalidAuthentication, got %v", err)
								}
							}
						}
					}
					log.last(t, bucket.BucketName(), attrs.Name, http.MethodGet, jsonReads)
				})
			}
		})
	}
}

func TestXMLReadsEncodedNames(t *testing.T) {
	ctx, bucket, _, _ := strictFixture(t, "xml-names")
	client, log := strictReadClient(t, strictAdminToken, false)
	for _, name := range []string{"space name.bin", "plus+name.bin", "percent%2Fname.bin", "日本語/雪.bin", "dir/file.bin"} {
		t.Run(name, func(t *testing.T) {
			payload := []byte("encoded XML object\x00\xff:" + name)
			attrs, err := strictUpload(ctx, bucket.Object(name), payload)
			if err != nil {
				t.Fatal(err)
			}
			reader, err := client.Bucket(bucket.BucketName()).Object(name).NewReader(ctx)
			if err != nil {
				t.Fatal(err)
			}
			requireReadBytes(t, reader, payload, attrs, "private, max-age=0")
			log.last(t, bucket.BucketName(), name, http.MethodGet, false)
		})
	}
}

func TestXMLReadsPermissive(t *testing.T) {
	ctx, admin, httpClient, endpoint := newEmulatorClient(t)
	var nonce [8]byte
	if _, err := rand.Read(nonce[:]); err != nil {
		t.Fatal(err)
	}
	bucket := admin.Bucket(fmt.Sprintf("sdk-go-xml-%x", nonce))
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
	object := bucket.Object("dir/permissive.bin")
	payload := []byte("permissive XML\x00\xff")
	attrs, err := strictUpload(ctx, object, payload)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		cleanupCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		if err := object.Delete(cleanupCtx); err != nil {
			t.Errorf("cleanup object: %v", err)
		}
	})
	log := &readRequestLog{base: httpClient.Transport}
	httpClient.Transport = log
	client, err := storage.NewClient(ctx, storage.WithDisabledClientMetrics(), option.WithoutAuthentication(),
		option.WithEndpoint(endpoint.String()+"/storage/v1/"), option.WithHTTPClient(httpClient))
	if err != nil {
		t.Fatal(err)
	}
	client.SetRetry(storage.WithPolicy(storage.RetryNever))
	t.Cleanup(func() {
		if err := client.Close(); err != nil {
			t.Errorf("close client: %v", err)
		}
	})
	reader, err := client.Bucket(bucket.BucketName()).Object(attrs.Name).NewReader(ctx)
	if err != nil {
		t.Fatal(err)
	}
	requireReadBytes(t, reader, payload, attrs, "private, max-age=0")
	log.last(t, bucket.BucketName(), attrs.Name, http.MethodGet, false)
}
