package mimicproc

import (
	"context"
	"os"
	"slices"
	"strings"
	"testing"

	"github.com/shiroemons/gcs-mimic/go/internal/fakechild"
)

func TestMain(m *testing.M) { fakechild.Main(m, ModuleVersion) }

func TestServeArgsLeaveTheProfileAndSchemeToTheCaller(t *testing.T) {
	for _, tc := range []struct {
		name  string
		serve Serve
		want  []string
	}{
		{"compatibility profile over HTTPS", Serve{CompatProfile: "fake-gcs-server", Project: "local-project", Listen: "127.0.0.1:0", Scheme: "https"},
			[]string{"serve", "--compat-profile", "fake-gcs-server", "--auth-mode", "permissive", "--seed-project", "local-project",
				"--admin-enabled", "true", "--listen", "127.0.0.1:0", "--scheme", "https", "--backend", "memory"}},
		{"native profile over HTTP on disk", Serve{Project: "other-project", Listen: "127.0.0.1:0", Scheme: "http", DataDir: "data"},
			[]string{"serve", "--auth-mode", "permissive", "--seed-project", "other-project", "--admin-enabled", "true",
				"--listen", "127.0.0.1:0", "--scheme", "http", "--backend", "disk", "--data-dir", "data"}},
		{"private socket", Serve{CompatProfile: "fake-gcs-server", Project: "local-project", Listen: "unused", SocketPath: "s", Scheme: "https"},
			[]string{"serve", "--compat-profile", "fake-gcs-server", "--auth-mode", "permissive", "--seed-project", "local-project",
				"--admin-enabled", "true", "--unix-socket", "s", "--scheme", "http", "--backend", "memory"}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if got := tc.serve.Args(); !slices.Equal(got, tc.want) {
				t.Fatalf("args = %q\nwant   %q", got, tc.want)
			}
		})
	}
}

func testSpec() Spec {
	return Spec{Product: "mimicproc", Executable: os.Args[0],
		Serve: Serve{Project: "local-project", Listen: "127.0.0.1:0", Scheme: "http"}}
}

func TestStopEndsTheChildAndRepeatsItsResult(t *testing.T) {
	p, err := Start(context.Background(), testSpec())
	if err != nil {
		t.Fatal(err)
	}
	if p.Version() != ModuleVersion || p.Endpoint().Scheme != "http" || p.Endpoint().Hostname() != "127.0.0.1" {
		t.Fatalf("ready: version %q, endpoint %v", p.Version(), p.Endpoint())
	}
	if err := p.Stop(); err != nil {
		t.Fatal(err)
	}
	select {
	case <-p.Done():
	default:
		t.Fatal("Stop returned before the child was reaped")
	}
	if err := p.Stop(); err != nil {
		t.Fatal(err)
	}
}

func TestAnEarlyExitReportsTheTailOfTheChildOutputOnlyWhenAsked(t *testing.T) {
	t.Setenv("GCSM_TEST_CHILD_MODE", fakechild.ExitBeforeReady)
	for _, tail := range []int{0, 4096, 16} {
		spec := testSpec()
		spec.OutputTail = tail
		_, err := Start(context.Background(), spec)
		if err == nil || !strings.Contains(err.Error(), "exited before readiness") {
			t.Fatalf("tail %d: %v", tail, err)
		}
		reported := strings.Contains(err.Error(), "its last output:")
		if reported != (tail > 0) {
			t.Fatalf("tail %d reported output %v: %v", tail, reported, err)
		}
		if tail == 4096 && !strings.Contains(err.Error(), "fake child refused its options") {
			t.Fatalf("tail lost the diagnostic: %v", err)
		}
		// The last 16 bytes are `d its options"}` and the newline the tail trims.
		if tail == 16 && !strings.Contains(err.Error(), `its last output: d its options"}`) {
			t.Fatalf("tail kept more than its limit: %v", err)
		}
	}
}
