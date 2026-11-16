package mimicproc

import (
	"archive/tar"
	"archive/zip"
	"bufio"
	"bytes"
	"compress/gzip"
	"context"
	"crypto/sha256"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"sync/atomic"
	"testing"
)

const testExecutable = "#!/bin/false\nnot really gcs-mimic\n"

// testRelease serves release archives under /vVERSION/ and counts the requests.
type testRelease struct {
	assets   map[string][]byte
	requests atomic.Int32
	server   *httptest.Server
}

func newTestRelease(t *testing.T, assets map[string][]byte) *testRelease {
	t.Helper()
	release := &testRelease{assets: assets}
	release.server = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		release.requests.Add(1)
		name, found := strings.CutPrefix(r.URL.Path, "/v"+ModuleVersion+"/")
		body, exists := release.assets[name]
		if !found || !exists {
			http.NotFound(w, r)
			return
		}
		_, _ = w.Write(body)
	}))
	t.Cleanup(release.server.Close)
	return release
}

// sums writes a SHA256SUMS file for the release's assets.
func (release *testRelease) sums() []byte {
	var out bytes.Buffer
	for name, body := range release.assets {
		fmt.Fprintf(&out, "%x  %s\n", sha256.Sum256(body), name)
	}
	return out.Bytes()
}

// resolver returns a resolver for platform that downloads from the release into a fresh cache and
// finds no gcs-mimic on PATH.
func (release *testRelease) resolver(t *testing.T, platform string) resolver {
	t.Helper()
	cache := t.TempDir()
	return resolver{
		lookPath: func(string) (string, error) { return "", exec.ErrNotFound },
		cacheDir: func() (string, error) { return cache, nil },
		client:   release.server.Client(),
		baseURL:  release.server.URL,
		digests:  release.sums(),
		platform: platform,
	}
}

// clearSelection removes the variables that select a binary, which CI sets for the integration
// tests of the same run.
func clearSelection(t *testing.T) {
	t.Helper()
	t.Setenv("GCSM_BINARY", "")
	t.Setenv("GCSM_DOWNLOAD", "")
}

func tarGz(t *testing.T, files map[string]string) []byte {
	t.Helper()
	var out bytes.Buffer
	compressed := gzip.NewWriter(&out)
	archive := tar.NewWriter(compressed)
	for name, body := range files {
		if err := archive.WriteHeader(&tar.Header{Name: name, Mode: 0o755, Size: int64(len(body)), Typeflag: tar.TypeReg}); err != nil {
			t.Fatal(err)
		}
		if _, err := archive.Write([]byte(body)); err != nil {
			t.Fatal(err)
		}
	}
	if err := archive.Close(); err != nil {
		t.Fatal(err)
	}
	if err := compressed.Close(); err != nil {
		t.Fatal(err)
	}
	return out.Bytes()
}

func zipFile(t *testing.T, files map[string]string) []byte {
	t.Helper()
	var out bytes.Buffer
	archive := zip.NewWriter(&out)
	for name, body := range files {
		entry, err := archive.Create(name)
		if err != nil {
			t.Fatal(err)
		}
		if _, err := entry.Write([]byte(body)); err != nil {
			t.Fatal(err)
		}
	}
	if err := archive.Close(); err != nil {
		t.Fatal(err)
	}
	return out.Bytes()
}

func darwinRelease(t *testing.T) *testRelease {
	t.Helper()
	dir := "gcs-mimic-" + ModuleVersion + "-aarch64-apple-darwin"
	return newTestRelease(t, map[string][]byte{dir + ".tar.gz": tarGz(t, map[string]string{
		dir + "/LICENSE-MIT": "license",
		dir + "/gcs-mimic":   testExecutable,
	})})
}

func TestResolveUsesAnExplicitBinaryAndGCSMBinaryAsGiven(t *testing.T) {
	clearSelection(t)
	untouched := resolver{
		lookPath: func(string) (string, error) { t.Fatal("looked on PATH"); return "", nil },
		cacheDir: func() (string, error) { t.Fatal("opened the cache"); return "", nil },
	}
	if got, err := untouched.resolve(context.Background(), "/opt/explicit"); err != nil || got != "/opt/explicit" {
		t.Fatalf("explicit: got %q, %v", got, err)
	}
	t.Setenv("GCSM_BINARY", "/opt/from-env")
	if got, err := untouched.resolve(context.Background(), ""); err != nil || got != "/opt/from-env" {
		t.Fatalf("GCSM_BINARY: got %q, %v", got, err)
	}
}

