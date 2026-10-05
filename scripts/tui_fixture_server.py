"""Bounded, local-only Responses/Messages streaming fixture; no upstream calls."""

from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import threading
import time
from urllib.parse import urlsplit

PARTIAL = 'COMPAT_BEGIN\nStreaming terminal update: cafe.\n'
FINAL = 'COMPAT_END\n'


def has_keyboard_prompt(body, path):
    """Observe the user's exact edited text, excluding instructions/tool data."""
    messages = body.get('input' if path.endswith('/responses') else 'messages', [])
    if not isinstance(messages, list):
        return False
    for message in messages:
        if not isinstance(message, dict) or message.get('role') != 'user':
            continue
        content = message.get('content')
        if content == 'compat-input-42':
            return True
        if isinstance(content, list):
            if any(not isinstance(part, dict) or part.get('type') not in ('text', 'input_text')
                   or not isinstance(part.get('text'), str) for part in content):
                continue
            # Claude 2.1.261 prepends a separate date/context reminder to the
            # user message. Ignore only whole reminder blocks; the typed block
            # must still match exactly once, without trimming or extra text.
            text = [part['text'] for part in content
                    if not (part['text'].strip().startswith('<system-reminder>')
                            and part['text'].strip().endswith('</system-reminder>'))]
            if text == ['compat-input-42']:
                return True
    return False


