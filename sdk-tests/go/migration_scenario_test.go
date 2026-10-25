package gcsmimic_test

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"hash/crc32"
	"io"
	"net/http"
	"net/url"
	"os"
	"strings"
	"testing"

	"cloud.google.com/go/storage"
)

func TestMigrationAppScenarioBucketList(t *testing.T) {
	ctx, _, httpClient, endpoint := newEmulatorClient(t)
	status := http.StatusOK
	if os.Getenv("GCS_MIMIC_COMPAT_PROFILE") != "legacy-emulator" {
		status = http.StatusBadRequest
	}
	_, body := migrationHTTP(t, ctx, httpClient, http.MethodGet, endpoint.String()+"/storage/v1/b", "", nil, status)
	if status == http.StatusBadRequest {
		if !strings.Contains(string(body), "?project=local-project") {
			t.Fatalf("missing project repair example: %s", body)
		}
		return
	}
	var listed struct{ Items []struct{ Name string } }
	if err := json.Unmarshal(body, &listed); err != nil {
		t.Fatal(err)
	}
	found := map[string]bool{}
	for _, bucket := range listed.Items {
		found[bucket.Name] = true
	}
	if !found["assets"] || !found["exports"] {
		t.Fatalf("omitted project must list seed project buckets: %s", body)
	}
}

func TestMigrationAppScenarioGo(t *testing.T) {
	ctx, _, httpClient, _ := newEmulatorClient(t)
	client, log := recordedResumableClient(t, ctx, httpClient, false)
	assets, exports := client.Bucket("assets"), client.Bucket("exports")
	for _, bucket := range []*storage.BucketHandle{assets, exports} {
		if _, err := bucket.Attrs(ctx); err != nil {
			t.Fatalf("buckets file must initialize %s: %v", bucket.BucketName(), err)
		}
	}
	for _, mode := range []string{"default", "single-request"} {
		t.Run(mode, func(t *testing.T) {
			source := resumableObject(t, assets, "uploads/"+mode+".bin")
			target := resumableObject(t, exports, "published/"+mode+".bin")
			writer := source.NewWriter(ctx)
			size := 37
			if mode == "default" {
				// Exceed the SDK default threshold without changing ChunkSize.
				size += writer.ChunkSize
			} else {
				writer.ChunkSize = 0
			}
			payload := bytes.Repeat([]byte("x"), size)
			writer.ContentType = "application/octet-stream"
			log.mu.Lock()
			before := len(log.requests)
			log.mu.Unlock()
			if _, err := writer.Write(payload); err != nil {
				t.Fatal(err)
			}
			if err := writer.Close(); err != nil {
				t.Fatal(err)
			}
			log.mu.Lock()
			chunks, location := len(log.requests)-before, log.location
			log.mu.Unlock()
			if (mode == "default" && (chunks == 0 || location == "")) || (mode == "single-request" && chunks != 0) {
				t.Fatalf("%s Writer: chunks=%d, location=%q", mode, chunks, location)
			}
			attrs, err := source.Attrs(ctx)
			if err != nil {
				t.Fatal(err)
			}
			checksum := crc32.Checksum(payload, crc32.MakeTable(crc32.Castagnoli))
			if attrs.Generation <= 0 || attrs.Generation != writer.Attrs().Generation || attrs.CRC32C != checksum || attrs.Size != int64(size) {
				t.Fatalf("upload Attrs: %+v; expected CRC32C=%08x, size=%d", attrs, checksum, size)
			}
			readObject(t, ctx, source, payload, attrs.Generation)
			copier := target.If(storage.Conditions{DoesNotExist: true}).CopierFrom(source.Generation(attrs.Generation))
			copied, err := copier.Run(ctx)
			if err != nil {
				t.Fatal(err)
			}
			if copied.Generation <= 0 || copied.CRC32C != checksum || copied.Size != attrs.Size {
				t.Fatalf("copy Attrs: %+v", copied)
			}
			_, err = copier.Run(ctx)
			requireObjectStatus(t, err, http.StatusPreconditionFailed)
			readObject(t, ctx, target, payload, copied.Generation)
			for _, object := range []*storage.ObjectHandle{source.Generation(attrs.Generation), target.Generation(copied.Generation)} {
				if err := object.Delete(ctx); err != nil {
					t.Fatal(err)
				}
				requireObjectMissing(t, ctx, object)
			}
		})
	}
}

func migrationHTTP(t *testing.T, ctx context.Context, client *http.Client, method, target, body string, headers http.Header, status int) (*http.Response, []byte) {
	t.Helper()
	request, err := http.NewRequestWithContext(ctx, method, target, strings.NewReader(body))
	if err != nil {
		t.Fatal(err)
	}
	request.Header = headers.Clone()
	request.Host = headers.Get("Host")
	response, err := client.Do(request)
	if err != nil {
		t.Fatal(err)
	}
	data, readErr := io.ReadAll(io.LimitReader(response.Body, 64<<10))
	closeErr := response.Body.Close()
	if readErr != nil || closeErr != nil || response.StatusCode != status {
		t.Fatalf("%s %s: status=%d, want=%d, read=%v, close=%v, body=%s", method, request.URL.Path, response.StatusCode, status, readErr, closeErr, data)
	}
	return response, data
}

