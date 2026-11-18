#!/usr/bin/env python3
"""Select the GPU1 LlamaFarm model and synchronize OpenCode/Kilo on this Mac."""

import json
import os
import pathlib
import base64
import re
import sqlite3
import stat
import subprocess
import sys
import time
from urllib.error import HTTPError
from urllib.request import Request, urlopen

HOME = pathlib.Path.home()
OPEN = HOME / '.config/opencode/opencode.json'
KILO = HOME / '.config/kilo/kilo.jsonc'
STATE = HOME / '.config/opencode/gpu1-state.json'
BACKUPS = HOME / '.config/opencode/backups'
REMOTE_SCRIPT = '/home/bat/LlamaFarm-dual-v100-deploy/scripts/kilo/gpu1-profile.sh'
SSH = ['ssh', '-i', str(HOME / '.ssh/id_ed25519_popos1'), '-o',
       'IdentitiesOnly=yes', '-o', 'BatchMode=yes', 'bat@100.107.226.49']

# model, Ollama slots, context, OpenCode response ceiling, actual weight quant
PROFILES = {
    'A': ('qwen3.8-unqwen:27b-q8-64k', 1, 65536, 8192, 'Q8_0'),
    'q8-64k': ('qwen3.8-unqwen:27b-q8-64k', 1, 65536, 8192, 'Q8_0'),
    'q8-32k': ('qwen3.8-unqwen:27b-q8-32k', 1, 32768, 8192, 'Q8_0'),
    'q4-256k': ('qwen3.8-unqwen:27b-256k', 1, 262144, 8192, 'Q4_K_M'),
    'B': ('qwen3.5-rana:9b-32k', 2, 32768, 4096, 'Q8_0'),
    'rana2': ('qwen3.5-rana:9b-32k', 2, 32768, 4096, 'Q8_0'),
    'C': ('qwen3.5-rana:9b-32k', 3, 32768, 4096, 'Q8_0'),
    'rana3': ('qwen3.5-rana:9b-32k', 3, 32768, 4096, 'Q8_0'),
    'D': ('qwen3.5-rana:9b-32k', 1, 32768, 4096, 'Q8_0'),
    'rana1': ('qwen3.5-rana:9b-32k', 1, 32768, 4096, 'Q8_0'),
    'D256': ('qwen3.5-rana:9b-256k', 1, 262144, 32768, 'Q8_0'),
    'rana256': ('qwen3.5-rana:9b-256k', 1, 262144, 32768, 'Q8_0'),
}


def write_json(path, data):
    temp = path.with_suffix(path.suffix + '.tmp')
    temp.write_text(json.dumps(data, indent=2) + '\n')
    os.chmod(temp, stat.S_IMODE(path.stat().st_mode))
    os.replace(temp, path)


