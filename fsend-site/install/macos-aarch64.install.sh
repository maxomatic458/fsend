tmp=$(mktemp -d)
curl -fsSL https://github.com/maxomatic458/fsend/releases/latest/download/fsend-aarch64-apple-darwin.tar.gz | tar xz -C "$tmp" --strip-components=1

sudo install -m 0755 "$tmp/fsend" /usr/local/bin/fsend
rm -rf "$tmp"
fsend version
