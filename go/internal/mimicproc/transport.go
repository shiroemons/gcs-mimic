package mimicproc

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
	"time"
)

func (p *Process) configureTransport(ctx context.Context, spec Spec) error {
	p.transport = &http.Transport{Proxy: nil, MaxIdleConnsPerHost: 4,
		IdleConnTimeout: 30 * time.Second, ResponseHeaderTimeout: 30 * time.Second,
		DialContext: (&net.Dialer{Timeout: 5 * time.Second}).DialContext}
	if p.socketPath != "" {
		p.transport.MaxConnsPerHost = 32
		p.transport.DialContext = func(ctx context.Context, _, _ string) (net.Conn, error) {
			return (&net.Dialer{Timeout: 5 * time.Second}).DialContext(ctx, "unix", p.socketPath)
		}
		return nil
	}
	if p.endpoint.Scheme != "https" {
		return nil
	}
	var pem []byte
	var err error
	if spec.CertificateFile != "" {
		var file *os.File
		file, err = os.Open(spec.CertificateFile)
		if err == nil {
			pem, err = io.ReadAll(io.LimitReader(file, (1<<20)+1))
			err = errors.Join(err, file.Close())
		}
	} else {
		trustCtx, cancel := context.WithTimeout(ctx, 10*time.Second)
		defer cancel()
		command := exec.CommandContext(trustCtx, spec.Executable, "tls", "ca")
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
	p.transport.TLSClientConfig = &tls.Config{MinVersion: tls.VersionTLS12, RootCAs: pool}
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

// Transport is the connection pool to the child, for a package that proxies requests itself.
func (p *Process) Transport() *http.Transport { return p.transport }

// RoundTripper forwards to the child the requests whose origin allowed accepts, keeping their
// Host, and refuses every other request before dialing. A client therefore cannot reach Google
// or the metadata server by accident, whatever URL an SDK builds or a redirect names.
func (p *Process) RoundTripper(allowed func(*url.URL) bool) http.RoundTripper {
	return &guard{process: p, allowed: allowed}
}

type guard struct {
	process *Process
	allowed func(*url.URL) bool
}

func (g *guard) RoundTrip(request *http.Request) (*http.Response, error) {
	p := g.process
	select {
	case <-p.closing:
		return nil, errors.New("gcs-mimic facade is closed")
	case <-p.done:
		return nil, errors.New("gcs-mimic subprocess exited")
	default:
	}
	target := request.URL
	if target == nil {
		return nil, errors.New("refusing a request without a URL")
	}
	if !plainOrigin(target) || !g.allowed(target) {
		return nil, fmt.Errorf("refusing request outside the emulator facade: %s://%s", target.Scheme, target.Host)
	}
	forward := request.Clone(request.Context())
	rewritten := *target
	rewritten.Scheme, rewritten.Host = p.endpoint.Scheme, p.endpoint.Host
	forward.URL = &rewritten
	if forward.Host == "" {
		forward.Host = target.Host
	}
	return p.transport.RoundTrip(forward)
}

// plainOrigin refuses what no emulator origin carries: credentials, a fragment or a scheme
// other than HTTP.
func plainOrigin(target *url.URL) bool {
	return target.User == nil && target.Fragment == "" && (target.Scheme == "http" || target.Scheme == "https")
}
