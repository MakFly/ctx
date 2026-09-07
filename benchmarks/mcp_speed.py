#!/usr/bin/env python3
"""Compare a persistent release MCP session with fresh indexed CLI requests.
Uses committed FastAPI Python files in a disposable checkout; never builds.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import select
import sqlite3
import subprocess
import tempfile
import time
from search_speed import run, stats


class Client:
    def __init__(self, binary, root):
        env = os.environ.copy()
        env.pop('CTX_DIR', None)
        self.log = tempfile.TemporaryFile()
        self.process = subprocess.Popen([str(binary), 'mcp'], cwd=root, env=env,
                                        stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=self.log)
        self.sequence = 0

    def send(self, message):
        self.process.stdin.write((json.dumps(message) + '\n').encode())
        self.process.stdin.flush()

    def call(self, method, params):
        self.sequence += 1
        started = time.perf_counter()
        self.send(dict(jsonrpc='2.0', id=self.sequence, method=method, params=params))
        deadline = started + 60
        while True:
            if not select.select([self.process.stdout], [], [], max(0, deadline-time.perf_counter()))[0]:
                raise TimeoutError(method)
            line = self.process.stdout.readline()
            if not line:
                raise RuntimeError('MCP exited before response')
            message = json.loads(line)
            if message.get('id') == self.sequence:
                if 'error' in message:
                    raise RuntimeError(message['error'])
                self.last_response_bytes = len(line)
                return (time.perf_counter()-started)*1000, message['result']

    def search(self, arguments):
        elapsed, result = self.call('tools/call', dict(name='ctx_search', arguments=arguments))
        if result.get('isError'):
            raise RuntimeError(result)
        envelope = result.get('structuredContent')
        if envelope is None:
            envelope = json.loads(next(c['text'] for c in result['content'] if c['type']=='text'))
        return elapsed, envelope

    def close(self):
        self.process.stdin.close()
        try:
            self.process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait()
            raise RuntimeError('MCP required forced termination')
        finally:
            self.process.stdout.close()
            self.log.close()
        if self.process.returncode != 0:
            raise RuntimeError(f'MCP exit {self.process.returncode}')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=Path('target/release/ctx'))
    parser.add_argument('--repo', type=Path, required=True)
    parser.add_argument('--reference', type=Path, default=Path('benchmarks/results/search-speed-fastapi-fixed-release.json'))
    parser.add_argument('--protocol', default='2025-06-18', choices=['2025-03-26', '2025-06-18', '2025-11-25'])
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    reference = json.loads(args.reference.read_text())['corpora'][1]
    provenance = reference['provenance']
    with tempfile.TemporaryDirectory(prefix='ctx-mcp-speed-') as directory:
        root = Path(directory)
        for name in provenance['files']:
            target = root / name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(subprocess.check_output(['git', 'show', provenance['revision']+':'+name], cwd=args.repo))
        run(binary, root, 'index', '.')
        client = Client(binary, root)
        try:
            initialization_ms, _ = client.call('initialize', dict(protocolVersion=args.protocol, capabilities={}, clientInfo=dict(name='ctx-benchmark', version='1')))
            client.send(dict(jsonrpc='2.0', method='notifications/initialized'))
            _, tools = client.call('tools/list', {})
            assert 'ctx_search' in [tool['name'] for tool in tools['tools']]
            ready_deadline = time.monotonic() + 20
            while True:
                try:
                    state = json.loads((root/'.ctx/watch-state.json').read_text())
                except (OSError, ValueError):
                    state = {}
                if state.get('pid') == client.process.pid and state.get('watcher') == 'active' and state.get('catching_up') is False:
                    break
                assert time.monotonic() < ready_deadline, 'Watcher startup did not converge'
                time.sleep(.025)
            rows = []
            for scenario in reference['scenarios']:
                arguments = dict(query=scenario['query'], mode=scenario['mode'], ignore_case=scenario['ignore_case'], limit=10000000, budget_tokens=1000000000)
                cli_args = ['search', arguments['query'], '--mode', arguments['mode'], '--json', '--limit', '10000000', '--budget-tokens', '1000000000']
                if arguments['ignore_case']:
                    cli_args.append('--ignore-case')
                samples = {'cli': [], 'mcp': []}
                expected = None
                first_mcp_ms = None
                wire_bytes = []
                for iteration in range(12):
                    for transport in (['cli', 'mcp'] if iteration % 2 == 0 else ['mcp', 'cli']):
                        if transport == 'cli':
                            started = time.perf_counter()
                            _, body = run(binary, root, *cli_args)
                            envelope = json.loads(body)
                            elapsed = (time.perf_counter()-started)*1000
                        else:
                            elapsed, envelope = client.search(arguments)
                            if first_mcp_ms is None:
                                first_mcp_ms = elapsed
                        evidence = {k: envelope[k] for k in ('hits', 'tokens', 'coverage', 'hint')}
                        if expected is None:
                            expected = evidence
                        assert evidence == expected, (scenario['name'], transport)
                        assert len(evidence['hits']) == scenario['hits'] and evidence['hint'] is None
                        if iteration >= 2:
                            samples[transport].append(elapsed)
                            if transport == 'mcp':
                                wire_bytes.append(client.last_response_bytes)
                cli, mcp = stats(samples['cli']), stats(samples['mcp'])
                rows.append(dict(name=scenario['name'], arguments=arguments, cli=cli, mcp=mcp, first_mcp_ms=first_mcp_ms, response_bytes=wire_bytes, speedup=cli['p50_ms']/mcp['p50_ms'], parity=True, hits=len(expected['hits'])))
                print(f"{scenario['name']}: CLI {cli['p50_ms']:.2f} ms / MCP {mcp['p50_ms']:.2f} ms", flush=True)
            # Verify a session with a populated reader cache sees watcher publications.
            probe = root / 'ctx_mcp_probe.py'
            live = []
            for content in ['mcp_unique_alpha = 1\n', 'mcp_unique_beta = 2\n', None]:
                started = time.perf_counter()
                if content is None:
                    probe.unlink()
                else:
                    probe.write_text(content)
                deadline = time.monotonic()+20
                while True:
                    with sqlite3.connect(root/'.ctx/index.sqlite') as db:
                        row = db.execute('SELECT c.content FROM file_contents c JOIN files f ON f.id=c.file_id WHERE f.path=?', (probe.name,)).fetchone()
                    if (row[0] if row else None) == content:
                        break
                    assert time.monotonic() < deadline, 'Watcher did not converge'
                    time.sleep(.025)
                published_ms = (time.perf_counter()-started)*1000
                for word in ['mcp_unique_alpha', 'mcp_unique_beta']:
                    _, envelope = client.search(dict(query=word, mode='literal'))
                    assert bool(envelope['hits']) == (content is not None and word in content)
                live.append(dict(content=content, observed_publication_ms=published_ms))
        finally:
            client.close()
        result = dict(schema=1, binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(), provenance=provenance, initialization_ms=initialization_ms, iterations=10, warmups=2, protocol=args.protocol, scenarios=rows, watcher=live, graceful_shutdown=True,
                      methodology='One persistent full MCP stdio server with watcher; alternating fresh indexed CLI and MCP requests on identical committed Python sources. Startup reconciliation completed before samples. Warm OS cache, unlimited evidence budgets, no LLM. Both times include transport, response receipt and Python JSON decoding. Ratios combine avoided process startup and session reader reuse, not isolated cache savings. First per-scenario call is not necessarily a cold reader. Watcher observations include 25 ms polling granularity. No performance threshold asserted.')
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(result, indent=2)+'\n')


if __name__ == '__main__':
    main()
