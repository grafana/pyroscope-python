package main

import "testing"

func TestMapMemalloc(t *testing.T) {
	cases := []struct{ in, want string }{
		// kept, at the upstream path: the memalloc sources, both before and
		// after the upstream .c -> .cpp port, so history flows through it
		{ddCollector + "/_memalloc.c", ddCollector + "/_memalloc.c"},
		{ddCollector + "/_memalloc.cpp", ddCollector + "/_memalloc.cpp"},
		{ddCollector + "/_memalloc_tb.h", ddCollector + "/_memalloc_tb.h"},
		{ddCollector + "/_memalloc_gc_guard.hpp", ddCollector + "/_memalloc_gc_guard.hpp"},
		{ddCollector + "/_pymacro.h", ddCollector + "/_pymacro.h"},
		{ddHelpers + "/frame_accessors.h", ddHelpers + "/frame_accessors.h"},

		// dropped: python glue, upstream build files, unrelated collectors
		{ddCollector + "/memalloc.py", ""},
		{ddCollector + "/_memalloc.pyi", ""},
		{ddCollector + "/CMakeLists.txt", ""},
		{ddCollector + "/_lock.py", ""},
		{ddCollector + "/stack.pyx", ""},
		{ddCollector + "/_traceback.c", ""},
		{ddCollector + "/nested/_memalloc.c", ""},
		{ddHelpers + "/nested/frame_accessors.h", ""},
		{"ddtrace/internal/datadog/profiling/stack/src/sampler.cpp", ""},
		{"tests/profiling/collector/test_memalloc.py", ""},
	}
	for _, c := range cases {
		if got := mapMemalloc(c.in); got != c.want {
			t.Errorf("mapMemalloc(%q) = %q, want %q", c.in, got, c.want)
		}
	}
}

// Ownership and mapping must agree: a replay writes what it owns, and owns what
// it writes. Anything else means one component's replay could drop or claim
// another's files.
func TestOwnershipMatchesMapping(t *testing.T) {
	owned := []string{
		ddCollector + "/_memalloc.cpp",
		ddCollector + "/_memalloc_heap.h",
		ddCollector + "/_pymacro.h",
		ddHelpers + "/version_compat.h",
	}
	notOwned := []string{
		"cpp/Pyroscope.h",
		"cpp/CMakeLists.txt",
		"cpp/BundleStaticLibrary.cmake",
		ddCollector + "/memalloc.py",
		"ddtrace/internal/datadog/profiling/stack/src/sampler.cpp",
		"rust/src/lib.rs",
	}
	c := components["memalloc"]
	for _, p := range owned {
		if !c.ownsPath(p) {
			t.Errorf("ownsPath(%q) = false, want true", p)
		}
		if got := c.mapPath(p); got != p {
			t.Errorf("mapPath(%q) = %q, want it unchanged", p, got)
		}
	}
	for _, p := range notOwned {
		if c.ownsPath(p) {
			t.Errorf("ownsPath(%q) = true, want false", p)
		}
	}
}