def active_opencode_sessions():
    # The v2 service is authoritative for live turns. A completed background
    # child can retain a stale time_busy value in SQLite after it goes idle.
    live = subprocess.run(
        ['opencode', 'api', 'GET', '/api/session/active'],
        capture_output=True, text=True, check=False,
    )
    if live.returncode == 0:
        try:
            data = json.loads(live.stdout).get('data', {})
            # GPU0 primary turns can continue while the independent GPU1
            # service changes profile. Only an active GPU1 child blocks it.
            db = HOME / '.local/share/opencode/opencode.db'
            if not data:
                return []
            with sqlite3.connect(db) as connection:
                return [(session_id, status.get('type', 'running'))
                        for session_id, status in data.items()
                        if (connection.execute('SELECT agent FROM session_v2 WHERE id = ?',
                                               (session_id,)).fetchone() or ('unknown',))[0]
                        != 'build']
        except (ValueError, AttributeError):
            pass

    db = HOME / '.local/share/opencode/opencode.db'
    if not db.exists():
        return []
    with sqlite3.connect(db) as connection:
        busy = connection.execute(
            "SELECT id, agent FROM session_v2 WHERE agent IN ('build','gpu1-specialist') "
            "AND time_busy IS NOT NULL AND (time_idle IS NULL OR time_busy > time_idle) "
            "AND time_updated > ?", (int(time.time() * 1000) - 3600_000,)
        ).fetchall()
        if busy:
            return busy
        # Compaction can update time_idle while the model is still generating.
        # A recent agent message together with an established Ollama connection
        # is a stronger live-work signal than the session timestamps alone.
        recent = connection.execute(
            "SELECT s.id, s.agent FROM session_v2 s "
            "JOIN session_message m ON m.session_id = s.id "
            "WHERE s.agent IN ('build','gpu1-specialist') "
            "AND m.time_updated > ? GROUP BY s.id, s.agent",
            (int(time.time() * 1000) - 300_000,),
        ).fetchall()
    if not recent:
        return []
    for port in (11434, 11436):
        socket = subprocess.run(
            ['lsof', '-nP', f'-iTCP@100.107.226.49:{port}',
             '-sTCP:ESTABLISHED'], capture_output=True, text=True, check=False,
        )
        if any(line.lstrip().startswith('opencode ') for line in socket.stdout.splitlines()):
            return recent
    return []


def reload_kilo_if_idle():
    """Reload the installed Kilo extension server; 409 means work is active."""
    processes = subprocess.run(
        ['/bin/ps', '-axo', 'pid=,command='], capture_output=True,
        text=True, check=True,
    ).stdout
    for line in processes.splitlines():
        if not re.search(r'/bin/kilo serve --port(?: 0)?(?:\s|$)', line):
            continue
        pid = line.strip().split(None, 1)[0]
        environment = subprocess.run(
            ['/bin/ps', 'eww', '-p', pid], capture_output=True,
            text=True, check=True,
        ).stdout
        match = re.search(r'(?:^| )KILO_SERVER_PASSWORD=([^ ]+)', environment)
        if not match:
            raise SystemExit('Cannot safely reload the active Kilo server')
        listeners = subprocess.run(
            ['/usr/sbin/lsof', '-nP', '-a', '-p', pid, '-iTCP',
             '-sTCP:LISTEN', '-F', 'n'], capture_output=True,
            text=True, check=True,
        ).stdout
        ports = re.findall(r'^n127\.0\.0\.1:(\d+)$', listeners, re.MULTILINE)
        if len(ports) != 1:
            raise SystemExit('Cannot identify the active Kilo server port')
        token = base64.b64encode(f'kilo:{match.group(1)}'.encode()).decode()
        request = Request(
            f'http://127.0.0.1:{ports[0]}/instance/reload', data=b'{}',
            headers={'Authorization': f'Basic {token}',
                     'Content-Type': 'application/json'}, method='POST',
        )
        try:
            with urlopen(request, timeout=10) as response:
                if response.status != 200 or json.load(response) is not True:
                    raise SystemExit('Kilo did not confirm its configuration reload')
        except HTTPError as error:
            if error.code == 409:
                raise SystemExit('Kilo has an active task; finish it before switching GPU1') from None
            raise


