package integrationtest

import (
	"encoding/json"
	"fmt"
	"net/http"
	"os/exec"
	"slices"
	"strings"
	"testing"
	"time"

	"pyroscope-python-integration-test/dockertest"
)

func TestPythonProfilerUWSGI(t *testing.T) {
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
					testUWSGIProfiler(t, image, wheelDir, tc.cfg, mode.command)
				})
			}
		})
	}
}

func testUWSGIProfiler(t *testing.T, image, wheelDir string, cfg profileConfig, command []string) {
	t.Helper()
	net := dockertest.CreateNetwork(t)
	pyroscopeURL := startPyroscope(t, net)
	appName := fmt.Sprintf("pyroscopers.python.test.uwsgi.%d", time.Now().UnixNano())
	canary := randomHex(t, 16)
	server := dockertest.StartContainer(t, dockertest.ContainerRequest{
		Image: image, Platform: wheelDockerPlatform(), Network: net.Name,
		ExposedPorts: []string{"8080/tcp"},
		Env: map[string]string{
			"PYTHONUNBUFFERED":              "1",
			"PYTHONDONTWRITEBYTECODE":       "1",
			"PIP_DISABLE_PIP_VERSION_CHECK": "1",
			"PYROSCOPE_APPLICATION_NAME":    appName,
			"PYROSCOPE_SERVER_ADDRESS":      "http://pyroscope:4040",
			"CPU_IMPLEMENTATION":            cfg.implementation(),
			"SIGNAL_TIMER":                  cfg.signalTimer,
			"CANARY":                        canary,
		},
		Volumes: []string{
			repoRoot() + ":/pyroscope-python:ro",
			wheelDir + ":/pyroscope-wheels:ro",
		},
		Cmd: append([]string{"sh", "-ec", `
python -m pip install --no-cache-dir --no-index --find-links /pyroscope-wheels pyroscope-io
exec "$@" --http-socket :8080 --master --processes 2 \
    --enable-threads --single-interpreter --lazy-apps --py-call-uwsgi-fork-hooks \
    --need-app --die-on-term --disable-logging \
    --wsgi-file /pyroscope-python/integration-test/testdata/uwsgi_workload.py
`, "uwsgi-test"}, command...),
		WaitFor: dockertest.WaitForHTTP("/ready", "8080/tcp", time.Minute),
	})
	workers := driveUWSGI(t, "http://"+server.HostPort(t, "8080/tcp"))
	// Stop after the requests finish, so the queries include the final
	// flushed profiles. The container helper also removes the entire
	// process tree if readiness, traffic or an assertion fails.
	requireCleanStop(t, server)
	logs := server.Logs(t)
	for worker := 1; worker <= 2; worker++ {
		pid := workers[worker]
		stopped := fmt.Sprintf("profiler stopped in worker %d pid %d", worker, pid)
		if !slices.Contains(strings.Split(logs, "\n"), stopped) {
			t.Errorf("worker did not shut down its profiler: %s\nlogs:\n%s", stopped, logs)
		}
		selector := fmt.Sprintf(`{service_name="%s",canary="%s",worker="%d",worker_pid="%d"}`,
			appName, canary, worker, pid)
		// uWSGI calls application directly from C: there is no <module>
		// or Thread bootstrap prefix. These module-level function names
		// are identical on CPython 3.11–3.15 for every CPU implementation
		// (including Stack's wall sampler on 3.11). Each request spends
		// equal work in the two leaves: allow 15 percentage points of
		// sampling/scheduling noise, at most 10% other weight, and require
		// at least 2 sampled CPU seconds per worker, not a lucky sample.
		requireProfileShape(t, server, pyroscopeURL, cpuProfileTypeID, selector, profileShape{
			stacks: []stackShare{
				{"application;workload;first_branch;burn_cpu", 0.35, 0.65},
				{"application;workload;second_branch;burn_cpu", 0.35, 0.65},
			},
			minTotal: int64(2 * time.Second), minCoverage: 0.90,
		}, time.Minute)
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

func driveUWSGI(t *testing.T, url string) map[int]int {
	t.Helper()
	type response struct {
		Worker int
		PID    int
		Result uint64
	}
	type result struct {
		responses []response
		err       error
	}
	client := &http.Client{Timeout: 15 * time.Second}
	defer client.CloseIdleConnections()
	deadline := time.Now().Add(15 * time.Second)
	results := make(chan result, 2)
	for range 2 {
		go func() {
			var r result
			defer func() { results <- r }()
			for time.Now().Before(deadline) {
				resp, err := client.Get(url + "/work")
				if err != nil {
					r.err = err
					return
				}
				var body response
				err = json.NewDecoder(resp.Body).Decode(&body)
				resp.Body.Close()
				if err != nil || resp.StatusCode != http.StatusOK {
					r.err = fmt.Errorf("uWSGI HTTP status %d: %v", resp.StatusCode, err)
					return
				}
				r.responses = append(r.responses, body)
			}
		}()
	}
	workers := make(map[int]int)
	counts := make(map[int]int)
	for range 2 {
		r := <-results
		if r.err != nil {
			t.Errorf("uWSGI load: %v", r.err)
		}
		for _, resp := range r.responses {
			if resp.Worker < 1 || resp.Worker > 2 || resp.PID <= 0 || resp.Result != 1319342467 {
				t.Errorf("unexpected uWSGI response: %+v", resp)
			}
			if pid, ok := workers[resp.Worker]; ok && pid != resp.PID {
				t.Errorf("uWSGI worker %d restarted: PID %d -> %d", resp.Worker, pid, resp.PID)
			}
			workers[resp.Worker] = resp.PID
			counts[resp.Worker]++
		}
	}
	if len(workers) != 2 || workers[1] == workers[2] {
		t.Errorf("expected HTTP responses from two distinct uWSGI workers, got %v", workers)
	}
	t.Logf("uWSGI worker PIDs: %v; completed requests: %v", workers, counts)
	if t.Failed() {
		t.FailNow()
	}
	return workers
}
