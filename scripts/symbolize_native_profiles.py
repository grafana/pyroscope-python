#!/usr/bin/env python3
"""Symbolize address-only native pprofs using matching ELF files and llvm-symbolizer.

The binary root mirrors the profiled process's filesystem, for example:
  binaries/usr/local/lib/python3.13/site-packages/pyroscope/_native.so
Raw profiles are preserved; symbolized copies go into a separate directory.
Uses only the Python standard library. Keep ELF symbols/debug info when building.
"""
import argparse
from collections import defaultdict
import gzip
import json
from pathlib import Path
import struct
import subprocess


def varint(data, pos):
    value = shift = 0
    while True:
        byte = data[pos]
        pos += 1
        value |= (byte & 127) << shift
        if byte < 128:
            return value, pos
        shift += 7
        if shift > 63:
            raise ValueError('oversized protobuf varint')


def message(data):
    fields = defaultdict(list)
    pos = 0
    while pos < len(data):
        key, pos = varint(data, pos)
        if key & 7 == 0:
            value, pos = varint(data, pos)
        elif key & 7 == 2:
            size, pos = varint(data, pos)
            value = data[pos:pos + size]
            pos += size
        else:
            raise ValueError(f'unexpected pprof wire type {key & 7}')
        fields[key >> 3].append(value)
    if pos != len(data):
        raise ValueError('truncated protobuf')
    return fields


def one(fields, number, default=0):
    return fields.get(number, [default])[0]


def encode_varint(value):
    data = bytearray()
    while value > 127:
        data.append((value & 127) | 128)
        value >>= 7
    data.append(value)
    return bytes(data)


def encode(fields):
    chunks = []
    for number, values in sorted(fields.items()):
        for value in values:
            if isinstance(value, bytes):
                chunks.extend((encode_varint(number << 3 | 2), encode_varint(len(value)), value))
            else:
                chunks.extend((encode_varint(number << 3), encode_varint(value)))
    return b''.join(chunks)


def read_profile(path):
    raw = path.read_bytes()
    if raw.startswith(b'\x1f\x8b'):
        raw = gzip.decompress(raw)
    return message(raw)


def elf_segments(path):
    with path.open('rb') as source:
        header = source.read(64)
        if header[:6] != b'\x7fELF\x02\x01':
            raise ValueError(f'{path}: expected a little-endian ELF64 binary')
        phoff = struct.unpack_from('<Q', header, 32)[0]
        entry_size, count = struct.unpack_from('<HH', header, 54)
        segments = []
        for i in range(count):
            source.seek(phoff + i * entry_size)
            kind, flags, offset, address, _, size, _, align = struct.unpack('<IIQQQQQQ', source.read(56))
            if kind == 1 and flags & 1:
                segments.append((offset, address, size, align))
        return segments


def lookup_address(mapping, address, segments):
    offset = one(mapping, 4)
    if not one(mapping, 2) <= address < one(mapping, 3):
        raise ValueError('location outside its executable mapping')
    for file_offset, svma, size, align in segments:
        # Linux mappings begin at a page boundary, potentially before PT_LOAD.
        # This also handles non-PIE executables, whose SVMA is not a file offset.
        if file_offset // max(align, 1) * max(align, 1) <= offset < file_offset + size:
            return svma + offset - file_offset + address - one(mapping, 2)
    raise ValueError(f'no executable ELF segment for mapping offset {offset:#x}')


