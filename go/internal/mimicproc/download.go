package mimicproc

import (
	"archive/tar"
	"archive/zip"
	"bufio"
	"bytes"
	"compress/gzip"
	"context"
	"crypto/sha256"
	_ "embed" // releaseDigests
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"os/exec"
	"path"
	"path/filepath"
	"runtime"
	"strings"
	"sync"
	"time"
)

// releaseDigests is the SHA256SUMS file of the GitHub release this module downloads. The Go
// checksum database fixes the module's content, so a release asset swapped on GitHub cannot
// bring a matching digest along.
//
//go:embed SHA256SUMS
var releaseDigests []byte

const (
	releaseBaseURL  = "https://github.com/shiroemons/gcs-mimic/releases/download"
	downloadTimeout = 5 * time.Minute
	// maxDownloadBytes bounds the archive and the executable taken from it. Release archives
	// are a few megabytes; the bound only keeps a misbehaving server from filling the disk.
	maxDownloadBytes = 256 << 20
)

// installMu keeps the parallel tests of one process from downloading the same archive at once.
// Separate processes, such as the packages of `go test ./...`, can still race; each installs
// through its own temporary file and a rename, so whichever rename wins leaves a whole file.
var installMu sync.Mutex

var errMissingMember = errors.New("the archive has no such file")

// Resolve returns the gcs-mimic executable to start. An explicit path, then GCSM_BINARY, is used
// as given, so a binary of another release still fails with UnsupportedBinaryError. Otherwise
// gcs-mimic on PATH is used when it reports a supported release; failing that, the release this
// module belongs to is downloaded once from GitHub Releases, checked against the SHA-256 recorded
// in SHA256SUMS, and kept in the user cache directory. GCSM_DOWNLOAD=off turns the download off
// and keeps the earlier lookup: gcs-mimic on PATH, whatever its release.
func Resolve(ctx context.Context, explicit string) (string, error) {
	return defaultResolver().resolve(ctx, explicit)
}

// resolver holds what Resolve takes from the machine, so tests can replace each part.
type resolver struct {
	lookPath func(string) (string, error)
	cacheDir func() (string, error)
	client   *http.Client
	baseURL  string // the release download URL without the tag
	digests  []byte // a SHA256SUMS file
	platform string // GOOS/GOARCH
}

func defaultResolver() resolver {
	return resolver{
		lookPath: exec.LookPath,
		cacheDir: os.UserCacheDir,
		client:   http.DefaultClient,
		baseURL:  releaseBaseURL,
		digests:  releaseDigests,
		platform: runtime.GOOS + "/" + runtime.GOARCH,
	}
}

func (r resolver) resolve(ctx context.Context, explicit string) (string, error) {
	if explicit != "" {
		return explicit, nil
	}
	if binary := os.Getenv("GCSM_BINARY"); binary != "" {
		return binary, nil
	}
	download, err := downloadEnabled()
	if err != nil {
		return "", err
	}
	if !download {
		return "gcs-mimic", nil
	}
	var skipped string
	if onPath, err := r.lookPath("gcs-mimic"); err == nil {
		version, ok := reportedVersion(ctx, onPath)
		if ok && supportsRelease(version) {
			return onPath, nil
		}
		if ok {
			version = "version " + version
		} else {
			version = "no version"
		}
		skipped = fmt.Sprintf("skipped gcs-mimic at %s, which reported %s; ", onPath, version)
	}
	executable, err := r.install(ctx)
	if err != nil {
		return "", fmt.Errorf("%scannot download gcs-mimic %s: %w; set GCSM_BINARY or put gcs-mimic %s on PATH "+
			"(see go/README.md in the gcs-mimic repository)", skipped, ModuleVersion, err,
			mustParseRelease(ModuleVersion).supportedRange())
	}
	return executable, nil
}

// downloadEnabled reads GCSM_DOWNLOAD: unset or "on" downloads a missing binary, and "off"
// keeps the lookup to GCSM_BINARY and PATH, for machines that must not reach GitHub.
func downloadEnabled() (bool, error) {
	switch value := os.Getenv("GCSM_DOWNLOAD"); value {
	case "", "on":
		return true, nil
	case "off":
		return false, nil
	default:
		return false, fmt.Errorf("GCSM_DOWNLOAD must be on or off, not %q", value)
	}
}

