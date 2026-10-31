// Package sourceexport captures an application-quiesced fake-gcs-server v1.56.1
// memory backend. It is a migration tool, separate from the Rust runtime facade.
package sourceexport

import (
	"net/http"
	"sync"
)

// Handler holds the application barrier for the entire request. All HTTP access,
// including source.HTTPClient(), must use this barrier before an Export.
func Handler(barrier *sync.RWMutex, next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		barrier.RLock()
		defer barrier.RUnlock()
		next.ServeHTTP(w, r)
	})
}

// Transport gates the source's in-process HTTP transport. Direct API and backend
// mutations must independently hold the same barrier's RLock until completion.
func Transport(barrier *sync.RWMutex, next http.RoundTripper) http.RoundTripper {
	return &gatedTransport{barrier, next}
}

type gatedTransport struct {
	barrier *sync.RWMutex
	next    http.RoundTripper
}

func (t *gatedTransport) RoundTrip(r *http.Request) (*http.Response, error) {
	t.barrier.RLock()
	defer t.barrier.RUnlock()
	return t.next.RoundTrip(r)
}
