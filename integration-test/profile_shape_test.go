package integrationtest

import (
	"fmt"
	"strconv"
	"strings"
	"testing"
	"time"
)

type stackShare struct {
	stack    string
	min, max float64
}

// Match the whole root-to-leaf path. CPython 3.11 introduced co_qualname.
// Stack and Signal's CTimer use a bootstrap wrapper only on Python 3.10.
func threadStackPrefix(cfg profileConfig, minor int) string {
	bootstrap, inner, run := "_bootstrap", "_bootstrap_inner", "run"
	if minor >= 11 {
		bootstrap, inner, run = "Thread._bootstrap", "Thread._bootstrap_inner", "Thread.run"
	}
	prefix := bootstrap + ";"
	if minor == 10 && (cfg.stack || cfg.signalTimer == "CTimer") {
		prefix += "_bootstrap_inner;"
	}
	return prefix + inner + ";" + run + ";"
}

func configurationStacks(cfg profileConfig, minor int) []stackShare {
	prefix := threadStackPrefix(cfg, minor)
	wait := "wait;wait"
	if minor >= 11 {
		wait = "Event.wait;Condition.wait"
	}
	// Both workers spend one part hashing and two parts asleep (100/200ms,
	// or 2/4s for Stack's adaptive wall sampler). CPU/GIL-only
	// profiles should split almost all weight equally between the hash leaves.
	// Wall profiles also include the main thread waiting: ~1/3 main wait,
	// ~2/9 each sleeping worker, and ~1/9 each hash leaf. Allow scheduling
	// and sampling noise without accepting a missing worker or idle CPU profile.
	if !cfg.onCPU && !cfg.gilOnly {
		return []stackShare{
			{prefix + "multihash;hash_value", 0.06, 0.17},
			{prefix + "multihash2;hash_value", 0.06, 0.17},
			{prefix + "multihash", 0.15, 0.30},
			{prefix + "multihash2", 0.15, 0.30},
			{"<module>;main;" + wait, 0.25, 0.40},
		}
	}
	if cfg.profilerName() == "py-spy" && minor == 10 && cfg.onCPU && !cfg.gilOnly {
		// CPython 3.10 has no native_thread_id. With py-spy's unwind feature
		// disabled, its idle heuristic recognizes threading.wait but not
		// time.sleep. The main thread is excluded; each worker contributes
		// ~1/6 hashing and ~1/3 sleeping to this onCPU profile.
		return []stackShare{
			{prefix + "multihash;hash_value", 0.10, 0.25},
			{prefix + "multihash2;hash_value", 0.10, 0.25},
			{prefix + "multihash", 0.25, 0.40},
			{prefix + "multihash2", 0.25, 0.40},
		}
	}
	return []stackShare{
		{prefix + "multihash;hash_value", 0.35, 0.65},
		{prefix + "multihash2;hash_value", 0.35, 0.65},
	}
}

type profileShape struct {
	stacks      []stackShare
	minTotal    int64
	minCoverage float64
}

func checkConfigurationProfile(collapsed string, cfg profileConfig, minor int) error {
	// At 100Hz this is at least 200 samples. Do not accept two lucky samples.
	return checkProfileShape(collapsed, profileShape{
		stacks:      configurationStacks(cfg, minor),
		minTotal:    int64(2 * time.Second),
		minCoverage: 0.90,
	})
}

func checkProfileShape(collapsed string, want profileShape) error {
	values := make(map[string]int64)
	var total int64
	for _, line := range strings.Split(strings.TrimSpace(collapsed), "\n") {
		i := strings.LastIndexByte(line, ' ')
		if i < 1 {
			return fmt.Errorf("invalid collapsed stack %q", line)
		}
		value, err := strconv.ParseInt(line[i+1:], 10, 64)
		if err != nil || value <= 0 {
			return fmt.Errorf("invalid sample value in %q", line)
		}
		// WriteCollapsed includes the tree's empty root as a leading ';'.
		stack := strings.TrimPrefix(line[:i], ";")
		values[stack] += value
		total += value
	}
	if total < want.minTotal {
		return fmt.Errorf("total sample value is %d; need at least %d", total, want.minTotal)
	}
	var matched int64
	for _, want := range want.stacks {
		value := values[want.stack]
		share := float64(value) / float64(total)
		if share < want.min || share > want.max {
			return fmt.Errorf("stack %q has %d/%d (%.1f%%); want %.0f–%.0f%%", want.stack, value, total, 100*share, 100*want.min, 100*want.max)
		}
		matched += value
	}
	// Startup and loop bookkeeping can contribute a few samples; bound their
	// share using all observed weight, including unexpected stacks.
	if share := float64(matched) / float64(total); share < want.minCoverage {
		return fmt.Errorf("exact expected stacks account for %.1f%% of sample weight; want at least %.0f%%", 100*share, 100*want.minCoverage)
	}
	return nil
}

