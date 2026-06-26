# glassy.spec — official-Fedora / Copr build recipe.
#
# This is the "proper" rpmbuild path, for Copr or a Fedora package review. The
# DEFAULT working dnf repo (https://alliecatowo.github.io/glassy/rpm/) does NOT
# use this spec — it republishes the binary .rpm that cargo-generate-rpm builds
# in CI (see [package.metadata.generate-rpm] in Cargo.toml). Use this spec when
# you want a from-source build managed by rpmbuild/Copr instead.
#
# Build locally:
#   spectool -g -R packaging/rpm/glassy.spec      # fetch the source tarball
#   rpmbuild -ba packaging/rpm/glassy.spec
#
# Copr:
#   copr-cli create glassy --chroot fedora-rawhide-x86_64
#   copr-cli build glassy packaging/rpm/glassy.spec
#   # end users: sudo dnf copr enable alliecatowo/glassy && sudo dnf install glassy

%global crate glassy

Name:           glassy
Version:        0.2.0
Release:        1%{?dist}
Summary:        Fast, minimal GPU-accelerated terminal emulator written in Rust

License:        MIT
URL:            https://github.com/alliecatowo/glassy
# Uses the release-uploaded source tarball asset (matches the Homebrew formula).
Source0:        %{url}/releases/download/v%{version}/%{crate}-%{version}-src.tar.gz

BuildRequires:  cargo
BuildRequires:  rust
BuildRequires:  pkgconfig
BuildRequires:  gcc
BuildRequires:  pkgconfig(libxkbcommon)
BuildRequires:  pkgconfig(wayland-client)
BuildRequires:  pkgconfig(vulkan)
BuildRequires:  pkgconfig(fontconfig)
BuildRequires:  pkgconfig(dbus-1)

# Runtime shared libraries glassy links against (rpm auto-detects most, pinned
# here for clarity / clean-chroot installs).
Requires:       libxkbcommon
Requires:       wayland-libs-client
Requires:       vulkan-loader
Requires:       fontconfig

%description
glassy is a small, fast, GPU-accelerated terminal emulator written in Rust.
It draws the grid with an instanced wgpu renderer fed by a dynamic glyph atlas,
and supports 24-bit truecolor, color emoji, procedural box-drawing, mouse
reporting, selection, clipboard, scrollback, OSC 8 hyperlinks, tabs, window
decorations, configurable cursor, translucency, and themeable config.

%prep
%autosetup -n %{crate}-%{version}

%build
# Honor the lean release profile from Cargo.toml (fat LTO, strip, panic=abort).
cargo build --release --locked

%install
install -Dm0755 target/release/glassy %{buildroot}%{_bindir}/glassy
install -Dm0644 extra/glassy.desktop  %{buildroot}%{_datadir}/applications/glassy.desktop
install -Dm0644 extra/glassy.1        %{buildroot}%{_mandir}/man1/glassy.1
for sz in 16 32 48 64 128 256 512; do
  install -Dm0644 assets/icons/glassy-${sz}.png \
    %{buildroot}%{_datadir}/icons/hicolor/${sz}x${sz}/apps/glassy.png
done
install -Dm0644 LICENSE %{buildroot}%{_licensedir}/glassy/LICENSE

%files
%license LICENSE
%{_bindir}/glassy
%{_datadir}/applications/glassy.desktop
%{_mandir}/man1/glassy.1*
%{_datadir}/icons/hicolor/*/apps/glassy.png

%changelog
* Wed Jun 25 2026 Allie <allisonemilycoleman@gmail.com> - 0.2.0-1
- Initial spec for the official-Fedora / Copr build path.