func TestResolveWithTheDownloadOffKeepsThePathLookup(t *testing.T) {
	clearSelection(t)
	untouched := resolver{
		lookPath: func(string) (string, error) { t.Fatal("probed PATH"); return "", nil },
		cacheDir: func() (string, error) { t.Fatal("opened the cache"); return "", nil },
	}
	t.Setenv("GCSM_DOWNLOAD", "off")
	if got, err := untouched.resolve(context.Background(), ""); err != nil || got != "gcs-mimic" {
		t.Fatalf("got %q, %v", got, err)
	}
	t.Setenv("GCSM_DOWNLOAD", "false")
	if _, err := untouched.resolve(context.Background(), ""); err == nil || !strings.Contains(err.Error(), "GCSM_DOWNLOAD must be on or off") {
		t.Fatalf("want the accepted values, got %v", err)
	}
}

func TestResolveUsesASupportedReleaseOnPath(t *testing.T) {
	clearSelection(t)
	release := darwinRelease(t)
	r := release.resolver(t, "darwin/arm64")
	r.lookPath = func(string) (string, error) { return os.Args[0], nil }
	got, err := r.resolve(context.Background(), "")
	if err != nil || got != os.Args[0] {
		t.Fatalf("got %q, %v", got, err)
	}
	if n := release.requests.Load(); n != 0 {
		t.Fatalf("downloaded %d times", n)
	}
}

func TestResolveDownloadsOnceAndReusesTheCache(t *testing.T) {
	clearSelection(t)
	release := darwinRelease(t)
	r := release.resolver(t, "darwin/arm64")
	// Another release on PATH is skipped rather than started.
	t.Setenv("GCSM_TEST_CHILD_VERSION", "0.0.1")
	r.lookPath = func(string) (string, error) { return os.Args[0], nil }
	first, err := r.resolve(context.Background(), "")
	if err != nil {
		t.Fatal(err)
	}
	root, _ := r.cacheDir()
	if want := filepath.Join(root, "gcs-mimic", ModuleVersion, "aarch64-apple-darwin", "gcs-mimic"); first != want {
		t.Fatalf("installed at %q, want %q", first, want)
	}
	body, err := os.ReadFile(first)
	if err != nil || string(body) != testExecutable {
		t.Fatalf("installed %q, %v", body, err)
	}
	info, err := os.Stat(first)
	if err != nil {
		t.Fatal(err)
	}
	if runtime.GOOS != "windows" && info.Mode().Perm()&0o100 == 0 {
		t.Fatalf("not executable: %v", info.Mode())
	}
	second, err := r.resolve(context.Background(), "")
	if err != nil || second != first {
		t.Fatalf("second resolve: %q, %v", second, err)
	}
	if n := release.requests.Load(); n != 1 {
		t.Fatalf("want one download, got %d", n)
	}
	entries, err := os.ReadDir(filepath.Dir(first))
	if err != nil || len(entries) != 1 {
		t.Fatalf("want only the executable in the cache, got %v, %v", entries, err)
	}
}

