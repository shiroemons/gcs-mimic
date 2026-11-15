//go:build !windows

package mimicproc

import (
	"os"
	"os/exec"
)

func prepare(*exec.Cmd) {}

// interrupt sends SIGINT, which gcs-mimic answers with a graceful shutdown.
func interrupt(process *os.Process) error {
	return process.Signal(os.Interrupt)
}
