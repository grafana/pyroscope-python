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

func TestPythonProfilerGunicorn(t *testing.T) {
	testPythonHTTPServer(t, "gunicorn")
}

func TestPythonProfilerUvicorn(t *testing.T) {
	testPythonHTTPServer(t, "uvicorn")
}

func testPythonHTTPServer(t *testing.T, server string) {
	t.Helper()
	t.Setenv("PYTHON_VERSION", envOrDefault("PYTHON_VERSION", "3.15"))
	var image string
	t.Cleanup(func() {
		if image != "" {
			if out, err := exec.Command("docker", "image", "rm", image).CombinedOutput(); err != nil {
				t.Errorf("remove HTTP server image: %v\n%s", err, out)
			}
		}
	})
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
				image = dockertest.BuildImage(t, dockertest.BuildRequest{
					Context:    repoRoot() + "/integration-test/testdata",
					Dockerfile: repoRoot() + "/integration-test/testdata/http-servers.Dockerfile",
					BuildArgs:  map[string]string{"PYTHON_IMAGE": pythonImage()},
					Platform:   wheelDockerPlatform(),
					Tag:        fmt.Sprintf("pyroscope-python-http-test:%d", time.Now().UnixNano()),
				})
			}
			var command []string
			switch server {
			case "gunicorn":
				command = []string{"gunicorn", "--bind", "0.0.0.0:8080", "--workers", "2",
					"--worker-class", "sync", "--timeout", "60", "--graceful-timeout", "25",
					"--config", "/pyroscope-python/integration-test/testdata/gunicorn_conf.py",
					"http_workload:application"}
			case "uvicorn":
				command = []string{"uvicorn", "http_workload:asgi_application", "--host", "0.0.0.0",
					"--port", "8080", "--workers", "2", "--loop", "asyncio", "--http", "h11",
					"--ws", "none", "--interface", "asgi3", "--lifespan", "on",
					"--no-proxy-headers", "--no-access-log", "--timeout-graceful-shutdown", "25"}
			default:
				t.Fatalf("unknown HTTP server %q", server)
			}
			testHTTPServerProfiler(t, image, wheelDir, tc.cfg, command, httpServerStackPrefix(t, server))
		})
	}
}

func httpServerStackPrefix(t *testing.T, server string) string {
	t.Helper()
	if server == "gunicorn" {
		// Forking preserves the CLI/master frames below the sync request worker.
		return "<module>;run;Application.run;BaseApplication.run;Arbiter.run;" +
			"Arbiter.manage_workers;Arbiter.spawn_workers;Arbiter.spawn_worker;Worker.init_process;" +
			"SyncWorker.run;SyncWorker.run_for_one;SyncWorker.accept;SyncWorker.handle;" +
			"SyncWorker.handle_request;application;"
	}
	// Uvicorn uses spawn and schedules the ASGI request on the asyncio loop.
	// Its CPython 3.11 compatibility shim supplies loop_factory via Runner;
	// CPython 3.12+ supplies this argument in asyncio.run itself.
	_, minor := pythonVersion(t)
	runner := "run"
	if minor == 11 {
		runner = "asyncio_run"
	}
	return "<module>;spawn_main;_main;BaseProcess._bootstrap;BaseProcess.run;" +
		"subprocess_started;Process.target;Server.run;" + runner + ";Runner.run;" +
		"BaseEventLoop.run_until_complete;BaseEventLoop.run_forever;BaseEventLoop._run_once;" +
		"Handle._run;RequestResponseCycle.run_asgi;asgi_application;"
}

func testHTTPServerProfiler(t *testing.T, image, wheelDir string, cfg profileConfig, command []string, prefix string) {
	t.Helper()
	net := dockertest.CreateNetwork(t)
	pyroscopeURL := startPyroscope(t, net)
	appName := fmt.Sprintf("pyroscopers.python.test.http.%d", time.Now().UnixNano())
	canary := randomHex(t, 16)
	server := dockertest.StartContainer(t, dockertest.ContainerRequest{
		Image: image, Platform: wheelDockerPlatform(), Network: net.Name,
		ExposedPorts: []string{"8080/tcp"},
		Env: map[string]string{
			"PYTHONPATH":                    "/pyroscope-python/integration-test/testdata",
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
exec "$@"
`, "http-server-test"}, command...),
		WaitFor: dockertest.WaitForHTTP("/ready", "8080/tcp", time.Minute),
	})
	workers := driveHTTPWorkload(t, "http://"+server.HostPort(t, "8080/tcp"))
	// Stop after the requests finish, so the queries include the final
	// flushed profiles. The container helper also removes the entire
	// process tree if readiness, traffic or an assertion fails.
	requireCleanStop(t, server)
	logs := server.Logs(t)
	for worker, pid := range workers {
		stopped := fmt.Sprintf("profiler stopped in worker %d pid %d", worker, pid)
		if !slices.Contains(strings.Split(logs, "\n"), stopped) {
			t.Errorf("worker did not shut down its profiler: %s\nlogs:\n%s", stopped, logs)
		}
		selector := fmt.Sprintf(`{service_name="%s",canary="%s",worker="%d",worker_pid="%d"}`,
			appName, canary, worker, pid)
		// Each request performs equal work in two leaves. Allow 15 percentage
		// points of sampling/scheduling noise and at most 10% other weight.
		// Require at least two sampled CPU seconds per worker (200 periods
		// at 100 Hz). The prefix includes every observed server/bootstrap frame.
		requireProfileShape(t, server, pyroscopeURL, cpuProfileTypeID, selector, profileShape{
			stacks: []stackShare{
				{prefix + "workload;first_branch;burn_cpu", 0.35, 0.65},
				{prefix + "workload;second_branch;burn_cpu", 0.35, 0.65},
			},
			minTotal: int64(2 * time.Second), minCoverage: 0.90,
		}, time.Minute)
	}
}

func driveHTTPWorkload(t *testing.T, url string) map[int]int {
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
	// New connections avoid pinning both clients to the same Uvicorn worker.
	client := &http.Client{
		Timeout:   15 * time.Second,
		Transport: &http.Transport{DisableKeepAlives: true},
	}
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
					r.err = fmt.Errorf("request returned HTTP status %d: %v", resp.StatusCode, err)
					return
				}
				r.responses = append(r.responses, body)
			}
		}()
	}
	workers := make(map[int]int)
	pids := make(map[int]bool)
	counts := make(map[int]int)
	for range 2 {
		r := <-results
		if r.err != nil {
			t.Errorf("HTTP server load: %v", r.err)
		}
		for _, resp := range r.responses {
			if resp.Worker < 1 || resp.PID <= 0 || resp.Result != 1319342467 {
				t.Errorf("unexpected HTTP server response: %+v", resp)
			}
			if pid, ok := workers[resp.Worker]; ok && pid != resp.PID {
				t.Errorf("HTTP server worker %d restarted: PID %d -> %d", resp.Worker, pid, resp.PID)
			}
			workers[resp.Worker] = resp.PID
			pids[resp.PID] = true
			counts[resp.Worker]++
		}
	}
	if len(workers) != 2 || len(pids) != 2 {
		t.Errorf("expected HTTP responses from two distinct HTTP server workers, got %v", workers)
	}
	t.Logf("HTTP server worker PIDs: %v; completed requests: %v", workers, counts)
	if t.Failed() {
		t.FailNow()
	}
	return workers
}
