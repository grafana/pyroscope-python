package integrationtest

import (
	"bytes"
	"context"
	"crypto/rand"
	"encoding/hex"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
	"time"

	"pyroscope-python-integration-test/api/querier"
	"pyroscope-python-integration-test/dockertest"
	"pyroscope-python-integration-test/pyroscope/model"
	"pyroscope-python-integration-test/require"
)

const (
	cpuProfileTypeID              = "process_cpu:cpu:nanoseconds:cpu:nanoseconds"
	memoryAllocSpaceProfileTypeID = "memory:alloc_space:bytes:space:bytes"
	memoryInuseSpaceProfileTypeID = "memory:inuse_space:bytes:space:bytes"
)

type profileConfig struct {
	onCPU       bool
	gilOnly     bool
	stack       bool
	signalTimer string
}

func TestPythonProfilerOnCPUWithGILOnly(t *testing.T) {
	testPythonProfilerConfiguration(t, profileConfig{onCPU: true, gilOnly: true})
}

func TestPythonProfilerOnCPUWithoutGILOnly(t *testing.T) {
	testPythonProfilerConfiguration(t, profileConfig{onCPU: true, gilOnly: false})
}

func TestPythonProfilerOffCPUWithGILOnly(t *testing.T) {
	testPythonProfilerConfiguration(t, profileConfig{onCPU: false, gilOnly: true})
}

func TestPythonProfilerOffCPUWithoutGILOnly(t *testing.T) {
	testPythonProfilerConfiguration(t, profileConfig{onCPU: false, gilOnly: false})
}

func TestPythonStackProfilerOnCPU(t *testing.T) {
	testPythonProfilerConfiguration(t, profileConfig{onCPU: true, stack: true})
}

func TestPythonSignalProfilerITimer(t *testing.T) {
	skipUnlessPythonSupportsSignal(t)
	testPythonProfilerConfiguration(t, profileConfig{onCPU: true, signalTimer: "ITimer"})
}

func TestPythonSignalProfilerCTimer(t *testing.T) {
	skipUnlessPythonSupportsSignal(t)
	testPythonProfilerConfiguration(t, profileConfig{onCPU: true, signalTimer: "CTimer"})
}

func TestPythonSignalProfilerCTimerRawFork(t *testing.T) {
	skipUnlessPythonSupportsSignal(t)
	wheelDir := ensureWheel(t)
	net := dockertest.CreateNetwork(t)
	pyroscopeURL := startPyroscope(t, net)
	for _, scenario := range []string{"main", "unregistered"} {
		t.Run(scenario, func(t *testing.T) {
			appName := fmt.Sprintf("pyroscopers.python.test.rawfork.%s.%d", scenario, time.Now().UnixNano())
			canary := randomHex(t, 16)
			workload := startPythonTestContainer(t, net, wheelDir, "signal_ctimer_workload.py", map[string]string{
				"SCENARIO":                   scenario,
				"PYROSCOPE_APPLICATION_NAME": appName,
				"CANARY":                     canary,
			})
			selector := fmt.Sprintf(`{service_name="%s",canary="%s"}`, appName, canary)
			stack := "<module>;raw_fork;fork_and_profile;raw_fork_child_burn"
			if scenario == "unregistered" {
				stack = "raw_fork.<locals>.worker;fork_and_profile;raw_fork_child_burn"
				if _, minor := pythonVersion(t); minor < 11 {
					stack = "worker;fork_and_profile;raw_fork_child_burn"
				}
			}
			requireProfileShape(t, workload, pyroscopeURL, cpuProfileTypeID, selector, profileShape{
				stacks:   []stackShare{{stack, 0.90, 1}},
				minTotal: int64(time.Second), minCoverage: 0.90,
			}, 45*time.Second)
		})
	}
}

func TestPythonSignalProfilerCTimerArmingFailure(t *testing.T) {
	skipUnlessPythonSupportsSignal(t)
	wheelDir := ensureWheel(t)
	net := dockertest.CreateNetwork(t)
	startPyroscope(t, net)
	workload := startPythonTestContainer(t, net, wheelDir, "signal_ctimer_workload.py", map[string]string{
		"SCENARIO": "arming_failure",
	})
	requireContainerExit(t, workload, 0, time.Minute)
	t.Log(workload.Logs(t))
}

