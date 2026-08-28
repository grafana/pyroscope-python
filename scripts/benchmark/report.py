"""Render results as markdown and as a self-contained HTML page.

Every number is printed with its noise figure, and any difference smaller than
that figure is rendered as `~` rather than ranked. The correctness column
(seen/used) sits in the same table as the performance columns on purpose: a
profiler that drops samples is cheap, so the two cannot honestly be read apart.
"""

import html as html_mod
import math


def _fmt(x, digits=2, suffix=""):
    if x is None or (isinstance(x, float) and (math.isnan(x) or math.isinf(x))):
        return "n/a"
    return f"{x:.{digits}f}{suffix}"


def _overhead_cell(v, markdown=True, invalid=False):
    """Render one overhead figure, or refuse to.

    A cell whose run failed a guard is not shown as a number at all. Printing it
    alongside valid cells invites exactly the ranking the guard exists to
    prevent.
    """
    if invalid:
        return "**invalid**" if markdown else "invalid"
    oh, noise = v["cpu_overhead_pct"], v["noise_pct"]
    if oh != oh:
        return "n/a"
    if not v["resolvable"]:
        return f"~ (<{_fmt(noise, 1)}%)"
    body = f"{oh:+.1f}%"
    return f"**{body}**" if markdown else body


# Shapes whose measured region ends on a work count rather than after a fixed
# span. Their wall time is a function of achieved throughput, so a wall
# "overhead" for them is not interpretable and is not printed.
THROUGHPUT_PACED_SHAPES = {"http"}


def detail_walks(meta, variant):
    return bool(meta.get("variant_detail", {})
                .get(variant, {}).get("walks_all_threads"))


def _wall_cell(ent, v):
    if ent["shape"] in THROUGHPUT_PACED_SHAPES:
        return "n/a*"
    return _fmt(v["wall_overhead_pct"], 1, "%")


def _rows(agg, variant):
    for key in sorted(agg):
        e = agg[key]
        if variant not in e["variants"]:
            continue
        yield key, e, e["variants"][variant]


def _collapse(checks):
    """Group repeated check results so the distinct ones stay visible.

    A structural property of the profiler fires once per run, which at seven
    repeats across seven shapes is forty-nine identical lines. Grouping by check
    name and shape keeps the list readable without dropping anything: the
    occurrence count is preserved and one full detail line is kept per group.
    """
    groups = {}
    for c in checks:
        shape = c.scope.split("/")[0] if c.scope else ""
        groups.setdefault((c.name, shape), []).append(c)
    return sorted(
        ((name, shape, len(items), items[0].detail)
         for (name, shape), items in groups.items()),
        key=lambda t: (t[0], t[1]),
    )


def _noise_summary(agg, variants):
    """The worst noise floor in the table.

    Stated up front because it is the threshold every other number in the table
    has to clear to mean anything.
    """
    noises = [v["noise_pct"] for variant in variants if variant != "none"
              for _, _, v in _rows(agg, variant)
              if v["noise_pct"] == v["noise_pct"]]
    return max(noises) if noises else float("nan")


