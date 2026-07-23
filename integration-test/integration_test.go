package integrationtest

import (
	"bytes"
	"context"
	"crypto/rand"
	"encoding/hex"
	"fmt"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
	"time"

	"pyroscope-python-integration-test/api/querier"
	"pyroscope-python-integration-test/dockertest"
	"pyroscope-python-integration-test/processtest"
	"pyroscope-python-integration-test/pyroscope/model"
	"pyroscope-python-integration-test/require"
)

const (
	cpuProfileTypeID              = "process_cpu:cpu:nanoseconds:cpu:nanoseconds"
	memoryAllocSpaceProfileTypeID = "memory:alloc_space:bytes:space:bytes"
	memoryInuseSpaceProfileTypeID = "memory:inuse_space:bytes:space:bytes"
)

type profileConfig struct {
	onCPU   bool
	gilOnly bool
}

type workloadState struct {
	running  bool
	exitCode int
}

type testWorkload interface {
	stop(*testing.T, time.Duration)
	wait(*testing.T, time.Duration) int
	logs(*testing.T) string
	state() (workloadState, error)
}

type containerWorkload struct {
	container *dockertest.Container
}

func (w *containerWorkload) stop(t *testing.T, timeout time.Duration) {
	w.container.Stop(t, timeout)
}

func (w *containerWorkload) wait(t *testing.T, timeout time.Duration) int {
	return w.container.Wait(t, timeout)
}

func (w *containerWorkload) logs(t *testing.T) string {
	return w.container.Logs(t)
}

func (w *containerWorkload) state() (workloadState, error) {
	state, err := w.container.State()
	return workloadState{running: state.Running, exitCode: state.ExitCode}, err
}

type processWorkload struct {
	process *processtest.Process
}

func (w *processWorkload) stop(t *testing.T, timeout time.Duration) {
	w.process.Stop(t, timeout)
}

func (w *processWorkload) wait(t *testing.T, timeout time.Duration) int {
	return w.process.Wait(t, timeout)
}

func (w *processWorkload) logs(_ *testing.T) string {
	return w.process.Logs()
}

