package main

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

type syncer struct {
	o    *options
	c    *component
	repo git // this repository

	scratchDir string
	scratch    git // clone of upstream, where the replay happens

	configArgs []string // -c flags carrying our identity and signing config
	committer  []string // GIT_COMMITTER_NAME/EMAIL for the replayed commits
	indexFile  string
}

func cmdSync(args []string) {
	o, c := parseFlags("sync", args)
	if o.ref == "" {
		fatal("-ref is required")
	}
	s := &syncer{o: o, c: c, repo: git{repoRoot()}}
	s.preflight()
	s.makeScratch()
	if !o.keepScratch {
		defer os.RemoveAll(s.scratchDir)
	}

	refSHA := s.resolveRef()
	tip, _ := s.scratch.revParse("refs/heads/mirror")
	resume := s.resumePoint()

	commits := s.commitsToReplay(resume, refSHA)
	if o.dryRun {
		s.printPlan(resume, refSHA, commits)
		return
	}

	tip = s.replay(commits, tip)
	tip = s.reconcile(refSHA, tip)
	if tip == "" {
		fatal("nothing on the mirror for component %s, refusing to continue", c.name)
	}
	s.scratch.must("branch", "-f", "mirror", tip)

	s.publishMirror()
	if o.noMerge {
		fmt.Printf("\nmirror branch %s updated, merge skipped (-no-merge)\n", o.mirror)
		return
	}
	s.merge(refSHA, len(commits))
}

func (s *syncer) preflight() {
	// Untracked files are fine: the merge only touches tracked state, and git
	// refuses on its own if an untracked file is in the way.
	if out := s.repo.must("status", "--porcelain", "--untracked-files=no"); out != "" {
		fatal("working tree has uncommitted changes:\n%s", out)
	}
	branch := s.repo.must("rev-parse", "--abbrev-ref", "HEAD")
	if branch == "main" || branch == "HEAD" {
		fatal("refusing to work on %q, switch to a feature branch", branch)
	}
	name := s.repo.must("config", "user.name")
	email := s.repo.must("config", "user.email")
	s.committer = []string{"GIT_COMMITTER_NAME=" + name, "GIT_COMMITTER_EMAIL=" + email}

	if !s.o.noSign {
		key, err := s.repo.run("config", "user.signingkey")
		if err != nil || key == "" {
			fatal("user.signingkey is not set; grafana requires signed commits (use -no-sign to rehearse)")
		}
		s.configArgs = append(s.configArgs, "-c", "user.signingkey="+key)
		if format, err := s.repo.run("config", "gpg.format"); err == nil && format != "" {
			s.configArgs = append(s.configArgs, "-c", "gpg.format="+format)
		}
	}
	s.configArgs = append(s.configArgs, "-c", "user.name="+name, "-c", "user.email="+email)
}

// makeScratch clones upstream with --shared, so the replay writes its objects
// next to upstream's without copying them, and this repository never sees them.
func (s *syncer) makeScratch() {
	dir, err := os.MkdirTemp("", "vendor-ddtrace-")
	if err != nil {
		fatal("mktemp: %v", err)
	}
	s.scratchDir = filepath.Join(dir, "scratch")
	s.indexFile = filepath.Join(dir, "index")

	clone := []string{"clone", "--quiet", "--no-checkout"}
	if !strings.Contains(s.o.upstream, "://") {
		clone = append(clone, "--shared")
	}
	clone = append(clone, s.o.upstream, s.scratchDir)
	fmt.Printf("cloning upstream %s\n", s.o.upstream)
	if _, err := (git{dir}).run(clone...); err != nil {
		fatal("clone upstream: %v", err)
	}
	s.scratch = git{s.scratchDir}

	// Bring in the mirror branch as it stands, from this repository or origin.
	for _, src := range []string{"refs/heads/" + s.o.mirror, "refs/remotes/origin/" + s.o.mirror} {
		if _, ok := s.repo.revParse(src); !ok {
			continue
		}
		s.scratch.must("fetch", "--quiet", s.repo.dir, src+":refs/heads/mirror")
		fmt.Printf("mirror %s found at %s\n", s.o.mirror, short(s.scratch.must("rev-parse", "refs/heads/mirror")))
		break
	}
}

func (s *syncer) resolveRef() string {
	sha, ok := s.scratch.revParse(s.o.ref + "^{commit}")
	if !ok {
		fatal("upstream ref %q not found in %s", s.o.ref, s.o.upstream)
	}
	return sha
}

// resumePoint is the upstream commit the mirror already carries for this
// component, read back from the trailer the replay writes.
func (s *syncer) resumePoint() string {
	if _, ok := s.scratch.revParse("refs/heads/mirror"); !ok {
		return ""
	}
	body, err := s.scratch.run("log", "--max-count=1", "--format=%B",
		"--grep=^"+trailerComponent+": "+s.c.name+"$", "refs/heads/mirror")
	if err != nil || body == "" {
		return ""
	}
	for _, line := range strings.Split(body, "\n") {
		if v, ok := strings.CutPrefix(strings.TrimSpace(line), trailerCommit+":"); ok {
			return strings.TrimSpace(v)
		}
	}
	return ""
}