// releaseAsset names one platform's archive in a gcs-mimic release.
type releaseAsset struct {
	target     string // the Rust target of the release matrix
	archive    string // gcs-mimic-VERSION-TARGET.tar.gz, or .zip on Windows
	member     string // the executable's path inside the archive
	executable string // gcs-mimic, or gcs-mimic.exe on Windows
}

// releaseTargets maps Go platforms to the release matrix. Linux takes the static musl builds,
// which run on any distribution; the glibc builds need Ubuntu 24.04's glibc or a newer one.
var releaseTargets = map[string]string{
	"darwin/amd64":  "x86_64-apple-darwin",
	"darwin/arm64":  "aarch64-apple-darwin",
	"linux/amd64":   "x86_64-unknown-linux-musl",
	"linux/arm64":   "aarch64-unknown-linux-musl",
	"windows/amd64": "x86_64-pc-windows-msvc",
	"windows/arm64": "aarch64-pc-windows-msvc",
}

func assetFor(platform, version string) (releaseAsset, error) {
	target, ok := releaseTargets[platform]
	if !ok {
		return releaseAsset{}, fmt.Errorf("gcs-mimic publishes no release archive for %s", platform)
	}
	name := "gcs-mimic-" + version + "-" + target
	asset := releaseAsset{target: target, archive: name + ".tar.gz", executable: "gcs-mimic"}
	if strings.HasPrefix(platform, "windows/") {
		asset.archive, asset.executable = name+".zip", "gcs-mimic.exe"
	}
	asset.member = name + "/" + asset.executable
	return asset, nil
}

// recordedDigest returns the SHA-256 that sums records for archive. A module revision whose
// SHA256SUMS belongs to another release, such as main between the preparation of a release and
// the commit that records its digests, has none.
func recordedDigest(sums []byte, archive string) ([]byte, error) {
	scanner := bufio.NewScanner(bytes.NewReader(sums))
	for scanner.Scan() {
		digest, name, found := strings.Cut(scanner.Text(), "  ")
		if !found || name != archive {
			continue
		}
		if sum, err := hex.DecodeString(digest); err == nil && len(sum) == sha256.Size {
			return sum, nil
		}
	}
	return nil, fmt.Errorf("this revision of the Go module records no SHA-256 for %s", archive)
}

// install returns the cached executable of ModuleVersion for this platform, downloading and
// checking its release archive first when the cache lacks it.
func (r resolver) install(ctx context.Context) (string, error) {
	asset, err := assetFor(r.platform, ModuleVersion)
	if err != nil {
		return "", err
	}
	digest, err := recordedDigest(r.digests, asset.archive)
	if err != nil {
		return "", err
	}
	root, err := r.cacheDir()
	if err != nil {
		return "", fmt.Errorf("locate the user cache directory: %w", err)
	}
	dir := filepath.Join(root, "gcs-mimic", ModuleVersion, asset.target)
	executable := filepath.Join(dir, asset.executable)
	installMu.Lock()
	defer installMu.Unlock()
	if installed(executable) {
		return executable, nil
	}
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return "", fmt.Errorf("create the download cache: %w", err)
	}
	ctx, cancel := context.WithTimeout(ctx, downloadTimeout)
	defer cancel()
	archive, err := r.fetch(ctx, dir, asset.archive, digest)
	if err != nil {
		return "", err
	}
	defer removeTemporary(archive)
	if err := extract(archive, asset, executable); err != nil {
		return "", err
	}
	return executable, nil
}

func installed(executable string) bool {
	info, err := os.Stat(executable)
	return err == nil && info.Mode().IsRegular()
}