class Fixture:
    def __init__(self, directory):
        self.directory = Path(directory)
        self.release = threading.Event()
        self.requested = threading.Event()
        self.partial = threading.Event()
        self.completed = threading.Event()
        self.errors = []
        self.requests = []
        self.lock = threading.RLock()
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            def setup(self):
                super().setup()
                self.connection.settimeout(5)

            def log_message(self, *args):
                pass

            def json(self, value, code=200):
                data = json.dumps(value).encode()
                self.send_response(code)
                self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            def do_GET(self):
                # Some clients discover the locally configured model before the
                # first turn. This endpoint cannot proxy or resolve other hosts.
                if urlsplit(self.path).path in ('/v1/models', '/models'):
                    self.json({'object': 'list', 'data': [{'id': 'compat-fixture', 'object': 'model', 'owned_by': 'local-fixture'}]})
                else:
                    self.json({'error': {'message': 'unsupported local fixture route'}}, 404)

            def do_POST(self):
                try:
                    try:
                        length = int(self.headers.get('Content-Length', '0'))
                    except ValueError:
                        self.json({'error': {'message': 'invalid content length'}}, 400)
                        return
                    if not 0 < length <= 2 * 1024 * 1024:
                        self.json({'error': {'message': 'request size outside fixture limit'}}, 413)
                        return
                    try:
                        body = json.loads(self.rfile.read(length))
                    except (ValueError, UnicodeDecodeError):
                        self.json({'error': {'message': 'invalid JSON request'}}, 400)
                        return
                    if not isinstance(body, dict):
                        self.json({'error': {'message': 'request must be an object'}}, 400)
                        return
                    credentials = (self.headers.get('x-api-key'), self.headers.get('Authorization'))
                    allowed = (None, 'compat-fixture-no-real-credentials', 'Bearer compat-fixture-no-real-credentials')
                    if any(value not in allowed for value in credentials):
                        self.json({'error': {'message': 'only fixture credentials are accepted'}}, 403)
                        return
                    path = urlsplit(self.path).path
                    if path in ('/v1/messages/count_tokens', '/messages/count_tokens'):
                        self.json({'input_tokens': 16})
                        return
                    if path not in ('/v1/responses', '/responses', '/v1/messages', '/messages'):
                        self.json({'error': {'message': 'unsupported local fixture route'}}, 404)
                        return
                    if (not isinstance(body.get('model'), str) or len(body['model']) > 128
                            or type(body.get('stream')) is not bool):
                        self.json({'error': {'message': 'model and stream have invalid types or size'}}, 400)
                        return
                    # Retain only deterministic observations, never headers or
                    # system prompts which may contain machine-specific paths.
                    request = {'path': path, 'model': body.get('model'), 'stream': body.get('stream'),
                               'prompt_received': has_keyboard_prompt(body, path), 'request_bytes': length}
                    with fixture.lock:
                        fixture.requests.append(request)
                        fixture.save()
                    fixture.requested.set()
                    if not body.get('stream'):
                        self.json({'error': {'message': 'fixture requires actual streaming'}}, 400)
                        return
                    self.send_response(200)
                    self.send_header('Content-Type', 'text/event-stream')
                    self.send_header('Cache-Control', 'no-cache')
                    self.end_headers()
                    if path.endswith('/responses'):
                        self.emit_responses()
                    else:
                        self.messages()
                    fixture.completed.set()
                except (BrokenPipeError, ConnectionResetError):
                    with fixture.lock:
                        fixture.errors.append('client disconnected before fixture stream completed')
                        fixture.save()
                except Exception as error:
                    with fixture.lock:
                        fixture.errors.append(f'{type(error).__name__}: {error}')
                        fixture.save()

            def event(self, name, value):
                self.wfile.write(('event: ' + name + '\ndata: ' + json.dumps(value) + '\n\n').encode())
                self.wfile.flush()

            def pause(self):
                fixture.partial.set()
                if not fixture.release.wait(45):
                    raise TimeoutError('controller did not release the partial stream')

            def messages(self):
                message = {'id': 'msg_compat', 'type': 'message', 'role': 'assistant', 'model': 'compat-fixture',
                           'content': [], 'stop_reason': None, 'stop_sequence': None,
                           'usage': {'input_tokens': 16, 'output_tokens': 0}}
                self.event('message_start', {'type': 'message_start', 'message': message})
                self.event('content_block_start', {'type': 'content_block_start', 'index': 0,
                           'content_block': {'type': 'text', 'text': ''}})
                for text in (PARTIAL, FINAL):
                    self.event('content_block_delta', {'type': 'content_block_delta', 'index': 0,
                               'delta': {'type': 'text_delta', 'text': text}})
                    if text == PARTIAL:
                        self.pause()
                self.event('content_block_stop', {'type': 'content_block_stop', 'index': 0})
                self.event('message_delta', {'type': 'message_delta', 'delta': {'stop_reason': 'end_turn', 'stop_sequence': None},
                           'usage': {'output_tokens': 16}})
                self.event('message_stop', {'type': 'message_stop'})

            def emit_responses(self):
                response = {'id': 'resp_compat', 'object': 'response', 'created_at': 1, 'model': 'compat-fixture',
                            'status': 'in_progress', 'output': []}
                item = {'id': 'msg_compat', 'type': 'message', 'role': 'assistant', 'status': 'in_progress', 'content': []}
                self.event('response.created', {'type': 'response.created', 'response': response})
                self.event('response.output_item.added', {'type': 'response.output_item.added', 'output_index': 0, 'item': item})
                part = {'type': 'output_text', 'text': '', 'annotations': []}
                self.event('response.content_part.added', {'type': 'response.content_part.added', 'item_id': 'msg_compat',
                           'output_index': 0, 'content_index': 0, 'part': part})
                for text in (PARTIAL, FINAL):
                    self.event('response.output_text.delta', {'type': 'response.output_text.delta', 'item_id': 'msg_compat',
                               'output_index': 0, 'content_index': 0, 'delta': text})
                    if text == PARTIAL:
                        self.pause()
                part['text'] = PARTIAL + FINAL
                self.event('response.output_text.done', {'type': 'response.output_text.done', 'item_id': 'msg_compat',
                           'output_index': 0, 'content_index': 0, 'text': part['text']})
                self.event('response.content_part.done', {'type': 'response.content_part.done', 'item_id': 'msg_compat',
                           'output_index': 0, 'content_index': 0, 'part': part})
                item.update(status='completed', content=[part])
                self.event('response.output_item.done', {'type': 'response.output_item.done', 'output_index': 0, 'item': item})
                response.update(status='completed', output=[item], usage={'input_tokens': 16, 'output_tokens': 16, 'total_tokens': 32})
                self.event('response.completed', {'type': 'response.completed', 'response': response})

        self.server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self.server.daemon_threads = False
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)

    @property
    def url(self):
        return f'http://127.0.0.1:{self.server.server_port}'

    def save(self):
        with self.lock:
            self._save_locked()

    def _save_locked(self):
        self.directory.mkdir(parents=True, exist_ok=True)
        path = self.directory / 'endpoint.json.pending'
        path.write_text(json.dumps({'scope': 'real CLI talking only to deterministic localhost fixture; no model service',
                                    'requests': self.requests, 'errors': self.errors}, indent=2) + '\n')
        path.replace(self.directory / 'endpoint.json')

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *args):
        self.release.set()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)
        self.save()


