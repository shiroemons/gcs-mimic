package fakestorage

import (
	"net/url"
	"strings"
)

// allowedURL accepts the origins a fake-gcs-server client may address: the instance, its
// advertised and public URLs, the Google API hosts the official SDK defaults to, and
// virtual-host bucket names under the public host. The shared transport forwards each of them
// to the subprocess with its original Host.
func (s *Server) allowedURL(target *url.URL) bool {
	for _, base := range []string{s.endpoint.String(), s.url, s.publicURL,
		"https://storage.googleapis.com", "https://www.googleapis.com"} {
		parsed, err := url.Parse(base)
		if err == nil && parsed.Scheme == target.Scheme && parsed.Host == target.Host {
			return true
		}
	}
	public, err := url.Parse(s.publicURL)
	return err == nil && target.Scheme == public.Scheme && target.Port() == public.Port() &&
		strings.HasSuffix(target.Hostname(), "."+public.Hostname())
}
