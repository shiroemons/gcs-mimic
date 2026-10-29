package gcsmimic_test

import (
	"context"
	"crypto/rand"
	"errors"
	"fmt"
	"maps"
	"slices"
	"testing"
	"time"

	"cloud.google.com/go/storage"
	"google.golang.org/api/googleapi"
)

// TestBucketNotifications stores and reads back Pub/Sub notification configurations. gcs-mimic
// never publishes events, so only the configuration CRUD is observable.
func TestBucketNotifications(t *testing.T) {
	ctx, client, _, _ := newEmulatorClient(t)
	var nonce [8]byte
	if _, err := rand.Read(nonce[:]); err != nil {
		t.Fatal(err)
	}
	bucket := client.Bucket(fmt.Sprintf("sdk-go-notifications-%x", nonce))
	if err := bucket.Create(ctx, "local-project", nil); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		cleanup, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		if err := bucket.Delete(cleanup); err != nil && !errors.Is(err, storage.ErrBucketNotExist) {
			t.Errorf("cleanup: %v", err)
		}
	})
	want := &storage.Notification{
		TopicProjectID:   "local-project",
		TopicID:          "uploads",
		EventTypes:       []string{storage.ObjectFinalizeEvent, storage.ObjectDeleteEvent},
		CustomAttributes: map[string]string{"team": "storage"},
		PayloadFormat:    storage.NoPayload,
		ObjectNamePrefix: "logs/",
	}
	created, err := bucket.AddNotification(ctx, want)
	if err != nil {
		t.Fatalf("add notification: %v", err)
	}
	if created.ID == "" || created.TopicProjectID != want.TopicProjectID || created.TopicID != want.TopicID ||
		!slices.Equal(created.EventTypes, want.EventTypes) || !maps.Equal(created.CustomAttributes, want.CustomAttributes) ||
		created.PayloadFormat != want.PayloadFormat || created.ObjectNamePrefix != want.ObjectNamePrefix {
		t.Fatalf("unexpected notification %+v", created)
	}
	defaults, err := bucket.AddNotification(ctx, &storage.Notification{TopicProjectID: "local-project", TopicID: "audit", PayloadFormat: storage.JSONPayload})
	if err != nil {
		t.Fatalf("add default notification: %v", err)
	}
	all, err := bucket.Notifications(ctx)
	if err != nil {
		t.Fatalf("list notifications: %v", err)
	}
	if len(all) != 2 || all[created.ID] == nil || all[defaults.ID] == nil ||
		all[defaults.ID].PayloadFormat != storage.JSONPayload || len(all[defaults.ID].EventTypes) != 0 {
		t.Fatalf("unexpected listing %+v", all)
	}
	_, err = bucket.AddNotification(ctx, &storage.Notification{TopicProjectID: "local-project", TopicID: "uploads", PayloadFormat: "XML"})
	var apiError *googleapi.Error
	if !errors.As(err, &apiError) || apiError.Code != 400 {
		t.Fatalf("invalid payload format: %v", err)
	}
	if err := bucket.DeleteNotification(ctx, created.ID); err != nil {
		t.Fatalf("delete notification: %v", err)
	}
	if err := bucket.DeleteNotification(ctx, created.ID); !errors.As(err, &apiError) || apiError.Code != 404 {
		t.Fatalf("second delete: %v", err)
	}
	all, err = bucket.Notifications(ctx)
	if err != nil || len(all) != 1 || all[defaults.ID] == nil {
		t.Fatalf("listing after delete: %+v, %v", all, err)
	}
}

func TestStrictAuthBucketNotifications(t *testing.T) {
	ctx, bucket, _, _ := strictFixture(t, "notifications")
	_, viewer := newStrictClient(t, strictViewerToken)
	viewerBucket := viewer.Bucket(bucket.BucketName())
	created, err := bucket.AddNotification(ctx, &storage.Notification{TopicProjectID: strictProject, TopicID: "uploads", PayloadFormat: storage.JSONPayload})
	if err != nil {
		t.Fatalf("admin add notification: %v", err)
	}
	_, err = viewerBucket.Notifications(ctx)
	requireStrictError(t, err, 403, "forbidden")
	_, err = viewerBucket.AddNotification(ctx, &storage.Notification{TopicProjectID: strictProject, TopicID: "other", PayloadFormat: storage.JSONPayload})
	requireStrictError(t, err, 403, "forbidden")
	requireStrictError(t, viewerBucket.DeleteNotification(ctx, created.ID), 403, "forbidden")
	if err := bucket.DeleteNotification(ctx, created.ID); err != nil {
		t.Fatalf("admin delete notification: %v", err)
	}
}
