package gcsmimic

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"strings"
	"testing"

	"google.golang.org/api/googleapi"
)

// resetToStartup asks /_internal/reset to apply the startup buckets and fixtures again after
// clearing; without "state" it would leave the instance empty.
const resetToStartup = `{"confirm":"gcs-mimic","state":"startup"}`

// resetAnswer is what the reset removed and what it published again. The helper reads it only
// to check the answer's form; the counts depend on the startup configuration.
type resetAnswer struct {
	Buckets  int `json:"buckets"`
	Objects  int `json:"objects"`
	Restored struct {
		Buckets int `json:"buckets"`
		Objects int `json:"objects"`
	} `json:"restored"`
}

// Reset returns the server to its state at startup. It deletes every bucket and object,
// including TempBucket buckets and the data of tests running in parallel, then applies the
// startup buckets (WithBuckets or WithBucketsDocument) and fixtures (WithSeedDir) again, in
// one step, so a startup bucket never goes missing. Reset ignores holds and retention. It fails
// tb if the reset fails; a failed reset changes nothing.
func (s *Server) Reset(tb testing.TB) {
	tb.Helper()
	if err := s.reset(tb.Context()); err != nil {
		tb.Fatalf("gcsmimic: reset: %v", err)
	}
}

func (s *Server) reset(ctx context.Context) error {
	err := retryBusy(ctx, func() error {
		var answer resetAnswer
		return s.admin(ctx, http.MethodPost, "/_internal/reset", strings.NewReader(resetToStartup),
			"application/json", &answer)
	})
	switch apiStatus(err) {
	case http.StatusServiceUnavailable:
		return fmt.Errorf("the server made no changes; close open uploads and retry: %w", err)
	case http.StatusBadRequest:
		return fmt.Errorf("the server made no changes; %s: %w", refusedResetCause(err), err)
	default:
		return err
	}
}

// refusedResetCause explains a 400. gcs-mimic answers "invalid" when the startup fixtures,
// which it reads again from WithSeedDir, can no longer be published; any other reason means
// it did not accept the request, as a release without the startup state does not.
func refusedResetCause(err error) string {
	var api *googleapi.Error
	if errors.As(err, &api) && len(api.Errors) != 0 && api.Errors[0].Reason == "invalid" {
		return "the startup state could not be applied again, for example because the WithSeedDir tree changed"
	}
	return "it refused the request; this gcs-mimic may predate resetting to the startup state"
}
