#!/usr/bin/env python3
"""Compare existing ctx and Zoekt CLI binaries; never install, compile or serve."""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import tempfile
import time

from search_speed import stats

# Deliberately shared, single-line regex syntax; no query-language escaping needed.
QUERIES = [
    ("rare_literal", "literal", "unique_handler_7391", False),
    ("common_literal", "literal", "common_handler", False),
    ("absent_literal", "literal", "missing_handler_9842", False),
    ("selective_regex", "regex", "unique_handler_[0-9]+", False),
    ("short_literal", "literal", "if", False),
    ("unicode_case", "literal", "éclair", True),
]


def corpus(count):
    return {f"module_{i:05d}.py": (
        f"# module {i}\n" + "# ordinary padding\n" * 12
        + "def common_handler():\n    if True:\n        return 1\n"
        + "# gap\n" * 12
        + ("unique_handler_7391 = 'ÉCLAIR'\n" if i == count // 2 else "value = 0\n")
    ) for i in range(count)}


def spans(source, numbers):
    lines = source.splitlines()
    merged = []
    for number in sorted(set(numbers)):
        if not 1 <= number <= len(lines):
            raise AssertionError(f"Invalid matching line: {number}")
        start, end = max(1, number - 2), min(len(lines), number + 2)
        if merged and start <= merged[-1][1] + 1:
            merged[-1][1] = max(end, merged[-1][1])
        else:
            merged.append([start, end])
    return [(a, b, "\n".join(lines[a-1:b])) for a, b in merged]


def oracle(sources, pattern, ignore_case):
    rx = re.compile(pattern, re.IGNORECASE if ignore_case else 0)
    return sorted((path, *span) for path, source in sources.items()
                  for span in spans(source, [i for i, line in enumerate(source.splitlines(), 1)
                                             if rx.search(line)]))


def normalize_ctx(output):
    result = json.loads(output)
    if result['hint'] is not None or result['coverage'] != 'text_only':
        raise AssertionError('ctx returned partial evidence')
    if any(h.get('snippet_truncated', False) for h in result['hits']):
        raise AssertionError('ctx truncated a snippet')
    return sorted((h['path'], h['start'], h['end'], h['snippet']) for h in result['hits'])


def normalize_zoekt(output, sources):
    found = {}
    for row in output.splitlines():
        result = json.loads(row)
        path = result['FileName']
        if path not in sources:
            raise AssertionError(f'Unexpected Zoekt path: {path}')
        lines = sources[path].splitlines()
        for match in result.get('LineMatches', []):
            number = match['LineNumber']
            if match['FileName'] or not 1 <= number <= len(lines):
                raise AssertionError('Unexpected filename match or line number')
            actual = base64.b64decode(match['Line'], validate=True).decode().rstrip('\n')
            if actual != lines[number-1]:
                raise AssertionError('Zoekt returned stale/different source text')
            found.setdefault(path, []).append(number)
    return sorted((path, *span) for path, numbers in found.items()
                  for span in spans(sources[path], numbers))


def invoke(command, root):
    env = os.environ.copy()
    env.pop('CTX_DIR', None)
    started = time.perf_counter()
    result = subprocess.run([str(x) for x in command], cwd=root, env=env,
                            capture_output=True, check=True, timeout=120)
    return (time.perf_counter()-started)*1000, result.stdout


def executable(value):
    path = shutil.which(value)
    if path is None:
        raise ValueError(f'Missing executable: {value}. Supply an existing binary; no automatic build/install.')
    return Path(path).resolve()


def self_test():
    source = 'first\nneedle\nthird\nfourth\nfifth\nneedle\nlast\n'
    expected = [('a.py', 1, 7, source.rstrip('\n'))]
    payload = json.dumps({'FileName': 'a.py', 'LineMatches': [
        {'LineNumber': n, 'Line': base64.b64encode(b'needle\n').decode(), 'FileName': False}
        for n in [2, 6]]}).encode()
    assert normalize_zoekt(payload, {'a.py': source}) == expected
    assert oracle({'a.py': source}, 'needle', False) == expected
    assert normalize_zoekt(b'', {'a.py': source}) == []
    assert oracle(corpus(2), 'éclair', True)
    try:
        normalize_zoekt(payload, {'a.py': source.replace('needle', 'changed')})
    except AssertionError:
        pass
    else:
        raise AssertionError('Stale source was accepted')
    print('Adapter self-test passed; this does not validate actual Zoekt integration.')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--ctx', default='target/release/ctx')
    parser.add_argument('--ctx-profile', choices=['release', 'debug', 'unknown'], default='unknown')
    parser.add_argument('--zoekt', default='zoekt')
    parser.add_argument('--zoekt-index', default='zoekt-index')
    parser.add_argument('--files', type=int, default=100)
    parser.add_argument('--iterations', type=int, default=10)
    parser.add_argument('--warmups', type=int, default=2)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--self-test', action='store_true')
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return
    if not args.output or not 1 <= args.files <= 1000 or args.iterations < 2 or args.warmups < 0:
        parser.error('--output, 1..1000 files, >=2 iterations and >=0 warmups required')
    try:
        ctx, zoekt, indexer = map(executable, [args.ctx, args.zoekt, args.zoekt_index])
    except ValueError as error:
        parser.error(str(error))
    sources = corpus(args.files)
    result = dict(schema=1, platform=platform.platform(), ctx_profile=args.ctx_profile,
                  binaries={str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in [ctx, zoekt, indexer]},
                  files=args.files, source_bytes=sum(len(s.encode()) for s in sources.values()),
                  corpus_sha256=hashlib.sha256(json.dumps(sources, sort_keys=True).encode()).hexdigest(),
                  iterations=args.iterations, warmups=args.warmups, scenarios=[],
                  methodology='Alternating fresh CLI processes; warm OS cache. Timings include startup and native output capture, exclude parsing/normalization. ctx emits merged context; Zoekt emits matching lines. Independent single-line oracle checks complete normalized context, ignoring ranking. Index costs recorded separately. Synthetic corpus, no MCP, watcher, semantic search or release equivalence claim. Zoekt default result caps are not disabled: any missing evidence fails parity.')
    with tempfile.TemporaryDirectory(prefix='ctx-zoekt-') as temporary:
        root = Path(temporary)/'source'
        root.mkdir()
        index = Path(temporary)/'zoekt-index'
        for name, content in sources.items():
            (root/name).write_text(content, encoding='utf-8')
        # Index Zoekt first, before ctx creates its own artifacts.
        result['zoekt_index_ms'], _ = invoke([indexer, '-index', index, '-disable_ctags', root], root)
        result['ctx_index_ms'], _ = invoke([ctx, 'index', '.'], root)
        result['index_bytes'] = {name: sum(p.stat().st_size for p in folder.rglob('*') if p.is_file())
                                 for name, folder in [('ctx', root/'.ctx'), ('zoekt', index)]}
        for name, mode, pattern, folding in QUERIES:
            expected = oracle(sources, pattern, folding)
            commands = {'ctx': [ctx, 'search', pattern, '--mode', mode, '--json', '--limit', '10000000', '--budget-tokens', '1000000000'] + (['--ignore-case'] if folding else []),
                        'zoekt': [zoekt, '-index_dir', index, '-jsonl', f'case:{"no" if folding else "yes"} content:{pattern}']}
            samples = {'ctx': [], 'zoekt': []}
            for iteration in range(args.warmups + args.iterations):
                for engine in (['ctx', 'zoekt'] if iteration % 2 == 0 else ['zoekt', 'ctx']):
                    elapsed, output = invoke(commands[engine], root)
                    actual = normalize_ctx(output) if engine == 'ctx' else normalize_zoekt(output, sources)
                    if actual != expected:
                        raise AssertionError(f'{name}/{engine}: evidence mismatch; expected {len(expected)}, got {len(actual)} spans')
                    if iteration >= args.warmups:
                        samples[engine].append(elapsed)
            row = dict(name=name, pattern=pattern, mode=mode, ignore_case=folding, parity=True,
                       spans=len(expected), **{engine: stats(values) for engine, values in samples.items()})
            row['zoekt_over_ctx'] = row['zoekt']['p50_ms']/row['ctx']['p50_ms']
            result['scenarios'].append(row)
            print(f'{name}: ctx {row["ctx"]["p50_ms"]:.2f} ms / Zoekt {row["zoekt"]["p50_ms"]:.2f} ms', flush=True)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2)+'\n')


if __name__ == '__main__':
    main()