func TestResolveExtractsTheWindowsZip(t *testing.T) {
	clearSelection(t)
	for _, separator := range []string{"/", `\`} {
		t.Run("separator "+separator, func(t *testing.T) {
			dir := "gcs-mimic-" + ModuleVersion + "-x86_64-pc-windows-msvc"
			release := newTestRelease(t, map[string][]byte{dir + ".zip": zipFile(t, map[string]string{
				dir + separator + "LICENSE-MIT":   "license",
				dir + separator + "gcs-mimic.exe": testExecutable,
			})})
			got, err := release.resolver(t, "windows/amd64").resolve(context.Background(), "")
			if err != nil || filepath.Base(got) != "gcs-mimic.exe" {
				t.Fatalf("got %q, %v", got, err)
			}
			if body, err := os.ReadFile(got); err != nil || string(body) != testExecutable {
				t.Fatalf("installed %q, %v", body, err)
			}
		})
	}
}

func TestResolveRefusesWhatItCannotVerifyOrFind(t *testing.T) {
	dir := "gcs-mimic-" + ModuleVersion + "-x86_64-unknown-linux-musl"
	archive := dir + ".tar.gz"
	for _, tc := range []struct {
		name     string
		platform string
		files    map[string]string
		sums     func(release *testRelease) []byte
		want     string
	}{
		{"another digest", "linux/amd64", map[string]string{dir + "/gcs-mimic": testExecutable},
			func(*testRelease) []byte {
				return fmt.Appendf(nil, "%x  %s\n", sha256.Sum256([]byte("other")), archive)
			},
			"differs from the recorded"},
		{"no recorded digest", "linux/amd64", map[string]string{dir + "/gcs-mimic": testExecutable},
			func(*testRelease) []byte { return []byte("0000  gcs-mimic-0.0.1-x86_64-unknown-linux-musl.tar.gz\n") },
			"records no SHA-256 for " + archive},
		{"no executable in the archive", "linux/amd64", map[string]string{dir + "/LICENSE-MIT": "license"},
			(*testRelease).sums, "the archive has no such file"},
		{"no archive for the platform", "plan9/amd64", map[string]string{dir + "/gcs-mimic": testExecutable},
			(*testRelease).sums, "no release archive for plan9/amd64"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			clearSelection(t)
			release := newTestRelease(t, map[string][]byte{archive: tarGz(t, tc.files)})
			r := release.resolver(t, tc.platform)
			r.digests = tc.sums(release)
			_, err := r.resolve(context.Background(), "")
			if err == nil || !strings.Contains(err.Error(), tc.want) || !strings.Contains(err.Error(), "GCSM_BINARY") {
				t.Fatalf("want an error naming %q and GCSM_BINARY, got %v", tc.want, err)
			}
			root, _ := r.cacheDir()
			leftovers, _ := filepath.Glob(filepath.Join(root, "gcs-mimic", ModuleVersion, "*", "*"))
			if len(leftovers) != 0 {
				t.Fatalf("the cache keeps %v", leftovers)
			}
		})
	}
}

func TestResolveReportsTheSkippedBinaryWhenTheDownloadFails(t *testing.T) {
	clearSelection(t)
	t.Setenv("GCSM_TEST_CHILD_VERSION", "0.0.1")
	release := newTestRelease(t, nil)
	r := release.resolver(t, "linux/amd64")
	r.digests = fmt.Appendf(nil, "%x  gcs-mimic-%s-x86_64-unknown-linux-musl.tar.gz\n", sha256.Sum256(nil), ModuleVersion)
	r.lookPath = func(string) (string, error) { return os.Args[0], nil }
	_, err := r.resolve(context.Background(), "")
	for _, want := range []string{"skipped gcs-mimic at " + os.Args[0] + ", which reported version 0.0.1", "404 Not Found"} {
		if err == nil || !strings.Contains(err.Error(), want) {
			t.Fatalf("want %q in %v", want, err)
		}
	}
}

// releaseOf returns the gcs-mimic version in an archive name: the longest leading part that is
// a release, so that a candidate such as 1.0.0-rc.1 keeps its suffix. It is empty for a name
// that starts with no release.
func releaseOf(archive string) string {
	rest, _ := strings.CutPrefix(archive, "gcs-mimic-")
	parts := strings.Split(rest, "-")
	version := ""
	for i := range parts {
		if candidate := strings.Join(parts[:i+1], "-"); validRelease(candidate) {
			version = candidate
		}
	}
	return version
}

func validRelease(value string) bool {
	_, err := parseRelease(value)
	return err == nil
}

func TestReleaseOfKeepsTheCandidateSuffix(t *testing.T) {
	for archive, want := range map[string]string{
		"gcs-mimic-0.25.0-aarch64-apple-darwin.tar.gz":           "0.25.0",
		"gcs-mimic-1.0.0-rc.1-x86_64-pc-windows-msvc.zip":        "1.0.0-rc.1",
		"gcs-mimic-1.0.0-rc.12-x86_64-unknown-linux-musl.tar.gz": "1.0.0-rc.12",
		"other-1.0.0.tar.gz": "",
	} {
		if got := releaseOf(archive); got != want {
			t.Errorf("releaseOf(%q) = %q, want %q", archive, got, want)
		}
	}
}

// The recorded digests are one release's SHA256SUMS and cover every platform the download
// supports, so the platform table and the release matrix cannot drift apart.
func TestRecordedDigestsCoverEveryDownloadPlatform(t *testing.T) {
	versions := map[string]bool{}
	scanner := bufio.NewScanner(bytes.NewReader(releaseDigests))
	for scanner.Scan() {
		_, name, _ := strings.Cut(scanner.Text(), "  ")
		versions[releaseOf(name)] = true
	}
	if len(versions) != 1 {
		t.Fatalf("SHA256SUMS names releases %v, want one", versions)
	}
	for version := range versions {
		for platform := range releaseTargets {
			asset, err := assetFor(platform, version)
			if err != nil {
				t.Fatal(err)
			}
			if _, err := recordedDigest(releaseDigests, asset.archive); err != nil {
				t.Errorf("%s: %v", platform, err)
			}
		}
	}
}
