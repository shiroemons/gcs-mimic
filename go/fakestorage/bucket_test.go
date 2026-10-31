package fakestorage

import (
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"testing"
)

func TestCreateBucketHelpersForwardAllSettings(t *testing.T) {
	for _, explicit := range []bool{false, true} {
		t.Run(map[bool]string{false: "defaults", true: "options"}[explicit], func(t *testing.T) {
			bodies := make(chan map[string]any, 1)
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				if r.Method != http.MethodPost || r.URL.Path != "/storage/v1/b" || r.URL.Query().Get("project") != "local-project" {
					t.Errorf("unexpected bucket request: %s %s", r.Method, r.URL)
				}
				var body map[string]any
				if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
					t.Error(err)
				}
				bodies <- body
				w.Header().Set("Content-Type", "application/json")
				_, _ = io.WriteString(w, `{"name":"fixture-bucket"}`)
			}))
			defer server.Close()
			endpoint, _ := url.Parse(server.URL)
			facade := &Server{endpoint: endpoint, client: server.Client()}
			if explicit {
				facade.CreateBucketWithOpts(CreateBucketOpts{Name: "fixture-bucket", VersioningEnabled: true, DefaultEventBasedHold: true})
			} else {
				facade.CreateBucket("fixture-bucket")
			}
			body := <-bodies
			versioning, _ := body["versioning"].(map[string]any)
			if body["name"] != "fixture-bucket" {
				t.Fatalf("bucket name lost: %#v", body)
			}
			if explicit && (body["defaultEventBasedHold"] != true || versioning["enabled"] != true) {
				t.Fatalf("bucket options lost: %#v", body)
			}
			if !explicit && (body["defaultEventBasedHold"] == true || versioning["enabled"] == true) {
				t.Fatalf("default bucket was protected/versioned: %#v", body)
			}
		})
	}
}

func TestCreateBucketDuplicatePreservesHoldAndRejectsVersioningChanges(t *testing.T) {
	for _, versioning := range []bool{false, true} {
		t.Run(map[bool]string{false: "matching", true: "mismatch"}[versioning], func(t *testing.T) {
			methods := make(chan string, 3)
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				methods <- r.Method
				w.Header().Set("Content-Type", "application/json")
				if r.Method == http.MethodPost {
					w.WriteHeader(409)
					_, _ = io.WriteString(w, `{"error":{"code":409,"message":"exists"}}`)
					return
				}
				_, _ = io.WriteString(w, `{"name":"fixture-bucket","versioning":{"enabled":false},"defaultEventBasedHold":true}`)
			}))
			defer server.Close()
			endpoint, _ := url.Parse(server.URL)
			facade := &Server{endpoint: endpoint, client: server.Client()}
			err := facade.createBucket(CreateBucketOpts{Name: "fixture-bucket", VersioningEnabled: versioning})
			if (err != nil) != versioning {
				t.Fatalf("duplicate policy: %v", err)
			}
			if got := strings.Join([]string{<-methods, <-methods}, ","); got != "POST,GET" || len(methods) != 0 {
				t.Fatalf("duplicate helper mutated settings: %v", got)
			}
		})
	}
}

func TestCreateBucketFailuresRetainTheSourcePanicContract(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(403)
		_, _ = io.WriteString(w, `{"error":{"code":403,"message":"denied"}}`)
	}))
	defer server.Close()
	endpoint, _ := url.Parse(server.URL)
	facade := &Server{endpoint: endpoint, client: server.Client()}
	defer func() {
		if recover() == nil {
			t.Error("source-compatible helper did not panic on backend failure")
		}
	}()
	facade.CreateBucket("denied")
}
