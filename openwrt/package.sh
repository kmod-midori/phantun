#!/usr/bin/env bash
# Stage existing musl binaries and call the official IPK/APK builders.
set -euo pipefail
umask 022

repo=$(cd "$(dirname "$0")/.." && pwd)
templates="$repo/openwrt/templates"
apk_image='alpine:3.23@sha256:85fe1e81d6758c208f3e1eed4338a1997e19d4be002d4dd32d3100c9a8c010a0'
if [[ $# -ne 5 ]]; then
	echo "Usage: $0 TAG ARCH BINARIES OUTPUT IPKG_BUILD" >&2
	exit 1
fi
version=${1#v}
arch=$2
binaries=$(realpath "$3")
mkdir -p "$4"
output=$(realpath "$4")
ipkg_build=$(realpath "$5")

export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-0}

# Translate common prerelease tags to apk version syntax.
version=${version%%+*}
if [[ "$version" =~ ^([0-9]+\.[0-9]+\.[0-9]+)(-(alpha|beta|pre|rc)[.-]?([0-9]+))?$ ]]; then
	version=${BASH_REMATCH[1]}
	if [[ -n "${BASH_REMATCH[3]}" ]]; then
		version+="_${BASH_REMATCH[3]}${BASH_REMATCH[4]}"
	fi
	version+="-r1"
else
	echo "Unsupported package version: $version" >&2
	exit 1
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
root="$work/root"
install -d "$root/usr/bin" "$root/etc/init.d" "$root/etc/config" "$root/usr/share/licenses/phantun"
install -m 755 "$binaries/phantun_client" "$binaries/phantun_server" "$root/usr/bin/"
install -m 755 "$repo/openwrt/phantun/files/phantun.init" "$root/etc/init.d/phantun"
install -m 600 "$repo/openwrt/phantun/files/phantun.config" "$root/etc/config/phantun"
install -m 644 "$repo/LICENSE-MIT" "$repo/LICENSE-APACHE" "$root/usr/share/licenses/phantun/"

build_ipk() {
	local package_root="$work/ipk-root"
	cp -a "$root" "$package_root"
	mkdir "$package_root/CONTROL"
	sed -e "s/@VERSION@/$version/g" -e "s/@ARCH@/$arch/g" \
		"$templates/ipk/control" > "$package_root/CONTROL/control"
	install -m 644 "$templates/conffiles" "$package_root/CONTROL/conffiles"
	install -m 755 "$templates/ipk/postinst" "$templates/ipk/prerm" "$package_root/CONTROL/"
	fakeroot sh "$ipkg_build" "$package_root" "$output"
	mv "$output/phantun_${version}_${arch}.ipk" "$output/openwrt-24.10-phantun_${version}_${arch}.ipk"
}

build_apk() {
	local metadata="$root/lib/apk/packages"
	local scripts="$work/scripts"
	local checksum
	mkdir -p "$metadata" "$scripts"
	# OpenWrt uses this list for init scripts and these config records for upgrades.
	(cd "$root" && find . -type f -printf '/%P\n' | LC_ALL=C sort) > "$work/phantun.list"
	mv "$work/phantun.list" "$metadata/phantun.list"
	install -m 644 "$templates/conffiles" "$metadata/phantun.conffiles"
	checksum=$(sha256sum "$root/etc/config/phantun")
	printf '/etc/config/phantun %s\n' "${checksum%% *}" > "$metadata/phantun.conffiles_static"
	install -m 755 "$templates/apk/"* "$scripts/"
	find "$root" -exec touch -d "@$SOURCE_DATE_EPOCH" {} +
	# Set root ownership inside the container without changing host files.
	docker run --rm --network=none \
		-v "$work:/stage:ro" -v "$output:/out" "$apk_image" \
		sh -ec 'cp -a /stage/root /tmp/root; chown -R 0:0 /tmp/root; exec "$@"' sh \
		apk mkpkg --compression deflate:9 --compat 3.0.0 \
		--info 'name:phantun' \
		--info "version:$version" \
		--info "arch:$arch" \
		--info 'description:UDP to TCP obfuscator with a multi-instance UCI/procd service' \
		--info 'license:MIT OR Apache-2.0' \
		--info 'url:https://github.com/dndx/phantun' \
		--info 'depends:kmod-tun' \
		--files /tmp/root \
		--output "/out/openwrt-25.12-phantun-${version}.${arch}.apk" \
		--script post-install:/stage/scripts/post-install \
		--script post-upgrade:/stage/scripts/post-upgrade \
		--script pre-deinstall:/stage/scripts/pre-deinstall
}

build_ipk
build_apk