func TestPythonSignalProfilerLifecycle(t *testing.T) {
	skipUnlessPythonSupportsSignal(t)
	wheelDir := ensureWheel(t)
	net := dockertest.CreateNetwork(t)
	startPyroscope(t, net)
	for _, scenario := range []string{"zero_rate", "ctimer_shutdown_fork", "recovery_handlers_preserved", "bounded_drain", "fault_fallback"} {
		t.Run(scenario, func(t *testing.T) {
			workload := startPythonTestContainer(t, net, wheelDir, "signal_lifecycle_workload.py", map[string]string{
				"SCENARIO": scenario,
			})
			requireContainerExit(t, workload, 0, 2*time.Minute)
		})
	}
	if _, minor := pythonVersion(t); minor >= 11 {
		t.Run("ctimer_preexisting_thread_cleanup", func(t *testing.T) {
			workload := startPythonTestContainer(t, net, wheelDir, "signal_lifecycle_workload.py", map[string]string{
				"SCENARIO": "ctimer_preexisting_thread_cleanup",
			})
			requireContainerExit(t, workload, 0, 2*time.Minute)
		})
	}
}

func TestPythonStackProfilerRestart(t *testing.T) {
	skipUnlessProfiler(t, "stack")
	wheelDir := ensureWheel(t)

	net := dockertest.CreateNetwork(t)
	pyroscopeURL := startPyroscope(t, net)
	appName := fmt.Sprintf("pyroscopers.python.test.restart.%d", time.Now().UnixNano())
	canaryFirst := randomHex(t, 16)
	canarySecond := randomHex(t, 16)
	workload := startPythonTestContainer(t, net, wheelDir, "restart_workload.py", map[string]string{
		"PYROSCOPE_APPLICATION_NAME": appName,
		"CANARY_FIRST":               canaryFirst,
		"CANARY_SECOND":              canarySecond,
	})
	t.Cleanup(func() {
		requireCleanStop(t, workload)
	})

	selector := func(canary string) string {
		return fmt.Sprintf(`{service_name="%s",canary="%s"}`, appName, canary)
	}
	_, minor := pythonVersion(t)
	prefix := threadStackPrefix(profileConfig{stack: true}, minor)
	isSet := "Event.is_set"
	if minor < 11 {
		isSet = "is_set"
	}
	for _, phase := range []struct{ canary, leaf string }{{canaryFirst, "burn_first"}, {canarySecond, "burn_second"}} {
		requireProfileShape(t, workload, pyroscopeURL, cpuProfileTypeID, selector(phase.canary), profileShape{
			stacks: []stackShare{
				{prefix + phase.leaf, 0.60, 1},
				{prefix + phase.leaf + ";" + isSet, 0, 0.40},
			},
			minTotal: int64(time.Second), minCoverage: 0.90,
		}, 3*time.Minute)
	}
}

func TestPythonStackProfilerCpuTimerFork(t *testing.T) {
	skipUnlessProfiler(t, "stack")
	if !pythonSupportsCPUTimer(t) {
		t.Skip("CPU timers require CPython 3.12+; older versions use the wall sampler")
	}
	wheelDir := ensureWheel(t)

	net := dockertest.CreateNetwork(t)
	pyroscopeURL := startPyroscope(t, net)
	appName := fmt.Sprintf("pyroscopers.python.test.cputimerfork.%d", time.Now().UnixNano())
	canary := randomHex(t, 16)
	workload := startPythonTestContainer(t, net, wheelDir, "sighandler_workload.py", map[string]string{
		"PYROSCOPE_APPLICATION_NAME": appName,
		"SCENARIO":                   "cpu_timer_worker_fork_onstack_signal",
		"CANARY":                     canary,
	})

	selector := fmt.Sprintf(`{service_name="%s",canary="%s"}`, appName, canary)
	_, minor := pythonVersion(t)
	prefix := threadStackPrefix(profileConfig{stack: true}, minor) +
		"run_worker.<locals>.work;cpu_timer_worker_fork_onstack_signal.<locals>.work;burn_after_fork;burn_for"
	// One second is spent in burn_for before the fork, then 15 seconds after.
	// The inline arithmetic loop stays in one Python frame, so the complete
	// post-fork stack should receive about 15/16 (94%) of the CPU sample weight.
	requireProfileShape(t, workload, pyroscopeURL, cpuProfileTypeID, selector, profileShape{
		stacks: []stackShare{
			{prefix, 0.90, 1},
		},
		minTotal: int64(3 * time.Second), minCoverage: 0.90,
	}, 3*time.Minute)
	requireContainerExit(t, workload, 0, 3*time.Minute)
}

