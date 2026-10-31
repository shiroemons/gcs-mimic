package fakestorage

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"strings"
	"time"
)

func (s *Server) configureTransport(ctx context.Context, options Options) error {
	s.transport = &http.Transport{Proxy: nil, MaxIdleConnsPerHost: 4,
		IdleConnTimeout: 30 * time.Second, ResponseHeaderTimeout: 30 * time.Second,
		DialContext: (&net.Dialer{Timeout: 5 * time.Second}).DialContext}
	if options.NoListener {
		s.transport.MaxConnsPerHost = 32
		s.transport.DialContext = func(ctx context.Context, _, _ string) (net.Conn, error) {
			return (&net.Dialer{Timeout: 5 * time.Second}).DialContext(ctx, "unix", s.socketPath)
		}
		return nil
	}
	if s.endpoint.Scheme != "https" {
		return nil
	}
	var pem []byte
	var err error
	if options.CertificateLocation != "" {
		var file *os.File
		file, err = os.Open(options.CertificateLocation)
		if err == nil {
			pem, err = io.ReadAll(io.LimitReader(file, (1<<20)+1))
			err = errors.Join(err, file.Close())
		}
	} else {
		trustCtx, cancel := context.WithTimeout(ctx, 10*time.Second)
		defer cancel()
		command := exec.CommandContext(trustCtx, executable(options), "tls", "ca")
		command.Env = childEnvironment()
		var output boundedOutput
		command.Stdout = &output
		err = command.Run()
		pem = output.data
	}
	if err != nil {
		return fmt.Errorf("load local TLS trust: %w", err)
	}
	pool := x509.NewCertPool()
	if len(pem) > 1<<20 || !pool.AppendCertsFromPEM(pem) {
		return errors.New("local TLS trust must contain at most 1 MiB of PEM certificates")
	}
	s.transport.TLSClientConfig = &tls.Config{MinVersion: tls.VersionTLS12, RootCAs: pool}
	return nil
}

type boundedOutput struct{ data []byte }

func (w *boundedOutput) Write(p []byte) (int, error) {
	if len(w.data)+len(p) > 1<<20 {
		return 0, errors.New("TLS trust output exceeded 1 MiB")
	}
	w.data = append(w.data, p...)
	return len(p), nil
}

type localTransport struct{ server *Server }

func (t *localTransport) RoundTrip(request *http.Request) (*http.Response, error) {
	s := t.server
	select {
	case <-s.closing:
		return nil, errors.New("gcs-mimic facade is closed")
	case <-s.done:
		return nil, errors.New("gcs-mimic subprocess exited")
	default:
	}
	if !s.allowedURL(request.URL) {
		return nil, fmt.Errorf("refusing request outside the emulator facade: %s://%s", request.URL.Scheme, request.URL.Host)
	}
	forward := request.Clone(request.Context())
	target := *request.URL
	target.Scheme, target.Host = s.endpoint.Scheme, s.endpoint.Host
	forward.URL = &target
	if forward.Host == "" {
		forward.Host = request.URL.Host
	}
	return s.transport.RoundTrip(forward)
}

func (s *Server) allowedURL(target *url.URL) bool {
	if target == nil || target.User != nil || target.Fragment != "" || (target.Scheme != "http" && target.Scheme != "https") {
		return false
	}
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