func TestConfigurationProfileShape(t *testing.T) {
	const first = "Thread._bootstrap;Thread._bootstrap_inner;Thread.run;multihash;hash_value"
	const second = "Thread._bootstrap;Thread._bootstrap_inner;Thread.run;multihash2;hash_value"
	cpu := ";" + first + " 5000000000\n;" + second + " 5000000000\n"
	wall := ";" + first + " 1000000000\n;" + second + " 1000000000\n" +
		";Thread._bootstrap;Thread._bootstrap_inner;Thread.run;multihash 2000000000\n" +
		";Thread._bootstrap;Thread._bootstrap_inner;Thread.run;multihash2 2000000000\n" +
		";<module>;main;Event.wait;Condition.wait 3000000000\n"
	for _, tc := range []struct {
		name    string
		profile string
		cfg     profileConfig
		minor   int
		valid   bool
	}{
		{"cpu", cpu, profileConfig{onCPU: true}, 12, true},
		{"gil only", cpu, profileConfig{gilOnly: true}, 12, true},
		{"wall", wall, profileConfig{}, 12, true},
		{"python 310 gil only", strings.ReplaceAll(cpu, "Thread.", ""), profileConfig{onCPU: true, gilOnly: true}, 10, true},
		{"python 310 idle heuristic", strings.ReplaceAll(strings.ReplaceAll(wall, ";<module>;main;Event.wait;Condition.wait 3000000000\n", ""), "Thread.", ""), profileConfig{onCPU: true}, 10, true},
		{"stack 311+", cpu, profileConfig{onCPU: true, stack: true}, 12, true},
		{"ctimer 311+", cpu, profileConfig{onCPU: true, signalTimer: "CTimer"}, 12, true},
		{"stack 310 wrapper", strings.ReplaceAll(strings.ReplaceAll(cpu, "Thread.", ""), "_bootstrap_inner;", "_bootstrap_inner;_bootstrap_inner;"), profileConfig{onCPU: true, stack: true}, 10, true},
		{"ctimer 310 wrapper", strings.ReplaceAll(strings.ReplaceAll(cpu, "Thread.", ""), "_bootstrap_inner;", "_bootstrap_inner;_bootstrap_inner;"), profileConfig{onCPU: true, signalTimer: "CTimer"}, 10, true},
		{"itimer", cpu, profileConfig{onCPU: true, signalTimer: "ITimer"}, 12, true},
		{"truncated stack", strings.ReplaceAll(cpu, "Thread._bootstrap;", ""), profileConfig{onCPU: true}, 12, false},
		{"wrong order", strings.ReplaceAll(cpu, "multihash;hash_value", "hash_value;multihash"), profileConfig{onCPU: true}, 12, false},
		{"substring match", strings.ReplaceAll(cpu, "multihash2", "not_multihash2"), profileConfig{onCPU: true}, 12, false},
		{"missing worker", ";" + first + " 10000000000\n", profileConfig{onCPU: true}, 12, false},
		{"unbalanced workers", ";" + first + " 9000000000\n;" + second + " 1000000000\n", profileConfig{onCPU: true}, 12, false},
		{"unexpected weight", cpu + ";unexpected 2000000000\n", profileConfig{onCPU: true}, 12, false},
		{"idle cpu samples", wall, profileConfig{onCPU: true}, 12, false},
		{"missing idle samples", cpu, profileConfig{}, 12, false},
		{"too few samples", ";" + first + " 10000000\n;" + second + " 10000000\n", profileConfig{onCPU: true}, 12, false},
		{"empty", "", profileConfig{onCPU: true}, 12, false},
		{"bad value", cpu + ";unexpected nope\n", profileConfig{onCPU: true}, 12, false},
		{"negative value", cpu + ";unexpected -1\n", profileConfig{onCPU: true}, 12, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			err := checkConfigurationProfile(tc.profile, tc.cfg, tc.minor)
			if (err == nil) != tc.valid {
				t.Fatalf("valid=%t, error=%v", tc.valid, err)
			}
		})
	}
}
