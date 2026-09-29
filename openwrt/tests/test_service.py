"""Run the actual init script with mocked OpenWrt config/procd shell APIs."""
import pathlib
import shlex
import subprocess
import unittest

SCRIPT = pathlib.Path(__file__).resolve().parents[1] / 'phantun/files/phantun.init'


def instance(kind='client', **overrides):
    result = dict(kind=kind, enabled='1', local='127.0.0.1:1234',
                  remote='example.com:4567', tun='pc1',
                  tun_local='192.168.200.1', tun_peer='192.168.200.2')
    result.update(overrides)
    return result


def run_service(sections):
    cases = []
    for name, options in sections.items():
        for key, value in options.items():
            cases.append(f'{shlex.quote(name + ":" + key)}) _mock_value={shlex.quote(value)} ;;')
    foreach = []
    for name, options in sections.items():
        foreach.append(f'[ "$2" != {shlex.quote(options["kind"])} ] || '
                       f'"$1" {shlex.quote(name)} "$3"')
    mock = '''
config_get() {
    local _mock_value="$4"
    case "$2:$3" in
    CASES
    esac
    export "$1=$_mock_value"
}
config_get_bool() { config_get "$@"; }
config_load() { :; }
config_foreach() {
    FOREACH
    return 0
}
record() { printf '%s' "$1"; shift; printf '|%s' "$@"; printf '\\n'; }
procd_open_instance() { record open "$@"; }
procd_close_instance() { record close; }
procd_set_param() { record set "$@"; }
procd_append_param() { record append "$@"; }
procd_add_reload_trigger() { record trigger "$@"; }
logger() { record log "$@"; }
'''.replace('CASES', '\n'.join(cases)).replace('FOREACH', '\n'.join(foreach))
    result = subprocess.run(['sh'], input=f'. {shlex.quote(str(SCRIPT))}\n' + mock +
                            '\nstart_service\nservice_triggers\n', text=True,
                            capture_output=True, check=True)
    return result.stdout


class ServiceTests(unittest.TestCase):
    def test_multiple_clients_and_servers(self):
        sections = {}
        for n, kind in enumerate(['client', 'client', 'server', 'server']):
            sections[f'i{n}'] = instance(kind, tun=f'pt{n}',
                                        tun_local=f'192.168.{200+n}.1',
                                        tun_peer=f'192.168.{200+n}.2',
                                        local=str(4567+n) if kind == 'server' else f'127.0.0.1:{1234+n}')
        output = run_service(sections)
        self.assertEqual(output.count('open|'), 4)
        self.assertEqual(output.count('set|command|/usr/bin/phantun_client|'), 2)
        self.assertEqual(output.count('set|command|/usr/bin/phantun_server|'), 2)
        self.assertEqual(output.count('set|respawn|3600|5|5'), 4)
        self.assertIn('trigger|phantun', output)

    def test_disabled_and_incomplete_sections(self):
        output = run_service({'off': instance(enabled='0'), 'bad': instance(remote=''),
                              'ok': instance()})
        self.assertEqual(output.count('open|'), 1)
        self.assertIn('open|ok', output)
        self.assertIn('Skipping bad', output)

    def test_duplicate_tun_or_address(self):
        for overrides in [dict(tun='pc1', tun_local='10.0.0.1', tun_peer='10.0.0.2'),
                          dict(tun='pc2'), dict(tun='pc2', tun_local='192.168.200.2')]:
            with self.subTest(overrides=overrides):
                output = run_service({'a': instance(), 'b': instance(**overrides)})
                self.assertEqual(output.count('open|'), 1)
                self.assertIn('Skipping b: duplicate', output)

    def test_ipv6_and_quoted_arguments(self):
        output = run_service({'v6': instance(ipv4_only='0', tun_local6='fcc8::1',
                                             tun_peer6='fcc8::2',
                                             handshake_packet='/etc/phantun/a file;$(false)',
                                             log_level='debug')})
        self.assertIn('append|command|--tun-local6|fcc8::1|--tun-peer6|fcc8::2', output)
        self.assertNotIn('--ipv4-only', output)
        self.assertIn('append|command|--handshake-packet|/etc/phantun/a file;$(false)', output)
        self.assertIn('set|env|RUST_LOG=debug', output)

    def test_ipv4_omits_ipv6_arguments(self):
        output = run_service({'a': instance(tun_local6='fcc8::1', tun_peer6='fcc8::2')})
        self.assertIn('append|command|--ipv4-only', output)
        self.assertNotIn('--tun-local6', output)

    def test_invalid_tun_and_missing_ipv6(self):
        for overrides in [dict(tun='long_interface_name'), dict(tun='bad/name'),
                          dict(ipv4_only='0'), dict(tun_peer='192.168.200.1')]:
            with self.subTest(overrides=overrides):
                output = run_service({'bad': instance(**overrides)})
                self.assertNotIn('open|', output)
                self.assertIn('Skipping bad', output)


if __name__ == '__main__':
    unittest.main()