def markdown(meta, agg, rep):
    L = []
    A = L.append
    A("# CPU profiler overhead")
    A("")
    A(f"- measured: `{meta['timestamp_utc']}`{'  ' + meta['label'] if meta['label'] else ''}")
    A(f"- host: `{meta['uname']}`, CPython {meta['python']}")
    A(f"- subject pinned to CPUs `{meta['subject_cpus']}`, "
      f"sink `{meta['sink_cpus']}`, load generator `{meta['loadgen_cpus']}`")
    A(f"- {meta['repeats']} repeats, round-robin across the whole matrix, "
      f"~{meta['target_region_s']:.0f}s per measured region, "
      f"{meta['warmup_s']:.0f}s unmeasured warmup")
    for vname in meta["variants"]:
        cfg = meta.get("variant_detail", {}).get(vname, {}).get("config", "")
        if cfg:
            A(f"- `{vname}`: `{cfg}`")
    noise = _noise_summary(agg, meta["variants"])
    A(f"- **worst noise floor in this table: {_fmt(noise, 1)}%.** No difference "
      "smaller than its own row's noise figure is ranked.")
    A("")

    for variant in meta["variants"]:
        if variant == "none":
            continue
        A(f"## `{variant}` vs no profiling")
        A("")
        A("| workload | thr | cpu none (s) | cpu profiled (s) | **cpu overhead** "
          "| noise | % of a core | wall oh | mem static delta | seen/used | seen/wall |")
        A("|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|")
        failed = rep.failed_scopes()
        for key, e, v in _rows(agg, variant):
            invalid = f"{key}/{variant}" in failed
            A("| `{k}` | {thr} | {b} ±{bs}% | {c} ±{cs}% | {oh} | {n}% | {cc} "
              "| {w} | {md} MB | {su} | {sm} |".format(
                  k=key, thr=e["threads"],
                  b=_fmt(e["none"]["cpu_s"]), bs=_fmt(e["none"]["cpu_spread_pct"], 1),
                  c=_fmt(v["cpu_s"]), cs=_fmt(v["cpu_spread_pct"], 1),
                  oh=_overhead_cell(v, invalid=invalid),
                  n=_fmt(v["noise_pct"], 1),
                  cc=_fmt(v["cpu_cost_cores_pct"], 2, "%"),
                  w=_wall_cell(e, v),
                  md=_fmt(v["mem_delta_mb"], 1),
                  su=_fmt(v["seen_over_used"], 2),
                  sm=_fmt(v["seen_over_wall"], 2)))
        A("")

    http_rows = [(vn, k, ent, v) for vn in meta["variants"] if vn != "none"
                 for k, ent, v in _rows(agg, vn) if ent["shape"] == "http"]
    if http_rows:
        A("## HTTP shape detail")
        A("")
        A("Fixed request count, so **cpu per request** is the comparable figure. "
          "Throughput and wall time are context only: the region ends when the "
          "last request is served, so both move with how warm the machine is. A "
          "throughput gain here is not the profiler making the server faster.")
        A("")
        A("| variant | cpu us/request | vs baseline | rps | rps delta |")
        A("|---|--:|--:|--:|--:|")
        for vname, key, ent, v in http_rows:
            A("| `{k}` | {c} | {o} | {r} | {rd} |".format(
                k=vname, c=_fmt(v["cpu_us_per_request"], 1),
                o=_fmt(v["cpu_us_per_request_overhead_pct"], 1, "%"),
                r=_fmt(v["rps"], 0), rd=_fmt(v["rps_delta_pct"], 1, "%")))
        A("")
        A(f"Unprofiled baseline: "
          f"{_fmt(http_rows[0][2]['none_cpu_us_per_request'], 1)} us cpu/request "
          f"at {_fmt(http_rows[0][2]['none_rps'], 0)} rps.")
        A("")

    A("## How to read this")
    A("")
    A("- `invalid` marks a cell whose runs failed a guard. It is shown without a "
      "number on purpose, so it cannot be ranked against the valid cells.")
    A("- **cpu overhead** is the headline. It is a cgroup `cpu.stat` delta over a "
      "fixed amount of work, so the profiler's cost has nowhere to hide. "
      "`~` means the difference is smaller than the noise floor and is not ranked.")
    A("- **% of a core** is the same cost in absolute terms: how much of one CPU "
      "core the profiler occupies. Read it together with the relative figure. A "
      "workload that barely touches the CPU can show a large percentage overhead "
      "for a small absolute cost, and that distinction matters for deciding "
      "whether to enable profiling.")
    A("- **wall overhead** is shown for reference only. It picks up scheduler "
      "jitter and frequency scaling, and compresses the ranking into the noise.")
    A("- `n/a*` in the wall column marks a shape whose region ends after a fixed "
      "amount of *work* rather than a fixed span, so its duration is a function "
      "of achieved throughput. A warmer machine finishes sooner, which would read "
      "as a faster profiler. CPU per unit of work is the comparable figure there.")
    A("- **mem static delta** is the steady-state footprint the profiler adds, "
      "sampled after warmup and before the workload's own peak. **mem peak delta** "
      "conflates profiler and workload peaks, which occur at different moments, "
      "so it is secondary.")
    A("- **seen/used** is the CPU the profile claims divided by the CPU the process "
      "actually consumed. It is a correctness figure, and it belongs next to the "
      "cost figure: a profiler that drops samples is cheap.")
    A("")
    A("### What a unit of configuration buys")
    A("")
    detail = meta.get("variant_detail", {})
    if detail:
        A("| variant | what it is | cost of one tick |")
        A("|---|---|---|")
        for vname in meta["variants"]:
            d = detail.get(vname, {})
            A(f"| `{vname}` | {d.get('label', '')} | {d.get('per_tick', '')} |")
        A("")
    A("`gil_only` and `oncpu` are applied at different points, and the difference "
      "decides how cost scales. `gil_only` is applied by py-spy inside its "
      "per-thread loop **before** the unwind (`python_spy.rs`), so at the "
      "shipping default a tick unwinds exactly one stack -- the GIL holder -- and "
      "costs one pointer read per other thread. `oncpu` is applied by the "
      "consumer in `rust/src/pyspy_backend.rs` **after** the unwind, so that walk "
      "is already paid for. Consequences: per-tick cost is close to flat in "
      "thread count, and a `seen/used` well below 1 on a multi-threaded shape is "
      "the pre-walk filter working rather than the profiler failing.")
    A("")

    A("## Invariants")
    A("")
    if rep.green and not rep.warnings:
        A(f"All {len(rep.checks)} checks passed.")
    else:
        A(f"{len(rep.checks) - len(rep.failures) - len(rep.warnings)} passed, "
          f"{len(rep.failures)} failed, {len(rep.warnings)} warned.")
    for name, _shape, count, detail in _collapse(rep.failures):
        times = f" (x{count})" if count > 1 else ""
        A(f"- **FAIL** `{name}`{times} -- {detail}")
    for name, _shape, count, detail in _collapse(rep.warnings):
        times = f" (x{count})" if count > 1 else ""
        A(f"- warn `{name}`{times} -- {detail}")
    if rep.failures:
        A("")
        A("> Cells covered by a failed check are not ranked. A failed physical "
          "invariant invalidates the table, not just the offending row.")
    A("")
    return "\n".join(L)


