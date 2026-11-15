package gcsmimic

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"errors"
	"fmt"
	"net/http"
	"net/url"
	"strconv"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"cloud.google.com/go/storage"
)

// BucketOption configures a bucket that TempBucket creates.
type BucketOption func(*bucketConfig)

type bucketConfig struct {
	attrs           *storage.BucketAttrs
	objectRetention bool
}

// WithBucketAttrs creates the bucket with attrs, such as VersioningEnabled, CORS, Labels, a
// RetentionPolicy or DefaultEventBasedHold. TempBucket chooses the name, so attrs.Name must be
// empty.
func WithBucketAttrs(attrs *storage.BucketAttrs) BucketOption {
	return func(c *bucketConfig) { c.attrs = attrs }
}

// WithObjectRetention enables object retention, so that objects in the bucket can carry a
// retention configuration of their own.
func WithObjectRetention() BucketOption {
	return func(c *bucketConfig) { c.objectRetention = true }
}

// TempBucket creates a bucket that only this test uses, in the server's project, and returns
// its handle. When tb ends, the bucket is deleted with every object generation, soft-deleted
// object and upload session, even objects under a hold or retention. Deleting objects through
// the API still follows those protections; only this cleanup ignores them.
//
// The name starts with "tmp-", continues with the test's name, lower-cased and shortened, and
// ends with a part unique to the call and the run, so a failed run shows which test made a
// bucket, and parallel tests never share one.
func (s *Server) TempBucket(tb testing.TB, opts ...BucketOption) *storage.BucketHandle {
	tb.Helper()
	var c bucketConfig
	for _, opt := range opts {
		opt(&c)
	}
	var attrs storage.BucketAttrs
	if c.attrs != nil {
		if c.attrs.Name != "" {
			tb.Fatalf("gcsmimic: TempBucket chooses the bucket name; leave BucketAttrs.Name empty, not %q", c.attrs.Name)
		}
		attrs = *c.attrs
	}
	name := tempBucketName(tb.Name())
	handle := s.sdk.Bucket(name)
	create := handle
	if c.objectRetention {
		create = handle.SetObjectRetention(true)
	}
	if err := create.Create(tb.Context(), s.project, &attrs); err != nil {
		tb.Fatalf("gcsmimic: create bucket %s: %v", name, err)
	}
	tb.Cleanup(func() {
		// tb.Context has ended by now, so the cleanup bounds itself.
		ctx, cancel := context.WithTimeout(context.Background(), purgeTimeout)
		defer cancel()
		if err := s.purge(ctx, name); err != nil {
			tb.Errorf("gcsmimic: delete bucket %s: %v", name, err)
		}
	})
	// The official client's create request leaves defaultEventBasedHold out (storage 1.70.0),
	// so the setting would otherwise be lost without an error.
	if attrs.DefaultEventBasedHold {
		if _, err := handle.Update(tb.Context(), storage.BucketAttrsToUpdate{DefaultEventBasedHold: true}); err != nil {
			tb.Fatalf("gcsmimic: set the default event-based hold of bucket %s: %v", name, err)
		}
	}
	return handle
}

const (
	tempBucketPrefix = "tmp-"
	maxBucketName    = 63 // Cloud Storage's limit for a name without dots
)

// runID tells the buckets of this process from those of other runs, such as earlier runs
// against the same WithDataDir directory. Hex digits cannot spell a word Cloud Storage refuses.
var runID = func() string {
	var id [4]byte
	_, _ = rand.Read(id[:]) // crypto/rand.Read never fails; it crashes the program instead.
	return hex.EncodeToString(id[:])
}()

var bucketSequence atomic.Uint64

// tempBucketName returns tmp-<test>-<run>-<sequence>, a valid bucket name of at most 63 bytes.
func tempBucketName(testName string) string {
	suffix := "-" + runID + "-" + strconv.FormatUint(bucketSequence.Add(1), 10)
	label := bucketLabel(testName, maxBucketName-len(tempBucketPrefix)-len(suffix))
	if label == "" {
		label = "test"
	}
	return tempBucketPrefix + label + suffix
}

// bucketLabel turns a test name into lower-case ASCII letters, digits and single dashes of at
// most limit bytes. Cloud Storage refuses names that contain "google" or a near spelling with
// zeros, which a test name may well contain, so a dash breaks every such run.
func bucketLabel(testName string, limit int) string {
	var label strings.Builder
	dash := false
	for _, r := range strings.ToLower(testName) {
		switch {
		case r >= 'a' && r <= 'z' || r >= '0' && r <= '9':
			label.WriteRune(r)
			dash = false
		case !dash && label.Len() > 0:
			label.WriteByte('-')
			dash = true
		}
	}
	text := label.String()
	for {
		index := strings.Index(strings.ReplaceAll(text, "0", "o"), "google")
		if index < 0 {
			break
		}
		text = text[:index+3] + "-" + text[index+3:]
	}
	text = text[:min(len(text), limit)]
	return strings.Trim(text, "-")
}

// createBucket creates a bucket with default settings in the server's project; an existing
// bucket is kept as it is.
func (s *Server) createBucket(ctx context.Context, name string) error {
	err := s.sdk.Bucket(name).Create(ctx, s.project, nil)
	if err == nil || apiStatus(err) == http.StatusConflict {
		return nil
	}
	return fmt.Errorf("create bucket %s: %w", name, err)
}

// purgeTimeout bounds the whole cleanup of a TempBucket, requests and retries included.
const purgeTimeout = time.Minute

// purgeAnswer is the purge route's answer. A pending cleanup is not a failure: the bucket is
// gone, and the server reclaims the remaining blobs itself.
type purgeAnswer struct {
	Objects        int  `json:"objects"`
	CleanupPending bool `json:"cleanupPending"`
}

// purge deletes a bucket and everything in it through DELETE /_internal/buckets/{bucket}. The
// route answers 503 while an upload session of the bucket is still held or the catalog is
// busy, which retryBusy waits out within a bound.
func (s *Server) purge(ctx context.Context, name string) error {
	path := "/_internal/buckets/" + url.PathEscape(name)
	err := retryBusy(ctx, func() error {
		var answer purgeAnswer
		return s.admin(ctx, http.MethodDelete, path, nil, "", &answer)
	})
	switch apiStatus(err) {
	case http.StatusNotFound:
		return s.confirmDeleted(ctx, name, err)
	case http.StatusServiceUnavailable:
		return fmt.Errorf("an upload to the bucket may still be open: %w", err)
	default:
		return err
	}
}

// confirmDeleted accepts a 404 from the purge route only when the bucket is indeed gone, as
// after the test deleted it or called Reset. A binary without the route also answers 404, and
// would otherwise leave the bucket behind unnoticed.
func (s *Server) confirmDeleted(ctx context.Context, name string, purgeErr error) error {
	_, err := s.sdk.Bucket(name).Attrs(ctx)
	switch {
	case errors.Is(err, storage.ErrBucketNotExist):
		return nil
	case err == nil:
		return fmt.Errorf("the bucket still exists although the purge route answered 404; this gcs-mimic "+
			"may predate DELETE /_internal/buckets: %w", purgeErr)
	default:
		return errors.Join(purgeErr, fmt.Errorf("check that the bucket is gone: %w", err))
	}
}