func TestPythonNonCPUIntegrationSuites(t *testing.T) {
	t.Run("memory profiler", testPythonMemoryProfiler)
	t.Run("concurrent configure shutdown", testPythonConcurrentConfigureShutdown)
	t.Run("atexit shutdown", testPythonAtexitShutdown)
	t.Run("fork child", testPythonForkChild)
}

func TestPythonSignalHandlerSuites(t *testing.T) {
	skipUnlessProfiler(t, "stack", "py-spy")
	wheelDir := ensureWheel(t)

	net := dockertest.CreateNetwork(t)
	startPyroscope(t, net)
	scenarios := []string{
		"import_installs_nothing",
		"stack_offcpu_refused",
		"pyspy_and_memory_install_nothing",
		"first_cpu_configure_wins",
		"stack_installs_both",
		"faulthandler_unpatched",
		"reconfigure_keeps_handlers",
		"foreign_after_configure_is_not_reclaimed",
		"crash_with_our_handler",
		"sigbus_with_our_handler",
		"crash_chains_to_earlier_faulthandler",
		"crash_after_faulthandler_takeover",
		"faulthandler_enable_after_configure_stops_sampling",
		"takeover_stops_sampling",
		"cpu_timer_unsupported_fallback",
		"cpu_timer_restart_refused",
		"cpu_timer_fork_child_refused",
		"cpu_timer_shutdown_elsewhere_then_fork",
		"cpu_timer_fork_keeps_app_altstack",
		"cpu_timer_fork_on_altstack_keeps_mapping",
	}
	for _, scenario := range scenarios {
		t.Run(scenario, func(t *testing.T) {
			switch scenario {
			case "import_installs_nothing":
			case "pyspy_and_memory_install_nothing":
				skipUnlessProfiler(t, "py-spy")
			default:
				skipUnlessProfiler(t, "stack")
			}
			if scenario == "cpu_timer_unsupported_fallback" {
				if pythonSupportsCPUTimer(t) {
					t.Skip("fallback applies to CPython older than 3.12")
				}
			} else if strings.HasPrefix(scenario, "cpu_timer_") && !pythonSupportsCPUTimer(t) {
				t.Skip("CPU timers require CPython 3.12+; older versions use the wall sampler")
			}
			appName := fmt.Sprintf("pyroscopers.python.test.sighandler.%d", time.Now().UnixNano())
			workload := startPythonTestContainer(t, net, wheelDir, "sighandler_workload.py", map[string]string{
				"PYROSCOPE_APPLICATION_NAME": appName,
				"SCENARIO":                   scenario,
			})
			requireContainerExit(t, workload, 0, 3*time.Minute)
		})
	}
}

func skipUnlessPythonSupportsSignal(t *testing.T) {
	t.Helper()
	skipUnlessProfiler(t, "signal")
	major, minor := pythonVersion(t)
	if major != 3 || minor < 10 || minor > 14 {
		t.Skip("the Signal profiler supports only CPython 3.10 to 3.14")
	}
}

func pythonVersion(t *testing.T) (int, int) {
	t.Helper()
	var major, minor int
	version := envOrDefault("PYTHON_VERSION", "3.11")
	if _, err := fmt.Sscanf(version, "%d.%d", &major, &minor); err != nil {
		t.Fatalf("invalid PYTHON_VERSION %q: %v", version, err)
	}
	return major, minor
}

func pythonSupportsCPUTimer(t *testing.T) bool {
	t.Helper()
	var major, minor int
	version := envOrDefault("PYTHON_VERSION", "3.11")
	if _, err := fmt.Sscanf(version, "%d.%d", &major, &minor); err != nil {
		t.Fatalf("invalid PYTHON_VERSION %q: %v", version, err)
	}
	return major > 3 || major == 3 && minor >= 12
}