# --- HTML --------------------------------------------------------------------

_CSS = """
:root { --fg:#1c1e21; --mut:#666; --line:#dfe1e5; --bg:#fff;
        --good:#1a7f37; --bad:#b42318; --warn:#b54708; --bar:#3b6ea5; }
* { box-sizing: border-box; }
body { font: 14px/1.55 -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
       color: var(--fg); background: var(--bg); margin: 0; padding: 32px;
       max-width: 1180px; margin-inline: auto; }
h1 { font-size: 24px; margin: 0 0 4px; }
h2 { font-size: 17px; margin: 36px 0 12px; padding-bottom: 6px;
     border-bottom: 1px solid var(--line); }
h3 { font-size: 14px; margin: 22px 0 8px; }
.meta { color: var(--mut); font-size: 12.5px; margin-bottom: 24px; }
.meta code { background:#f2f3f5; padding:1px 5px; border-radius:3px; }
table { border-collapse: collapse; width: 100%; font-variant-numeric: tabular-nums;
        font-size: 13px; }
th, td { padding: 7px 10px; border-bottom: 1px solid var(--line); text-align: right; }
th:first-child, td:first-child { text-align: left; }
th { font-weight: 600; color: var(--mut); font-size: 11.5px;
     text-transform: uppercase; letter-spacing: .04em; }
tr:hover td { background: #fafbfc; }
td.big { font-weight: 700; }
td.fail { font-weight: 700; color: var(--bad); }
.noise { color: var(--mut); font-weight: 400; }
.unres { color: var(--mut); font-style: italic; }
.pill { display:inline-block; padding:2px 9px; border-radius:11px;
        font-size:12px; font-weight:600; }
.pass { background:#e7f5ec; color:var(--good); }
.fail { background:#fdeceb; color:var(--bad); }
.warnp{ background:#fdf3e7; color:var(--warn); }
ul.checks { list-style:none; padding:0; margin:10px 0; }
ul.checks li { padding:7px 11px; border-left:3px solid var(--line);
               margin-bottom:5px; background:#fafbfc; font-size:12.5px; }
ul.checks li.f { border-left-color: var(--bad); }
ul.checks li.w { border-left-color: var(--warn); }
ul.checks li b { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
.note { background:#f7f9fb; border-left:3px solid var(--bar); padding:11px 14px;
        margin:14px 0; font-size:13px; }
figure { margin: 18px 0 26px; }
figcaption { color: var(--mut); font-size: 12px; margin-bottom: 8px; }
code { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12.5px; }
"""


