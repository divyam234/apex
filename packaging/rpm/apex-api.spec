Name:           apex-api
Version:        0.1.0
Release:        0.1.alpha1%{?dist}
Summary:        Local-first API development client and CLI
License:        Apache-2.0
URL:            https://github.com/example/apex-api
Source0:        %{name}-%{version}.tar.gz
BuildRequires:  cargo
BuildRequires:  rust

%description
ApexAPI is a local-first API development client with human-readable workspace files.

%build
cargo build --release --locked -p apex-cli

%install
install -Dm755 target/release/apex %{buildroot}%{_bindir}/apex

%files
%license LICENSE
%{_bindir}/apex
