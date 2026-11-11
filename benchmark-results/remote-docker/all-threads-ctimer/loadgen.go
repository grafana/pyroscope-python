package main

import (
	"context"
	"encoding/json"
	"flag"
	"io"
	"net/http"
	"os"
	"os/signal"
	"sync"
	"sync/atomic"
	"syscall"
	"time"
)

// Same closed-loop GET workload as the branch's bench_loadgen.go, with live
// counters so the CPU measurement and request count cover the same interval.
func main() {
	dst := flag.String("dst", "http://127.0.0.1:18080", "destination")
	n := flag.Int("nworkers", 12, "concurrent clients")
	stats := flag.String("stats", "127.0.0.1:18082", "counter endpoint")
	flag.Parse()
	var success, failures atomic.Uint64
	http.HandleFunc("/stats", func(w http.ResponseWriter, r *http.Request) {
		json.NewEncoder(w).Encode(map[string]uint64{"success": success.Load(), "errors": failures.Load()})
	})
	go func() { if err := http.ListenAndServe(*stats, nil); err != nil { panic(err) } }()
	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer cancel()
	var wg sync.WaitGroup
	for i := 0; i < *n; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			client := &http.Client{Timeout: 10 * time.Second}
			for ctx.Err() == nil {
				resp, err := client.Get(*dst)
				if err != nil { failures.Add(1); continue }
				_, err = io.Copy(io.Discard, resp.Body)
				resp.Body.Close()
				if err != nil || resp.StatusCode < 200 || resp.StatusCode >= 300 {
					failures.Add(1)
				} else { success.Add(1) }
			}
		}()
	}
	wg.Wait()
	json.NewEncoder(os.Stdout).Encode(map[string]uint64{"success": success.Load(), "errors": failures.Load()})
}