def symbolize(input_dir, output_dir, binary_root, symbolizer):
    if input_dir.resolve() == output_dir.resolve():
        raise ValueError('output directory must differ from raw input directory')
    paths = sorted(input_dir.glob('*.pprof'), key=lambda p: int(p.stem))
    if not paths:
        raise ValueError('no raw pprof files')
    profiles = []
    segments = {}
    addresses = defaultdict(set)
    for path in paths:
        p = read_profile(path)
        if p[5] or any(message(loc)[4] for loc in p[4]):
            raise ValueError(f'{path}: expected an address-only pprof')
        strings = [s.decode() for s in p[6]]
        mappings = {one(m, 1): m for m in map(message, p[3])}
        locations = list(map(message, p[4]))
        lookups = {}
        for loc in locations:
            mapping_id = one(loc, 2)
            if not mapping_id:
                continue
            mapping = mappings[mapping_id]
            filename = strings[one(mapping, 5)]
            binary = (binary_root / filename.lstrip('/')).resolve()
            if not binary.is_relative_to(binary_root.resolve()):
                raise ValueError(f'binary path escapes root: {filename}')
            if binary not in segments:
                segments[binary] = elf_segments(binary)
            address = lookup_address(mapping, one(loc, 3), segments[binary])
            lookups[one(loc, 1)] = (binary, address)
            addresses[binary].add(address)
        profiles.append((path, p, mappings, locations, lookups))
    resolved = {}
    for binary, unique in addresses.items():
        ordered = sorted(unique)
        result = subprocess.run(
            [symbolizer, '--output-style=JSON', '--demangle', '--inlines', '--obj=' + str(binary)],
            input=''.join(f'0x{a:x}\n' for a in ordered), text=True, capture_output=True, check=True,
        )
        records = [json.loads(line) for line in result.stdout.splitlines()]
        if len(records) != len(ordered):
            raise ValueError(f'{binary}: symbolizer returned {len(records)} records for {len(ordered)} addresses')
        for address, record in zip(ordered, records):
            if int(record['Address'], 16) != address or 'Error' in record:
                raise ValueError(f'{binary}: {record}')
            resolved[binary, address] = [s for s in record['Symbol'] if s.get('FunctionName') not in ('', '??')]
    output_dir.mkdir(parents=True, exist_ok=True)
    summary = {'files': len(profiles), 'binaries': len(addresses), 'unique_addresses': sum(map(len, addresses.values())),
               'resolved_locations': 0, 'unresolved_locations': 0, 'inline_locations': 0}
    for path, p, mappings, locations, lookups in profiles:
        strings = {value: i for i, value in enumerate(p[6])}
        functions = {}

        def intern(value):
            value = value.encode()
            if value not in strings:
                strings[value] = len(p[6])
                p[6].append(value)
            return strings[value]

        for loc in locations:
            frames = resolved.get(lookups.get(one(loc, 1)), [])
            if not frames:
                summary['unresolved_locations'] += 1
                continue
            summary['resolved_locations'] += 1
            summary['inline_locations'] += len(frames) > 1
            # LLVM returns the innermost inline frame first, as pprof expects.
            for frame in frames:
                name = frame['FunctionName']
                filename = frame.get('FileName') or frame.get('StartFileName', '')
                start_line = max(frame.get('StartLine', 0), 0)
                key = (name, filename, start_line)
                if key not in functions:
                    fid = len(functions) + 1
                    functions[key] = fid
                    p[5].append(encode({1: [fid], 2: [intern(name)], 3: [intern(name)],
                                        4: [intern(filename)], 5: [start_line]}))
                loc[4].append(encode({1: [functions[key]], 2: [max(frame.get('Line', 0), 0)]}))
        p[4] = list(map(encode, locations))
        for mid, mapping in mappings.items():
            mapped = [loc for loc in locations if one(loc, 2) == mid]
            mapping[7] = [int(bool(mapped) and all(loc[4] for loc in mapped))]
            mapping[10] = [int(any(len(loc[4]) > 1 for loc in mapped))]
        p[3] = list(map(encode, mappings.values()))
        encoded = encode(p)
        original = read_profile(path)
        decoded = message(encoded)
        # Symbolization must preserve every stack's physical locations, weight,
        # labels, and timing; only symbol metadata and mapping flags may change.
        for number in (1, 2, 9, 10, 11, 12):
            assert decoded[number] == original[number], (path, number)
        assert [tuple(one(message(loc), n) for n in (1, 2, 3)) for loc in decoded[4]] == [
            tuple(one(message(loc), n) for n in (1, 2, 3)) for loc in original[4]
        ], path
        (output_dir / path.name).write_bytes(encoded)
    (output_dir / 'symbolization.json').write_text(json.dumps(summary, indent=2))
    print(json.dumps(summary, indent=2))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('input', type=Path)
    parser.add_argument('output', type=Path)
    parser.add_argument('--binary-root', type=Path, required=True)
    parser.add_argument('--symbolizer', default='llvm-symbolizer')
    args = parser.parse_args()
    symbolize(args.input, args.output, args.binary_root, args.symbolizer)