func migrationSession(t *testing.T, ctx context.Context, client *http.Client, endpoint *url.URL, name string) string {
	t.Helper()
	target := endpoint.String() + "/upload/storage/v1/b/assets/o?uploadType=resumable&name=" + url.QueryEscape(name)
	response, _ := migrationHTTP(t, ctx, client, http.MethodPost, target, "{}", http.Header{
		"Content-Type": {"application/json"}, "X-Upload-Content-Type": {"text/plain"},
	}, http.StatusOK)
	location := response.Header.Get("Location")
	origin, err := url.Parse(location)
	if err != nil || origin.Scheme != endpoint.Scheme || origin.Host != endpoint.Host || origin.RawQuery == "" {
		t.Fatalf("session Location must stay on the emulator: %q, error=%v", location, err)
	}
	return location
}

func TestMigrationAppScenarioBrowser(t *testing.T) {
	ctx, client, httpClient, endpoint := newEmulatorClient(t)
	const origin = "http://localhost:3000"
	const payload = "browser migration bytes\n"
	for _, mode := range []string{"media", "resumable"} {
		t.Run(mode, func(t *testing.T) {
			name := "browser/" + mode + ".txt"
			object := resumableObject(t, client.Bucket("assets"), name)
			target := endpoint.String() + "/upload/storage/v1/b/assets/o?uploadType=media&name=" + url.QueryEscape(name)
			method := http.MethodPost
			headers := http.Header{"Origin": {origin}, "Content-Type": {"text/plain"}}
			requestedHeaders := "content-type"
			if mode == "resumable" {
				target = migrationSession(t, ctx, httpClient, endpoint, name)
				method = http.MethodPut
				headers.Del("Content-Type")
				headers.Set("Host", "localhost:"+endpoint.Port())
				headers.Set("Content-Range", fmt.Sprintf("bytes 0-%d/%d", len(payload)-1, len(payload)))
				requestedHeaders += ",content-range"
			}
			preflight := http.Header{
				"Origin": {origin}, "Access-Control-Request-Method": {method},
				"Access-Control-Request-Headers": {requestedHeaders},
			}
			if host := headers.Get("Host"); host != "" {
				preflight.Set("Host", host)
			}
			response, _ := migrationHTTP(t, ctx, httpClient, http.MethodOptions, target, "", preflight, http.StatusOK)
			if response.Header.Get("Access-Control-Allow-Origin") != origin || !strings.Contains(response.Header.Get("Access-Control-Allow-Methods"), method) {
				t.Fatalf("preflight headers: %v", response.Header)
			}
			for _, header := range strings.Split(requestedHeaders, ",") {
				if !strings.Contains(strings.ToLower(response.Header.Get("Access-Control-Allow-Headers")), header) {
					t.Fatalf("preflight did not allow %s: %v", header, response.Header)
				}
			}
			response, _ = migrationHTTP(t, ctx, httpClient, method, target, payload, headers, http.StatusOK)
			if response.Header.Get("Access-Control-Allow-Origin") != origin {
				t.Fatalf("upload CORS headers: %v", response.Header)
			}
			attrs, err := object.Attrs(ctx)
			if err != nil || attrs.ContentType != "text/plain" {
				t.Fatalf("session content type: attrs=%+v, error=%v", attrs, err)
			}
			for _, path := range []string{"/storage/v1/b/assets/o/" + url.PathEscape(name) + "?alt=media", "/assets/" + name} {
				response, data := migrationHTTP(t, ctx, httpClient, http.MethodGet, endpoint.String()+path, "", http.Header{"Origin": {origin}, "Host": {"localhost:" + endpoint.Port()}}, http.StatusOK)
				if string(data) != payload || (strings.HasPrefix(path, "/storage/") && response.Header.Get("Access-Control-Allow-Origin") != origin) {
					t.Fatalf("anonymous read %s: body=%q, headers=%v", path, data, response.Header)
				}
			}
		})
	}
}

func TestMigrationAppScenarioCancellation(t *testing.T) {
	ctx, client, httpClient, endpoint := newEmulatorClient(t)
	object := resumableObject(t, client.Bucket("assets"), "browser/cancelled.txt")
	location := migrationSession(t, ctx, httpClient, endpoint, object.ObjectName())
	migrationHTTP(t, ctx, httpClient, http.MethodDelete, location, "", nil, 499)
	migrationHTTP(t, ctx, httpClient, http.MethodPut, location, "", http.Header{"Content-Range": {"bytes */0"}}, http.StatusGone)
	requireObjectMissing(t, ctx, object)
}
