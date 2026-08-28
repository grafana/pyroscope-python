"""Aggregation and the noise figure that every number must be read against."""

import statistics


def median(xs):
    return statistics.median(xs) if xs else float("nan")


def spread_pct(xs):
    """Half the min-max range as a percentage of the median.

    This is the companion figure to every reported number. Without one there is
    no way to tell a result from an artefact, and a table of pure noise is very
    easy to rank convincingly.
    """
    if len(xs) < 2:
        return float("nan")
    med = statistics.median(xs)
    if med == 0:
        return float("inf")
    return 100.0 * (max(xs) - min(xs)) / 2.0 / abs(med)


def overhead_pct(baseline, treatment):
    if not baseline or baseline == 0:
        return float("nan")
    return 100.0 * (treatment - baseline) / baseline


def combined_noise_pct(baseline_xs, treatment_xs):
    """Noise floor for a comparison between two sets of repeats.

    Both sides contribute; a difference smaller than this is not a difference.
    """
    a = spread_pct(baseline_xs)
    b = spread_pct(treatment_xs)
    parts = [x for x in (a, b) if x == x]  # drop NaN
    if not parts:
        return float("nan")
    return sum(parts)


def resolvable(diff_pct, noise_pct):
    if diff_pct != diff_pct or noise_pct != noise_pct:
        return False
    return abs(diff_pct) > noise_pct
