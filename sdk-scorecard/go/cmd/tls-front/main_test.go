package main

import (
	"bytes"
	"compress/gzip"
	"io"
	"net/http"
	"net/http/httptest"
	"net/url"
	"testing"
)

func TestFrontendPreservesWireAndBoundsLocations(t *testing.T) {
	var compressed bytes.Buffer
	z := gzip.NewWriter(&compressed)
	_, _ = z.Write([]byte("compressed payload"))
	_ = z.Close()
	var location string
	backend := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Host != "127.0.0.1:4443" || r.URL.EscapedPath() != "/bucket/a%2Fb" || r.URL.RawQuery != "x=1&x=2" ||
			r.Header.Get("Content-Range") != "bytes 0-262143/524288" || r.Header.Get("X-Goog-Hash") != "crc32c=AAAAAA==" {
			t.Error("frontend changed request target or upload headers")
		}
		body, err := io.ReadAll(r.Body)
		if err != nil || !bytes.Equal(body, bytes.Repeat([]byte("a"), 262144)) {
			t.Error("upload bytes changed")
		}
		w.Header().Set("Location", location)
		w.Header().Set("Range", "bytes=0-262143")
		w.Header().Set("Content-Encoding", "gzip")
		w.WriteHeader(308)
		_, _ = w.Write(compressed.Bytes())
	}))
	defer backend.Close()
	target, _ := url.Parse(backend.URL)
	origin, _ := url.Parse("https://127.0.0.1:4443")
	proxy := httptest.NewTLSServer(frontend(target, origin))
	defer proxy.Close()
	client := proxy.Client()
	client.Transport.(*http.Transport).DisableCompression = true
	client.CheckRedirect = func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }
	for _, tc := range []struct {
		location, want string
		status         int
	}{
		{backend.URL + "/upload?upload_id=test", origin.String() + "/upload?upload_id=test", 308},
		{"http://" + origin.Host + "/upload?upload_id=test", origin.String() + "/upload?upload_id=test", 308},
		{origin.String() + "/upload?upload_id=test", origin.String() + "/upload?upload_id=test", 308},
		{"https://storage.googleapis.com/upload?upload_id=test", "", 502},
		{"//storage.googleapis.com/upload", "", 502},
	} {
		location = tc.location
		r, _ := http.NewRequest("PUT", proxy.URL+"/bucket/a%2Fb?x=1&x=2", bytes.NewReader(bytes.Repeat([]byte("a"), 262144)))
		r.Header.Set("Content-Range", "bytes 0-262143/524288")
		r.Header.Set("X-Goog-Hash", "crc32c=AAAAAA==")
		response, err := client.Do(r)
		if err != nil {
			t.Fatal(err)
		}
		body, _ := io.ReadAll(response.Body)
		response.Body.Close()
		if response.StatusCode != tc.status || response.Header.Get("Location") != tc.want {
			t.Errorf("unexpected status or Location: %d", response.StatusCode)
		}
		if tc.status == 308 && (!bytes.Equal(body, compressed.Bytes()) || response.Header.Get("Range") != "bytes=0-262143") {
			t.Error("308/range/gzip response changed")
		}
	}
}

func TestLoopbackConfiguration(t *testing.T) {
	for _, raw := range []string{"http://storage.googleapis.com:80", "http://127.0.0.1", "http://user@127.0.0.1:80", "http://127.0.0.1:80/path", "http://127.0.0.1:80?x=1", "https://127.0.0.1:80"} {
		if _, err := loopbackOrigin(raw, "http"); err == nil {
			t.Errorf("unsafe backend accepted: %s", raw)
		}
	}
}
