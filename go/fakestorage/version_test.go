package fakestorage

import (
	"errors"
	"strings"
	"testing"

	"github.com/shiroemons/gcs-mimic/go/internal/mimicproc"
)

func TestStartRefusesABinaryOfAnotherRelease(t *testing.T) {
	for _, tc := range []struct {
		name, version, host string
		reported            string
	}{
		{"ready reports another minor", "0.0.1", "", "0.0.1"},
		{"ready reports no version", "none", "", ""},
		{"exit before ready, another minor", "0.0.1", "exit-before-ready", "0.0.1"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Setenv("GCSM_TEST_CHILD_VERSION", tc.version)
			options := testOptions()
			options.Host = tc.host
			s, err := NewServerWithOptions(options)
			if err == nil {
				s.Stop()
				t.Fatal("started a binary of another release")
			}
			var unsupported *UnsupportedBinaryError
			if !errors.As(err, &unsupported) {
				t.Fatalf("want *UnsupportedBinaryError, got %v", err)
			}
			if unsupported.Version != tc.reported || unsupported.Path != options.Executable {
				t.Fatalf("error = %+v", unsupported)
			}
			version := mimicproc.ModuleVersion
			if !strings.Contains(err.Error(), version[:strings.LastIndex(version, ".")]) {
				t.Fatalf("error does not name the supported range: %v", err)
			}
		})
	}
}

// The facade keeps the message it had before the start code was shared, word for word.
func TestUnsupportedBinaryMessageNamesFakestorage(t *testing.T) {
	t.Setenv("GCSM_TEST_CHILD_VERSION", "0.0.1")
	options := testOptions()
	_, err := NewServerWithOptions(options)
	var unsupported *UnsupportedBinaryError
	if !errors.As(err, &unsupported) {
		t.Fatalf("want *UnsupportedBinaryError, got %v", err)
	}
	want := "gcs-mimic at " + options.Executable + " reported version 0.0.1; fakestorage " +
		mimicproc.ModuleVersion + " supports "
	if !strings.HasPrefix(unsupported.Error(), want) {
		t.Fatalf("message = %q, want the prefix %q", unsupported.Error(), want)
	}
}

// A child that fails for another reason keeps its own error: the version probe only adds a
// cause when the version is the likely one.
func TestAnEarlyExitOfTheSameReleaseKeepsItsOwnError(t *testing.T) {
	options := testOptions()
	options.Host = "exit-before-ready"
	_, err := NewServerWithOptions(options)
	var unsupported *UnsupportedBinaryError
	if err == nil || errors.As(err, &unsupported) || !strings.Contains(err.Error(), "17") {
		t.Fatalf("want the child's exit status 17 alone, got %v", err)
	}
}