func testPythonMemoryProfiler(t *testing.T) {
	// Memory profiling is independent of the CPU implementation; run it in every suite.
	wheelDir := ensureWheel(t)

	net := dockertest.CreateNetwork(t)
	pyroscopeURL := startPyroscope(t, net)
	appName := fmt.Sprintf("pyroscopers.python.test.memory.%d", time.Now().UnixNano())
	canary := randomHex(t, 16)
	workload := startPythonTestContainer(t, net, wheelDir, "memory_workload.py", map[string]string{
		"PYROSCOPE_APPLICATION_NAME": appName,
		"CANARY":                     canary,
	})
	t.Cleanup(func() {
		requireCleanStop(t, workload)
	})

	labelSelector := fmt.Sprintf(`{service_name="%s",canary="%s"}`, appName, canary)
	profiles := []struct {
		name          string
		profileTypeID string
	}{
		{name: "alloc_space", profileTypeID: memoryAllocSpaceProfileTypeID},
		{name: "inuse_space", profileTypeID: memoryInuseSpaceProfileTypeID},
	}
	_, minor := pythonVersion(t)
	stack := threadStackPrefix(profileConfig{}, minor) + "memhog"
	for _, profile := range profiles {
		t.Run(profile.name, func(t *testing.T) {
			// The workload retains 8–16 MiB of bytearrays allocated at memhog.
			// Allow heap sampling variance but require several MiB and dominance.
			requireProfileShape(t, workload, pyroscopeURL, profile.profileTypeID, labelSelector, profileShape{
				stacks:   []stackShare{{stack, 0.90, 1}},
				minTotal: 4 * 1024 * 1024, minCoverage: 0.90,
			}, 3*time.Minute)
		})
	}

	collapsed, err := queryProfile(pyroscopeURL, cpuProfileTypeID, labelSelector)
	require.NoError(t, err)
	require.Equal(t, "", collapsed)
}

func testPythonConcurrentConfigureShutdown(t *testing.T) {
	skipUnlessProfiler(t, "py-spy")
	wheelDir := ensureWheel(t)

	net := dockertest.CreateNetwork(t)
	startPyroscope(t, net)
	appName := fmt.Sprintf("pyroscopers.python.test.concurrency.%d", time.Now().UnixNano())
	workload := startPythonTestContainer(t, net, wheelDir, "concurrency_workload.py", map[string]string{
		"PYROSCOPE_APPLICATION_NAME": appName,
	})

	requireContainerExit(t, workload, 0, 3*time.Minute)
}

func testPythonAtexitShutdown(t *testing.T) {
	skipUnlessProfiler(t, "py-spy")
	wheelDir := ensureWheel(t)

	net := dockertest.CreateNetwork(t)
	startPyroscope(t, net)
	appName := fmt.Sprintf("pyroscopers.python.test.atexit.%d", time.Now().UnixNano())
	workload := startPythonTestContainer(t, net, wheelDir, "atexit_workload.py", map[string]string{
		"PYROSCOPE_APPLICATION_NAME": appName,
	})

	requireContainerExit(t, workload, 0, 2*time.Minute)
}

func testPythonForkChild(t *testing.T) {
	skipUnlessProfiler(t, "stack")
	wheelDir := ensureWheel(t)

	net := dockertest.CreateNetwork(t)
	pyroscopeURL := startPyroscope(t, net)
	appName := fmt.Sprintf("pyroscopers.python.test.fork.%d", time.Now().UnixNano())
	canary := randomHex(t, 16)
	workload := startPythonTestContainer(t, net, wheelDir, "fork_workload.py", map[string]string{
		"PYROSCOPE_APPLICATION_NAME": appName,
		"CANARY":                     canary,
	})

	labelSelector := fmt.Sprintf(`{service_name="%s",canary="%s"}`, appName, canary)
	requireProfileShape(t, workload, pyroscopeURL, cpuProfileTypeID, labelSelector, profileShape{
		stacks:   []stackShare{{"<module>;main;fork;fork_child_main;fork_child_burn", 0.90, 1}},
		minTotal: int64(time.Second), minCoverage: 0.90,
	}, 3*time.Minute)

	requireContainerExit(t, workload, 0, 3*time.Minute)
}