func (s *syncer) commitsToReplay(resume, refSHA string) []string {
	rang := refSHA
	if resume != "" {
		if _, ok := s.scratch.revParse(resume); !ok {
			fatal("resume point %s is not in %s; upstream history was rewritten?", resume, s.o.upstream)
		}
		rang = resume + ".." + refSHA
	}
	args := append([]string{"rev-list", "--topo-order", "--reverse", rang, "--"}, s.c.historyPaths...)
	out := s.scratch.must(args...)
	if out == "" {
		return nil
	}
	return strings.Split(out, "\n")
}

func (s *syncer) printPlan(resume, refSHA string, commits []string) {
	fmt.Printf("\ncomponent %s\n", s.c.name)
	if resume == "" {
		fmt.Printf("first import, up to %s (%s)\n", s.o.ref, short(refSHA))
	} else {
		fmt.Printf("resuming at %s, up to %s (%s)\n", short(resume), s.o.ref, short(refSHA))
	}
	fmt.Printf("%d upstream commits touch this component:\n", len(commits))
	for _, c := range commits {
		fmt.Printf("  %s %s\n", short(c), s.scratch.must("log", "-1", "--format=%s", c))
	}
}

func (s *syncer) replay(commits []string, tip string) string {
	prevTree := ""
	if tip != "" {
		prevTree = s.scratch.must("rev-parse", tip+"^{tree}")
	}
	written := 0
	for i, c := range commits {
		tree := s.buildTree(c, prevTree)
		if tree == prevTree {
			fmt.Printf("[%d/%d] %s skipped, no change to vendored files\n", i+1, len(commits), short(c))
			continue
		}
		tip = s.commit(c, tree, tip)
		prevTree = tree
		written++
		fmt.Printf("[%d/%d] %s -> %s %s\n", i+1, len(commits), short(c), short(tip),
			s.scratch.must("log", "-1", "--format=%s", c))
	}
	fmt.Printf("replayed %d of %d upstream commits\n", written, len(commits))
	return tip
}

// buildTree renders the vendored tree for an upstream commit: this component's
// files taken from upstream and mapped to our paths, plus every file on the
// mirror that belongs to some other component, left untouched.
func (s *syncer) buildTree(upstreamCommit, baseTree string) string {
	var lines []string
	if baseTree != "" {
		for _, e := range s.lsTree(baseTree) {
			if !s.c.ownsPath(e.path) {
				lines = append(lines, fmt.Sprintf("%s %s\t%s", e.mode, e.sha, e.path))
			}
		}
	}
	for _, e := range s.lsTree(upstreamCommit, s.c.treePaths...) {
		if e.mode == "160000" { // a submodule pointer is not a file we can vendor
			continue
		}
		if v := s.c.mapPath(e.path); v != "" {
			lines = append(lines, fmt.Sprintf("%s %s\t%s", e.mode, e.sha, v))
		}
	}

	if len(lines) == 0 {
		fatal("commit %s maps to no vendored files at all, refusing to write an empty tree", short(upstreamCommit))
	}
	_ = os.Remove(s.indexFile)
	env := gitCall{stdin: strings.NewReader(strings.Join(lines, "\n") + "\n"), env: []string{"GIT_INDEX_FILE=" + s.indexFile}}
	if _, err := s.scratch.output(env, "update-index", "--index-info"); err != nil {
		fatal("build tree for %s: %v", short(upstreamCommit), err)
	}
	tree, err := s.scratch.output(gitCall{env: []string{"GIT_INDEX_FILE=" + s.indexFile}}, "write-tree")
	if err != nil {
		fatal("write-tree for %s: %v", short(upstreamCommit), err)
	}
	return tree
}

type treeEntry struct{ mode, sha, path string }

func (s *syncer) lsTree(rev string, paths ...string) []treeEntry {
	args := append([]string{"ls-tree", "-r", "-z", rev, "--"}, paths...)
	out := s.scratch.must(args...)
	var entries []treeEntry
	for _, rec := range strings.Split(out, "\x00") {
		if rec == "" {
			continue
		}
		meta, path, ok := strings.Cut(rec, "\t")
		if !ok {
			fatal("unparsable ls-tree record %q", rec)
		}
		fields := strings.Fields(meta)
		if len(fields) != 3 {
			fatal("unparsable ls-tree meta %q", meta)
		}
		entries = append(entries, treeEntry{mode: fields[0], sha: fields[2], path: path})
	}
	return entries
}

