package mimicproc

import (
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

func TestUnsupportedBinaryErrorNamesTheRefusingPackage(t *testing.T) {
	for _, product := range []string{"fakestorage", "gcsmimic"} {
		err := checkBinary(product, "/opt/gcs-mimic", "0.0.1")
		if err == nil {
			t.Fatal("accepted another minor release")
		}
		want := "gcs-mimic at /opt/gcs-mimic reported version 0.0.1; " + product + " " + ModuleVersion + " supports " +
			mustParseRelease(ModuleVersion).supportedRange()
		if err.Error() != want {
			t.Fatalf("message = %q, want %q", err.Error(), want)
		}
	}
	if err := checkBinary("gcsmimic", "/opt/gcs-mimic", ""); err == nil ||
		err.Error() != "gcs-mimic at /opt/gcs-mimic reported no version; gcsmimic "+ModuleVersion+" supports "+
			mustParseRelease(ModuleVersion).supportedRange() {
		t.Fatalf("missing version: %v", err)
	}
}
