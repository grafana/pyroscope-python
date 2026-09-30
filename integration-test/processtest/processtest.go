package processtest

import (
	"bytes"
	"net/http"
	"os"
	"os/exec"
	"sync"
	"testing"
	"time"
)

type State struct {
	Running  bool
	ExitCode int
}

type Request struct {
	Name    string
	Args    []string
	Dir     string
	Env     map[string]string
	WaitURL string
	Timeout time.Duration
}

type Process struct {
	cmd  *exec.Cmd
	logs lockedBuffer
	done chan struct{}

	mu       sync.RWMutex
	running  bool
	exitCode int
}

type lockedBuffer struct {
	mu sync.RWMutex
	b  bytes.Buffer
}

func (b *lockedBuffer) Write(p []byte) (int, error) {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.b.Write(p)
}

func (b *lockedBuffer) String() string {
	b.mu.RLock()
	defer b.mu.RUnlock()
	return b.b.String()
}

func Start(t *testing.T, req Request) *Process {
	t.Helper()
	if req.Name == "" {
		t.Fatal("processtest: process name is required")
	}

	cmd := exec.Command(req.Name, req.Args...)
	cmd.Dir = req.Dir
	cmd.Env = os.Environ()
	for key, value := range req.Env {
		cmd.Env = append(cmd.Env, key+"="+value)
	}

	p := &Process{
		cmd:      cmd,
		done:     make(chan struct{}),
		running:  true,
		exitCode: -1,
	}
	cmd.Stdout = &p.logs
	cmd.Stderr = &p.logs
	if err := cmd.Start(); err != nil {
		t.Fatalf("processtest: start %s: %v", req.Name, err)
	}

	go func() {
		err := cmd.Wait()
		exitCode := 0
		if err != nil {
			exitCode = cmd.ProcessState.ExitCode()
		}
		p.mu.Lock()
		p.running = false
		p.exitCode = exitCode
		p.mu.Unlock()
		close(p.done)
	}()

	t.Cleanup(func() {
		if t.Failed() {
			t.Logf("processtest: %s logs:\n%s", req.Name, p.Logs())
		}
		p.stop(10 * time.Second)
	})

	if req.WaitURL != "" {
		timeout := req.Timeout
		if timeout == 0 {
			timeout = 2 * time.Minute
		}
		p.waitHTTP(t, req.WaitURL, timeout)
	}
	return p
}

func (p *Process) State() State {
	p.mu.RLock()
	defer p.mu.RUnlock()
	return State{Running: p.running, ExitCode: p.exitCode}
}

func (p *Process) Stop(t *testing.T, timeout time.Duration) {
	t.Helper()
	p.stop(timeout)
}

func (p *Process) stop(timeout time.Duration) {
	if !p.State().Running {
		return
	}
	_ = p.cmd.Process.Signal(os.Interrupt)
	select {
	case <-p.done:
		return
	case <-time.After(timeout):
	}
	_ = p.cmd.Process.Kill()
	<-p.done
}

func (p *Process) Wait(t *testing.T, timeout time.Duration) int {
	t.Helper()
	select {
	case <-p.done:
		return p.State().ExitCode
	case <-time.After(timeout):
		p.stop(5 * time.Second)
		t.Fatalf("processtest: process did not exit after %v\nlogs:\n%s", timeout, p.Logs())
		return -1
	}
}

func (p *Process) Logs() string {
	return p.logs.String()
}

func (p *Process) waitHTTP(t *testing.T, endpoint string, timeout time.Duration) {
	t.Helper()
	client := &http.Client{Timeout: 2 * time.Second}
	deadline := time.Now().Add(timeout)
	for time.Now().Before(deadline) {
		if !p.State().Running {
			t.Fatalf(
				"processtest: process exited before %s became ready (exit code %d)\nlogs:\n%s",
				endpoint,
				p.State().ExitCode,
				p.Logs(),
			)
		}
		resp, err := client.Get(endpoint)
		if err == nil {
			resp.Body.Close()
			if resp.StatusCode >= 200 && resp.StatusCode < 400 {
				return
			}
		}
		time.Sleep(time.Second)
	}
	t.Fatalf("processtest: %s was not ready after %v\nlogs:\n%s", endpoint, timeout, p.Logs())
}

func Run(t *testing.T, name string, args ...string) string {
	t.Helper()
	cmd := exec.Command(name, args...)
	var output lockedBuffer
	cmd.Stdout = &output
	cmd.Stderr = &output
	if err := cmd.Run(); err != nil {
		t.Fatalf("processtest: %s failed: %v\n%s", name, err, output.String())
	}
	return output.String()
}