def _svg_bars(items, caption, unit="%", width=1100, row_h=30, pad_left=150):
    """Horizontal bars with the noise floor drawn as an error whisker.

    Drawn as inline SVG so the report is a single file with no CDN or JS.
    """
    items = [(lab, v, n) for lab, v, n in items
             if v is not None and v == v and not math.isinf(v)]
    if not items:
        return ""
    height = row_h * len(items) + 44
    lo = min(0.0, min(v - (n or 0) for _, v, n in items))
    hi = max(v + (n or 0) for _, v, n in items)
    hi = max(hi, lo + 1e-6)
    span = hi - lo
    plot_w = width - pad_left - 90

    def x(val):
        return pad_left + (val - lo) / span * plot_w

    parts = [f'<figure><figcaption>{html_mod.escape(caption)}</figcaption>',
             f'<svg viewBox="0 0 {width} {height}" width="100%" '
             f'role="img" aria-label="{html_mod.escape(caption)}">']
    zero = x(0.0)
    parts.append(f'<line x1="{zero:.1f}" y1="18" x2="{zero:.1f}" y2="{height - 26}" '
                 'stroke="#c9ccd1" stroke-width="1"/>')
    for i, (label, val, noise) in enumerate(items):
        y = 24 + i * row_h
        bx0, bx1 = (zero, x(val)) if val >= 0 else (x(val), zero)
        parts.append(
            f'<text x="{pad_left - 10}" y="{y + 12}" text-anchor="end" '
            f'font-size="12" fill="#1c1e21">{html_mod.escape(label)}</text>')
        parts.append(
            f'<rect x="{bx0:.1f}" y="{y + 2}" width="{max(bx1 - bx0, 1):.1f}" '
            f'height="16" fill="#3b6ea5" rx="2"/>')
        if noise:
            e0, e1 = x(val - noise), x(val + noise)
            ym = y + 10
            parts.append(
                f'<line x1="{e0:.1f}" y1="{ym}" x2="{e1:.1f}" y2="{ym}" '
                'stroke="#1c1e21" stroke-width="1" opacity=".55"/>'
                f'<line x1="{e0:.1f}" y1="{ym - 4}" x2="{e0:.1f}" y2="{ym + 4}" '
                'stroke="#1c1e21" stroke-width="1" opacity=".55"/>'
                f'<line x1="{e1:.1f}" y1="{ym - 4}" x2="{e1:.1f}" y2="{ym + 4}" '
                'stroke="#1c1e21" stroke-width="1" opacity=".55"/>')
        parts.append(
            f'<text x="{max(bx1, zero) + 8:.1f}" y="{y + 14}" font-size="12" '
            f'fill="#444">{val:+.1f}{unit}</text>')
    parts.append(f'<text x="{pad_left}" y="{height - 8}" font-size="11" '
                 f'fill="#666">whisker = noise floor</text>')
    parts.append("</svg></figure>")
    return "".join(parts)


