package mimicproc

import (
	"bytes"
	"io"
	"mime/multipart"
	"net/textproto"
	"strings"
)

// FixtureBody frames one POST /_internal/objects request as multipart/related: the JSON
// metadata part, then content as the media part. It returns the body and its Content-Type.
// Only the framing is buffered; content streams through, so a fixture never sits in memory.
func FixtureBody(metadata []byte, content io.Reader) (io.Reader, string, error) {
	var prefix bytes.Buffer
	writer := multipart.NewWriter(&prefix)
	part, err := writer.CreatePart(textproto.MIMEHeader{"Content-Type": {"application/json"}})
	if err != nil {
		return nil, "", err
	}
	if _, err := part.Write(metadata); err != nil {
		return nil, "", err
	}
	if _, err := writer.CreatePart(textproto.MIMEHeader{"Content-Type": {"application/octet-stream"}}); err != nil {
		return nil, "", err
	}
	boundary := writer.Boundary()
	return io.MultiReader(bytes.NewReader(prefix.Bytes()), content,
		strings.NewReader("\r\n--"+boundary+"--\r\n")), "multipart/related; boundary=" + boundary, nil
}
