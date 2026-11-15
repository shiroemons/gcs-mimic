// Package mimicproc starts, reaches and stops one gcs-mimic child process for the module's Go
// packages. fakestorage and gcsmimic share it so that the start protocol, the release check,
// the child environment and the stop sequence cannot drift apart between them.
package mimicproc

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"strings"
	"sync"
	"time"
)

// Spec is everything one package decides about its child.
type Spec struct {
	Product         string   // the package, named in version errors
	Executable      string   // the binary to start, already resolved with Executable
	Serve           Serve    // the shared part of the command line
	Extra           []string // package-specific arguments after the shared ones
	CertificateFile string   // TLS trust for an HTTPS listener; empty asks the binary for its CA
	// Output receives the child's standard output lines and standard error; nil discards them.
	Output io.Writer
	// OutputTail, when positive, keeps that many final bytes of the output so that an exit
	// before readiness can say why. The child usually explains a refused option on stderr.
	OutputTail int
}

const (
	startupTimeout  = 30 * time.Second
	shutdownTimeout = 10 * time.Second
)

// Process owns one started child.
type Process struct {
	cmd        *exec.Cmd
	done       chan struct{} // closed once the child is reaped
	exitErr    error         // set before done closes
	closing    chan struct{} // closed when Stop begins
	stopOnce   sync.Once
	stopErr    error
	endpoint   *url.URL
	version    string
	socketPath string
	transport  *http.Transport
}

// readyEvent holds the fields of gcs-mimic's ready line that the packages read.
type readyEvent struct {
	Event      string `json:"event"`
	Version    string `json:"version"`
	Listen     string `json:"listen"`
	Transport  string `json:"transport"`
	UnixSocket string `json:"unix_socket"`
}

// Start runs the child and returns once it is ready. ctx bounds the startup only; the child
// then runs until Stop. A failed start stops and reaps whatever it started.
func Start(ctx context.Context, spec Spec) (*Process, error) {
	p := &Process{done: make(chan struct{}), closing: make(chan struct{}), socketPath: spec.Serve.SocketPath}
	if err := p.start(ctx, spec); err != nil {
		return nil, errors.Join(err, p.Stop())
	}
	return p, nil
}

func (p *Process) start(ctx context.Context, spec Spec) error {
	p.cmd = exec.Command(spec.Executable, append(spec.Serve.Args(), spec.Extra...)...)
	p.cmd.Env = childEnvironment()
	prepare(p.cmd)
	logs := &lockedWriter{writer: spec.Output}
	if spec.OutputTail > 0 {
		logs.tail = &tailWriter{limit: spec.OutputTail}
	}
	p.cmd.Stderr = logs
	stdout, err := p.cmd.StdoutPipe()
	if err != nil {
		return fmt.Errorf("open child output: %w", err)
	}
	if err := p.cmd.Start(); err != nil {
		return fmt.Errorf("start gcs-mimic: %w", err)
	}
	ready := make(chan readyEvent, 1)
	go p.readOutput(stdout, logs, ready)
	timer := time.NewTimer(startupTimeout)
	defer timer.Stop()
	select {
	case event := <-ready:
		return p.accept(ctx, spec, event)
	case <-p.done:
		exited := errors.New("gcs-mimic exited before readiness")
		if p.exitErr != nil {
			exited = fmt.Errorf("gcs-mimic exited before readiness: %w", p.exitErr)
		}
		if tail := logs.tailText(); tail != "" {
			exited = fmt.Errorf("%w; its last output: %s", exited, tail)
		}
		return errors.Join(probeBinary(ctx, spec.Product, spec.Executable), exited)
	case <-ctx.Done():
		return ctx.Err()
	case <-timer.C:
		return errors.New("gcs-mimic startup exceeded 30 seconds")
	}
}

// readOutput copies the child's stdout to logs, reports the ready event, and reaps the child.
func (p *Process) readOutput(stdout io.Reader, logs *lockedWriter, ready chan<- readyEvent) {
	scanner := bufio.NewScanner(stdout)
	scanner.Buffer(make([]byte, 4096), 256<<10)
	var outputErr error
	for scanner.Scan() {
		line := scanner.Bytes()
		if _, outputErr = logs.Write(append(append([]byte(nil), line...), '\n')); outputErr != nil {
			break
		}
		var event readyEvent
		if json.Unmarshal(line, &event) == nil && event.Event == "ready" {
			select {
			case ready <- event:
			default:
			}
		}
	}
	if scanner.Err() != nil || outputErr != nil {
		_ = p.cmd.Process.Kill()
	}
	p.exitErr = errors.Join(scanner.Err(), outputErr, p.cmd.Wait())
	close(p.done)
}

