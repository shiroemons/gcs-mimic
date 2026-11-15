package main

import (
	"fmt"
	"net/http"
	"net/url"
	"slices"
	"sync"

	"cloud.google.com/go/storage"
	"google.golang.org/api/iterator"
	"google.golang.org/api/option"
)

// sentRequest is one request as the SDK handed it to the transport.
type sentRequest struct {
	method string
	url    url.URL
	header http.Header
}

// wireRecorder records each request before the emulator guard sends it, so a case can assert
// on the endpoint and headers the SDK chose rather than only on the bytes it returned.
type wireRecorder struct {
	base http.RoundTripper
	mu   sync.Mutex
	sent []sentRequest
}

func (w *wireRecorder) RoundTrip(r *http.Request) (*http.Response, error) {
	w.mu.Lock()
	w.sent = append(w.sent, sentRequest{r.Method, *r.URL, r.Header.Clone()})
	w.mu.Unlock()
	return w.base.RoundTrip(r)
}

func (w *wireRecorder) requests() []sentRequest {
	w.mu.Lock()
	defer w.mu.Unlock()
	return slices.Clone(w.sent)
}

// recordingClient builds an HTTP client configured like p.client, plus opts, whose requests
// are recorded.
func (p *probe) recordingClient(opts ...option.ClientOption) (*storage.Client, *wireRecorder, error) {
	rec := &wireRecorder{base: http.DefaultClient.Transport}
	hc := *http.DefaultClient
	hc.Transport = rec
	base := []option.ClientOption{option.WithoutAuthentication(), option.WithHTTPClient(&hc)}
	client, err := storage.NewClient(p.ctx, append(base, opts...)...)
	return client, rec, err
}

// sdkCases are the Go SDK's own idioms beyond the shared core.
func sdkCases() []testCase {
	return []testCase{
		{"sdk.json_reads", "sdk", func(p *probe) error {
			b, err := p.bucket(nil)
			if err != nil {
				return err
			}
			data := payload(4096)
			if _, err := write(p.ctx, b.Object("json.bin"), data, nil); err != nil {
				return err
			}
			client, rec, err := p.recordingClient(storage.WithJSONReads())
			if err != nil {
				return err
			}
			defer client.Close()
			o := client.Bucket(b.BucketName()).Object("json.bin")
			if err := expectBytesArgs(read(p.ctx, o))(data); err != nil {
				return fmt.Errorf("whole read: %w", err)
			}
			if err := expectBytesArgs(readRange(p.ctx, o, 10, 10))(data[10:20]); err != nil {
				return fmt.Errorf("range read: %w", err)
			}
			media := "/storage/v1/b/" + b.BucketName() + "/o/json.bin"
			sent := rec.requests()
			for _, r := range sent {
				if r.method != http.MethodGet || r.url.EscapedPath() != media || r.url.Query().Get("alt") != "media" {
					return fmt.Errorf("read sent %s %s, want GET %s?alt=media", r.method, r.url.RequestURI(), media)
				}
			}
			if len(sent) != 2 {
				return fmt.Errorf("sent %d media reads, want 2", len(sent))
			}
			if got := sent[1].header.Get("Range"); got != "bytes=10-19" {
				return fmt.Errorf("range read sent Range %q, want bytes=10-19", got)
			}
			return nil
		}},
		{"sdk.writer_progress", "sdk", func(p *probe) error {
			b, err := p.bucket(nil)
			if err != nil {
				return err
			}
			data := payload(1<<20 + 5)
			o := b.Object("progress.bin")
			// Close waits for the upload goroutine that calls ProgressFunc, so the
			// offsets are complete when write returns.
			var offsets []int64
			attrs, err := write(p.ctx, o, data, func(w *storage.Writer) {
				w.ChunkSize = 256 * 1024
				w.ProgressFunc = func(n int64) { offsets = append(offsets, n) }
			})
			if err != nil {
				return err
			}
			if len(offsets) < 2 {
				return fmt.Errorf("progress offsets %v, want one per chunk", offsets)
			}
			for i := 1; i < len(offsets); i++ {
				if offsets[i] <= offsets[i-1] {
					return fmt.Errorf("progress offsets %v do not increase", offsets)
				}
			}
			if last := offsets[len(offsets)-1]; last != int64(len(data)) || attrs.Size != int64(len(data)) {
				return fmt.Errorf("last progress offset %d and size %d, want %d", last, attrs.Size, len(data))
			}
			return expectBytesArgs(read(p.ctx, o))(data)
		}},
		{"sdk.pager_resume", "sdk", func(p *probe) error {
			b, err := p.bucket(nil)
			if err != nil {
				return err
			}
			all := []string{"p1", "p2", "p3", "p4", "p5"}
			if err := seedObjects(p.ctx, b, all...); err != nil {
				return err
			}
			var first []*storage.ObjectAttrs
			token, err := iterator.NewPager(b.Objects(p.ctx, nil), 2, "").NextPage(&first)
			if err != nil {
				return err
			}
			if len(first) != 2 || token == "" {
				return fmt.Errorf("first page has %d names and token %q, want 2 names and a token", len(first), token)
			}
			remaining := slices.Clone(all)
			for _, a := range first {
				i := slices.Index(remaining, a.Name)
				if i < 0 {
					return fmt.Errorf("first page name %q is not a seeded name or repeats", a.Name)
				}
				remaining = slices.Delete(remaining, i, i+1)
			}
			pager := iterator.NewPager(b.Objects(p.ctx, nil), 2, token)
			var rest []string
			for {
				var page []*storage.ObjectAttrs
				next, err := pager.NextPage(&page)
				if err != nil {
					return err
				}
				if len(page) > 2 {
					return fmt.Errorf("page size %d exceeds 2", len(page))
				}
				for _, a := range page {
					rest = append(rest, a.Name)
				}
				if next == "" {
					break
				}
			}
			return expectList(rest, remaining...)
		}},
	}
}
