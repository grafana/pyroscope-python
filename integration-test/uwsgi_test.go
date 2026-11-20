package integrationtest

import (
	"fmt"
	"os/exec"
	"testing"
	"time"

	"pyroscope-python-integration-test/dockertest"
)

func TestPythonProfilerUWSGI(t *testing.T) {
	// Default local runs to CPython 3.15; CI selects versions explicitly.
	t.Setenv("PYTHON_VERSION", envOrDefault("PYTHON_VERSION", "3.15"))

	var image string
	t.Cleanup(func() {
		if image != "" {
			if out, err := exec.Command("docker", "image", "rm", image).CombinedOutput(); err != nil {
				t.Errorf("remove uWSGI image: %v\n%s", err, out)
			}
		}
	})
	for _, mode := range []struct {
		name    string
		command []string
	}{
		{"embedded", []string{"uwsgi"}},
		{"plugin", []string{"uwsgi-core", "--plugin", "/usr/local/lib/uwsgi/python_plugin.so"}},
	} {
		t.Run(mode.name, func(t *testing.T) {
			for _, tc := range []struct {
				name string
				cfg  profileConfig
			}{
				{"py-spy", profileConfig{onCPU: true, gilOnly: true}},
				{"signal-itimer", profileConfig{onCPU: true, signalTimer: "ITimer"}},
				{"signal-ctimer", profileConfig{onCPU: true, signalTimer: "CTimer"}},
				{"stack", profileConfig{onCPU: true, stack: true}},
			} {
				t.Run(tc.name, func(t *testing.T) {
					skipUnlessProfiler(t, tc.cfg.profilerName())
					if tc.cfg.signalTimer != "" {
						skipUnlessPythonSupportsSignal(t)
					}
					wheelDir := ensureWheel(t)
					if image == "" {
						image = buildUWSGIImage(t)
					}
					command := append([]string{}, mode.command...)
					command = append(command, "--http-socket", ":8080", "--master", "--processes", "2",
						"--enable-threads", "--single-interpreter", "--lazy-apps", "--py-call-uwsgi-fork-hooks",
						"--need-app", "--die-on-term", "--disable-logging",
						"--wsgi-file", "/pyroscope-python/integration-test/testdata/uwsgi_workload.py")
					// uWSGI enters Python directly from C in both loading modes.
					testHTTPServerProfiler(t, image, wheelDir, tc.cfg, command, "application;")
				})
			}
		})
	}
}

func buildUWSGIImage(t *testing.T) string {
	t.Helper()
	return dockertest.BuildImage(t, dockertest.BuildRequest{
		Context:    repoRoot() + "/integration-test/testdata",
		Dockerfile: repoRoot() + "/integration-test/testdata/uwsgi.Dockerfile",
		BuildArgs:  map[string]string{"PYTHON_IMAGE": pythonImage()},
		Platform:   wheelDockerPlatform(),
		Tag:        fmt.Sprintf("pyroscope-python-uwsgi-test:%d", time.Now().UnixNano()),
	})
}
