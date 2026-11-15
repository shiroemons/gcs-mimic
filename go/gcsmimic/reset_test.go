package gcsmimic

import (
	"context"
	"net/http"
	"strings"
	"sync"
	"testing"
)

func TestResetAsksForTheStartupStateAndWaitsOutABusyServer(t *testing.T) {
	var mu sync.Mutex
	var requests []recorded
	s := newTestServer(t, func(w http.ResponseWriter, r *http.Request) {
		request := record(t, r)
		mu.Lock()
		requests = append(requests, request)
		first := len(requests) == 1
		mu.Unlock()
		if first {
			w.Header().Set("Retry-After", "0")
			writeJSON(w, http.StatusServiceUnavailable, apiError(http.StatusServiceUnavailable, "backendError"))
			return
		}
		writeJSON(w, http.StatusOK, `{"buckets":3,"objects":7,"restored":{"buckets":1,"objects":2}}`)
	})
	s.Reset(t)
	mu.Lock()
	defer mu.Unlock()
	if len(requests) != 2 {
		t.Fatalf("sent %d requests, want a retry after the 503", len(requests))
	}
	for _, request := range requests {
		if request.method != http.MethodPost || request.path != "/_internal/reset" || request.query != "" ||
			request.contentType != "application/json" || string(request.body) != `{"confirm":"gcs-mimic","state":"startup"}` {
			t.Fatalf("request %+v", request)
		}
	}
}

func TestAFailedResetSaysThatNothingChanged(t *testing.T) {
	for _, tc := range []struct {
		name   string
		status int
		reason string
		want   string
	}{
		{"busy", http.StatusServiceUnavailable, "backendError", "stayed busy for 5 attempts"},
		{"startup state unavailable", http.StatusBadRequest, "invalid", "could not be applied again"},
		{"request refused", http.StatusBadRequest, "invalidArgument", "may predate"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			s := newTestServer(t, func(w http.ResponseWriter, r *http.Request) {
				w.Header().Set("Retry-After", "0")
				writeJSON(w, tc.status, apiError(tc.status, tc.reason))
			})
			err := s.reset(context.Background())
			if err == nil || !strings.Contains(err.Error(), "made no changes") || !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("reset: %v", err)
			}
		})
	}
}
