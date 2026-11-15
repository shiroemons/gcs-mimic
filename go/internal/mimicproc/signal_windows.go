//go:build windows

package mimicproc

import (
	"os"
	"os/exec"
	"syscall"
)

// prepare starts the child in a process group of its own. Windows cannot deliver os.Interrupt
// to another process, so Stop sends a console control event instead, and a new group lets that
// event reach the child without also reaching the test process that shares its console.
func prepare(cmd *exec.Cmd) {
	cmd.SysProcAttr = &syscall.SysProcAttr{CreationFlags: syscall.CREATE_NEW_PROCESS_GROUP}
}

// kernel32.dll is a known DLL, which Windows always loads from the system directory.
var generateConsoleCtrlEvent = syscall.NewLazyDLL("kernel32.dll").NewProc("GenerateConsoleCtrlEvent")

// interrupt sends CTRL_BREAK to the child's process group, whose ID is the child's process ID.
// gcs-mimic stops gracefully on it, and a Go child receives it as os.Interrupt. CTRL_C cannot
// be used: a new process group starts with CTRL_C disabled.
func interrupt(process *os.Process) error {
	ok, _, err := generateConsoleCtrlEvent.Call(syscall.CTRL_BREAK_EVENT, uintptr(process.Pid))
	if ok == 0 {
		return os.NewSyscallError("GenerateConsoleCtrlEvent", err)
	}
	return nil
}
