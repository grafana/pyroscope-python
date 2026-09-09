package main

import "testing"

func TestMapMemalloc(t *testing.T) {
	cases := []struct{ in, want string }{
		// kept: the memalloc sources, both before and after the upstream
		// .c -> .cpp port, so history flows through the rename
		{ddCollector + "/_memalloc.c", "cpp/_memalloc.c"},
		{ddCollector + "/_memalloc.cpp", "cpp/_memalloc.cpp"},
		{ddCollector + "/_memalloc_tb.h", "cpp/_memalloc_tb.h"},
		{ddCollector + "/_memalloc_gc_guard.hpp", "cpp/_memalloc_gc_guard.hpp"},
		{ddCollector + "/_pymacro.h", "cpp/_pymacro.h"},
		{ddHelpers + "/frame_accessors.h", "cpp/profiling_helpers/frame_accessors.h"},

		// dropped: python glue, upstream build files, unrelated collectors
		{ddCollector + "/memalloc.py", ""},
		{ddCollector + "/_memalloc.pyi", ""},
		{ddCollector + "/CMakeLists.txt", ""},
		{ddCollector + "/_lock.py", ""},
		{ddCollector + "/stack.pyx", ""},
		{ddCollector + "/_traceback.c", ""},
		{ddCollector + "/nested/_memalloc.c", ""},
		{"ddtrace/internal/datadog/profiling/stack/src/sampler.cpp", ""},
		{"tests/profiling/collector/test_memalloc.py", ""},
	}
	for _, c := range cases {
		if got := mapMemalloc(c.in); got != c.want {
			t.Errorf("mapMemalloc(%q) = %q, want %q", c.in, got, c.want)
		}
	}
}

func TestOwnsMemalloc(t *testing.T) {
	owned := []string{
		"cpp/_memalloc.cpp",
		"cpp/_memalloc_heap.h",
		"cpp/_pymacro.h",
		"cpp/profiling_helpers/version_compat.h",
	}
	notOwned := []string{
		"cpp/Pyroscope.h",
		"cpp/CMakeLists.txt",
		"cpp/BundleStaticLibrary.cmake",
		"cpp/ddtrace_stack/src/sampler.cpp",
		"rust/src/lib.rs",
	}
	for _, p := range owned {
		if !ownsMemalloc(p) {
			t.Errorf("ownsMemalloc(%q) = false, want true", p)
		}
	}
	for _, p := range notOwned {
		if ownsMemalloc(p) {
			t.Errorf("ownsMemalloc(%q) = true, want false", p)
		}
	}
}

// Everything mapMemalloc keeps must be owned by the component, otherwise a
// replay would write files it does not consider its own.
func TestMapImpliesOwn(t *testing.T) {
	for _, p := range []string{
		ddCollector + "/_memalloc.c",
		ddCollector + "/_memalloc_reentrant.cpp",
		ddCollector + "/_pymacro.h",
		ddHelpers + "/linetable_parser.h",
	} {
		v := mapMemalloc(p)
		if v == "" {
			t.Fatalf("mapMemalloc(%q) dropped a path the test expects kept", p)
		}
		if !ownsMemalloc(v) {
			t.Errorf("mapMemalloc(%q) = %q which ownsMemalloc rejects", p, v)
		}
	}
}
