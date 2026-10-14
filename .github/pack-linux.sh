#!/bin/sh
# Packs a built Linux aether as a .deb and an .rpm.
# Usage: pack-linux.sh BINARY DEB_ARCH RPM_ARCH   (e.g. amd64 x86_64, arm64 aarch64)
set -eu

bin=$1
deb_arch=$2
rpm_arch=$3
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
summary="The whole machine as numbers"
about="CPU, memory, GPUs, sensors, disks, network and every process, in one terminal view or as JSON."
work=$(mktemp -d)

# .deb
d="$work/deb"
mkdir -p "$d/DEBIAN" "$d/usr/bin" "$d/usr/share/doc/aether"
install -m 755 "$bin" "$d/usr/bin/aether"
install -m 644 README.md "$d/usr/share/doc/aether/README.md"
install -m 644 LICENSE "$d/usr/share/doc/aether/copyright"
cat > "$d/DEBIAN/control" <<EOF
Package: aether
Version: $version
Architecture: $deb_arch
Maintainer: Tarik Ramadani <tarik.ramadani16@gmail.com>
Section: utils
Priority: optional
Homepage: https://github.com/tarikee/aether
Description: $summary
 $about
EOF
dpkg-deb --build --root-owner-group "$d" "aether_${version}_${deb_arch}.deb" >/dev/null

# .rpm
r="$work/rpm"
mkdir -p "$r/SOURCES" "$r/SPECS"
cp "$bin" "$r/SOURCES/aether"
cp README.md LICENSE "$r/SOURCES/"
cat > "$r/SPECS/aether.spec" <<EOF
%global debug_package %{nil}
%global __os_install_post %{nil}
%define _build_id_links none
%global source_date_epoch_from_changelog 0
Name: aether
Version: $version
Release: 1
Summary: $summary
License: GPL-3.0-only
URL: https://github.com/tarikee/aether

%description
$about

%install
install -Dm755 %{_sourcedir}/aether %{buildroot}/usr/bin/aether
install -Dm644 %{_sourcedir}/README.md %{buildroot}/usr/share/doc/aether/README.md
install -Dm644 %{_sourcedir}/LICENSE %{buildroot}/usr/share/licenses/aether/LICENSE

%files
/usr/bin/aether
%doc /usr/share/doc/aether/README.md
%license /usr/share/licenses/aether/LICENSE
EOF
rpmbuild -bb --quiet --define "_topdir $r" --target "$rpm_arch" "$r/SPECS/aether.spec"
cp "$r/RPMS/$rpm_arch/aether-$version-1.$rpm_arch.rpm" .

rm -rf "$work"
