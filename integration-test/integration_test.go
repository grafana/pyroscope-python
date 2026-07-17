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

const profileTypeID = "process_cpu:cpu:nanoseconds:cpu:nanoseconds"

type profileConfig struct {
	onCPU   bool
	gilOnly bool
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

func testPythonProfilerConfiguration(t *testing.T, cfg profileConfig) {
	t.Helper()
	wheelDir := ensureWheel(t)

	net := dockertest.CreateNetwork(t)
	pyroscopeURL := startPyroscope(t, net)
	appName := fmt.Sprintf("pyroscopers.python.test.%d", time.Now().UnixNano())
	canary := randomHex(t, 16)
	workload := startWorkload(t, net, appName, canary, cfg, wheelDir)
	t.Cleanup(func() {
		workload.Stop(t, 30*time.Second)
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

		collapsed, err := queryProfile(pyroscopeURL, labelSelector)
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
		t.Fatalf("workload container exited before producing the expected profile (exit code %d)", workloadExitCode)
	}
}

func startPyroscope(t *testing.T, net *dockertest.Network) string {
	t.Helper()
	c := dockertest.StartContainer(t, dockertest.ContainerRequest{
		Image:          envOrDefault("PYROSCOPE_IMAGE", "grafana/pyroscope"),
		ExposedPorts:   []string{"4040/tcp"},
		Network:        net.Name,
		NetworkAliases: []string{"pyroscope"},
		WaitFor:        dockertest.WaitForHTTP("/ready", "4040/tcp", 2*time.Minute),
	})
	return fmt.Sprintf("http://%s", c.HostPort(t, "4040/tcp"))
}

func startWorkload(t *testing.T, net *dockertest.Network, appName, canary string, cfg profileConfig, wheelDir string) *dockertest.Container {
	t.Helper()
	return dockertest.StartContainer(t, dockertest.ContainerRequest{
		Image:    pythonImage(),
		Platform: wheelDockerPlatform(),
		Network:  net.Name,
		Env: map[string]string{
			"PYTHONUNBUFFERED":              "1",
			"PYTHONDONTWRITEBYTECODE":       "1",
			"PYROSCOPE_APPLICATION_NAME":    appName,
			"PYROSCOPE_SERVER_ADDRESS":      "http://pyroscope:4040",
			"ONCPU":                         boolString(cfg.onCPU),
			"GIL_ONLY":                      boolString(cfg.gilOnly),
			"CANARY":                        canary,
			"PIP_DISABLE_PIP_VERSION_CHECK": "1",
		},
		Volumes: []string{
			repoRoot() + ":/pyroscope-python:ro",
			wheelDir + ":/pyroscope-wheels:ro",
		},
		Cmd: []string{
			"sh",
			"-c",
			"python -m pip install --no-cache-dir --no-index --find-links /pyroscope-wheels pyroscope-io && python /pyroscope-python/integration-test/testdata/workload.py",
		},
	})
}

func queryProfile(pyroscopeURL string, labelSelector string) (string, error) {
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

func (c profileConfig) String() string {
	return fmt.Sprintf("oncpu=%s/gil_only=%s", boolString(c.onCPU), boolString(c.gilOnly))
}