func testPythonProfilerConfiguration(t *testing.T, cfg profileConfig) {
	t.Helper()
	skipUnlessProfiler(t, cfg.profilerName())
	wheelDir := ensureWheel(t)

	net := dockertest.CreateNetwork(t)
	pyroscopeURL := startPyroscope(t, net)
	appName := fmt.Sprintf("pyroscopers.python.test.%d", time.Now().UnixNano())
	canary := randomHex(t, 16)
	workload := startWorkload(t, net, appName, canary, cfg, wheelDir)
	t.Cleanup(func() {
		requireCleanStop(t, workload)
	})

	labelSelector := fmt.Sprintf(
		`{service_name="%s",canary="%s",oncpu="%s",gil_only="%s"}`,
		appName,
		canary,
		boolString(cfg.onCPU),
		boolString(cfg.gilOnly),
	)

	_, minor := pythonVersion(t)
	collectUntil := time.Now().Add(15 * time.Second)
	var workloadExited bool
	var workloadExitCode int
	require.Eventually(t, func() bool {
		state, err := workload.State()
		if err != nil {
			t.Logf("failed to inspect workload container: %v", err)
			return false
		}
		if !state.Running {
			workloadExited = true
			workloadExitCode = state.ExitCode
			return true
		}

		if time.Now().Before(collectUntil) {
			return false
		}

		collapsed, err := queryProfile(pyroscopeURL, cpuProfileTypeID, labelSelector)
		if err != nil {
			t.Logf("query failed for %s: %v", cfg, err)
			return false
		}
		if collapsed == "" {
			t.Logf("profile for %s is empty", cfg)
			return false
		}
		if err := checkConfigurationProfile(collapsed, cfg, minor); err != nil {
			t.Logf("profile for %s: %v\n%s", cfg, err, collapsed)
			return false
		}
		t.Logf("profile for %s:\n%s", cfg, collapsed)
		return true
	}, 3*time.Minute, 5*time.Second, "expected exact stacks and sample proportions for %s", cfg)
	if workloadExited {
		t.Fatalf("workload container exited before producing the expected profile (exit code %d)", workloadExitCode)
	}
}

func startPyroscope(t *testing.T, net *dockertest.Network) string {
	t.Helper()
	c := dockertest.StartContainer(t, dockertest.ContainerRequest{
		Image:          envOrDefault("PYROSCOPE_IMAGE", "grafana/pyroscope:2.3.1"),
		ExposedPorts:   []string{"4040/tcp"},
		Network:        net.Name,
		NetworkAliases: []string{"pyroscope"},
		Cmd: []string{
			"-config.file=/etc/pyroscope/config.yaml",
			"-architecture.storage=v2",
			"-segment-writer.min-ready-duration=0s",
			"-metastore.min-ready-duration=0s",
		},
		WaitFor: dockertest.WaitForHTTP("/ready", "4040/tcp", 2*time.Minute),
	})
	return fmt.Sprintf("http://%s", c.HostPort(t, "4040/tcp"))
}

func startWorkload(t *testing.T, net *dockertest.Network, appName, canary string, cfg profileConfig, wheelDir string) *dockertest.Container {
	t.Helper()
	env := map[string]string{
		"PYTHONUNBUFFERED":              "1",
		"PYTHONDONTWRITEBYTECODE":       "1",
		"PYROSCOPE_APPLICATION_NAME":    appName,
		"PYROSCOPE_SERVER_ADDRESS":      "http://pyroscope:4040",
		"ONCPU":                         boolString(cfg.onCPU),
		"GIL_ONLY":                      boolString(cfg.gilOnly),
		"CPU_IMPLEMENTATION":            cfg.implementation(),
		"SIGNAL_TIMER":                  cfg.signalTimer,
		"CANARY":                        canary,
		"PIP_DISABLE_PIP_VERSION_CHECK": "1",
	}
	return dockertest.StartContainer(t, dockertest.ContainerRequest{
		Image:    pythonImage(),
		Platform: wheelDockerPlatform(),
		Network:  net.Name,
		Env:      env,
		Volumes: []string{
			repoRoot() + ":/pyroscope-python:ro",
			wheelDir + ":/pyroscope-wheels:ro",
		},
		Cmd: []string{
			"sh",
			"-c",
			"python -m pip install --no-cache-dir --no-index --find-links /pyroscope-wheels pyroscope-io && exec python /pyroscope-python/integration-test/testdata/workload.py",
		},
	})
}

