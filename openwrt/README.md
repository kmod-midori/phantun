# OpenWrt

The `phantun` package installs both binaries, `/etc/init.d/phantun`, and
`/etc/config/phantun`. It depends on `kmod-tun`, with no shared libc or libgcc
dependency. GitHub Actions reuses the statically linked musl release binaries
to build OpenWrt packages with the official [SDK action](https://github.com/openwrt/gh-action-sdk):
24.10.7 produces IPK packages and 25.12.5 produces APK packages. The matrix
covers `x86_64`, `aarch64_generic`, `arm_cortex-a15_neon-vfpv4`, `mips_24kc`,
and `mipsel_24kc`. Choose the package matching your firmware series and CPU
architecture. Other targets can be added to the workflow matrix.

Download packages from the workflow artifacts or a tagged GitHub release.
Install the IPK with `opkg install /tmp/<package>.ipk`, or the unsigned APK
with `apk add --allow-untrusted /tmp/<package>.apk`. Install only APK files
from a source you trust; no signing key is configured in this repository.

## Service configuration

The installed examples are disabled. Each `config client 'name'` or
`config server 'name'` section becomes a separate supervised procd instance.
Copy sections to run any combination of clients and servers. For example:

```uci
config client 'vpn1'
        option enabled '1'
        option local '127.0.0.1:1234'
        option remote 'vpn.example.com:4567'
        option tun 'phantunc1'
        option tun_local '192.168.200.1'
        option tun_peer '192.168.200.2'
        option ipv4_only '1'

config client 'vpn2'
        option enabled '1'
        option local '127.0.0.1:1235'
        option remote 'other.example.com:4567'
        option tun 'phantunc2'
        option tun_local '192.168.202.1'
        option tun_peer '192.168.202.2'
        option ipv4_only '1'

config server 'incoming'
        option enabled '1'
        option local '4567'
        option remote '127.0.0.1:51820'
        option tun 'phantuns1'
        option tun_local '192.168.201.1'
        option tun_peer '192.168.201.2'
        option ipv4_only '1'
```

`local`, `remote`, `tun`, `tun_local`, and `tun_peer` are required for enabled
sections. Client `local` is a UDP listening address and port; server `local`
is the incoming fake TCP port. `remote` is an address or hostname with a port
(the server's UDP destination, or the client's Phantun server).
Use `[IPv6]:port` for IPv6 endpoints.

TUN names must contain only letters, digits, `_` or `-`, and be at most 15
characters. Assign each instance a distinct address pair, on subnets that do
not overlap your existing networks. Duplicate names or exact address strings
are rejected; equivalent IPv6 spellings and overlapping subnets are not detected.
Client listening addresses/ports and incoming server ports must also be unique
where they would conflict. Invalid sections are logged and skipped so other
instances can start.

Optional settings:

| Option | Default | Meaning |
| --- | --- | --- |
| `enabled` | `0` | Start this instance |
| `ipv4_only` | `1` | Pass `--ipv4-only` |
| `tun_local6`, `tun_peer6` | none | Both required when `ipv4_only` is `0`; unique per instance |
| `handshake_packet` | none | Path to a file sent after the TCP handshake |
| `log_level` | `info` | Rust logging filter (`RUST_LOG`) |

When `ipv4_only` is enabled, IPv6 TUN options are omitted even if configured.
On servers this flag controls the TUN addresses, not the UDP destination family.

```sh
/etc/init.d/phantun enable
/etc/init.d/phantun restart
# After editing /etc/config/phantun:
/etc/init.d/phantun reload
logread -e phantun
# Stop every instance:
/etc/init.d/phantun stop
```

procd handles respawning, stopping, and reconciling instances on reload, including
removed/disabled sections. UCI changes applied through OpenWrt's service reload
mechanism trigger a reload too. The processes run as root to create TUN devices.

## Firewall and forwarding

Configure forwarding and NAT before using the service. It creates TUN devices
but does not change the router's firewall. OpenWrt's normal WAN masquerading can
serve the clients, provided forwarding from their TUN devices to WAN is allowed.
For the example above, add the following to `/etc/config/firewall` (adapt the
existing WAN zone name if necessary):

```uci
config zone
        option name 'phantun'
        list device 'phantunc1'
        list device 'phantunc2'
        list device 'phantuns1'
        option input 'REJECT'
        option output 'ACCEPT'
        option forward 'REJECT'

config forwarding
        option src 'phantun'
        option dest 'wan'

config redirect
        option name 'Phantun incoming'
        option family 'ipv4'
        option src 'wan'
        option src_dport '4567'
        option proto 'tcp'
        option dest 'phantun'
        option dest_ip '192.168.201.2'
        option dest_port '4567'
        option target 'DNAT'
```

Ensure the existing WAN zone has `option masq '1'`, and IPv4 forwarding is
enabled (`net.ipv4.ip_forward=1`). Apply with `/etc/init.d/firewall reload`.
Each additional server needs a redirect to its own `tun_peer` and listening
port. The redirect forwards TCP to the TUN peer; opening a TCP input port alone
is insufficient. If exposing a client's UDP listener to LAN, also allow that
UDP port in the router's input policy as appropriate.

The example is IPv4-only. IPv6 needs forwarding, IPv6 forwarding policies, and
corresponding IPv6 NAT rules for the chosen TUN addresses; see the
[main networking instructions](../README.md#2-add-required-firewall-rules).

## Building

The unified build workflow builds each musl target once, saves the binaries as workflow
artifacts, and calls the OpenWrt packaging workflow after the Linux builds finish.
Both SDK versions consume the same binaries; the SDK only assembles packages,
without building Rust or recompiling Phantun.

| OpenWrt architecture | Reused Rust musl target |
| --- | --- |
| `x86_64` | `x86_64-unknown-linux-musl` |
| `aarch64_generic` | `aarch64-unknown-linux-musl` |
| `arm_cortex-a15_neon-vfpv4` | `armv7-unknown-linux-musleabihf` |
| `mips_24kc` | `mips-unknown-linux-musl` (nightly) |
| `mipsel_24kc` | `mipsel-unknown-linux-musl` (nightly) |

Every branch push, `v*.*.*` tag push, pull request, and manual **Build and release**
run produces the same
Linux/macOS release ZIPs and OpenWrt IPK/APK workflow artifacts. The same workflow
also runs the Rust checks and host-side OpenWrt tests. The Docker build runs
only on `v*.*.*` tag pushes. Only `v*.*.*` tag
pushes publish those artifacts to a GitHub Release, after all jobs succeed.
Branch pushes, PRs, and manual runs upload workflow artifacts only. Package
filenames include the SDK release to distinguish IPK and APK builds.

For local packaging, extract the corresponding musl release ZIP into
`openwrt/phantun/prebuilt/`, then stage metadata from that same source revision:

```sh
cp LICENSE-MIT LICENSE-APACHE openwrt/phantun/prebuilt/
python3 - <<'PYTHON'
import pathlib
import tomllib
version = tomllib.loads(pathlib.Path('phantun/Cargo.toml').read_text())['package']['version']
pathlib.Path('openwrt/phantun/prebuilt/version').write_text(version + '\n')
PYTHON
```

Add `src-link phantun /absolute/path/to/phantun/openwrt` to the matching SDK's
`feeds.conf` alongside its default feeds, then run in the SDK:

```sh
./scripts/feeds update -a
./scripts/feeds install -p phantun phantun
make defconfig
make package/phantun/compile V=s
```

Package version comes from the staged `prebuilt/version` file. Outputs are under
`bin/packages/`. Use the SDK architecture matching the selected musl binary;
static linking removes shared-library dependencies, not CPU/ABI requirements.

Run host-side service tests with:

```sh
python3 -m unittest discover -s openwrt/tests -v
```

These tests mock UCI/procd to check command construction and instance isolation.
Actual packet forwarding, firewall integration, and service lifecycle should
also be checked on an OpenWrt device.
