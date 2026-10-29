// Command tls-front gives every scorecard product the same verified TLS origin.
package main

import (
	"crypto/tls"
	"errors"
	"flag"
	"fmt"
	"io"
	"log"
	"net/http"
	"net/http/httputil"
	"net/url"
	"sync"
	"sync/atomic"
	"time"
)

// Transport closes its request body after an early response. Keep the inbound
// body for bounded lingering, serializing with any read already in progress.
type lingeringBody struct {
	body   io.ReadCloser
	mu     sync.Mutex
	closed atomic.Bool
}

func (b *lingeringBody) Read(p []byte) (int, error) {
	b.mu.Lock()
	defer b.mu.Unlock()
	if b.closed.Load() {
		return 0, io.ErrClosedPipe
	}
	return b.body.Read(p)
}

func (b *lingeringBody) Close() error {
	b.closed.Store(true)
	return nil
}

func frontendHandler(backend, origin *url.URL) http.Handler {
	proxy := frontend(backend, origin)
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Host != origin.Host {
			http.Error(w, "unexpected frontend Host", http.StatusBadRequest)
			return
		}
		controller := http.NewResponseController(w)
		if r.ProtoMajor != 1 {
			proxy.ServeHTTP(w, r)
			return
		}
		if err := controller.EnableFullDuplex(); err != nil {
			http.Error(w, "frontend duplex unavailable", http.StatusInternalServerError)
			return
		}
		body := &lingeringBody{body: r.Body}
		defer body.body.Close()
		proxied := new(http.Request)
		*proxied = *r
		proxied.Body = body
		proxy.ServeHTTP(w, proxied)
		// A fixed deadline and byte cap bound clients that keep sending after rejection.
		if err := controller.SetReadDeadline(time.Now().Add(2 * time.Second)); err != nil {
			return
		}
		if err := controller.Flush(); err != nil {
			return
		}
		body.closed.Store(true)
		body.mu.Lock()
		_, err := io.Copy(io.Discard, io.LimitReader(body.body, 32*1024*1024))
		body.mu.Unlock()
		if closeErr := body.body.Close(); err == nil {
			err = closeErr
		}
		if resetErr := controller.SetReadDeadline(time.Time{}); err == nil {
			err = resetErr
		}
		if err != nil {
			log.Print("frontend lingering upload ended before EOF")
		}
	})
}

func loopbackOrigin(raw string, scheme string) (*url.URL, error) {
	u, err := url.Parse(raw)
	if err != nil || u.Scheme != scheme || u.User != nil || u.Path != "" || u.RawQuery != "" || u.Fragment != "" ||
		(u.Hostname() != "127.0.0.1" && u.Hostname() != "::1") || u.Port() == "" {
		return nil, errors.New("expected explicit loopback origin and port")
	}
	return u, nil
}

func frontend(backend, origin *url.URL) *httputil.ReverseProxy {
	transport := http.DefaultTransport.(*http.Transport).Clone()
	transport.Proxy = nil
	transport.DisableCompression = true
	return &httputil.ReverseProxy{
		Transport: transport,
		Rewrite: func(r *httputil.ProxyRequest) {
			r.SetURL(backend)
			r.Out.Host = origin.Host
		},
		ModifyResponse: func(r *http.Response) error {
			if location := r.Header.Get("Location"); location != "" {
				u, err := url.Parse(location)
				if err != nil || u.User != nil || u.Fragment != "" {
					return errors.New("invalid backend Location")
				}
				if u.IsAbs() {
					if !((u.Scheme == backend.Scheme && u.Host == backend.Host) ||
						((u.Scheme == "http" || u.Scheme == origin.Scheme) && u.Host == origin.Host)) {
						return errors.New("backend Location escaped emulator")
					}
					u.Scheme, u.Host = origin.Scheme, origin.Host
				} else if u.Host != "" {
					return errors.New("backend Location escaped emulator")
				}
				r.Header.Set("Location", u.String())
			}
			return nil
		},
		ErrorHandler: func(w http.ResponseWriter, _ *http.Request, _ error) {
			// Transport errors can embed upload capability URLs.
			http.Error(w, "scorecard TLS frontend rejected backend response", http.StatusBadGateway)
		},
	}
}

func main() {
	listen := flag.String("origin", "", "HTTPS loopback origin")
	target := flag.String("backend", "", "HTTP loopback backend origin")
	cert := flag.String("cert", "", "PEM server certificate")
	key := flag.String("key", "", "PEM server key")
	flag.Parse()
	origin, err := loopbackOrigin(*listen, "https")
	if err != nil {
		log.Fatal(err)
	}
	backend, err := loopbackOrigin(*target, "http")
	if err != nil {
		log.Fatal(err)
	}
	server := &http.Server{Addr: origin.Host, ReadHeaderTimeout: 10 * time.Second,
		TLSConfig: &tls.Config{MinVersion: tls.VersionTLS12},
		Handler:   frontendHandler(backend, origin)}
	fmt.Println("Scorecard TLS frontend ready")
	log.Fatal(server.ListenAndServeTLS(*cert, *key))
}
