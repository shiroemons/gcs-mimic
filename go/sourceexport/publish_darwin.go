package sourceexport

import "golang.org/x/sys/unix"

func publish(stage, destination string) error {
	return unix.RenameatxNp(unix.AT_FDCWD, stage, unix.AT_FDCWD, destination, unix.RENAME_EXCL)
}
