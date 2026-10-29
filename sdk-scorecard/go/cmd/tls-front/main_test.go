package main

import (
	"bufio"
	"bytes"
	"compress/gzip"
	"crypto/tls"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"net/url"
	"testing"
	"time"
)

func TestFrontendDeliversEarlyErrorAfterClientFinishesUpload(t *testing.T) {
	answered := make(chan struct{})
	backend := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path == "/after" {
			_, _ = w.Write([]byte("next request"))
			return
		}
		w.WriteHeader(http.StatusBadRequest)
		_, _ = w.Write([]byte("checksum rejected"))
		close(answered)
	}))
	defer backend.Close()
	target, _ := url.Parse(backend.URL)
	proxy := httptest.NewUnstartedServer(nil)
	origin, _ := url.Parse("https://" + proxy.Listener.Addr().String())
	proxy.Config.Handler = frontendHandler(target, origin)
	proxy.StartTLS()
	defer proxy.Close()
	trust := proxy.Client().Transport.(*http.Transport).TLSClientConfig
	conn, err := tls.Dial("tcp", origin.Host, trust)
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	if err := conn.SetDeadline(time.Now().Add(5 * time.Second)); err != nil {
		t.Fatal(err)
	}
	const length = 8*1024*1024 + 1
	if _, err := fmt.Fprintf(conn, "PUT /upload HTTP/1.1\r\nHost: %s\r\nContent-Length: %d\r\n\r\n", origin.Host, length); err != nil {
		t.Fatal(err)
	}
	<-answered
	// net/http otherwise closes an unread large body after its 500 ms reset delay.
	time.Sleep(750 * time.Millisecond)
	if _, err := io.CopyN(conn, bytes.NewReader(bytes.Repeat([]byte("a"), length)), length); err != nil {
		t.Fatal(err)
	}
	reader := bufio.NewReader(conn)
	response, err := http.ReadResponse(reader, &http.Request{Method: "PUT"})
	if err != nil {
		t.Fatal(err)
	}
	defer response.Body.Close()
	body, err := io.ReadAll(response.Body)
	if err != nil || response.StatusCode != 400 || string(body) != "checksum rejected" {
		t.Fatalf("lost early checksum response: status=%d body=%q error=%v", response.StatusCode, body, err)
	}
	if _, err := fmt.Fprintf(conn, "GET /after HTTP/1.1\r\nHost: %s\r\nConnection: close\r\n\r\n", origin.Host); err != nil {
		t.Fatal(err)
	}
	next, err := http.ReadResponse(reader, &http.Request{Method: "GET"})
	if err != nil {
		t.Fatal(err)
	}
	defer next.Body.Close()
	body, err = io.ReadAll(next.Body)
	if err != nil || next.StatusCode != 200 || string(body) != "next request" {
		t.Fatalf("upload drain desynchronized keep-alive: status=%d body=%q error=%v", next.StatusCode, body, err)
	}
}

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