// commit recreates an upstream commit: same author, same author date, same
// message plus provenance trailers, our committer, our signature.
func (s *syncer) commit(upstreamCommit, tree, parent string) string {
	// %x00 makes git emit NUL separators, so no field can be confused with the
	// message body.
	raw := s.scratch.must("log", "-1", "--format=%an%x00%ae%x00%aI%x00%B", upstreamCommit)
	parts := strings.SplitN(raw, "\x00", 4)
	if len(parts) != 4 {
		fatal("cannot read author of %s", short(upstreamCommit))
	}
	name, email, date, body := parts[0], parts[1], parts[2], parts[3]

	msg := upstreamRef.ReplaceAllString(body, "${1}DataDog/dd-trace-py#${2}")
	msg = strings.TrimRight(msg, "\n") + "\n"
	msg, err := s.scratch.output(gitCall{stdin: strings.NewReader(msg)}, "interpret-trailers",
		"--trailer", trailerComponent+": "+s.c.name,
		"--trailer", trailerCommit+": "+upstreamCommit)
	if err != nil {
		fatal("interpret-trailers for %s: %v", short(upstreamCommit), err)
	}

	env := append([]string{
		"GIT_AUTHOR_NAME=" + name,
		"GIT_AUTHOR_EMAIL=" + email,
		"GIT_AUTHOR_DATE=" + date,
		"GIT_COMMITTER_DATE=" + date,
	}, s.committer...)
	return s.commitTree(tree, parent, msg+"\n", env, upstreamCommit)
}

func (s *syncer) commitTree(tree, parent, msg string, env []string, what string) string {
	args := append([]string{}, s.configArgs...)
	args = append(args, "commit-tree")
	if !s.o.noSign {
		args = append(args, "-S")
	}
	if parent != "" {
		args = append(args, "-p", parent)
	}
	args = append(args, tree)
	sha, err := s.scratch.output(gitCall{stdin: strings.NewReader(msg), env: env}, args...)
	if err != nil {
		fatal("commit-tree for %s: %v", short(what), err)
	}
	return sha
}

// reconcile guarantees the mirror ends up holding exactly the state of -ref.
// Replaying a range can fall short of that when the new ref lives on a
// different dd-trace-py release branch than the old one, which is the normal
// shape of their release tags.
func (s *syncer) reconcile(refSHA, tip string) string {
	var tipTree string
	if tip != "" {
		tipTree = s.scratch.must("rev-parse", tip+"^{tree}")
	}
	want := s.buildTree(refSHA, tipTree)
	if want == tipTree {
		return tip
	}
	fmt.Printf("mirror tip does not match %s, adding a sync commit\n", s.o.ref)
	msg := fmt.Sprintf("vendor: sync %s to %s\n\nThe replayed range did not end on upstream's state at this ref, which\nhappens when the ref lives on a different release branch. This commit\ncarries the exact filtered tree of the ref.\n\n%s: %s\n%s: %s\n",
		s.c.name, s.o.ref, trailerComponent, s.c.name, trailerCommit, refSHA)
	return s.commitTree(want, tip, msg, s.committer, refSHA)
}

// publishMirror fast-forwards the mirror branch in this repository. A rewrite
// of vendored history would fail here, which is what we want: the mirror is
// append-only.
func (s *syncer) publishMirror() {
	if err := s.repo.stream("fetch", s.scratchDir, "refs/heads/mirror:refs/heads/"+s.o.mirror); err != nil {
		fatal("publish mirror: %v (the mirror is append-only, a rewrite is refused)", err)
	}
	fmt.Printf("mirror %s is now at %s\n", s.o.mirror, short(s.repo.must("rev-parse", s.o.mirror)))
}

func (s *syncer) merge(refSHA string, replayed int) {
	args := []string{"merge", "--no-ff"}
	if s.o.graft {
		args = append(args, "-s", "ours")
	}
	if !s.repo.ok("merge-base", "HEAD", s.o.mirror) {
		args = append(args, "--allow-unrelated-histories")
	}
	if !s.o.noSign {
		args = append(args, "-S")
	}
	args = append(args, "-m", s.mergeMessage(refSHA, replayed), s.o.mirror)
	if err := s.repo.stream(args...); err != nil {
		fatal("merge: %v", err)
	}
	fmt.Printf("\nmerged %s into %s\n", s.o.mirror, s.repo.must("rev-parse", "--abbrev-ref", "HEAD"))
}

func (s *syncer) mergeMessage(refSHA string, replayed int) string {
	var b strings.Builder
	fmt.Fprintf(&b, "chore(vendor): dd-trace-py %s history at %s\n\n", s.c.name, s.o.ref)
	if s.o.graft {
		fmt.Fprintf(&b, "Grafts the upstream history of the %s sources under the copy that is\nalready in the tree. Merged with -s ours, so not a single vendored byte\nchanges: this only records where the code came from.\n\n", s.c.name)
	} else {
		fmt.Fprintf(&b, "Brings in the %s sources with their upstream history.\n\n", s.c.name)
	}
	fmt.Fprintf(&b, "%d upstream commits were replayed onto %s, each keeping its original\nauthor, author date and message, re-pathed to where we keep the files and\nre-signed because upstream commits are unsigned.\n\n", replayed, s.o.mirror)
	fmt.Fprintf(&b, "Our patches are now exactly `git diff %s HEAD`, and an upstream upgrade\nis `vendor-ddtrace sync -component %s -ref <newer>`.\n\n", s.o.mirror, s.c.name)
	fmt.Fprintf(&b, "%s: %s\n%s: %s\n", trailerComponent, s.c.name, trailerCommit, refSHA)
	return b.String()
}

func short(sha string) string {
	if len(sha) > 10 {
		return sha[:10]
	}
	return sha
}
