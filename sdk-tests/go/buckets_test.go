package gcsmimic_test

import (
	"context"
	"crypto/rand"
	"errors"
	"fmt"
	"net"
	"net/http"
	"net/url"
	"os"
	"slices"
	"strings"
	"testing"
	"time"

	"cloud.google.com/go/storage"
	"google.golang.org/api/googleapi"
	"google.golang.org/api/iterator"
	"google.golang.org/api/option"
)

func emulatorEndpoint(raw, allowedHosts string) (*url.URL, error) {
	u, err := url.Parse(raw)
	if err != nil {
		return nil, err
	}
	if (u.Scheme != "http" && u.Scheme != "https") || u.Host == "" || u.User != nil ||
		(u.Path != "" && u.Path != "/") || u.RawQuery != "" || u.Fragment != "" {
		return nil, fmt.Errorf("endpoint must be an http(s) origin without credentials, query or fragment")
	}
	host := u.Hostname()
	ip := net.ParseIP(host)
	if host != "localhost" && (ip == nil || !ip.IsLoopback()) && !slices.Contains(strings.Split(allowedHosts, ","), host) {
		return nil, fmt.Errorf("endpoint host %q is not loopback; explicitly allow an emulator with GCS_MIMIC_ALLOWED_HOSTS", host)
	}
	return u, nil
}

type emulatorTransport struct {
	endpoint *url.URL
	base     *http.Transport
}

func (t emulatorTransport) RoundTrip(r *http.Request) (*http.Response, error) {
	if r.URL.Scheme != t.endpoint.Scheme || r.URL.Host != t.endpoint.Host {
		return nil, fmt.Errorf("refusing request outside emulator origin: %s://%s", r.URL.Scheme, r.URL.Host)
	}
	return t.base.RoundTrip(r)
}

func TestEmulatorEndpoint(t *testing.T) {
	for _, tc := range []struct {
		endpoint, allow string
		valid           bool
	}{
		{"http://127.0.0.1:4443", "", true},
		{"http://[::1]:4443/", "", true},
		{"https://localhost:4443", "", true},
		{"http://gcs:4443", "gcs,emulator", true},
		{"http://gcs:4443", "", false},
		{"https://storage.googleapis.com", "", false},
		{"http://169.254.169.254", "", false},
		{"http://127.0.0.1.evil.invalid", "", false},
		{"http://user@localhost", "", false},
		{"http://localhost/storage/v1", "", false},
		{"http://localhost?target=cloud", "", false},
		{"localhost:4443", "", false},
	} {
		t.Run(tc.endpoint, func(t *testing.T) {
			_, err := emulatorEndpoint(tc.endpoint, tc.allow)
			if (err == nil) != tc.valid {
				t.Fatalf("valid=%v, error=%v", tc.valid, err)
			}
		})
	}
	u, _ := url.Parse("http://127.0.0.1:4443")
	r, _ := http.NewRequest(http.MethodGet, "https://storage.googleapis.com/storage/v1/b", nil)
	if _, err := (emulatorTransport{endpoint: u}).RoundTrip(r); err == nil {
		t.Fatal("transport allowed an outbound cloud request")
	}
}

