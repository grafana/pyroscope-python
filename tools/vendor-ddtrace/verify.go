package main

import (
	"fmt"
	"os"
	"sort"
	"strings"
)

// cmdVerify prints our local patches: the difference between the pure upstream
// state on the mirror branch and what is in the tree now. With -ref it also
// checks that the mirror really holds that upstream ref.
func cmdVerify(args []string) {
	o, c := parseFlags("verify", args)
	repo := git{repoRoot()}

	mirror, ok := repo.revParse(o.mirror)
	if !ok {
		fatal("mirror branch %s does not exist; run sync first", o.mirror)
	}

	paths := ownedPaths(repo, c, mirror, "HEAD")
	if len(paths) == 0 {
		fatal("no files of component %s found on %s or HEAD", c.name, o.mirror)
	}

	fmt.Printf("component %s, mirror %s at %s, %d vendored files\n\n", c.name, o.mirror, short(mirror), len(paths))
	diff := append([]string{"diff", "--stat", mirror, "HEAD", "--"}, paths...)
	stat := repo.must(diff...)
	if stat == "" {
		fmt.Println("no local patches: the tree matches upstream exactly")
	} else {
		fmt.Println("local patches (mirror -> HEAD):")
		fmt.Println(stat)
	}

	if o.ref != "" {
		verifyRef(o, c, repo, mirror)
	}
}

// ownedPaths is every path the component owns, on either side of the diff, so
// that a file we added or deleted locally still shows up.
func ownedPaths(repo git, c *component, revs ...string) []string {
	seen := map[string]bool{}
	for _, rev := range revs {
		for _, p := range strings.Split(repo.must("ls-tree", "-r", "--name-only", rev), "\n") {
			if p != "" && c.ownsPath(p) {
				seen[p] = true
			}
		}
	}
	var paths []string
	for p := range seen {
		paths = append(paths, p)
	}
	sort.Strings(paths)
	return paths
}

// verifyRef rebuilds the filtered tree of an upstream ref and compares it with
// what the mirror carries, so the documented ref and the branch cannot drift.
func verifyRef(o *options, c *component, repo git, mirror string) {
	s := &syncer{o: o, c: c, repo: repo}
	s.makeScratch()
	if !o.keepScratch {
		defer os.RemoveAll(s.scratchDir)
	}
	refSHA := s.resolveRef()

	mirrorTree := s.scratch.must("rev-parse", "refs/heads/mirror^{tree}")
	want := s.buildTree(refSHA, mirrorTree)
	fmt.Printf("\nmirror tree %s\n%s tree %s\n", short(mirrorTree), o.ref, short(want))
	if want != mirrorTree {
		fmt.Printf("\nMIRROR DOES NOT MATCH %s:\n", o.ref)
		_ = s.scratch.stream("diff", "--stat", mirrorTree, want)
		os.Exit(1)
	}
	fmt.Printf("mirror matches upstream %s (%s) exactly\n", o.ref, short(refSHA))
}
