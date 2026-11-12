package fakestorage

import (
	"errors"
	"strings"
	"testing"
)

func TestReleaseGrammarMatchesThePublicationGrammar(t *testing.T) {
	for _, tc := range []struct {
		value string
		want  release
		ok    bool
	}{
		{"0.20.0", release{0, 20, 0, 0}, true},
		{"1.0.0-rc.1", release{1, 0, 0, 1}, true},
		{"10.2.30-rc.12", release{10, 2, 30, 12}, true},
		{"1.0", release{}, false},
		{"1.0.0.0", release{}, false},
		{"01.0.0", release{}, false},
		{"1.0.0-rc.0", release{}, false},
		{"1.0.0-rc.01", release{}, false},
		{"1.0.0-beta.1", release{}, false},
		{"1.0.0+build", release{}, false},
		{"+1.0.0", release{}, false},
		{"", release{}, false},
	} {
		t.Run(tc.value, func(t *testing.T) {
			got, err := parseRelease(tc.value)
			if (err == nil) != tc.ok || got != tc.want {
				t.Fatalf("parseRelease(%q) = %+v, %v; want %+v, ok %v", tc.value, got, err, tc.want, tc.ok)
			}
		})
	}
}

func TestSupportedBinaryRange(t *testing.T) {
	for _, tc := range []struct {
		module, binary string
		want           bool
	}{
		{"0.20.0", "0.20.0", true},
		{"0.20.0", "0.20.3", true},
		{"0.20.1", "0.20.0", true},
		{"0.20.0", "0.21.0", false},
		{"0.20.0", "0.19.1", false},
		{"0.20.0", "1.0.0", false},
		{"1.2.0", "1.2.0-rc.1", true},
		{"1.2.0", "1.3.0", true},
		{"1.2.0", "1.1.9", false},
		{"1.2.0", "2.0.0", false},
	} {
		t.Run(tc.module+"/"+tc.binary, func(t *testing.T) {
			got := mustParseRelease(tc.module).supports(mustParseRelease(tc.binary))
			if got != tc.want {
				t.Fatalf("%s supports %s = %v, want %v", tc.module, tc.binary, got, tc.want)
			}
		})
	}
}

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
			if !strings.Contains(err.Error(), moduleVersion[:strings.LastIndex(moduleVersion, ".")]) {
				t.Fatalf("error does not name the supported range: %v", err)
			}
		})
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
