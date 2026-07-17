package require

import (
	"testing"
	"time"
)

func Eventually(t *testing.T, condition func() bool, waitFor time.Duration, tick time.Duration, msgAndArgs ...interface{}) bool {
	t.Helper()

	deadline := time.Now().Add(waitFor)
	for {
		if condition() {
			return true
		}

		remaining := time.Until(deadline)
		if remaining <= 0 {
			return Fail(t, "Condition never satisfied", msgAndArgs...)
		}
		if tick < remaining {
			remaining = tick
		}
		time.Sleep(remaining)
	}
}
