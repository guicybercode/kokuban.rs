"""Exercise the local provider over HTTP, including its streaming barrier."""

import http.client
import json
from pathlib import Path
import queue
import tempfile
import threading
import unittest

from tui_fixture_server import FINAL, PARTIAL, Fixture


REQUEST_LIMIT = 2 * 1024 * 1024


def read_events(response, received):
    """Read flushed SSE events independently of the controller releasing them."""
    name, data = None, []
    try:
        for raw in iter(response.readline, b''):
            line = raw.decode('utf-8').rstrip('\r\n')
            if not line:
                if data:
                    received.put(('event', name, json.loads('\n'.join(data))))
                name, data = None, []
            elif line.startswith('event: '):
                name = line.removeprefix('event: ')
            elif line.startswith('data: '):
                data.append(line.removeprefix('data: '))
        if data:
            raise AssertionError('stream ended without the SSE event delimiter')
    except Exception as error:
        received.put(('error', error))
    finally:
        received.put(('end',))


class FixtureHTTPTests(unittest.TestCase):
    def connection(self, fixture):
        return http.client.HTTPConnection(*fixture.server.server_address, timeout=3)

    def next_event(self, received):
        item = received.get(timeout=3)
        if item[0] == 'error':
            raise item[1]
        return item

    def exercise_stream(self, protocol):
        prompt = 'compat-input-42'
        body = {'model': 'compat-fixture', 'stream': True}
        if protocol == 'messages':
            body.update(max_tokens=64, system='PRIVATE_SYSTEM_DO_NOT_PERSIST',
                        messages=[{'role': 'user', 'content': prompt}])
        else:
            body.update(instructions='PRIVATE_SYSTEM_DO_NOT_PERSIST',
                        input=[{'role': 'user', 'content': prompt}])
        encoded = json.dumps(body).encode()
        events = []
        with tempfile.TemporaryDirectory(prefix='kokuban-provider-test-') as temporary:
            with Fixture(temporary) as fixture:
                self.assertEqual(fixture.server.server_address[0], '127.0.0.1')
                connection = self.connection(fixture)
                received = queue.Queue()
                reader = None
                try:
                    connection.request('POST', '/v1/' + protocol + '?private=PRIVATE_QUERY_DO_NOT_PERSIST',
                                       body=encoded, headers={'Content-Type': 'application/json',
                                                             'Authorization': 'Bearer compat-fixture-no-real-credentials',
                                                             'X-Test-Private': 'SYNTHETIC_HEADER_SENTINEL'})
                    response = connection.getresponse()
                    self.assertEqual(response.status, 200)
                    self.assertEqual(response.getheader('Content-Type'), 'text/event-stream')
                    reader = threading.Thread(target=read_events, args=(response, received), daemon=True)
                    reader.start()
                    partial_type = ('content_block_delta' if protocol == 'messages'
                                    else 'response.output_text.delta')
                    while True:
                        item = self.next_event(received)
                        self.assertEqual(item[0], 'event', 'stream ended before partial text arrived')
                        _, name, value = item
                        events.append((name, value))
                        if name == partial_type:
                            delta = value['delta']['text'] if protocol == 'messages' else value['delta']
                            self.assertEqual(delta, PARTIAL)
                            break
                    self.assertTrue(fixture.partial.wait(timeout=3))
                    self.assertFalse(fixture.release.is_set())
                    self.assertFalse(fixture.completed.is_set())
                    self.assertNotIn(FINAL.rstrip('\n'), json.dumps(events))
                    # This bounded observation starts only after a real client
                    # consumed the partial event; no fixed startup sleep is used.
                    with self.assertRaises(queue.Empty, msg='server advanced before controller release'):
                        received.get(timeout=0.1)
                    fixture.release.set()
                    while True:
                        item = self.next_event(received)
                        if item[0] == 'end':
                            break
                        events.append((item[1], item[2]))
                    self.assertTrue(fixture.completed.wait(timeout=3))
                    self.assertEqual(fixture.errors, [])
                finally:
                    fixture.release.set()
                    connection.close()
                    if reader is not None:
                        reader.join(timeout=3)
                        self.assertFalse(reader.is_alive(), 'HTTP stream reader did not terminate')
            self.assertFalse(fixture.thread.is_alive(), 'fixture accept thread did not terminate')
            raw = (Path(temporary) / 'endpoint.json').read_text()
            for private in ('compat-input-42', 'PRIVATE_SYSTEM', 'PRIVATE_QUERY', 'SYNTHETIC_HEADER_SENTINEL'):
                self.assertNotIn(private, raw)
            evidence = json.loads(raw)
            self.assertEqual(evidence['requests'], [{'path': '/v1/' + protocol, 'model': 'compat-fixture',
                                                    'stream': True, 'prompt_received': True,
                                                    'request_bytes': len(encoded)}])
            self.assertEqual(evidence['errors'], [])
            self.assertFalse((Path(temporary) / 'endpoint.json.pending').exists())
        for name, value in events:
            self.assertEqual(value['type'], name)
        return events

    def test_messages_delivers_partial_before_release_and_stop_after_release(self):
        events = self.exercise_stream('messages')
        self.assertEqual(events[0][0], 'message_start')
        self.assertEqual(events[-1][0], 'message_stop')
        deltas = [value['delta']['text'] for name, value in events if name == 'content_block_delta']
        self.assertEqual(deltas, [PARTIAL, FINAL])
        stops = [value for name, value in events if name == 'message_delta']
        self.assertEqual(len(stops), 1)
        self.assertEqual(stops[0]['delta']['stop_reason'], 'end_turn')

    def test_responses_delivers_partial_before_release_and_completed_output_after_release(self):
        events = self.exercise_stream('responses')
        self.assertEqual(events[0][0], 'response.created')
        self.assertEqual(events[0][1]['response']['status'], 'in_progress')
        deltas = [value['delta'] for name, value in events if name == 'response.output_text.delta']
        self.assertEqual(deltas, [PARTIAL, FINAL])
        self.assertEqual(events[-1][0], 'response.completed')
        response = events[-1][1]['response']
        self.assertEqual(response['status'], 'completed')
        self.assertEqual(response['output'][0]['status'], 'completed')
        self.assertEqual(response['output'][0]['content'][0]['text'], ''.join(deltas))
        self.assertEqual(response['usage']['total_tokens'],
                         response['usage']['input_tokens'] + response['usage']['output_tokens'])

    def test_empty_negative_and_oversized_requests_are_rejected_before_reading_body(self):
        with tempfile.TemporaryDirectory(prefix='kokuban-provider-test-') as temporary:
            with Fixture(temporary) as fixture:
                for length in (0, -1, REQUEST_LIMIT + 1):
                    with self.subTest(content_length=length):
                        connection = self.connection(fixture)
                        try:
                            # Deliberately send no oversized payload: rejection
                            # must happen from the declared bound, before read().
                            connection.request('POST', '/v1/responses', body=b'',
                                               headers={'Content-Length': str(length)})
                            response = connection.getresponse()
                            self.assertEqual(response.status, 413)
                            self.assertIn('error', json.loads(response.read()))
                        finally:
                            connection.close()
                self.assertFalse(fixture.requested.is_set())
                self.assertEqual(fixture.requests, [])
                self.assertEqual(fixture.errors, [])

    def test_prompt_marker_must_be_exact_text_in_a_user_message(self):
        marker = 'compat-input-42'
        with tempfile.TemporaryDirectory(prefix='kokuban-provider-test-') as temporary:
            with Fixture(temporary) as fixture:
                for protocol, field in (('responses', 'input'), ('messages', 'messages')):
                    cases = [
                        ('system only', {'system': marker}, False),
                        ('instructions only', {'instructions': marker}, False),
                        ('substring', {field: [{'role': 'user', 'content': 'prefix ' + marker + ' suffix'}]}, False),
                        ('assistant only', {field: [{'role': 'assistant', 'content': marker}]}, False),
                    ]
                    cases.extend((kind, {field: [{'role': 'user', 'content': [{'type': kind, 'text': marker}]}]}, True)
                                 for kind in ('text', 'input_text'))
                    for label, content, expected in cases:
                        with self.subTest(protocol=protocol, case=label):
                            body = {'model': 'compat-fixture', 'stream': False,
                                    field: [{'role': 'user', 'content': 'unrelated user text'}]}
                            body.update(content)
                            previous_requests = len(fixture.requests)
                            connection = self.connection(fixture)
                            try:
                                connection.request('POST', '/v1/' + protocol, body=json.dumps(body).encode())
                                response = connection.getresponse()
                                self.assertEqual(response.status, 400)
                                self.assertIn('streaming', json.loads(response.read())['error']['message'])
                                self.assertEqual(len(fixture.requests), previous_requests + 1)
                                self.assertIs(fixture.requests[-1]['prompt_received'], expected)
                            finally:
                                connection.close()
                self.assertEqual(fixture.errors, [])

    def test_limit_sized_json_is_accepted_but_non_streaming_is_rejected(self):
        prefix = b'{"model":"compat-fixture","stream":false,"padding":"'
        suffix = b'"}'
        body = prefix + b'x' * (REQUEST_LIMIT - len(prefix) - len(suffix)) + suffix
        with tempfile.TemporaryDirectory(prefix='kokuban-provider-test-') as temporary:
            with Fixture(temporary) as fixture:
                connection = self.connection(fixture)
                try:
                    connection.request('POST', '/v1/responses', body=body,
                                       headers={'Content-Type': 'application/json'})
                    response = connection.getresponse()
                    self.assertEqual(response.status, 400)
                    self.assertIn('streaming', json.loads(response.read())['error']['message'])
                    self.assertTrue(fixture.requested.is_set())
                    self.assertEqual(fixture.requests[0]['request_bytes'], REQUEST_LIMIT)
                    self.assertFalse(fixture.requests[0]['prompt_received'])
                    self.assertFalse(fixture.partial.is_set())
                    self.assertFalse(fixture.completed.is_set())
                    self.assertEqual(fixture.errors, [])
                finally:
                    connection.close()
            self.assertLess((Path(temporary) / 'endpoint.json').stat().st_size, 1024)

    def test_unsupported_route_does_not_count_as_a_model_request(self):
        with tempfile.TemporaryDirectory(prefix='kokuban-provider-test-') as temporary:
            with Fixture(temporary) as fixture:
                connection = self.connection(fixture)
                try:
                    connection.request('POST', '/unsupported/responses', body=b'{}')
                    response = connection.getresponse()
                    self.assertEqual(response.status, 404)
                    self.assertIn('error', json.loads(response.read()))
                    self.assertEqual(fixture.requests, [])
                    self.assertFalse(fixture.requested.is_set())
                finally:
                    connection.close()

    def test_invalid_json_and_metadata_return_400_without_retaining_payloads(self):
        invalid = [b'{', b'\xff', b'null', b'[]', b'"SYNTHETIC_BODY_SENTINEL"']
        invalid.extend(json.dumps({'model': value, 'stream': True}).encode()
                       for value in (None, [], {'private': 'SYNTHETIC_MODEL_SENTINEL'}, 'x' * 129))
        invalid.extend(json.dumps({'model': 'compat-fixture', 'stream': value}).encode()
                       for value in (None, 0, 1, 'true', {}, []))
        with tempfile.TemporaryDirectory(prefix='kokuban-provider-test-') as temporary:
            with Fixture(temporary) as fixture:
                for body in invalid:
                    with self.subTest(body=body):
                        connection = self.connection(fixture)
                        try:
                            connection.request('POST', '/v1/messages', body=body)
                            response = connection.getresponse()
                            self.assertEqual(response.status, 400)
                            self.assertIn('error', json.loads(response.read()))
                        finally:
                            connection.close()
                self.assertEqual(fixture.requests, [])
                self.assertEqual(fixture.errors, [])
                self.assertFalse(fixture.requested.is_set())
            self.assertNotIn('SYNTHETIC_', (Path(temporary) / 'endpoint.json').read_text())

    def test_invalid_content_length_returns_400_without_echoing_header(self):
        with tempfile.TemporaryDirectory(prefix='kokuban-provider-test-') as temporary:
            with Fixture(temporary) as fixture:
                connection = self.connection(fixture)
                try:
                    connection.request('POST', '/v1/responses', body=b'',
                                       headers={'Content-Length': 'SYNTHETIC_LENGTH_SENTINEL'})
                    response = connection.getresponse()
                    self.assertEqual(response.status, 400)
                    self.assertNotIn(b'SYNTHETIC_LENGTH_SENTINEL', response.read())
                    self.assertEqual(fixture.requests, [])
                    self.assertEqual(fixture.errors, [])
                finally:
                    connection.close()

    def test_non_fixture_credentials_are_rejected_and_never_persisted(self):
        with tempfile.TemporaryDirectory(prefix='kokuban-provider-test-') as temporary:
            with Fixture(temporary) as fixture:
                for header in ('Authorization', 'x-api-key'):
                    with self.subTest(header=header):
                        connection = self.connection(fixture)
                        try:
                            connection.request('POST', '/v1/messages', body=b'{}',
                                               headers={header: 'SYNTHETIC_TOKEN_SENTINEL'})
                            response = connection.getresponse()
                            self.assertEqual(response.status, 403)
                            self.assertNotIn(b'SYNTHETIC_TOKEN_SENTINEL', response.read())
                        finally:
                            connection.close()
                self.assertEqual(fixture.requests, [])
                self.assertEqual(fixture.errors, [])
            self.assertNotIn('SYNTHETIC_TOKEN_SENTINEL', (Path(temporary) / 'endpoint.json').read_text())

    def test_context_exit_releases_pending_stream_and_waits_for_completion(self):
        with tempfile.TemporaryDirectory(prefix='kokuban-provider-test-') as temporary:
            connection = None
            try:
                with Fixture(temporary) as fixture:
                    connection = self.connection(fixture)
                    connection.request('POST', '/v1/messages',
                                       body=b'{"model":"compat-fixture","stream":true}')
                    response = connection.getresponse()
                    self.assertEqual(response.status, 200)
                    self.assertTrue(fixture.partial.wait(timeout=3))
                    self.assertFalse(fixture.completed.is_set())
                    self.assertFalse(fixture.release.is_set())
                self.assertTrue(fixture.completed.is_set())
                self.assertFalse(fixture.thread.is_alive())
                self.assertIn(b'event: message_stop\n', response.read())
                self.assertEqual(json.loads((Path(temporary) / 'endpoint.json').read_text())['errors'], [])
            finally:
                if connection is not None:
                    connection.close()


if __name__ == '__main__':
    unittest.main()
