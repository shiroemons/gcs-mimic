package gcsmimic_test

import (
	"context"
	"log"
	"os"
	"testing"

	"github.com/shiroemons/gcs-mimic/go/gcsmimic"
)

// These examples are compiled but not run: running them needs a gcs-mimic binary, and the
// package's ordinary tests use a stand-in child instead. The integration tests run the same calls.

// A test gets a server of its own, a bucket of its own, and fixtures from a directory.
func ExampleStart() {
	var t *testing.T // the test's *testing.T

	server := gcsmimic.Start(t)
	bucket := server.TempBucket(t)
	server.Seed(t, gcsmimic.Dir("testdata/fixtures", gcsmimic.Into(bucket.BucketName())))
	// Point the code under test at bucket, or at server.URL().
}

// One server serves every test of a package when TestMain starts it with New; each test then
// takes a TempBucket, so parallel tests never see each other's objects.
func ExampleNew() {
	var m *testing.M // the *testing.M of func TestMain(m *testing.M)

	server, err := gcsmimic.New(context.Background())
	if err != nil {
		log.Fatal(err)
	}
	code := m.Run()
	if err := server.Close(); err != nil {
		log.Print(err)
	}
	os.Exit(code)
}

// Code that builds its own client with storage.NewClient finds the server through
// STORAGE_EMULATOR_HOST, which the official Go client reads.
func ExampleServer_URL() {
	var t *testing.T // the test's *testing.T

	server := gcsmimic.Start(t)
	t.Setenv("STORAGE_EMULATOR_HOST", server.URL())
	// Run the code under test.
}
