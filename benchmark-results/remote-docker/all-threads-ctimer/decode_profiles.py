"""Decode saved PushRequest/pprof protobufs, preserving full frame sequences."""
from collections import defaultdict
import gzip
import json
from pathlib import Path
import sys


def varint(data, pos):
    value = shift = 0
    while True:
        b = data[pos]
        pos += 1
        value |= (b & 127) << shift
        if b < 128:
            return value, pos
        shift += 7


def message(data):
    fields = defaultdict(list)
    pos = 0
    while pos < len(data):
        key, pos = varint(data, pos)
        wire = key & 7
        if wire == 0:
            value, pos = varint(data, pos)
        elif wire == 2:
            size, pos = varint(data, pos)
            value = data[pos:pos+size]
            pos += size
        elif wire in (1, 5):
            size = 8 if wire == 1 else 4
            value = data[pos:pos+size]
            pos += size
        else:
            raise ValueError(f'Unexpected wire type {wire}')
        fields[key >> 3].append(value)
    return fields


def one(m, field, default=0):
    return m.get(field, [default])[0]


def numbers(values):
    result = []
    for value in values:
        if isinstance(value, int):
            result.append(value)
        else:
            pos = 0
            while pos < len(value):
                n, pos = varint(value, pos)
                result.append(n)
    return result


def decode(raw):
    if raw[:2] == b'\x1f\x8b':
        raw = gzip.decompress(raw)
    p = message(raw)
    strings = [s.decode('utf-8', 'replace') for s in p[6]]
    types = []
    for raw_type in p[1]:
        t = message(raw_type)
        types.append({'type': strings[one(t, 1)], 'unit': strings[one(t, 2)]})
    funcs = {}
    for raw_function in p[5]:
        f = message(raw_function)
        funcs[one(f, 1)] = {'name': strings[one(f, 2)], 'file': strings[one(f, 4)],
                            'start_line': one(f, 5)}
    locations = {}
    for raw_loc in p[4]:
        loc = message(raw_loc)
        frames = []
        for raw_line in loc[4]:
            line = message(raw_line)
            frames.append(dict(funcs[one(line, 1)], line=one(line, 2)))
        locations[one(loc, 1)] = frames
    samples = []
    totals = [0] * len(types)
    for raw_sample in p[2]:
        sample = message(raw_sample)
        values = numbers(sample[2])
        values = [v - 2**64 if v >= 2**63 else v for v in values]
        frames = []
        for loc_id in reversed(numbers(sample[1])):
            frames.extend(reversed(locations[loc_id]))
        for i, v in enumerate(values):
            totals[i] += v
        samples.append({'frames_root_to_leaf': frames, 'values': values})
    return {'sample_types': types, 'totals': totals, 'samples': samples,
            'time_nanos': one(p, 9), 'duration_nanos': one(p, 10)}


root = Path(sys.argv[1])
uploads = json.loads((root / 'uploads.json').read_text())
aggregated = {}
with gzip.open(root / 'decoded-profiles.jsonl.gz', 'wt') as output:
    for upload in uploads:
        request = message(gzip.decompress((root / upload['path']).read_bytes()))
        for raw_series in request[1]:
            series = message(raw_series)
            for raw_sample in series[2]:
                raw = one(message(raw_sample), 1)
                decoded = decode(raw)
                output.write(json.dumps(dict(upload=upload, profile=decoded)) + '\n')
                case = upload['case']
                if case not in aggregated:
                    aggregated[case] = {'profiles': 0, 'sample_types': decoded['sample_types'],
                                        'totals': [0]*len(decoded['totals']),
                                        'stacks': defaultdict(lambda: [0]*len(decoded['totals']))}
                agg = aggregated[case]
                assert agg['sample_types'] == decoded['sample_types']
                agg['profiles'] += 1
                for i, v in enumerate(decoded['totals']):
                    agg['totals'][i] += v
                for sample in decoded['samples']:
                    key = json.dumps(sample['frames_root_to_leaf'], sort_keys=True)
                    for i, v in enumerate(sample['values']):
                        agg['stacks'][key][i] += v
for case, agg in aggregated.items():
    agg['stacks'] = sorted([{'frames_root_to_leaf': json.loads(k), 'values': v}
                            for k, v in agg['stacks'].items()],
                           key=lambda s: s['values'][-1], reverse=True)
    print(case, agg['profiles'], agg['sample_types'], agg['totals'], 'distinct stacks', len(agg['stacks']))
with gzip.open(root / 'complete-stacks.json.gz', 'wt') as f:
    json.dump(aggregated, f)
(root / 'profile-summary.json').write_text(json.dumps({k: {a: b for a, b in v.items() if a != 'stacks'}
                                                    for k, v in aggregated.items()}, indent=2))
