package integrationtest

import (
	"net"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"syscall"
	"testing"
	"time"
)

// Each process owns a working directory and process group, so tests cannot
// share backend data and timed-out workloads cannot leave fork children behind.
type process struct {
	stopped bool
	cmd     *exec.Cmd
	logPath string
	done    chan struct{}
	err     error // written before done is closed
}

type processState struct {
	Running  bool
	ExitCode int
}

func startProcess(t *testing.T, executable string, args []string, env map[string]string) *process {
	t.Helper()
	executable, err := exec.LookPath(executable)
	if err != nil {
		t.Fatal(err)
	}
	executable, err = filepath.Abs(executable)
	if err != nil {
		t.Fatal(err)
	}
	dir := t.TempDir()
	logPath := filepath.Join(dir, "process.log")
	log, err := os.Create(logPath)
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(executable, args...)
	cmd.Dir = dir
	cmd.Env = os.Environ()
	for k, v := range env {
		cmd.Env = append(cmd.Env, k+"="+v)
	}
	cmd.Stdout, cmd.Stderr = log, log
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	if err := cmd.Start(); err != nil {
		log.Close()
		t.Fatalf("start %s: %v", executable, err)
	}
	p := &process{cmd: cmd, logPath: logPath, done: make(chan struct{})}
	go func() {
		p.err = cmd.Wait()
		log.Close()
		close(p.done)
	}()
	t.Cleanup(func() {
		p.Stop(t, 10*time.Second)
		if dir := os.Getenv("PYROSCOPE_TEST_LOG_DIR"); dir != "" {
			path := filepath.Join(dir, t.Name(), filepath.Base(executable)+"-"+strconv.Itoa(p.cmd.Process.Pid)+".log")
			if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
				t.Errorf("create process log directory: %v", err)
			} else if err := os.WriteFile(path, []byte(p.Logs(t)), 0o644); err != nil {
				t.Errorf("save process logs: %v", err)
			}
		}
		if t.Failed() {
			t.Logf("%s logs:\n%s", executable, p.Logs(t))
		}
	})
	return p
}

func (p *process) Stop(t *testing.T, timeout time.Duration) {
	t.Helper()
	if p.stopped {
		return
	}
	p.stopped = true
	// Signal the entire group even if its leader has already exited.
	_ = syscall.Kill(-p.cmd.Process.Pid, syscall.SIGTERM)
	select {
	case <-p.done:
	case <-time.After(timeout):
	}
	_ = syscall.Kill(-p.cmd.Process.Pid, syscall.SIGKILL)
	select {
	case <-p.done:
	case <-time.After(5 * time.Second):
		t.Errorf("process %d did not exit after SIGKILL", p.cmd.Process.Pid)
	}
}

func (p *process) Wait(t *testing.T, timeout time.Duration) int {
	t.Helper()
	select {
	case <-p.done:
		return p.cmd.ProcessState.ExitCode()
	case <-time.After(timeout):
		p.Stop(t, time.Second)
		t.Fatalf("process timed out after %s\n%s", timeout, p.Logs(t))
		return -1
	}
}

func (p *process) State() (processState, error) {
	select {
	case <-p.done:
		return processState{ExitCode: p.cmd.ProcessState.ExitCode()}, nil
	default:
		return processState{Running: true}, nil
	}
}

func (p *process) Logs(t *testing.T) string {
	t.Helper()
	b, err := os.ReadFile(p.logPath)
	if err != nil {
		t.Fatal(err)
	}
	return string(b)
}

func freePort(t *testing.T) string {
	t.Helper()
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer listener.Close()
	return strconv.Itoa(listener.Addr().(*net.TCPAddr).Port)
}

func startPyroscope(t *testing.T) string {
	t.Helper()
	httpPort, grpcPort, gossipPort, raftPort := freePort(t), freePort(t), freePort(t), freePort(t)
	grpcAddr, raftAddr := "127.0.0.1:"+grpcPort, "127.0.0.1:"+raftPort
	p := startProcess(t, envOrDefault("PYROSCOPE_BINARY", "pyroscope"), []string{
		"-architecture.storage=v2",
		"-server.http-listen-address=127.0.0.1",
		"-server.http-listen-port=" + httpPort,
		"-server.grpc-listen-address=127.0.0.1",
		"-server.grpc-listen-port=" + grpcPort,
		"-memberlist.bind-addr=127.0.0.1",
		"-memberlist.bind-port=" + gossipPort,
		"-memberlist.advertise-addr=127.0.0.1",
		"-memberlist.advertise-port=" + gossipPort,
		"-metastore.address=" + grpcAddr,
		"-query-backend.address=" + grpcAddr,
		"-metastore.raft.bind-address=" + raftAddr,
		"-metastore.raft.advertise-address=" + raftAddr,
		"-metastore.raft.server-id=" + raftAddr,
		"-segment-writer.lifecycler.addr=127.0.0.1",
		"-distributor.ring.instance-addr=127.0.0.1",
		"-overrides-exporter.ring.instance-addr=127.0.0.1",
		"-query-frontend.instance-addr=127.0.0.1",
		"-segment-writer.min-ready-duration=0s",
		"-metastore.min-ready-duration=0s",
		"-usage-stats.enabled=false",
	}, nil)
	url := "http://127.0.0.1:" + httpPort
	client := &http.Client{Timeout: time.Second}
	deadline := time.Now().Add(2 * time.Minute)
	for time.Now().Before(deadline) {
		state, _ := p.State()
		if !state.Running {
			t.Fatalf("Pyroscope exited: %v\n%s", p.err, p.Logs(t))
		}
		response, err := client.Get(url + "/ready")
		if err == nil {
			response.Body.Close()
			if response.StatusCode == http.StatusOK {
				return url
			}
		}
		time.Sleep(200 * time.Millisecond)
	}
	t.Fatalf("Pyroscope did not become ready at %s\n%s", url, p.Logs(t))
	return ""
}