def _svg_sweep(points, caption, width=760, height=300):
    """Overhead against thread count, with a linear reference from the first point."""
    points = [(n, v) for n, v in points if v is not None and v == v]
    if len(points) < 2:
        return ""
    pad_l, pad_b, pad_t, pad_r = 56, 40, 18, 20
    xs = [p[0] for p in points]
    ys = [p[1] for p in points]
    ref = [(n, ys[0] * n / xs[0]) for n in xs] if xs[0] else []
    hi = max(max(ys), max((v for _, v in ref), default=0)) * 1.15 or 1
    lo = min(0, min(ys))

    def px(n):
        return pad_l + (xs.index(n) / (len(xs) - 1)) * (width - pad_l - pad_r)

    def py(v):
        return height - pad_b - (v - lo) / (hi - lo) * (height - pad_b - pad_t)

    parts = [f'<figure><figcaption>{html_mod.escape(caption)}</figcaption>',
             f'<svg viewBox="0 0 {width} {height}" width="100%">']
    parts.append(f'<line x1="{pad_l}" y1="{py(lo):.1f}" x2="{width - pad_r}" '
                 f'y2="{py(lo):.1f}" stroke="#c9ccd1"/>')
    if ref:
        d = " ".join(f"{px(n):.1f},{py(v):.1f}" for n, v in ref)
        parts.append(f'<polyline points="{d}" fill="none" stroke="#c0392b" '
                     'stroke-width="1.5" stroke-dasharray="5 4" opacity=".7"/>')
    d = " ".join(f"{px(n):.1f},{py(v):.1f}" for n, v in points)
    parts.append(f'<polyline points="{d}" fill="none" stroke="#3b6ea5" stroke-width="2"/>')
    for n, v in points:
        parts.append(f'<circle cx="{px(n):.1f}" cy="{py(v):.1f}" r="4" fill="#3b6ea5"/>')
        parts.append(f'<text x="{px(n):.1f}" y="{py(v) - 11:.1f}" font-size="11" '
                     f'text-anchor="middle" fill="#444">{v:+.1f}%</text>')
        parts.append(f'<text x="{px(n):.1f}" y="{height - pad_b + 17:.1f}" '
                     f'font-size="11" text-anchor="middle" fill="#666">{n}</text>')
    parts.append(f'<text x="{width / 2:.0f}" y="{height - 6}" font-size="11" '
                 'text-anchor="middle" fill="#666">threads</text>')
    parts.append(f'<text x="{width - pad_r}" y="{pad_t + 4}" font-size="11" '
                 'text-anchor="end" fill="#c0392b">linear reference</text>')
    parts.append("</svg></figure>")
    return "".join(parts)


