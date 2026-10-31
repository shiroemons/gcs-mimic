package fakestorage

import (
	"context"
	"errors"
	"fmt"
	"time"

	"google.golang.org/api/googleapi"
	"google.golang.org/api/option"
	raw "google.golang.org/api/storage/v1"
)

type CreateBucketOpts struct {
	Name                  string
	VersioningEnabled     bool
	DefaultEventBasedHold bool
}

func (s *Server) CreateBucket(name string) {
	s.CreateBucketWithOpts(CreateBucketOpts{Name: name})
}

// CreateBucketWithOpts retains the pinned memory backend's duplicate behavior:
// matching versioning is a no-op, including when the requested default hold differs.
func (s *Server) CreateBucketWithOpts(opts CreateBucketOpts) {
	if err := s.createBucket(opts); err != nil {
		panic(err)
	}
}

func (s *Server) createBucket(opts CreateBucketOpts) error {
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	client, err := raw.NewService(ctx, option.WithHTTPClient(s.HTTPClient()), option.WithoutAuthentication(),
		option.WithEndpoint(s.endpoint.String()+"/storage/v1/"))
	if err != nil {
		return fmt.Errorf("configure fixture bucket client: %w", err)
	}
	// High-level create omits the default hold; one raw create avoids partial setup.
	_, err = client.Buckets.Insert("local-project", &raw.Bucket{
		Name: opts.Name, DefaultEventBasedHold: opts.DefaultEventBasedHold,
		Versioning:      &raw.BucketVersioning{Enabled: opts.VersioningEnabled, ForceSendFields: []string{"Enabled"}},
		ForceSendFields: []string{"DefaultEventBasedHold"},
	}).Context(ctx).Do()
	if err == nil {
		return nil
	}
	var api *googleapi.Error
	if !errors.As(err, &api) || api.Code != 409 {
		return fmt.Errorf("create fixture bucket: %w", err)
	}
	existing, err := client.Buckets.Get(opts.Name).Context(ctx).Do()
	if err != nil {
		return fmt.Errorf("inspect existing fixture bucket: %w", err)
	}
	if (existing.Versioning != nil && existing.Versioning.Enabled) != opts.VersioningEnabled {
		return fmt.Errorf("bucket %q already exists with different versioning", opts.Name)
	}
	return nil
}
