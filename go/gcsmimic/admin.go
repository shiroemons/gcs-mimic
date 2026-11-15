package gcsmimic

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strconv"
	"strings"
	"time"

	"google.golang.org/api/googleapi"
)

// maxAnswerBytes bounds a decoded administration answer; the helpers read only small JSON.
const maxAnswerBytes = 64 << 10

// admin sends one request to a local administration route and decodes the JSON answer into
// answer, or discards it when answer is nil. A failure status returns a *googleapi.Error that
// keeps the response headers, so that a caller can read the status and Retry-After.
func (s *Server) admin(ctx context.Context, method, path string, body io.Reader, contentType string, answer any) error {
	request, err := http.NewRequestWithContext(ctx, method, s.URL()+path, body)
	if err != nil {
		return err
	}
	if contentType != "" {
		request.Header.Set("Content-Type", contentType)
	}
	response, err := s.client.Do(request)
	if err != nil {
		return err
	}
	err = googleapi.CheckResponse(response)
	if err == nil && answer != nil {
		if decodeErr := json.NewDecoder(io.LimitReader(response.Body, maxAnswerBytes)).Decode(answer); decodeErr != nil {
			err = fmt.Errorf("decode the answer of %s %s: %w", method, path, decodeErr)
		}
	}
	_, drainErr := io.Copy(io.Discard, io.LimitReader(response.Body, maxAnswerBytes))
	return errors.Join(err, drainErr, response.Body.Close())
}

// apiStatus is the HTTP status of a failed request, or 0 when it failed without one.
func apiStatus(err error) int {
	var api *googleapi.Error
	if errors.As(err, &api) {
		return api.Code
	}
	return 0
}

const (
	busyAttempts  = 5
	busyBudget    = 10 * time.Second // the longest the retries may wait in total
	maxRetryDelay = 2 * time.Second
)

// retryBusy repeats request while it answers 503, which gcs-mimic's administration routes
// return without making changes when other work holds what they need. It stops after
// busyAttempts requests or busyBudget of waiting and returns the last 503, so that a stuck
// upload fails the test instead of hanging it.
func retryBusy(ctx context.Context, request func() error) error {
	var waited time.Duration
	for attempt := 1; ; attempt++ {
		err := request()
		if apiStatus(err) != http.StatusServiceUnavailable {
			return err
		}
		delay := retryDelay(err)
		if attempt == busyAttempts || waited+delay > busyBudget {
			return fmt.Errorf("the server stayed busy for %d attempts over %v: %w", attempt, waited, err)
		}
		if sleepErr := sleep(ctx, delay); sleepErr != nil {
			return errors.Join(sleepErr, fmt.Errorf("the server was busy: %w", err))
		}
		waited += delay
	}
}

// retryDelay reads the Retry-After seconds of a 503, within a bound so that a test stays
// short; without a usable value it waits one second, as gcs-mimic asks.
func retryDelay(err error) time.Duration {
	var api *googleapi.Error
	if !errors.As(err, &api) || api.Header == nil {
		return time.Second
	}
	seconds, parseErr := strconv.Atoi(strings.TrimSpace(api.Header.Get("Retry-After")))
	if parseErr != nil || seconds < 0 {
		return time.Second
	}
	return min(time.Duration(seconds)*time.Second, maxRetryDelay)
}

func sleep(ctx context.Context, delay time.Duration) error {
	timer := time.NewTimer(delay)
	defer timer.Stop()
	select {
	case <-timer.C:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}