def html(meta, agg, rep):
    e = html_mod.escape
    P = []
    A = P.append
    A("<!doctype html><meta charset='utf-8'>")
    A("<title>CPU profiler overhead</title>")
    A(f"<style>{_CSS}</style>")
    A("<h1>CPU profiler overhead</h1>")
    A("<div class='meta'>"
      f"measured <code>{e(meta['timestamp_utc'])}</code> &middot; "
      f"<code>{e(meta['uname'])}</code> &middot; CPython {e(meta['python'])}<br>"
      f"subject on CPUs <code>{e(meta['subject_cpus'])}</code>, sink "
      f"<code>{e(meta['sink_cpus'])}</code>, load generator "
      f"<code>{e(meta['loadgen_cpus'])}</code><br>"
      f"{meta['repeats']} repeats round-robin across the whole matrix &middot; "
      f"~{meta['target_region_s']:.0f}s per region &middot; "
      f"{meta['warmup_s']:.0f}s warmup &middot; "
      + " &middot; ".join(
          f"<code>{e(vn)}: {e(meta.get('variant_detail', {}).get(vn, {}).get('config', ''))}</code>"
          for vn in meta["variants"]
          if meta.get("variant_detail", {}).get(vn, {}).get("config")) + "<br>"
      f"<b>worst noise floor in this table: {_fmt(_noise_summary(agg, meta['variants']), 1)}%</b>"
      " &mdash; no smaller difference is ranked"
      "</div>")

    status = ("<span class='pill pass'>all checks passed</span>" if rep.green and not rep.warnings
              else "<span class='pill fail'>%d failed</span> " % len(rep.failures) if rep.failures
              else "<span class='pill warnp'>%d warnings</span>" % len(rep.warnings))
    A(f"<div>{status}</div>")

    for variant in meta["variants"]:
        if variant == "none":
            continue
        rows = list(_rows(agg, variant))
        if not rows:
            continue
        A(f"<h2><code>{e(variant)}</code> vs no profiling</h2>")

        if meta["mode"] == "sweep":
            pts = sorted((r[1]["threads"], r[2]["cpu_overhead_pct"]) for r in rows)
            A(_svg_sweep(
                pts,
                f"CPU overhead against thread count, `{variant}`. "
                + ("Every live thread is unwound each tick, so this must rise."
                   if detail_walks(meta, variant)
                   else "gil_only skips non-GIL threads before unwinding, so "
                        "this should stay close to flat.")))
        else:
            A(_svg_bars([(k, v["cpu_overhead_pct"], v["noise_pct"]) for k, _, v in rows],
                        "CPU overhead by workload shape (fixed work, cgroup cpu.stat)"))
            A(_svg_bars([(k, v["mem_delta_mb"], None) for k, _, v in rows],
                        "Steady-state memory added by the profiler", unit=" MB"))

        A("<table><thead><tr>"
          "<th>workload</th><th>thr</th><th>cpu none (s)</th><th>cpu profiled (s)</th>"
          "<th>cpu overhead</th><th>noise</th><th>% of a core</th><th>wall oh</th>"
          "<th>mem static &Delta;</th><th>mem peak &Delta;</th>"
          "<th>seen/used</th><th>seen/wall</th></tr></thead><tbody>")
        failed = rep.failed_scopes()
        for key, ent, v in rows:
            invalid = f"{key}/{variant}" in failed
            oh_txt = _overhead_cell(v, markdown=False, invalid=invalid)
            cls = "fail" if invalid else ("big" if v["resolvable"] else "unres")
            A("<tr>"
              f"<td><code>{e(key)}</code></td><td>{ent['threads']}</td>"
              f"<td>{_fmt(ent['none']['cpu_s'])} "
              f"<span class='noise'>&plusmn;{_fmt(ent['none']['cpu_spread_pct'], 1)}%</span></td>"
              f"<td>{_fmt(v['cpu_s'])} "
              f"<span class='noise'>&plusmn;{_fmt(v['cpu_spread_pct'], 1)}%</span></td>"
              f"<td class='{cls}'>{e(oh_txt)}</td>"
              f"<td class='noise'>{_fmt(v['noise_pct'], 1)}%</td>"
              f"<td>{_fmt(v['cpu_cost_cores_pct'], 2, '%')}</td>"
              f"<td>{_wall_cell(ent, v)}</td>"
              f"<td>{_fmt(v['mem_delta_mb'], 1)} MB</td>"
              f"<td>{_fmt(v['mem_peak_delta_mb'], 1)} MB</td>"
              f"<td>{_fmt(v['seen_over_used'], 2)}</td>"
              f"<td>{_fmt(v['seen_over_wall'], 2)}</td></tr>")
        A("</tbody></table>")

    http_rows = [(vn, k, ent, v) for vn in meta["variants"] if vn != "none"
                 for k, ent, v in _rows(agg, vn) if ent["shape"] == "http"]
    if http_rows:
        A("<h2>HTTP shape detail</h2>")
        A("<div class='note'>Fixed request count, so <b>cpu per request</b> is the "
          "comparable figure. Throughput and wall time are context only: the "
          "region ends when the last request is served, so both move with how "
          "warm the machine is. Do not read a throughput gain here as the "
          "profiler making the server faster.</div>")
        A("<table><thead><tr><th>variant</th><th>cpu us/request</th>"
          "<th>vs baseline</th><th>rps</th><th>rps delta</th></tr></thead><tbody>")
        for vname, key, ent, v in http_rows:
            A(f"<tr><td><code>{e(vname)}</code></td>"
              f"<td>{_fmt(v['cpu_us_per_request'], 1)}</td>"
              f"<td class='big'>{_fmt(v['cpu_us_per_request_overhead_pct'], 1, '%')}</td>"
              f"<td>{_fmt(v['rps'], 0)}</td>"
              f"<td>{_fmt(v['rps_delta_pct'], 1, '%')}</td></tr>")
        A("</tbody></table>")
        A(f"<p class='meta'>Unprofiled baseline: "
          f"{_fmt(http_rows[0][2]['none_cpu_us_per_request'], 1)} us cpu/request at "
          f"{_fmt(http_rows[0][2]['none_rps'], 0)} rps.</p>")

    A("<h2>How to read this</h2>")
    A("<div class='note'><b>% of a core</b> is the absolute cost: how much of one "
      "CPU core the profiler occupies. Read it together with the relative figure "
      "&mdash; a workload that barely touches the CPU can show a large percentage "
      "overhead for a small absolute cost.</div>")
    A("<div class='note'><b>cpu overhead</b> is the headline: a cgroup "
      "<code>cpu.stat</code> delta over a fixed amount of work, so the cost has "
      "nowhere to hide. <b>Italic <code>~</code></b> means the difference is "
      "smaller than the noise floor and is deliberately not ranked. "
      "<b>wall overhead</b> is reference only -- it absorbs scheduler jitter and "
      "compresses the ranking into the noise.</div>")
    A("<div class='note'><b>seen/used</b> is the CPU the profile claims divided by "
      "the CPU the process consumed while the agent was running. <b>seen/wall</b> "
      "is the same numerator over wall time &mdash; how many cores' worth the "
      "profile claims. At <code>gil_only=True</code> one stack is recorded per "
      "tick so 1.0 is the ceiling; at <code>gil_only=False</code> the ceiling is "
      "the thread count. Both are correctness numbers and they sit beside "
      "the cost number on purpose &mdash; a profiler that drops samples is cheap, "
      "so the trade cannot be read one without the other. Sample <em>records</em> "
      "are not reported as a volume: the encoder aggregates identical stacks, so a "
      "record count measures cardinality, not how much was collected.</div>")
    A("<h3>What a unit of configuration buys</h3>")
    detail = meta.get("variant_detail", {})
    if detail:
        A("<table><thead><tr><th>variant</th><th>what it is</th>"
          "<th>cost of one tick</th></tr></thead><tbody>")
        for vname in meta["variants"]:
            d = detail.get(vname, {})
            A(f"<tr><td><code>{e(vname)}</code></td>"
              f"<td>{e(d.get('label', ''))}</td>"
              f"<td>{e(d.get('per_tick', ''))}</td></tr>")
        A("</tbody></table>")
    A("<div class='note'><code>gil_only</code> and <code>oncpu</code> are applied "
      "at different points, and the difference decides how cost scales. "
      "<code>gil_only</code> is applied by py-spy inside its per-thread loop "
      "<em>before</em> the unwind (<code>python_spy.rs</code>), so at the shipping "
      "default a tick unwinds exactly one stack &mdash; the GIL holder &mdash; and "
      "costs one pointer read per other thread. <code>oncpu</code> is applied by "
      "the consumer in <code>rust/src/pyspy_backend.rs</code> <em>after</em> the "
      "unwind, so that walk is already paid for. Consequences: per-tick cost is "
      "close to flat in thread count, and a <code>seen/used</code> well below 1 on "
      "a multi-threaded shape is the pre-walk filter working rather than the "
      "profiler failing.</div>")

    A("<h2>Invariants</h2>")
    passed = len(rep.checks) - len(rep.failures) - len(rep.warnings)
    A(f"<p>{passed} passed, {len(rep.failures)} failed, {len(rep.warnings)} warned.</p>")
    if rep.failures or rep.warnings:
        A("<ul class='checks'>")
        for name, _shape, count, detail in _collapse(rep.failures):
            times = f" &times;{count}" if count > 1 else ""
            A(f"<li class='f'><b>{e(name)}</b>{times} &mdash; {e(detail)}</li>")
        for name, _shape, count, detail in _collapse(rep.warnings):
            times = f" &times;{count}" if count > 1 else ""
            A(f"<li class='w'><b>{e(name)}</b>{times} &mdash; {e(detail)}</li>")
        A("</ul>")
    if rep.failures:
        A("<div class='note'>Cells covered by a failed check are not ranked. A "
          "failed physical invariant invalidates the table, not just the "
          "offending row.</div>")
    return "\n".join(P)
