package integrationtest

import (
	"fmt"
	"testing"
	"time"

	"pyroscope-python-integration-test/dockertest"
)

func TestPythonSignalProfilerThreadState(t *testing.T) {
	skipUnlessPythonSupportsSignal(t)
	_, minor := pythonVersion(t)
	wheelDir := ensureWheel(t)
	net := dockertest.CreateNetwork(t)
	pyroscopeURL := startPyroscope(t, net)
	for _, timer := range []string{"ITimer", "CTimer"} {
		t.Run(timer, func(t *testing.T) {
			appName := fmt.Sprintf("pyroscopers.python.test.threadstate.%s.%d", timer, time.Now().UnixNano())
			workload := startPythonTestContainer(t, net, wheelDir, "signal_thread_state_workload.py", map[string]string{
				"PYROSCOPE_APPLICATION_NAME": appName,
				"SIGNAL_TIMER":               timer,
			})
			requireContainerExit(t, workload, 0, time.Minute)
			for phase := range 2 {
				selector := fmt.Sprintf(`{service_name="%s",phase="%d"}`, appName, phase)
				// Each thread burns two CPU seconds, including GIL-releasing C calls.
				// Allow sampling noise, but require both complete stacks in each
				// session so a missing caller or a lost cache on restart cannot pass.
				requireProfileShape(t, workload, pyroscopeURL, cpuProfileTypeID, selector, profileShape{
					stacks: []stackShare{
						{"<module>;main;burn", 0.35, 0.65},
						{threadStackPrefix(profileConfig{signalTimer: timer}, minor) + "worker;burn", 0.35, 0.65},
					},
					minTotal: int64(2 * time.Second), minCoverage: 0.90,
				}, time.Minute)
			}
		})
	}
}