func startPythonTestContainer(t *testing.T, net *dockertest.Network, wheelDir, script string, env map[string]string) *dockertest.Container {
	t.Helper()
	mergedEnv := map[string]string{
		"PYTHONUNBUFFERED":              "1",
		"PYTHONDONTWRITEBYTECODE":       "1",
		"PYROSCOPE_SERVER_ADDRESS":      "http://pyroscope:4040",
		"PIP_DISABLE_PIP_VERSION_CHECK": "1",
	}
	for k, v := range env {
		mergedEnv[k] = v
	}
	return dockertest.StartContainer(t, dockertest.ContainerRequest{
		Image:    pythonImage(),
		Platform: wheelDockerPlatform(),
		Network:  net.Name,
		Env:      mergedEnv,
		Volumes: []string{
			repoRoot() + ":/pyroscope-python:ro",
			wheelDir + ":/pyroscope-wheels:ro",
		},
		Cmd: []string{
			"sh",
			"-c",
			fmt.Sprintf(
				"python -m pip install --no-cache-dir --no-index --find-links /pyroscope-wheels pyroscope-io && exec python /pyroscope-python/integration-test/testdata/%s",
				script,
			),
		},
	})
}

func requireContainerExit(t *testing.T, container *dockertest.Container, expected int, timeout time.Duration) {
	t.Helper()
	code := container.Wait(t, timeout)
	if code != expected {
		t.Fatalf("container exited with %d, expected %d\nlogs:\n%s", code, expected, container.Logs(t))
	}
	requireNoSanitizerReport(t, container)
}

func requireCleanStop(t *testing.T, workload *dockertest.Container) {
	t.Helper()
	workload.Stop(t, 30*time.Second)
	state, err := workload.State()
	if err != nil {
		t.Errorf("failed to inspect workload container after stop: %v", err)
		return
	}
	if state.Running || state.ExitCode != 0 {
		t.Errorf("workload container did not exit cleanly after stop (running=%t, exit code %d)\nlogs:\n%s",
			state.Running, state.ExitCode, workload.Logs(t))
		return
	}
	requireNoSanitizerReport(t, workload)
}

func requireNoSanitizerReport(t *testing.T, container *dockertest.Container) {
	t.Helper()
	if logs := container.Logs(t); strings.Contains(logs, "ERROR: AddressSanitizer") {
		t.Errorf("container logged an AddressSanitizer report\nlogs:\n%s", logs)
	}
}

// Wait for a substantial profile and check exact complete stacks and their weight.
// A successful finite workload may exit before Pyroscope makes its data queryable.
func requireProfileShape(
	t *testing.T,
	workload *dockertest.Container,
	pyroscopeURL, profileTypeID, labelSelector string,
	want profileShape,
	timeout time.Duration,
) {
	t.Helper()
	require.Eventually(t, func() bool {
		state, err := workload.State()
		if err != nil {
			t.Logf("failed to inspect workload container: %v", err)
			return false
		}
		if !state.Running && state.ExitCode != 0 {
			t.Fatalf("workload container exited with %d while waiting for profile %s\nlogs:\n%s",
				state.ExitCode, labelSelector, workload.Logs(t))
		}
		collapsed, err := queryProfile(pyroscopeURL, profileTypeID, labelSelector)
		if err != nil {
			t.Logf("query failed for %s: %v", labelSelector, err)
			return false
		}
		if err := checkProfileShape(collapsed, want); err != nil {
			t.Logf("profile %s: %v\n%s", labelSelector, err, collapsed)
			return false
		}
		t.Logf("profile %s:\n%s", labelSelector, collapsed)
		return true
	}, timeout, 5*time.Second, "expected exact stacks and sample proportions in %s", labelSelector)
}

func queryProfile(pyroscopeURL string, profileTypeID string, labelSelector string) (string, error) {
	qc := querier.NewClient(&http.Client{Timeout: 10 * time.Second}, pyroscopeURL)

	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()

	to := time.Now()
	from := to.Add(-1 * time.Hour)
	maxNodes := int64(65536)
	resp, err := qc.SelectMergeStacktraces(ctx, &querier.SelectMergeStacktracesRequest{
		ProfileTypeID: profileTypeID,
		Start:         from.UnixMilli(),
		End:           to.UnixMilli(),
		LabelSelector: labelSelector,
		MaxNodes:      &maxNodes,
		Format:        querier.ProfileFormat_PROFILE_FORMAT_TREE,
	})
	if err != nil {
		return "", err
	}
	if len(resp.Tree) == 0 {
		return "", nil
	}
	tt, err := model.UnmarshalTree(resp.Tree)
	if err != nil {
		return "", err
	}
	buf := bytes.NewBuffer(nil)
	tt.WriteCollapsed(buf)
	return buf.String(), nil
}

