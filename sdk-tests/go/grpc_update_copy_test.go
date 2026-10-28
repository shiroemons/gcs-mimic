package gcsmimic_test

import (
	"context"
	"fmt"
	"os"
	"testing"
	"time"

	"cloud.google.com/go/storage"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/metadata"
	"google.golang.org/grpc/status"
)

func TestGRPCUpdateCopy(t *testing.T) {
	if os.Getenv("STORAGE_EMULATOR_HOST_GRPC") == "" {
		t.Skip("STORAGE_EMULATOR_HOST_GRPC is unset")
	}
	base, jsonClient, _, _ := newEmulatorClient(t)
	ctx, cancel := context.WithTimeout(base, 30*time.Second)
	defer cancel()
	client, err := storage.NewGRPCClient(ctx)
	if err != nil {
		t.Fatal(err)
	}
	defer client.Close()
	name := fmt.Sprintf("grpc-update-%d", time.Now().UnixNano())
	jb := jsonClient.Bucket(name)
	if err := jb.Create(ctx, "local-project", &storage.BucketAttrs{Labels: map[string]string{"keep": "yes"}}); err != nil {
		t.Fatal(err)
	}
	bucket := client.Bucket(name)
	attrs, err := bucket.Update(ctx, storage.BucketAttrsToUpdate{VersioningEnabled: true})
	if err != nil {
		t.Fatal(err)
	}
	if !attrs.VersioningEnabled || attrs.Labels["keep"] != "yes" {
		t.Fatalf("unmasked labels changed: %+v", attrs)
	}
	_, err = bucket.If(storage.BucketConditions{MetagenerationMatch: attrs.MetaGeneration + 1}).Update(ctx, storage.BucketAttrsToUpdate{VersioningEnabled: false})
	if status.Code(err) != codes.FailedPrecondition {
		t.Fatalf("bucket condition: %v", err)
	}
	if _, err := bucket.Update(ctx, storage.BucketAttrsToUpdate{RequesterPays: true}); status.Code(err) != codes.Unimplemented || storage.ShouldRetry(err) {
		t.Fatalf("unsupported billing must fail immediately: %v", err)
	}
	original := writeVersion(t, ctx, jb.Object("source"), "source bytes")
	updated, err := bucket.Object("source").Update(ctx, storage.ObjectAttrsToUpdate{ContentType: "text/plain", Metadata: map[string]string{"grpc": "yes"}})
	if err != nil || updated.Generation != original.Generation {
		t.Fatalf("object update: %+v %v", updated, err)
	}
	seen, err := jb.Object("source").Attrs(ctx)
	if err != nil || seen.Metadata["grpc"] != "yes" {
		t.Fatalf("JSON observation: %+v %v", seen, err)
	}
	updated, err = bucket.Object("source").Update(ctx, storage.ObjectAttrsToUpdate{TemporaryHold: true, CustomTime: time.Date(2026, 1, 1, 0, 0, 0, 123000000, time.UTC)})
	if err != nil || !updated.TemporaryHold || updated.CustomTime.Nanosecond() != 123000000 {
		t.Fatalf("hold/custom time: %+v %v", updated, err)
	}
	if _, err := bucket.Object("source").Update(ctx, storage.ObjectAttrsToUpdate{TemporaryHold: false}); err != nil {
		t.Fatal(err)
	}
	if _, err := bucket.Object("source").If(storage.Conditions{GenerationMatch: original.Generation + 1}).Update(ctx, storage.ObjectAttrsToUpdate{ContentType: "ignored"}); status.Code(err) != codes.FailedPrecondition {
		t.Fatalf("object condition: %v", err)
	}
	if _, err := bucket.Object("copy").CopierFrom(bucket.Object("source")).Run(ctx); err != nil {
		t.Fatal(err)
	}
	if _, err := bucket.Object("copy").If(storage.Conditions{DoesNotExist: true}).CopierFrom(bucket.Object("source")).Run(ctx); status.Code(err) != codes.FailedPrecondition {
		t.Fatalf("copy destination condition: %v", err)
	}
	otherName := name + "-other"
	if err := jsonClient.Bucket(otherName).Create(ctx, "local-project", &storage.BucketAttrs{Location: "EU"}); err != nil {
		t.Fatal(err)
	}
	copied, err := client.Bucket(otherName).Object("copied").CopierFrom(bucket.Object("source").Generation(original.Generation)).Run(ctx)
	if err != nil {
		t.Fatal(err)
	}
	readObject(t, ctx, jsonClient.Bucket(otherName).Object("copied"), []byte("source bytes"), copied.Generation)
	joined, err := bucket.Object("joined").ComposerFrom(bucket.Object("source"), bucket.Object("copy")).Run(ctx)
	if err != nil {
		t.Fatal(err)
	}
	readObject(t, ctx, jb.Object("joined"), []byte("source bytessource bytes"), joined.Generation)
	moved, err := bucket.Object("copy").Move(ctx, storage.MoveObjectDestination{Object: "moved"})
	if err != nil {
		t.Fatal(err)
	}
	readObject(t, ctx, jb.Object("moved"), []byte("source bytes"), moved.Generation)
	policy, err := bucket.IAM().Policy(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if err := bucket.IAM().SetPolicy(ctx, policy); err != nil {
		t.Fatal(err)
	}
	permissions, err := bucket.IAM().TestPermissions(ctx, []string{"storage.objects.get", "storage.buckets.update"})
	if err != nil || len(permissions) != 2 {
		t.Fatalf("permissions: %v %v", permissions, err)
	}
	attrs, err = bucket.Update(ctx, storage.BucketAttrsToUpdate{SoftDeletePolicy: &storage.SoftDeletePolicy{RetentionDuration: 7 * 24 * time.Hour}})
	if err != nil {
		t.Fatal(err)
	}
	deleted := writeVersion(t, ctx, jb.Object("restore"), "restore bytes")
	if err := jb.Object("restore").Generation(deleted.Generation).Delete(ctx); err != nil {
		t.Fatal(err)
	}
	restored, err := bucket.Object("restore").Generation(deleted.Generation).SoftDeleted().Restore(ctx, &storage.RestoreOptions{})
	if err != nil {
		t.Fatal(err)
	}
	readObject(t, ctx, jb.Object("restore"), []byte("restore bytes"), restored.Generation)
	if _, err := bucket.Object("restore").Generation(deleted.Generation).SoftDeleted().If(storage.Conditions{DoesNotExist: true}).Restore(ctx, &storage.RestoreOptions{}); status.Code(err) != codes.FailedPrecondition {
		t.Fatalf("restore destination condition: %v", err)
	}
	life := storage.Lifecycle{Rules: []storage.LifecycleRule{{Action: storage.LifecycleAction{Type: storage.DeleteAction}, Condition: storage.LifecycleCondition{AgeInDays: 30}}}}
	settings := storage.BucketAttrsToUpdate{VersioningEnabled: false, UniformBucketLevelAccess: &storage.UniformBucketLevelAccess{Enabled: true}, Lifecycle: &life}
	settings.SetLabel("owner", "grpc")
	attrs, err = bucket.Update(ctx, settings)
	if err != nil || attrs.VersioningEnabled || !attrs.UniformBucketLevelAccess.Enabled || len(attrs.Lifecycle.Rules) != 1 || attrs.Labels["keep"] != "yes" {
		t.Fatalf("bucket settings: %+v %v", attrs, err)
	}
	observed, err := jb.Attrs(ctx)
	if err != nil || observed.Labels["owner"] != "grpc" || len(observed.Lifecycle.Rules) != 1 || !observed.UniformBucketLevelAccess.Enabled {
		t.Fatalf("JSON settings: %+v %v", observed, err)
	}
	lockName := name + "-lock"
	if err := jsonClient.Bucket(lockName).Create(ctx, "local-project", &storage.BucketAttrs{RetentionPolicy: &storage.RetentionPolicy{RetentionPeriod: time.Hour}}); err != nil {
		t.Fatal(err)
	}
	lockAttrs, err := jsonClient.Bucket(lockName).Attrs(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if err := client.Bucket(lockName).If(storage.BucketConditions{MetagenerationMatch: lockAttrs.MetaGeneration}).LockRetentionPolicy(ctx); err != nil {
		t.Fatal(err)
	}
	iamName := name + "-iam"
	if err := jsonClient.Bucket(iamName).Create(ctx, "local-project", &storage.BucketAttrs{PublicAccessPrevention: storage.PublicAccessPreventionEnforced}); err != nil {
		t.Fatal(err)
	}
	iamBucket := client.Bucket(iamName)
	attrs, err = iamBucket.Update(ctx, storage.BucketAttrsToUpdate{UniformBucketLevelAccess: &storage.UniformBucketLevelAccess{Enabled: true}})
	if err != nil || attrs.PublicAccessPrevention != storage.PublicAccessPreventionEnforced || !attrs.UniformBucketLevelAccess.Enabled {
		t.Fatalf("UBLA update changed PAP: %+v %v", attrs, err)
	}
	attrs, err = iamBucket.Update(ctx, storage.BucketAttrsToUpdate{PublicAccessPrevention: storage.PublicAccessPreventionInherited})
	if err != nil || !attrs.UniformBucketLevelAccess.Enabled || attrs.PublicAccessPrevention != storage.PublicAccessPreventionInherited {
		t.Fatalf("PAP update changed UBLA: %+v %v", attrs, err)
	}
	observed, err = jsonClient.Bucket(iamName).Attrs(ctx)
	if err != nil || !observed.UniformBucketLevelAccess.Enabled || observed.PublicAccessPrevention != storage.PublicAccessPreventionInherited {
		t.Fatalf("JSON IAM configuration: %+v %v", observed, err)
	}
}

func TestGRPCUpdateCopyStrict(t *testing.T) {
	endpoint := os.Getenv("GCS_MIMIC_STRICT_GRPC_ENDPOINT")
	if endpoint == "" {
		t.Skip("GCS_MIMIC_STRICT_GRPC_ENDPOINT is unset")
	}
	host, err := emulatorEndpoint("http://"+endpoint, "")
	if err != nil || host.Host != endpoint {
		t.Fatalf("invalid strict endpoint: %v", err)
	}
	t.Setenv("STORAGE_EMULATOR_HOST_GRPC", endpoint)
	base, jsonClient := newStrictClient(t, strictAdminToken)
	ctx, cancel := context.WithTimeout(base, 30*time.Second)
	defer cancel()
	admin := metadata.AppendToOutgoingContext(ctx, "authorization", "Bearer "+strictAdminToken)
	viewer := metadata.AppendToOutgoingContext(ctx, "authorization", "Bearer "+strictViewerToken)
	client, err := storage.NewGRPCClient(ctx)
	if err != nil {
		t.Fatal(err)
	}
	defer client.Close()
	name := fmt.Sprintf("grpc-update-strict-%d", time.Now().UnixNano())
	jb := jsonClient.Bucket(name)
	if err := jb.Create(ctx, strictProject, nil); err != nil {
		t.Fatal(err)
	}
	writeVersion(t, ctx, jb.Object("source"), "strict bytes")
	b := client.Bucket(name)
	if _, err := b.Update(admin, storage.BucketAttrsToUpdate{VersioningEnabled: true}); err != nil {
		t.Fatal(err)
	}
	if _, err := b.Update(viewer, storage.BucketAttrsToUpdate{VersioningEnabled: false}); status.Code(err) != codes.PermissionDenied {
		t.Fatalf("viewer bucket update: %v", err)
	}
	if _, err := b.Object("source").Update(viewer, storage.ObjectAttrsToUpdate{ContentType: "text/plain"}); status.Code(err) != codes.PermissionDenied {
		t.Fatalf("viewer object update: %v", err)
	}
	if _, err := b.Object("source").Update(ctx, storage.ObjectAttrsToUpdate{ContentType: "text/plain"}); status.Code(err) != codes.Unauthenticated {
		t.Fatalf("anonymous object update: %v", err)
	}
	if _, err := b.Object("copy").CopierFrom(b.Object("source")).Run(viewer); status.Code(err) != codes.PermissionDenied {
		t.Fatalf("viewer copy: %v", err)
	}
	if _, err := b.Object("copy").CopierFrom(b.Object("source")).Run(admin); err != nil {
		t.Fatal(err)
	}
	if _, err := b.Object("joined").ComposerFrom(b.Object("source"), b.Object("copy")).Run(viewer); status.Code(err) != codes.PermissionDenied {
		t.Fatalf("viewer compose: %v", err)
	}
	if _, err := b.Object("joined").ComposerFrom(b.Object("source"), b.Object("copy")).Run(admin); err != nil {
		t.Fatal(err)
	}
	permissions, err := b.IAM().TestPermissions(viewer, []string{"storage.objects.get", "storage.objects.create", "storage.buckets.update"})
	if err != nil || len(permissions) != 1 || permissions[0] != "storage.objects.get" {
		t.Fatalf("viewer permissions: %v %v", permissions, err)
	}
	policy, err := b.IAM().Policy(admin)
	if err != nil {
		t.Fatal(err)
	}
	if err := b.IAM().SetPolicy(viewer, policy); status.Code(err) != codes.PermissionDenied {
		t.Fatalf("viewer policy write: %v", err)
	}
	if err := b.IAM().SetPolicy(admin, policy); err != nil {
		t.Fatal(err)
	}
	if err := b.IAM().SetPolicy(admin, policy); status.Code(err) != codes.FailedPrecondition {
		t.Fatalf("stale policy: %v", err)
	}
	if _, err := b.Object("copy").Move(viewer, storage.MoveObjectDestination{Object: "denied"}); status.Code(err) != codes.PermissionDenied {
		t.Fatalf("viewer move: %v", err)
	}
	if _, err := b.Object("copy").Move(admin, storage.MoveObjectDestination{Object: "moved"}); err != nil {
		t.Fatal(err)
	}
	if _, err := b.Update(admin, storage.BucketAttrsToUpdate{VersioningEnabled: false, SoftDeletePolicy: &storage.SoftDeletePolicy{RetentionDuration: 7 * 24 * time.Hour}}); err != nil {
		t.Fatal(err)
	}
	deleted := writeVersion(t, ctx, jb.Object("restore"), "restore strict")
	if err := jb.Object("restore").Delete(ctx); err != nil {
		t.Fatal(err)
	}
	selected := b.Object("restore").Generation(deleted.Generation).SoftDeleted()
	if _, err := selected.Restore(viewer, &storage.RestoreOptions{}); status.Code(err) != codes.PermissionDenied {
		t.Fatalf("viewer restore: %v", err)
	}
	if _, err := selected.Restore(admin, &storage.RestoreOptions{}); err != nil {
		t.Fatal(err)
	}
	if _, err := b.Update(admin, storage.BucketAttrsToUpdate{VersioningEnabled: true}); err != nil {
		t.Fatal(err)
	}
	seen, err := jb.Attrs(ctx)
	if err != nil || !seen.VersioningEnabled {
		t.Fatalf("denied mutation changed bucket: %+v %v", seen, err)
	}
}