func TestBucketLifecycle(t *testing.T) {
	raw := os.Getenv("GCS_MIMIC_ENDPOINT")
	if raw == "" {
		t.Skip("GCS_MIMIC_ENDPOINT is unset; bucket integration requires an explicit local emulator")
	}
	u, err := emulatorEndpoint(raw, os.Getenv("GCS_MIMIC_ALLOWED_HOSTS"))
	if err != nil {
		t.Fatal(err)
	}
	mod, err := os.ReadFile("go.mod")
	if err != nil {
		t.Fatal(err)
	}
	for _, line := range strings.Split(string(mod), "\n") {
		if fields := strings.Fields(line); len(fields) == 2 && fields[0] == "cloud.google.com/go/storage" {
			t.Logf("official Storage SDK %s; HTTP JSON transport; endpoint=%s", fields[1], u)
		}
	}
	t.Setenv("STORAGE_EMULATOR_HOST", u.String())
	transport := &http.Transport{
		Proxy:                 nil,
		MaxConnsPerHost:       2,
		MaxIdleConns:          2,
		IdleConnTimeout:       5 * time.Second,
		ResponseHeaderTimeout: 5 * time.Second,
		DialContext: func(ctx context.Context, network, address string) (net.Conn, error) {
			host, port, err := net.SplitHostPort(address)
			if err != nil {
				return nil, err
			}
			// Resolve localhost ourselves so hosts-file changes cannot send tests off-machine.
			if host == "localhost" {
				address = net.JoinHostPort("127.0.0.1", port)
			}
			return (&net.Dialer{Timeout: 5 * time.Second}).DialContext(ctx, network, address)
		},
	}
	defer transport.CloseIdleConnections()
	httpClient := &http.Client{
		Transport:     emulatorTransport{endpoint: u, base: transport},
		Timeout:       10 * time.Second,
		CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse },
	}
	ctx, cancel := context.WithTimeout(context.Background(), 45*time.Second)
	defer cancel()
	client, err := storage.NewClient(ctx, storage.WithJSONReads(), storage.WithDisabledClientMetrics(),
		option.WithoutAuthentication(), option.WithHTTPClient(httpClient))
	if err != nil {
		t.Fatal(err)
	}
	defer client.Close()
	var nonce [8]byte
	if _, err := rand.Read(nonce[:]); err != nil {
		t.Fatal(err)
	}
	prefix := fmt.Sprintf("sdk-go-%x-", nonce)
	project := fmt.Sprintf("sdk-project-%x", nonce)
	names := []string{prefix + "a0", prefix + "a1", prefix + "a2", prefix + "a3", prefix + "b0"}
	created := make(map[string]bool)
	defer func() {
		cleanupCtx, cleanupCancel := context.WithTimeout(context.Background(), 15*time.Second)
		defer cleanupCancel()
		for name := range created {
			if err := client.Bucket(name).Delete(cleanupCtx); err != nil {
				t.Errorf("cleanup %s: %v", name, err)
			}
		}
	}()
	for _, name := range names {
		bucket := client.Bucket(name)
		if err := bucket.Create(ctx, project, nil); err != nil {
			t.Fatalf("create %s: %v", name, err)
		}
		created[name] = true
		attrs, err := bucket.Attrs(ctx)
		if err != nil {
			t.Fatalf("get %s: %v", name, err)
		}
		if attrs.Name != name || attrs.MetaGeneration != 1 || attrs.Created.IsZero() {
			t.Fatalf("unexpected bucket identity: %+v", attrs)
		}
	}
	var apiError *googleapi.Error
	if err := client.Bucket(names[0]).Create(ctx, project, nil); !errors.As(err, &apiError) || apiError.Code != http.StatusConflict {
		t.Fatalf("duplicate create: want googleapi.Error 409, got %v", err)
	}
	it := client.Buckets(ctx, project)
	it.PageInfo().MaxSize = 2
	var got []string
	for len(got) <= len(names) {
		attrs, err := it.Next()
		if err == iterator.Done {
			break
		}
		if err != nil {
			t.Fatal(err)
		}
		got = append(got, attrs.Name)
	}
	if !slices.Equal(got, names) {
		t.Fatalf("project list: got %v, want %v", got, names)
	}
	it = client.Buckets(ctx, project)
	it.Prefix = prefix + "a"
	pager := iterator.NewPager(it, 1, "")
	got = nil
	pages := 0
	for pages < 6 {
		var page []*storage.BucketAttrs
		token, err := pager.NextPage(&page)
		if err != nil {
			t.Fatal(err)
		}
		pages++
		if len(page) != 1 {
			t.Fatalf("page %d: got %d buckets, want 1", pages, len(page))
		}
		got = append(got, page[0].Name)
		if token == "" {
			break
		}
	}
	if pages != 4 || !slices.Equal(got, names[:4]) {
		t.Fatalf("prefix pages: got %v in %d pages, want %v in 4 pages", got, pages, names[:4])
	}
	for _, name := range names {
		bucket := client.Bucket(name)
		if err := bucket.Delete(ctx); err != nil {
			t.Fatalf("delete %s: %v", name, err)
		}
		delete(created, name)
		if _, err := bucket.Attrs(ctx); !errors.Is(err, storage.ErrBucketNotExist) {
			t.Fatalf("get after delete: want ErrBucketNotExist, got %v", err)
		}
	}
	apiError = nil
	if err := client.Bucket(prefix + "missing").Delete(ctx); !errors.As(err, &apiError) || apiError.Code != http.StatusNotFound {
		t.Fatalf("delete missing: want googleapi.Error 404, got %v", err)
	}
}