def cli_invocation(client, directory, base_url):
    """Use fresh client state and a small environment, never inherited credentials."""
    import os
    import shutil
    directory = Path(directory).resolve()
    state = directory / 'client-state'
    state.mkdir(parents=True, exist_ok=True)
    executable = shutil.which(client)
    if executable is None:
        raise FileNotFoundError(f'{client} is not installed')
    environment = {name: os.environ[name] for name in ('PATH', 'DISPLAY', 'XAUTHORITY', 'LANG', 'LC_ALL') if name in os.environ}
    environment.update(TERM='xterm-256color', COLORTERM='truecolor',
                       HTTP_PROXY='http://127.0.0.1:1', HTTPS_PROXY='http://127.0.0.1:1',
                       ALL_PROXY='http://127.0.0.1:1', NO_PROXY='127.0.0.1,localhost',
                       http_proxy='http://127.0.0.1:1', https_proxy='http://127.0.0.1:1',
                       all_proxy='http://127.0.0.1:1', no_proxy='127.0.0.1,localhost')
    if client == 'codex':
        # This is the client's documented state-directory setting, scoped only
        # to the child. No user config, authentication file, or keychain is used.
        environment['CODEX_HOME'] = str(state)
        (state / 'config.toml').write_text('[projects.' + json.dumps(str(directory)) + ']\ntrust_level = "trusted"\n')
        args = [executable, '--strict-config', '-s', 'read-only', '-a', 'never',
                '--disable', 'plugins', '--disable', 'apps', '--disable', 'remote_plugin', '--disable', 'plugin_sharing']
        overrides = {
            'analytics.enabled': 'false', 'check_for_update_on_startup': 'false',
            'cli_auth_credentials_store': '"file"', 'model_provider': '"compat_fixture"',
            'model': '"compat-fixture"', 'model_providers.compat_fixture.name': '"Local compatibility fixture"',
            'model_providers.compat_fixture.base_url': json.dumps(base_url + '/v1'),
            'model_providers.compat_fixture.wire_api': '"responses"',
            'model_providers.compat_fixture.requires_openai_auth': 'false',
            'model_reasoning_effort': '"low"',
        }
        for key, value in overrides.items():
            args.extend(['-c', key + '=' + value])
    elif client == 'claude':
        # Fresh disposable state, verified against native Claude 2.1.261. Trust
        # applies only to this generated directory, and the sole approved key
        # suffix belongs to the dummy value below. No user config is read and
        # no global permission bypass is enabled. This smoke tests the real
        # TUI after onboarding, not the onboarding dialog's timing guard.
        (state / '.claude.json').write_text(json.dumps({
            'hasCompletedOnboarding': True, 'lastOnboardingVersion': '2.1.261',
            'customApiKeyResponses': {'approved': ['-no-real-credentials'], 'rejected': []},
            'projects': {str(directory): {'hasTrustDialogAccepted': True}},
        }))
        (state / 'settings.json').write_text(json.dumps({'theme': 'dark'}))
        environment.update(CLAUDE_CONFIG_DIR=str(state), ANTHROPIC_BASE_URL=base_url,
                           ANTHROPIC_API_KEY='compat-fixture-no-real-credentials',
                           DISABLE_AUTOUPDATER='1', DISABLE_TELEMETRY='1', DISABLE_ERROR_REPORTING='1')
        args = [executable, '--bare', '--strict-mcp-config', '--mcp-config', '{"mcpServers":{}}',
                '--tools', '', '--model', 'compat-fixture', '--permission-mode', 'plan', '--name', 'Compatibility fixture']
    else:
        raise ValueError('client must be claude or codex')
    return args, environment
