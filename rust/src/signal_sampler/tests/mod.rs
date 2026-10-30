//! Copied from pyroscope-py-spy's signal-sampler-tests, where each file is its own test binary.
//! Here they share one, so besides the import paths: the ring buffer tests also take
//! `common::serial`, since the timer tests expect the process to be idle, and
//! `handler_drop_ignores_signal` uses `SIGUSR2`, since `ringbuffer_stress` leaves its `SIGUSR1`
//! handler installed.

mod common;
mod ctimer;
mod itimer;
mod ringbuffer;
mod ringbuffer_stress;
