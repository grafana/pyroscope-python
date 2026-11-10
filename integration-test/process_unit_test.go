package integrationtest

import (
	"strings"
	"testing"
	"time"
)

func TestProcessExitAndLogs(t *testing.T) {
	p := startProcess(t, "sh", []string{"-c", `printf '%s\n' "$NATIVE_TEST_VALUE"; printf 'stderr\n' >&2; exit 7`}, map[string]string{"NATIVE_TEST_VALUE": "native child"})
	if code := p.Wait(t, 5*time.Second); code != 7 {
		t.Fatalf("exit code: %d", code)
	}
	state, err := p.State()
	if err != nil || state.Running || state.ExitCode != 7 {
		t.Fatalf("state: %+v, %v", state, err)
	}
	if logs := p.Logs(t); logs != "native child\nstderr\n" {
		t.Fatalf("logs: %q", logs)
	}
}

func TestProcessStop(t *testing.T) {
	p := startProcess(t, "sh", []string{"-c", `trap '' TERM; echo ready; while :; do sleep 60; done`}, nil)
	deadline := time.Now().Add(5 * time.Second)
	for !strings.Contains(p.Logs(t), "ready") {
		if time.Now().After(deadline) {
			t.Fatal("process did not start")
		}
		time.Sleep(10 * time.Millisecond)
	}
	p.Stop(t, 50*time.Millisecond)
	if state, _ := p.State(); state.Running || state.ExitCode == 0 {
		t.Fatalf("state after forced stop: %+v", state)
	}
	p.Stop(t, time.Second) // Cleanup must also be safe after an explicit stop.
}
