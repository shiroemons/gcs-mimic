package fakestorage

import (
	"context"
	"fmt"
	"os/exec"
	"strconv"
	"strings"
	"time"
)

// moduleVersion is the gcs-mimic release this package belongs to. It equals the package
// version in the repository's Cargo.toml, which scripts/check-release-recipes.sh enforces.
const moduleVersion = "0.20.0"

// UnsupportedBinaryError reports a gcs-mimic executable outside the releases this package
// supports. The package passes options and reads ready fields of its own release, so another
// release fails here, naming both versions, instead of later on an option or a route.
type UnsupportedBinaryError struct {
	Path    string // the executable that was started
	Version string // the version it reported; empty when it reported none
}

func (e *UnsupportedBinaryError) Error() string {
	reported := "no version"
	if e.Version != "" {
		reported = "version " + e.Version
	}
	return fmt.Sprintf("gcs-mimic at %s reported %s; fakestorage %s supports %s",
		e.Path, reported, moduleVersion, mustParseRelease(moduleVersion).supportedRange())
}

// release is a gcs-mimic version: MAJOR.MINOR.PATCH or MAJOR.MINOR.PATCH-rc.N with N >= 1,
// without leading zeros or build metadata, as xtask/src/release/version.rs accepts.
type release struct {
	major, minor, patch int
	candidate           int // 0 for a stable release
}

func parseRelease(value string) (release, error) {
	invalid := fmt.Errorf("invalid gcs-mimic version %q", value)
	core, candidate, isCandidate := strings.Cut(value, "-rc.")
	parts := strings.Split(core, ".")
	if len(parts) != 3 {
		return release{}, invalid
	}
	var numbers [3]int
	for i, part := range parts {
		n, ok := number(part)
		if !ok {
			return release{}, invalid
		}
		numbers[i] = n
	}
	r := release{major: numbers[0], minor: numbers[1], patch: numbers[2]}
	if isCandidate {
		n, ok := number(candidate)
		if !ok || n == 0 {
			return release{}, invalid
		}
		r.candidate = n
	}
	return r, nil
}

// number parses ASCII digits without a sign or a leading zero.
func number(value string) (int, bool) {
	if value == "" || (len(value) > 1 && value[0] == '0') ||
		strings.TrimLeft(value, "0123456789") != "" {
		return 0, false
	}
	n, err := strconv.Atoi(value)
	return n, err == nil
}

func mustParseRelease(value string) release {
	r, err := parseRelease(value)
	if err != nil {
		panic(err)
	}
	return r
}

// supports reports whether a binary of release b accepts what this package sends. A 0.x minor
// release may break, so 0.x needs the same minor; from 1.0.0 a later minor of the same major is
// compatible. Patches and candidates never change the surface the package uses.
func (m release) supports(b release) bool {
	if m.major == 0 {
		return b.major == 0 && b.minor == m.minor
	}
	return b.major == m.major && b.minor >= m.minor
}

func (m release) supportedRange() string {
	if m.major == 0 {
		return fmt.Sprintf("%d.%d.x", m.major, m.minor)
	}
	return fmt.Sprintf("%d.x from %d.%d.0", m.major, m.major, m.minor)
}

// checkBinary accepts the version a started binary reported in its ready event.
func checkBinary(path, version string) error {
	if r, err := parseRelease(version); err == nil && mustParseRelease(moduleVersion).supports(r) {
		return nil
	}
	return &UnsupportedBinaryError{Path: path, Version: version}
}

// versionProbeTimeout bounds the one `--version` call made after a failed start.
const versionProbeTimeout = 5 * time.Second

// probeBinary asks an executable that exited before readiness for its version. A binary of
// another release can refuse the options this package passes before it reports anything, so
// its version is then the likely cause. It returns nil when the version is supported or
// cannot be read, leaving the exit error as the explanation.
func probeBinary(ctx context.Context, path string) error {
	ctx, cancel := context.WithTimeout(ctx, versionProbeTimeout)
	defer cancel()
	cmd := exec.CommandContext(ctx, path, "--version")
	cmd.Env = childEnvironment()
	cmd.WaitDelay = time.Second
	output, err := cmd.Output()
	if err != nil {
		return nil
	}
	version, found := strings.CutPrefix(strings.TrimSpace(string(output)), "gcs-mimic ")
	if !found {
		return nil
	}
	if _, err := parseRelease(version); err != nil {
		return nil
	}
	return checkBinary(path, version)
}
