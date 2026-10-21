package gcsmimic_test

import (
	"bytes"
	"context"
	"crypto/rand"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"slices"
	"testing"
	"time"

	"cloud.google.com/go/storage"
	"google.golang.org/api/iterator"
)

func TestObjectList(t *testing.T) {
	ctx, client, httpClient, endpoint := newEmulatorClient(t)
	var nonce [8]byte
	if _, err := rand.Read(nonce[:]); err != nil {
		t.Fatal(err)
	}
	bucket := client.Bucket(fmt.Sprintf("sdk-go-list-%x", nonce))
	if err := bucket.Create(ctx, "local-list", nil); err != nil {
		t.Fatal(err)
	}
	names := []string{"a", "b", "dir/", "dir/a", "dir/sub/", "dir/sub/file", "x::", "x::one", "z", "雪/a", "雪/b"}
	t.Cleanup(func() {
		cleanup, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		for _, name := range names {
			if err := bucket.Object(name).Delete(cleanup); err != nil && !errors.Is(err, storage.ErrObjectNotExist) {
				t.Errorf("cleanup object %q: %v", name, err)
			}
		}
		if err := bucket.Delete(cleanup); err != nil {
			t.Errorf("cleanup bucket: %v", err)
		}
	})
	for _, name := range names {
		upload := *endpoint
		upload.Path = "/upload/storage/v1/b/" + bucket.BucketName() + "/o"
		upload.RawQuery = url.Values{"uploadType": {"media"}, "name": {name}}.Encode()
		request, err := http.NewRequestWithContext(ctx, http.MethodPost, upload.String(), bytes.NewBufferString("body"))
		if err != nil {
			t.Fatal(err)
		}
		request.Header.Set("Content-Type", "text/plain")
		response, err := httpClient.Do(request)
		if err != nil {
			t.Fatal(err)
		}
		body, readErr := io.ReadAll(io.LimitReader(response.Body, 64<<10))
		closeErr := response.Body.Close()
		if response.StatusCode != http.StatusOK || readErr != nil || closeErr != nil {
			t.Fatalf("upload %q: status=%d read=%v close=%v body=%s", name, response.StatusCode, readErr, closeErr, body)
		}
	}
	t.Run("IteratorSmallPages", func(t *testing.T) {
		it := bucket.Objects(ctx, nil)
		it.PageInfo().MaxSize = 1
		var got []string
		for {
			attrs, err := it.Next()
			if err == iterator.Done {
				break
			}
			if err != nil {
				t.Fatal(err)
			}
			if attrs.Bucket != bucket.BucketName() || attrs.Size != 4 || attrs.ContentType != "text/plain" || attrs.Generation <= 0 || attrs.Metageneration != 1 || len(attrs.MD5) != 16 || attrs.CRC32C == 0 {
				t.Fatalf("list metadata: %+v", attrs)
			}
			got = append(got, attrs.Name)
		}
		if !slices.Equal(got, names) {
			t.Fatalf("names=%q want=%q", got, names)
		}
	})
	for _, test := range []struct {
		name            string
		query           storage.Query
		items, prefixes []string
	}{
		{"Prefix", storage.Query{Prefix: "dir/"}, []string{"dir/", "dir/a", "dir/sub/", "dir/sub/file"}, nil},
		{"Delimiter", storage.Query{Delimiter: "/"}, []string{"a", "b", "x::", "x::one", "z"}, []string{"dir/", "雪/"}},
		{"TrailingDelimiter", storage.Query{Delimiter: "/", IncludeTrailingDelimiter: true}, []string{"a", "b", "dir/", "x::", "x::one", "z"}, []string{"dir/", "雪/"}},
		{"PrefixOffsetsDelimiter", storage.Query{Prefix: "dir/", Delimiter: "/", StartOffset: "dir/a", EndOffset: "dir/sub0", IncludeTrailingDelimiter: true}, []string{"dir/a", "dir/sub/"}, []string{"dir/sub/"}},
		{"Offsets", storage.Query{StartOffset: "b", EndOffset: "dir/sub/"}, []string{"b", "dir/", "dir/a"}, nil},
		{"ReverseOffsets", storage.Query{StartOffset: "z", EndOffset: "a"}, nil, nil},
		{"Multibyte", storage.Query{Prefix: "雪/", StartOffset: "雪/a", EndOffset: "雪/b"}, []string{"雪/a"}, nil},
		{"LongDelimiter", storage.Query{Prefix: "x", Delimiter: "::", IncludeTrailingDelimiter: true}, []string{"x::"}, []string{"x::"}},
	} {
		t.Run(test.name, func(t *testing.T) {
			pager := iterator.NewPager(bucket.Objects(ctx, &test.query), 1, "")
			var items, prefixes []string
			for {
				var page []*storage.ObjectAttrs
				next, err := pager.NextPage(&page)
				if err != nil {
					t.Fatal(err)
				}
				if len(page) > 1 {
					t.Fatalf("page exceeds combined maxResults=1: %+v", page)
				}
				for _, attrs := range page {
					if attrs.Prefix != "" {
						prefixes = append(prefixes, attrs.Prefix)
					} else {
						items = append(items, attrs.Name)
					}
				}
				if next == "" {
					break
				}
			}
			if !slices.Equal(items, test.items) || !slices.Equal(prefixes, test.prefixes) {
				t.Fatalf("items=%q prefixes=%q; want items=%q prefixes=%q", items, prefixes, test.items, test.prefixes)
			}
		})
	}
}
