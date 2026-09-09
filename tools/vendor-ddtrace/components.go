package main

import "strings"

// A component is one piece of dd-trace-py that this repository vendors.
//
// historyPaths is the pathspec for git rev-list, kept narrow with globs so a
// commit touching unrelated upstream files is never even considered.
// treePaths is the pathspec for git ls-tree, which does not glob, so it lists
// directories and lets mapPath do the filtering. ownsPath says which vendored
// paths belong to the component, so that replaying one component onto the
// mirror never disturbs another's files.
type component struct {
	name         string
	historyPaths []string
	treePaths    []string
	mapPath      func(upstream string) string
	ownsPath     func(vendored string) bool
}

const (
	ddCollector = "ddtrace/profiling/collector"
	ddHelpers   = "ddtrace/internal/datadog/profiling/profiling_helpers"
)

var components = map[string]*component{
	"memalloc": {
		name: "memalloc",
		historyPaths: []string{
			ddCollector + "/_memalloc*.c",
			ddCollector + "/_memalloc*.cpp",
			ddCollector + "/_memalloc*.h",
			ddCollector + "/_memalloc*.hpp",
			ddCollector + "/_pymacro.h",
			ddHelpers,
		},
		treePaths: []string{ddCollector, ddHelpers},
		mapPath:   mapMemalloc,
		ownsPath:  ownsMemalloc,
	},
}

// mapMemalloc returns the vendored path for an upstream path, or "" to drop it.
//
// The memory profiler sits flat in cpp/, so upstream's own renames (notably
// _memalloc.c -> _memalloc.cpp in 2292a4546d) replay as renames. Everything
// Python, the upstream build files and the other collectors are dropped.
func mapMemalloc(p string) string {
	if rel, ok := strings.CutPrefix(p, ddHelpers+"/"); ok {
		if strings.Contains(rel, "/") {
			return ""
		}
		return "cpp/profiling_helpers/" + rel
	}
	rel, ok := strings.CutPrefix(p, ddCollector+"/")
	if !ok || strings.Contains(rel, "/") {
		return ""
	}
	if strings.HasSuffix(rel, ".py") || strings.HasSuffix(rel, ".pyi") || strings.HasSuffix(rel, ".pyx") {
		return ""
	}
	if !strings.HasPrefix(rel, "_memalloc") && rel != "_pymacro.h" {
		return ""
	}
	return "cpp/" + rel
}

func ownsMemalloc(p string) bool {
	if rel, ok := strings.CutPrefix(p, "cpp/profiling_helpers/"); ok {
		return !strings.Contains(rel, "/")
	}
	rel, ok := strings.CutPrefix(p, "cpp/")
	if !ok || strings.Contains(rel, "/") {
		return false
	}
	return strings.HasPrefix(rel, "_memalloc") || rel == "_pymacro.h"
}
