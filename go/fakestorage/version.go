package fakestorage

import "github.com/shiroemons/gcs-mimic/go/internal/mimicproc"

// UnsupportedBinaryError reports a gcs-mimic executable outside the releases this package
// supports, naming the executable and both versions. It is the same type as
// gcsmimic.UnsupportedBinaryError; detect it with errors.As.
type UnsupportedBinaryError = mimicproc.UnsupportedBinaryError