func (w *processWorkload) state() (workloadState, error) {
	state := w.process.State()
	return workloadState{running: state.Running, exitCode: state.ExitCode}, nil
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

func TestPythonNonCPUIntegrationSuites(t *testing.T) {
	t.Run("memory profiler", testPythonMemoryProfiler)
	t.Run("concurrent configure shutdown", testPythonConcurrentConfigureShutdown)
	t.Run("atexit shutdown", testPythonAtexitShutdown)
}

func testPythonMemoryProfiler(t *testing.T) {
	wheelDir := ensureWheel(t)

	net := createNetwork(t)
	pyroscopeURL := startPyroscope(t, net)
	appName := fmt.Sprintf("pyroscopers.python.test.memory.%d", time.Now().UnixNano())
	canary := randomHex(t, 16)
	workload := startPythonTest(t, net, pyroscopeURL, wheelDir, "memory_workload.py", map[string]string{
		"PYROSCOPE_APPLICATION_NAME": appName,
		"CANARY":                     canary,
	})
	t.Cleanup(func() {
		workload.stop(t, 30*time.Second)
	})

	labelSelector := fmt.Sprintf(`{service_name="%s",canary="%s"}`, appName, canary)
	profiles := []struct {
		name          string
		profileTypeID string
	}{
		{name: "alloc_space", profileTypeID: memoryAllocSpaceProfileTypeID},
		{name: "inuse_space", profileTypeID: memoryInuseSpaceProfileTypeID},
	}
	for _, profile := range profiles {
		require.Eventually(t, func() bool {
			collapsed, err := queryProfile(pyroscopeURL, profile.profileTypeID, labelSelector)
			if err != nil {
				t.Logf("query failed for %s: %v", profile.name, err)
				return false
			}
			if collapsed == "" {
				t.Logf("memory profile %s is empty", profile.name)
				return false
			}
			if !strings.Contains(collapsed, "memhog") {
				t.Logf("memory profile %s does not contain memhog yet:\n%s", profile.name, collapsed)
				return false
			}
			return true
		}, 3*time.Minute, 5*time.Second, "expected memhog samples in %s", profile.name)
	}
}

func testPythonConcurrentConfigureShutdown(t *testing.T) {
	wheelDir := ensureWheel(t)

	net := createNetwork(t)
	pyroscopeURL := startPyroscope(t, net)
	appName := fmt.Sprintf("pyroscopers.python.test.concurrency.%d", time.Now().UnixNano())
	workload := startPythonTest(t, net, pyroscopeURL, wheelDir, "concurrency_workload.py", map[string]string{
		"PYROSCOPE_APPLICATION_NAME": appName,
	})

	requireWorkloadExit(t, workload, 0, 3*time.Minute)
}

func testPythonAtexitShutdown(t *testing.T) {
	wheelDir := ensureWheel(t)

	net := createNetwork(t)
	pyroscopeURL := startPyroscope(t, net)
	appName := fmt.Sprintf("pyroscopers.python.test.atexit.%d", time.Now().UnixNano())
	workload := startPythonTest(t, net, pyroscopeURL, wheelDir, "atexit_workload.py", map[string]string{
		"PYROSCOPE_APPLICATION_NAME": appName,
	})

	requireWorkloadExit(t, workload, 0, 2*time.Minute)
}

func testPythonProfilerConfiguration(t *testing.T, cfg profileConfig) {
	t.Helper()
	wheelDir := ensureWheel(t)

	net := createNetwork(t)
	pyroscopeURL := startPyroscope(t, net)
	appName := fmt.Sprintf("pyroscopers.python.test.%d", time.Now().UnixNano())
	canary := randomHex(t, 16)
	workload := startWorkload(t, net, pyroscopeURL, appName, canary, cfg, wheelDir)
	t.Cleanup(func() {
		workload.stop(t, 30*time.Second)
	})

	labelSelector := fmt.Sprintf(
		`{service_name="%s",canary="%s",oncpu="%s",gil_only="%s"}`,
		appName,
		canary,
		boolString(cfg.onCPU),
		boolString(cfg.gilOnly),
	)

	var workloadExited bool
	var workloadExitCode int
	require.Eventually(t, func() bool {
		state, err := workload.state()
		if err != nil {
			t.Logf("failed to inspect workload: %v", err)
			return false
		}
		if !state.running {
			workloadExited = true
			workloadExitCode = state.exitCode
			return true
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
		if !strings.Contains(collapsed, "multihash") {
			t.Logf("profile for %s does not contain multihash yet:\n%s", cfg, collapsed)
			return false
		}
		return true
	}, 3*time.Minute, 5*time.Second, "expected multihash samples for %s", cfg)
	if workloadExited {
		t.Fatalf("workload exited before producing the expected profile (exit code %d)", workloadExitCode)
	}
}

func createNetwork(t *testing.T) *dockertest.Network {
	t.Helper()
	if nativeMode() {
		return nil
	}
	return dockertest.CreateNetwork(t)
}

func startPyroscope(t *testing.T, net *dockertest.Network) string {
	t.Helper()
	if nativeMode() {
		httpPort := availablePort(t)
		endpoint := fmt.Sprintf("http://127.0.0.1:%d", httpPort)
		processtest.Start(t, processtest.Request{
			Name: envOrDefault("PYROSCOPE_BINARY", "pyroscope"),
			Args: []string{
				fmt.Sprintf("-server.http-listen-port=%d", httpPort),
			},
			Dir:     t.TempDir(),
			WaitURL: endpoint + "/ready",
			Timeout: 2 * time.Minute,
		})
		return endpoint
	}
	c := dockertest.StartContainer(t, dockertest.ContainerRequest{
		Image:          envOrDefault("PYROSCOPE_IMAGE", "grafana/pyroscope"),
		ExposedPorts:   []string{"4040/tcp"},
		Network:        net.Name,
		NetworkAliases: []string{"pyroscope"},
		WaitFor:        dockertest.WaitForHTTP("/ready", "4040/tcp", 2*time.Minute),
	})
	return fmt.Sprintf("http://%s", c.HostPort(t, "4040/tcp"))
}

func startWorkload(t *testing.T, net *dockertest.Network, pyroscopeURL, appName, canary string, cfg profileConfig, wheelDir string) testWorkload {
	t.Helper()
	env := map[string]string{
		"PYTHONUNBUFFERED":              "1",
		"PYTHONDONTWRITEBYTECODE":       "1",
		"PYROSCOPE_APPLICATION_NAME":    appName,
		"PYROSCOPE_SERVER_ADDRESS":      workloadServerAddress(pyroscopeURL),
		"ONCPU":                         boolString(cfg.onCPU),
		"GIL_ONLY":                      boolString(cfg.gilOnly),
		"CANARY":                        canary,
		"PIP_DISABLE_PIP_VERSION_CHECK": "1",
	}
	if nativeMode() {
		return startNativePython(t, wheelDir, "workload.py", env)
	}
	return &containerWorkload{container: dockertest.StartContainer(t, dockertest.ContainerRequest{
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
			"python -m pip install --no-cache-dir --no-index --find-links /pyroscope-wheels pyroscope-io && python /pyroscope-python/integration-test/testdata/workload.py",
		},
	})}
}

func startPythonTest(t *testing.T, net *dockertest.Network, pyroscopeURL, wheelDir, script string, env map[string]string) testWorkload {
	t.Helper()
	mergedEnv := map[string]string{
		"PYTHONUNBUFFERED":              "1",
		"PYTHONDONTWRITEBYTECODE":       "1",
		"PYROSCOPE_SERVER_ADDRESS":      workloadServerAddress(pyroscopeURL),
		"PIP_DISABLE_PIP_VERSION_CHECK": "1",
	}
	for k, v := range env {
		mergedEnv[k] = v
	}
	if nativeMode() {
		return startNativePython(t, wheelDir, script, mergedEnv)
	}
	return &containerWorkload{container: dockertest.StartContainer(t, dockertest.ContainerRequest{
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
				"python -m pip install --no-cache-dir --no-index --find-links /pyroscope-wheels pyroscope-io && python /pyroscope-python/integration-test/testdata/%s",
				script,
			),
		},
	})}
}

