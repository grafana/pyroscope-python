package integrationtest

import (
	"fmt"
	"strings"
	"testing"
	"time"
)

func TestPythonProfilerCallbacks(t *testing.T) {
	for _, tc := range []struct {
		name      string
		cfg       profileConfig
		stackWall bool
	}{
		{name: "py-spy", cfg: profileConfig{onCPU: true, gilOnly: true}},
		{name: "signal-itimer", cfg: profileConfig{onCPU: true, signalTimer: "ITimer"}},
		{name: "signal-ctimer", cfg: profileConfig{onCPU: true, signalTimer: "CTimer"}},
		{name: "stack", cfg: profileConfig{onCPU: true, stack: true}},
		{name: "stack-wall", cfg: profileConfig{onCPU: true, stack: true}, stackWall: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			skipUnlessProfiler(t, tc.cfg.profilerName())
			if tc.cfg.signalTimer != "" {
				skipUnlessPythonSupportsSignal(t)
			}
			if tc.cfg.signalTimer == "CTimer" {
				skipUnlessSignalCTimer(t)
			}

			pyroscopeURL := startPyroscope(t)
			appName := fmt.Sprintf("pyroscopers.python.test.callbacks.%d", time.Now().UnixNano())
			canary := randomHex(t, 16)
			timer := tc.cfg.signalTimer
			if timer == "" {
				timer = "ITimer"
			}
			workload := startPythonWorkload(t, pyroscopeURL, "callback_workload.py", map[string]string{
				"PYROSCOPE_APPLICATION_NAME": appName,
				"CANARY":                     canary,
				"CPU_IMPLEMENTATION":         tc.cfg.implementation(),
				"SIGNAL_TIMER":               timer,
				"STACK_WALL":                 boolString(tc.stackWall),
			})
			// Finish the entire stress interval, including shutdown, before
			// accepting a profile. Also reject recoverable sanitizer reports.
			requireProcessExit(t, workload, 0, 2*time.Minute)
			if logs := workload.Logs(t); strings.Contains(logs, "ERROR: AddressSanitizer") {
				t.Fatalf("callback workload reported an ASAN error:\n%s", logs)
			}
			selector := fmt.Sprintf(`{service_name="%s",canary="%s"}`, appName, canary)
			// The main thread alternates between map's C loop and the Python
			// callback. Neither introduces Python thread-registration frames;
			// these module-level names are identical on Python 3.10+.
			// Allow their relative cost to vary across Python versions and
			// profilers, but require both complete paths and 90% total coverage.
			requireProfileShape(t, workload, pyroscopeURL, cpuProfileTypeID, selector, profileShape{
				stacks: []stackShare{
					{"<module>;main", 0.05, 0.95},
					{"<module>;main;callback", 0.05, 0.95},
				},
				minTotal: int64(2 * time.Second), minCoverage: 0.90,
			}, 2*time.Minute)
		})
	}
}
