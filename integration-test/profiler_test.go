package integrationtest

import (
	"flag"
	"fmt"
	"os"
	"slices"
	"testing"
)

var profiler = flag.String("profiler", "all", "CPU implementation to test: all, py-spy, signal, stack (alias: dd-trace-py)")

func TestMain(m *testing.M) {
	flag.Parse()
	if *profiler == "dd-trace-py" {
		*profiler = "stack"
	}
	if !slices.Contains([]string{"all", "py-spy", "signal", "stack"}, *profiler) {
		fmt.Fprintf(os.Stderr, "invalid -profiler=%q; want all, py-spy, signal, stack, or dd-trace-py\n", *profiler)
		os.Exit(2)
	}
	os.Exit(m.Run())
}

func skipUnlessProfiler(t *testing.T, implementations ...string) {
	t.Helper()
	if *profiler != "all" && !slices.Contains(implementations, *profiler) {
		t.Skipf("not part of the %s profiler suite", *profiler)
	}
}

func (c profileConfig) profilerName() string {
	if c.stack {
		return "stack"
	}
	if c.signalTimer != "" {
		return "signal"
	}
	return "py-spy"
}
