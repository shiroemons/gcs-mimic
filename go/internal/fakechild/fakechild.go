// Package fakechild lets a Go test binary stand in for gcs-mimic, so the packages' lifecycle
// tests need no Rust build or shell. A test starts its own executable with the serve arguments
// the package builds; TestMain then runs it as the child instead of running the tests.
//
// The child answers the ready protocol on a loopback listener or the Unix socket it is given,
// echoes request bodies, and stops on os.Interrupt. A mode selects a misbehavior: the host part
// of --listen (fakestorage tests set Options.Host) or, for a package that always listens on
// 127.0.0.1, the GCSM_TEST_CHILD_MODE variable.
package fakechild

import (
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"os/signal"
	"strings"
	"testing"
	"time"
)

// Modes.
const (
	ExitBeforeReady = "exit-before-ready" // writes a diagnostic to stderr and exits with 17
	NeverReady      = "never-ready"       // waits for an interrupt without becoming ready
	StallFixture    = "stall-fixture"     // never answers POST /_internal/objects
	IgnoreInterrupt = "ignore-interrupt"  // keeps running after an interrupt
)

// Main runs the binary as the child when a test started it as one, and otherwise runs the
// tests. version is what the child reports unless GCSM_TEST_CHILD_VERSION overrides it, where
// "none" leaves the version out of the ready event.
func Main(m *testing.M, version string) {
	if override := os.Getenv("GCSM_TEST_CHILD_VERSION"); override != "" {
		version = override
	}
	if len(os.Args) > 1 && os.Args[1] == "serve" {
		serve(version)
		return
	}
	if len(os.Args) == 2 && os.Args[1] == "--version" {
		fmt.Println("gcs-mimic " + version)
		return
	}
	os.Exit(m.Run())
}

func serve(version string) {
	listen := ""
	socketPath := ""
	for index, arg := range os.Args {
		if arg == "--listen" {
			listen = os.Args[index+1]
		}
		if arg == "--unix-socket" {
			socketPath = os.Args[index+1]
		}
	}
	mode, _, _ := strings.Cut(listen, ":")
	if env := os.Getenv("GCSM_TEST_CHILD_MODE"); env != "" {
		mode = env
	}
	if mode == ExitBeforeReady {
		fmt.Fprintln(os.Stderr, `{"level":"error","message":"fake child refused its options"}`)
		os.Exit(17)
	}
	signals := make(chan os.Signal, 1)
	signal.Notify(signals, os.Interrupt)
	if mode == NeverReady {
		<-signals
		return
	}
	network, address := "tcp", "127.0.0.1:0"
	if socketPath != "" {
		network, address = "unix", socketPath
	}
	listener, err := net.Listen(network, address)
	if err != nil {
		panic(err)
	}
	if socketPath != "" {
		if err := os.Chmod(socketPath, 0600); err != nil {
			panic(err)
		}
	}
	server := &http.Server{Handler: http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if mode == StallFixture && r.URL.Path == "/_internal/objects" {
			<-r.Context().Done()
			return
		}
		if r.URL.Path == "/redirect" {
			http.Redirect(w, r, "http://metadata.google.internal/latest/meta-data/", http.StatusFound)
			return
		}
		if r.URL.Path == "/wait" {
			<-r.Context().Done()
			return
		}
		w.Header().Set("X-Test-Host", r.Host)
		_, _ = io.Copy(w, r.Body)
	}), ReadHeaderTimeout: time.Second}
	go func() { _ = server.Serve(listener) }()
	ready := map[string]string{"event": "ready", "listen": listener.Addr().String(), "transport": "tcp"}
	if version != "none" {
		ready["version"] = version
	}
	if socketPath != "" {
		ready["listen"] = ""
		ready["transport"] = "unix"
		ready["unix_socket"] = socketPath
	}
	_ = json.NewEncoder(os.Stdout).Encode(ready)
	if mode == IgnoreInterrupt {
		for range signals {
		}
	}
	<-signals
	_ = server.Close()
}
