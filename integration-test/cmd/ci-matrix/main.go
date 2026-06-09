package main

import (
	"bytes"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"strings"
)

func main() {
	cmd := exec.Command("go", "test", "-list", "^Test", ".")
	var stderr bytes.Buffer
	cmd.Stderr = &stderr

	output, err := cmd.Output()
	if err != nil {
		fmt.Fprintf(os.Stderr, "failed to list integration tests: %v\n%s", err, stderr.String())
		os.Exit(1)
	}

	var tests []string
	for _, line := range strings.Split(string(output), "\n") {
		name := strings.TrimSpace(line)
		if strings.HasPrefix(name, "Test") {
			tests = append(tests, name)
		}
	}
	if len(tests) == 0 {
		fmt.Fprintln(os.Stderr, "no integration tests discovered")
		os.Exit(1)
	}

	if err := json.NewEncoder(os.Stdout).Encode(tests); err != nil {
		fmt.Fprintf(os.Stderr, "failed to encode test matrix: %v\n", err)
		os.Exit(1)
	}
}
