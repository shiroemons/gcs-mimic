package mimicproc

import (
	"fmt"
	"os"
	"strings"
)

// Serve is the part of the serve command line that both packages build alike. Each package
// chooses the profile and the scheme; the packages always run a permissive instance with
// local administration enabled, because their helpers write fixtures through /_internal.
type Serve struct {
	CompatProfile string // empty keeps the native profile
	Project       string // --seed-project, the project of the buckets the helpers create
	Listen        string // host:port of the TCP listener; unused with SocketPath
	SocketPath    string // a private HTTP-over-Unix socket instead of a TCP listener
	Scheme        string // scheme of the TCP listener; a Unix socket always serves HTTP
	DataDir       string // a caller-owned disk catalog; empty selects the memory backend
}

// Args returns the arguments after the executable, starting with the subcommand.
func (s Serve) Args() []string {
	args := []string{"serve"}
	if s.CompatProfile != "" {
		args = append(args, "--compat-profile", s.CompatProfile)
	}
	args = append(args, "--auth-mode", "permissive", "--seed-project", s.Project, "--admin-enabled", "true")
	if s.SocketPath != "" {
		args = append(args, "--unix-socket", s.SocketPath, "--scheme", "http")
	} else {
		args = append(args, "--listen", s.Listen, "--scheme", s.endpointScheme())
	}
	if s.DataDir == "" {
		args = append(args, "--backend", "memory")
	} else {
		args = append(args, "--backend", "disk", "--data-dir", s.DataDir)
	}
	return args
}

func (s Serve) endpointScheme() string {
	if s.SocketPath != "" || s.Scheme == "" {
		return "http"
	}
	return s.Scheme
}

// CheckRenamedBinary refuses the former GCS_MIMIC_BINARY when nothing else selects a binary.
// The child environment drops every GCS_MIMIC_* name, so the old variable would otherwise be
// ignored silently. option names the package's own way to select the executable.
func CheckRenamedBinary(explicit, option string) error {
	if explicit != "" || os.Getenv("GCSM_BINARY") != "" {
		return nil
	}
	if _, exists := os.LookupEnv("GCS_MIMIC_BINARY"); exists {
		return fmt.Errorf("GCS_MIMIC_BINARY was renamed to GCSM_BINARY; rename the variable or set %s", option)
	}
	return nil
}

// childEnvironment drops inherited server settings: each package fully configures its child,
// and a GCS_MIMIC_* variable of the parent would otherwise override that configuration.
// GCSM_* client variables pass through.
func childEnvironment() []string {
	var env []string
	for _, value := range os.Environ() {
		name, _, _ := strings.Cut(value, "=")
		if !strings.HasPrefix(name, "GCS_MIMIC_") {
			env = append(env, value)
		}
	}
	return env
}
