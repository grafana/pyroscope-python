// Command vendor-ddtrace imports dd-trace-py sources into this repository with
// their upstream commit history, and upgrades them later.
//
// The upstream history is replayed onto a mirror branch (vendor/dd by default)
// that holds pure upstream code and nothing else: every commit keeps its
// original author, author date and message, is re-pathed to where we keep the
// files, and is re-signed with our key because grafana requires signed commits
// and upstream's are unsigned. Our own patches then live as the diff between
// that mirror and mainline, and an upstream upgrade is an ordinary three-way
// merge instead of a re-copy.
//
//	go run . sync   -component memalloc -ref v4.11.1 -graft
//	go run . verify -component memalloc
package main

import (
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strings"
)

const defaultMirror = "vendor/dd"

const upstreamURL = "https://github.com/DataDog/dd-trace-py"

func main() {
	if len(os.Args) < 2 {
		usage()
		os.Exit(2)
	}
	switch os.Args[1] {
	case "sync":
		cmdSync(os.Args[2:])
	case "verify":
		cmdVerify(os.Args[2:])
	case "-h", "--help", "help":
		usage()
	default:
		fmt.Fprintf(os.Stderr, "unknown command %q\n\n", os.Args[1])
		usage()
		os.Exit(2)
	}
}

func usage() {
	fmt.Fprint(os.Stderr, `usage:
  vendor-ddtrace sync   -component <name> -ref <upstream ref> [options]
  vendor-ddtrace verify -component <name> [-ref <upstream ref>]

sync replays the upstream history of a component onto the mirror branch and
merges the mirror into the current branch. verify prints our local patches,
that is the diff between the mirror and the working tree.

options:
  -component string  component to vendor (memalloc)
  -ref string        upstream tag or commit to vendor up to
  -upstream string   dd-trace-py path or URL (default $DDTRACE_REPO, else
                     ~/dd/dd-trace-py if it exists, else `+upstreamURL+`)
  -mirror string     mirror branch (default `+defaultMirror+`)
  -import-branch     land the replay here instead of on the mirror, so it can
                     reach the mirror through a pull request. Nothing is
                     pushed to the mirror directly.
  -graft             merge with -s ours: keep our tree, record the ancestry
                     only. Use when the component is already in the tree from
                     an earlier squashed import.
  -no-merge          update the mirror branch, do not merge it
  -no-sign           do not sign; for rehearsals, such commits cannot be pushed
  -dry-run           list the commits that would be replayed, then stop
  -keep-scratch      keep the scratch clone for inspection
`)
}

type options struct {
	component    string
	ref          string
	upstream     string
	mirror       string
	importBranch string
	graft        bool
	noMerge      bool
	noSign       bool
	dryRun       bool
	keepScratch  bool
}

func parseFlags(name string, args []string) (*options, *component) {
	var o options
	fs := flag.NewFlagSet(name, flag.ExitOnError)
	fs.StringVar(&o.component, "component", "", "component to vendor")
	fs.StringVar(&o.ref, "ref", "", "upstream tag or commit")
	fs.StringVar(&o.upstream, "upstream", "", "dd-trace-py path or URL")
	fs.StringVar(&o.mirror, "mirror", defaultMirror, "mirror branch")
	fs.StringVar(&o.importBranch, "import-branch", "", "land the replay here, for review via a pull request into the mirror")
	fs.BoolVar(&o.graft, "graft", false, "merge with -s ours")
	fs.BoolVar(&o.noMerge, "no-merge", false, "do not merge the mirror")
	fs.BoolVar(&o.noSign, "no-sign", false, "do not sign commits")
	fs.BoolVar(&o.dryRun, "dry-run", false, "list commits and stop")
	fs.BoolVar(&o.keepScratch, "keep-scratch", false, "keep the scratch clone")
	fs.Usage = usage
	_ = fs.Parse(args)

	if o.component == "" {
		fatal("-component is required (have: %s)", strings.Join(componentNames(), ", "))
	}
	c, ok := components[o.component]
	if !ok {
		fatal("unknown component %q (have: %s)", o.component, strings.Join(componentNames(), ", "))
	}
	if o.upstream == "" {
		o.upstream = defaultUpstream()
	}
	return &o, c
}

func componentNames() []string {
	var names []string
	for n := range components {
		names = append(names, n)
	}
	sort.Strings(names)
	return names
}

func defaultUpstream() string {
	if v := os.Getenv("DDTRACE_REPO"); v != "" {
		return v
	}
	if home, err := os.UserHomeDir(); err == nil {
		p := filepath.Join(home, "dd", "dd-trace-py")
		if st, err := os.Stat(p); err == nil && st.IsDir() {
			return p
		}
	}
	return upstreamURL
}

func fatal(format string, args ...any) {
	fmt.Fprintf(os.Stderr, "vendor-ddtrace: "+format+"\n", args...)
	os.Exit(1)
}

// repoRoot is the top level of the repository the tool was invoked from.
func repoRoot() string {
	wd, err := os.Getwd()
	if err != nil {
		fatal("getwd: %v", err)
	}
	root, err := git{wd}.run("rev-parse", "--show-toplevel")
	if err != nil {
		fatal("not inside a git repository: %v", err)
	}
	return root
}

// upstreamRef links bare "#1234" references to dd-trace-py, where they came
// from, instead of to an unrelated pull request in this repository.
var upstreamRef = regexp.MustCompile(`(^|[^A-Za-z0-9_/-])#([0-9]+)`)

const (
	trailerCommit    = "Upstream-Commit"
	trailerComponent = "Vendor-Component"
)