func startNativePython(t *testing.T, wheelDir, script string, env map[string]string) testWorkload {
	t.Helper()
	venv := filepath.Join(t.TempDir(), "venv")
	python := envOrDefault("PYROSCOPE_PYTHON_BINARY", "python3")
	processtest.Run(t, python, "-m", "venv", venv)
	venvPython := filepath.Join(venv, "bin", "python")
	processtest.Run(
		t,
		venvPython,
		"-m", "pip", "install",
		"--no-cache-dir",
		"--no-index",
		"--find-links", wheelDir,
		"pyroscope-io",
	)
	return &processWorkload{process: processtest.Start(t, processtest.Request{
		Name: venvPython,
		Args: []string{filepath.Join(repoRoot(), "integration-test", "testdata", script)},
		Dir:  repoRoot(),
		Env:  env,
	})}
}

func workloadServerAddress(pyroscopeURL string) string {
	if nativeMode() {
		return pyroscopeURL
	}
	return "http://pyroscope:4040"
}

func requireWorkloadExit(t *testing.T, workload testWorkload, expected int, timeout time.Duration) {
	t.Helper()
	code := workload.wait(t, timeout)
	if code != expected {
		t.Fatalf("workload exited with %d, expected %d\nlogs:\n%s", code, expected, workload.logs(t))
	}
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
	target := wheelBuildTarget()
	platform := "manylinux"
	arch := wheelArch(target)
	switch {
	case strings.HasPrefix(target, "musllinux/"):
		platform = "musllinux"
	case strings.HasPrefix(target, "mac/"):
		platform = "macosx_11_0"
		if arch == "aarch64" {
			arch = "arm64"
		}
	}
	return fmt.Sprintf("*%s*%s*.whl", platform, arch)
}

func wheelBuildTarget() string {
	if target := os.Getenv("PYROSCOPE_WHEEL_TARGET"); target != "" {
		return target
	}
	if runtime.GOOS == "darwin" {
		return "mac/" + makeArch()
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

func nativeMode() bool {
	return os.Getenv("PYROSCOPE_INTEGRATION_TEST_MODE") == "native"
}

func availablePort(t *testing.T) int {
	t.Helper()
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("failed to reserve a local port: %v", err)
	}
	defer listener.Close()
	return listener.Addr().(*net.TCPAddr).Port
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

func (c profileConfig) String() string {
	return fmt.Sprintf("oncpu=%s/gil_only=%s", boolString(c.onCPU), boolString(c.gilOnly))
}