def main():
    if len(sys.argv) != 2 or sys.argv[1] not in (*PROFILES, 'status'):
        raise SystemExit('usage: gpu1-profile.py A|B|C|D|D256|q8-64k|q8-32k|q4-256k|rana1|rana2|rana3|rana256|status')
    profile = sys.argv[1]
    if profile == 'status':
        remote = subprocess.check_output(SSH + [f'bash {REMOTE_SCRIPT} status'], text=True)
        print(remote, end='')
        local = json.loads(OPEN.read_text())
        configured = local['providers']['gpu1']['models']['specialist']['modelID']
        print('OpenCode specialist:', configured)
        print('OpenCode child limit:', (local.get('plugins') or [{}])[0].get('options', {}).get('limit'))
        print('Kilo default:', json.loads(KILO.read_text())['model'])
        print('saved state:', STATE.read_text().strip() if STATE.exists() else 'none')
        for line in remote.splitlines():
            if line.startswith('{"models":'):
                loaded = [item['name'] for item in json.loads(line)['models']]
                if loaded and loaded != [configured]:
                    print('NOTICE: resident GPU1 model differs from configured default:', loaded)
        return

    active = active_opencode_sessions()
    if active:
        raise SystemExit(f'OpenCode work is active; finish it before switching GPU1: {active}')
    reload_kilo_if_idle()

    model, parallel, context, output, quant = PROFILES[profile]
    if model == 'qwen3.5-rana:9b-32k' and parallel > 1:
        stable = int(subprocess.check_output(SSH + [f'bash {REMOTE_SCRIPT} capacity'], text=True).strip())
        if stable < parallel:
            selected = 'B' if stable == 2 else 'D'
            print(f'Profile {profile} exceeds tested GPU1 capacity {stable}; selecting {selected}.')
            profile = selected
            model, parallel, context, output, quant = PROFILES[profile]
    open_config = json.loads(OPEN.read_text())
    kilo_config = json.loads(KILO.read_text())
    specialist = open_config['providers']['gpu1']['models']['specialist']
    specialist['modelID'] = model
    specialist['name'] = f'GPU1 {model} ({parallel} slot{"s" if parallel != 1 else ""})'
    specialist['limit'] = {'context': context, 'output': output}
    open_config['providers']['gpu1']['models'] = {'specialist': specialist}
    open_config['plugins'] = [{'package': './gpu1-limit-plugin',
                               'options': {'limit': parallel}}]
    open_config['agents']['build']['system'] = (
        'Delegate a bounded, useful independent task to gpu1-specialist only when it '
        'helps the user request. Do not fill slots automatically. Review delegated '
        'results, integrate changes, run appropriate tests, and own the final answer.'
    )

    choice = f'ollamav100gpu1/{model}'
    gpu1_provider = kilo_config['provider']['ollamav100gpu1']
    kilo_config['enabled_providers'] = ['ollamav100gpu1']
    kilo_config['provider'] = {'ollamav100gpu1': gpu1_provider}
    # Only advertise the selected profile. Picking an inactive model directly
    # would make Ollama swap weights without updating LlamaFarm's context cap.
    gpu1_provider['models'] = {
        model: {'name': f'GPU1 {model} ({quant}, {context} context)',
                'tool_call': True,
                'limit': {'context': context, 'output': output}}
    }
    kilo_config['model'] = choice
    kilo_config['small_model'] = choice
    for name in ('code', 'plan', 'debug', 'review'):
        if name in kilo_config.get('agent', {}):
            kilo_config['agent'][name]['model'] = choice

    stamp = time.strftime('%Y%m%d-%H%M%S')
    backup = BACKUPS / f'gpu1-switch-{stamp}'
    backup.mkdir(mode=0o700, parents=True, exist_ok=False)
    (backup / 'opencode.json').write_bytes(OPEN.read_bytes())
    (backup / 'kilo.jsonc').write_bytes(KILO.read_bytes())
    if STATE.exists():
        (backup / 'gpu1-state.json').write_bytes(STATE.read_bytes())
    for file in backup.iterdir():
        file.chmod(0o600)

    subprocess.run(SSH + [f'bash {REMOTE_SCRIPT} {profile}'], check=True)
    write_json(OPEN, open_config)
    write_json(KILO, kilo_config)
    write_json(STATE, {'profile': profile, 'model': model, 'ollama_parallel': parallel,
                       'context': context, 'quantization': quant})
    subprocess.run(['opencode', 'reload'], check=True)
    reload_kilo_if_idle()
    print(f'Profile {profile} selected on both clients; sessions preserved.')


if __name__ == '__main__':
    main()