func ensureWheel(t *testing.T) string {
	t.Helper()

	dir := wheelDir()
	target := wheelBuildTarget()
	pattern := wheelPattern()
	matches, err := filepath.Glob(filepath.Join(dir, pattern))
	if err != nil {
		t.Fatalf("invalid integration test wheel pattern %q: %v", pattern, err)
	}
	if len(matches) == 0 {
		t.Fatalf(
			"missing prebuilt integration test wheel in %s; expected %q for target %q. Build it before running tests with `make -C %s %s`, or set PYROSCOPE_WHEEL_DIR to a directory containing the wheel.",
			dir,
			pattern,
			target,
			repoRoot(),
			target,
		)
	}
	t.Logf("using integration test wheel %s", strings.Join(matches, ", "))
	return dir
}

func wheelDir() string {
	if dir := os.Getenv("PYROSCOPE_WHEEL_DIR"); dir != "" {
		return absPath(dir)
	}
	return filepath.Join(repoRoot(), "dist")
}

func wheelPattern() string {
	if glob := os.Getenv("PYROSCOPE_WHEEL_GLOB"); glob != "" {
		return glob
	}
	target := wheelBuildTarget()
	platform := "manylinux"
	if strings.HasPrefix(target, "musllinux/") {
		platform = "musllinux"
	}
	return fmt.Sprintf("*%s*%s*.whl", platform, wheelArch(target))
}

func wheelBuildTarget() string {
	if target := os.Getenv("PYROSCOPE_WHEEL_TARGET"); target != "" {
		return target
	}
	target := "linux"
	if strings.Contains(pythonImageSuffix(), "alpine") {
		target = "musllinux"
	}
	return target + "/" + makeArch()
}

func wheelDockerPlatform() string {
	parts := strings.Split(wheelBuildTarget(), "/")
	if len(parts) != 2 {
		return ""
	}
	return "linux/" + parts[1]
}

func wheelArch(target string) string {
	if strings.HasSuffix(target, "/arm64") {
		return "aarch64"
	}
	return "x86_64"
}

func makeArch() string {
	switch runtime.GOARCH {
	case "arm64":
		return "arm64"
	default:
		return "amd64"
	}
}

func pythonImage() string {
	if image := os.Getenv("PYTHON_IMAGE"); image != "" {
		return image
	}
	return fmt.Sprintf(
		"python:%s-%s",
		envOrDefault("PYTHON_VERSION", "3.11"),
		pythonImageSuffix(),
	)
}

func pythonImageSuffix() string {
	return envOrDefault("PYTHON_IMAGE_SUFFIX", "slim")
}

func repoRoot() string {
	_, filename, _, _ := runtime.Caller(0)
	return absPath(filepath.Dir(filepath.Dir(filename)))
}

func absPath(path string) string {
	absolute, err := filepath.Abs(path)
	if err != nil {
		panic(err)
	}
	return absolute
}

func randomHex(t *testing.T, n int) string {
	t.Helper()
	b := make([]byte, n)
	if _, err := rand.Read(b); err != nil {
		t.Fatalf("failed to generate random canary: %v", err)
	}
	return hex.EncodeToString(b)
}

func envOrDefault(key, defaultValue string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return defaultValue
}

func boolString(v bool) string {
	if v {
		return "true"
	}
	return "false"
}

func (c profileConfig) implementation() string {
	if c.stack {
		return "Stack"
	}
	if c.signalTimer != "" {
		return "Signal"
	}
	return "PySpy"
}

func (c profileConfig) String() string {
	s := fmt.Sprintf("oncpu=%s/gil_only=%s/impl=%s", boolString(c.onCPU), boolString(c.gilOnly), c.implementation())
	if c.signalTimer != "" {
		s += "/timer=" + c.signalTimer
	}
	return s
}