// fetch downloads archive into a temporary file in dir and returns it, positioned at its start,
// once its SHA-256 equals digest. The caller removes the file.
func (r resolver) fetch(ctx context.Context, dir, archive string, digest []byte) (*os.File, error) {
	url := r.baseURL + "/v" + ModuleVersion + "/" + archive
	file, err := os.CreateTemp(dir, ".download-*")
	if err != nil {
		return nil, fmt.Errorf("create the download file: %w", err)
	}
	if err := r.fetchInto(ctx, url, file, digest); err != nil {
		removeTemporary(file)
		return nil, fmt.Errorf("GET %s: %w", url, err)
	}
	return file, nil
}

func (r resolver) fetchInto(ctx context.Context, url string, file *os.File, digest []byte) error {
	request, err := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	if err != nil {
		return err
	}
	response, err := r.client.Do(request)
	if err != nil {
		return err
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		return errors.New(response.Status)
	}
	hash := sha256.New()
	if err := copyBounded(io.MultiWriter(file, hash), response.Body); err != nil {
		return err
	}
	if sum := hash.Sum(nil); !bytes.Equal(sum, digest) {
		return fmt.Errorf("SHA-256 %x differs from the recorded %x", sum, digest)
	}
	_, err = file.Seek(0, io.SeekStart)
	return err
}

// extract copies asset's executable from archive to executable through a temporary file and a
// rename, so that another process never starts a partly written file.
func extract(archive *os.File, asset releaseAsset, executable string) error {
	temporary, err := os.CreateTemp(filepath.Dir(executable), ".gcs-mimic-*")
	if err != nil {
		return fmt.Errorf("create the executable: %w", err)
	}
	defer removeTemporary(temporary)
	if strings.HasSuffix(asset.archive, ".zip") {
		err = copyZipMember(temporary, archive, asset.member)
	} else {
		err = copyTarMember(temporary, archive, asset.member)
	}
	if err == nil {
		err = temporary.Chmod(0o755)
	}
	if err == nil {
		err = temporary.Close()
	}
	if err != nil {
		return fmt.Errorf("extract %s from %s: %w", asset.member, asset.archive, err)
	}
	if err := os.Rename(temporary.Name(), executable); err != nil {
		// Windows refuses to replace an executable that another test process is running. That
		// process installed the same verified file, so it serves this one as well.
		if installed(executable) {
			return nil
		}
		return fmt.Errorf("install %s: %w", executable, err)
	}
	return nil
}

func copyTarMember(dst io.Writer, archive io.Reader, member string) error {
	decompressed, err := gzip.NewReader(archive)
	if err != nil {
		return err
	}
	entries := tar.NewReader(decompressed)
	for {
		header, err := entries.Next()
		if errors.Is(err, io.EOF) {
			return errMissingMember
		}
		if err != nil {
			return err
		}
		if header.Typeflag == tar.TypeReg && path.Clean(header.Name) == member {
			return copyBounded(dst, entries)
		}
	}
}

func copyZipMember(dst io.Writer, archive *os.File, member string) error {
	info, err := archive.Stat()
	if err != nil {
		return err
	}
	entries, err := zip.NewReader(archive, info.Size())
	if err != nil {
		return err
	}
	for _, entry := range entries.File {
		// Archives written on Windows may separate their paths with backslashes.
		if entry.Mode().IsRegular() && path.Clean(strings.ReplaceAll(entry.Name, `\`, "/")) == member {
			return copyZipEntry(dst, entry)
		}
	}
	return errMissingMember
}

func copyZipEntry(dst io.Writer, entry *zip.File) error {
	src, err := entry.Open()
	if err != nil {
		return err
	}
	defer src.Close()
	return copyBounded(dst, src)
}

func copyBounded(dst io.Writer, src io.Reader) error {
	n, err := io.Copy(dst, io.LimitReader(src, maxDownloadBytes+1))
	if err == nil && n > maxDownloadBytes {
		err = fmt.Errorf("larger than %d bytes", maxDownloadBytes)
	}
	return err
}

// removeTemporary closes and deletes a temporary file of the download cache. A failure leaves
// only a dot-file beside the cached executable, which no lookup reads, so it is not reported.
func removeTemporary(file *os.File) {
	_ = file.Close()
	_ = os.Remove(file.Name())
}
