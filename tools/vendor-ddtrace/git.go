package main

import (
	"bytes"
	"fmt"
	"io"
	"os"
	"os/exec"
	"strings"
)

// git runs git commands in a fixed directory.
type git struct {
	dir string
}

type gitCall struct {
	stdin io.Reader
	env   []string // extra environment, appended to os.Environ()
}

func (g git) output(call gitCall, args ...string) (string, error) {
	cmd := exec.Command("git", args...)
	cmd.Dir = g.dir
	cmd.Stdin = call.stdin
	if len(call.env) > 0 {
		cmd.Env = append(os.Environ(), call.env...)
	}
	var stdout, stderr bytes.Buffer
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr
	if err := cmd.Run(); err != nil {
		return "", fmt.Errorf("git %s: %w: %s", strings.Join(args, " "), err, strings.TrimSpace(stderr.String()))
	}
	return strings.TrimRight(stdout.String(), "\n"), nil
}

// run executes git and returns its trimmed stdout.
func (g git) run(args ...string) (string, error) {
	return g.output(gitCall{}, args...)
}

// must executes git and aborts the program on failure.
func (g git) must(args ...string) string {
	out, err := g.run(args...)
	if err != nil {
		fatal("%v", err)
	}
	return out
}

// ok reports whether the command succeeded, discarding its output.
func (g git) ok(args ...string) bool {
	_, err := g.run(args...)
	return err == nil
}

// stream runs git with stdout and stderr attached to the terminal.
func (g git) stream(args ...string) error {
	cmd := exec.Command("git", args...)
	cmd.Dir = g.dir
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	if err := cmd.Run(); err != nil {
		return fmt.Errorf("git %s: %w", strings.Join(args, " "), err)
	}
	return nil
}

func (g git) revParse(rev string) (string, bool) {
	out, err := g.run("rev-parse", "--verify", "--quiet", rev)
	if err != nil || out == "" {
		return "", false
	}
	return out, true
}
