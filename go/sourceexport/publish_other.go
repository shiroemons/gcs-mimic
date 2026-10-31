//go:build !darwin && !linux

package sourceexport

import "errors"

func publish(stage, destination string) error {
	return errors.New("memory snapshot publication currently requires macOS or Linux; no unsafe rename fallback is used")
}
