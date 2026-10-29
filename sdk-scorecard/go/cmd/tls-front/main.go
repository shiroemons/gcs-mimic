// Command tls-front gives every scorecard product the same verified TLS origin.
package main

import (
	"crypto/tls"
	"errors"
	"flag"
	"fmt"
	"log"
	"net/http"
	"net/http/httputil"
	"net/url"
	"time"
)

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
	proxy := frontend(backend, origin)
	server := &http.Server{Addr: origin.Host, ReadHeaderTimeout: 10 * time.Second,
		TLSConfig: &tls.Config{MinVersion: tls.VersionTLS12},
		Handler: http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			if r.Host != origin.Host {
				http.Error(w, "unexpected frontend Host", http.StatusBadRequest)
				return
			}
			proxy.ServeHTTP(w, r)
		})}
	fmt.Println("Scorecard TLS frontend ready")
	log.Fatal(server.ListenAndServeTLS(*cert, *key))
}