// accept checks the ready event and opens the transport to the endpoint it names.
func (p *Process) accept(ctx context.Context, spec Spec, event readyEvent) error {
	if err := checkBinary(spec.Product, spec.Executable, event.Version); err != nil {
		return err
	}
	p.version = event.Version
	var err error
	if p.socketPath != "" {
		if event.Transport != "unix" || event.UnixSocket != p.socketPath || event.Listen != "" {
			return errors.New("NoListener child did not confirm the private Unix transport")
		}
		p.endpoint, err = url.Parse("http://localhost")
	} else {
		p.endpoint, err = url.Parse(spec.Serve.endpointScheme() + "://" + event.Listen)
	}
	if err != nil {
		return fmt.Errorf("invalid ready endpoint: %w", err)
	}
	return p.configureTransport(ctx, spec)
}

// Endpoint is where the child listens: its TCP address, or http://localhost over the socket.
func (p *Process) Endpoint() *url.URL { return p.endpoint }

// Version is the release the child reported in its ready event.
func (p *Process) Version() string { return p.version }

// Cmd is the started command, for tests that check its arguments and environment.
func (p *Process) Cmd() *exec.Cmd { return p.cmd }

// Done is closed once the child has exited and been reaped.
func (p *Process) Done() <-chan struct{} { return p.done }

// Closing is closed when Stop begins.
func (p *Process) Closing() <-chan struct{} { return p.closing }

// Stop asks the child to stop, forcibly terminates it after ten seconds, and reaps it. Every
// call returns the result of the first.
func (p *Process) Stop() error {
	p.stopOnce.Do(func() {
		close(p.closing)
		if p.transport != nil {
			p.transport.CloseIdleConnections()
		}
		if p.cmd == nil || p.cmd.Process == nil {
			return
		}
		select {
		case <-p.done:
		default:
			p.stopErr = p.terminate()
		}
		p.stopErr = errors.Join(p.stopErr, p.exitErr)
	})
	return p.stopErr
}

func (p *Process) terminate() error {
	if err := interrupt(p.cmd.Process); err != nil && !errors.Is(err, os.ErrProcessDone) {
		// The child never learns that it should stop, so waiting would only delay the kill.
		err = errors.Join(err, p.cmd.Process.Kill())
		<-p.done
		return err
	}
	timer := time.NewTimer(shutdownTimeout)
	defer timer.Stop()
	select {
	case <-p.done:
		return nil
	case <-timer.C:
		err := errors.Join(errors.New("child shutdown exceeded 10 seconds"), p.cmd.Process.Kill())
		<-p.done
		return err
	}
}

// lockedWriter serializes the stdout copy and the stderr copy, which run on separate goroutines.
type lockedWriter struct {
	sync.Mutex
	writer io.Writer
	tail   *tailWriter
}

func (w *lockedWriter) Write(p []byte) (int, error) {
	w.Lock()
	defer w.Unlock()
	if w.tail != nil {
		w.tail.write(p)
	}
	if w.writer == nil {
		return len(p), nil
	}
	return w.writer.Write(p)
}

func (w *lockedWriter) tailText() string {
	w.Lock()
	defer w.Unlock()
	if w.tail == nil {
		return ""
	}
	return strings.TrimSpace(string(w.tail.data))
}

// tailWriter keeps the last limit bytes written, so a chatty child cannot grow it.
type tailWriter struct {
	limit int
	data  []byte
}

func (w *tailWriter) write(p []byte) {
	if len(p) >= w.limit {
		w.data = append(w.data[:0], p[len(p)-w.limit:]...)
		return
	}
	if keep := w.limit - len(p); len(w.data) > keep {
		w.data = append(w.data[:0], w.data[len(w.data)-keep:]...)
	}
	w.data = append(w.data, p...)
}
