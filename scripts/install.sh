#!/usr/bin/env bash
# Ferry installer: downloads and installs pre-compiled release binaries.
set -euo pipefail

REPO="nafisX/ferry"
VERSION="${FERRY_VERSION:-latest}"

# 1. Detect OS
OS_RAW="$(uname -s)"
case "$OS_RAW" in
    Darwin)
        OS="apple-darwin"
        ;;
    Linux)
        OS="unknown-linux-gnu"
        ;;
    *)
        echo "Error: Unsupported operating system: $OS_RAW" >&2
        exit 1
        ;;
esac

# 2. Detect Architecture
ARCH_RAW="$(uname -m)"
case "$ARCH_RAW" in
    x86_64|amd64)
        ARCH="x86_64"
        ;;
    arm64|aarch64)
        ARCH="aarch64"
        ;;
    *)
        echo "Error: Unsupported architecture: $ARCH_RAW" >&2
        exit 1
        ;;
esac

TARGET="${ARCH}-${OS}"
ARTIFACT="ferry-${TARGET}"

# 3. Determine release version
if [ "$VERSION" = "latest" ]; then
    RELEASE_URL="https://api.github.com/repos/${REPO}/releases/latest"
    TAG="$(curl -sSL -H "Accept: application/vnd.github.v3+json" "$RELEASE_URL" | grep '"tag_name":' | head -1 | cut -d'"' -f4 || true)"
    if [ -z "$TAG" ]; then
        TAG="v0.1.0"
    fi
else
    TAG="$VERSION"
fi

echo "Installing Ferry ${TAG} for ${TARGET}..."

# 4. Download archive and checksum
TMPDIR="$(mktemp -d)"
cleanup() {
    rm -rf "$TMPDIR"
}
trap cleanup EXIT INT TERM

TAR_FILE="${ARTIFACT}.tar.gz"
SHA_FILE="${ARTIFACT}.tar.gz.sha256"
DOWNLOAD_URL="https://github.com/${REPO}/releases/download/${TAG}/${TAR_FILE}"
CHECKSUM_URL="https://github.com/${REPO}/releases/download/${TAG}/${SHA_FILE}"

echo "Downloading ${DOWNLOAD_URL}..."
if ! curl -sSLf -o "${TMPDIR}/${TAR_FILE}" "$DOWNLOAD_URL"; then
    echo "Error: Failed to download release archive from $DOWNLOAD_URL" >&2
    exit 1
fi

if curl -sSLf -o "${TMPDIR}/${SHA_FILE}" "$CHECKSUM_URL"; then
    echo "Verifying checksum..."
    (
        cd "$TMPDIR"
        if command -v sha256sum >/dev/null 2>&1; then
            sha256sum -c "$SHA_FILE"
        elif command -v shasum >/dev/null 2>&1; then
            shasum -a 256 -c "$SHA_FILE"
        fi
    )
fi

# 5. Extract
echo "Extracting binary..."
tar -xzf "${TMPDIR}/${TAR_FILE}" -C "$TMPDIR"

# 6. Locate extracted binary
EXTRACTED_BIN=""
for cand in "${TMPDIR}/${ARTIFACT}/ferry" "${TMPDIR}/ferry"; do
    if [ -f "$cand" ]; then
        EXTRACTED_BIN="$cand"
        break
    fi
done

if [ -z "$EXTRACTED_BIN" ]; then
    echo "Error: Binary 'ferry' not found in extracted archive" >&2
    exit 1
fi

# 7. Select destination
DEST_DIR=""
if [ -d "$HOME/.cargo/bin" ] && [ -w "$HOME/.cargo/bin" ]; then
    DEST_DIR="$HOME/.cargo/bin"
elif [ -w "/usr/local/bin" ]; then
    DEST_DIR="/usr/local/bin"
elif [ -d "$HOME/.local/bin" ] && [ -w "$HOME/.local/bin" ]; then
    DEST_DIR="$HOME/.local/bin"
else
    DEST_DIR="$HOME/.cargo/bin"
    mkdir -p "$DEST_DIR"
fi

DEST_BIN="${DEST_DIR}/ferry"
mv "$EXTRACTED_BIN" "$DEST_BIN"
chmod +x "$DEST_BIN"

echo "Successfully installed Ferry to ${DEST_BIN}"

# 8. Check PATH
case ":$PATH:" in
    *":$DEST_DIR:"*) ;;
    *)
        echo "Note: ${DEST_DIR} is not in your PATH."
        echo "Add it by running:"
        echo "  export PATH=\"\$PATH:${DEST_DIR}\""
        ;;
esac

echo "Run 'ferry --help' to get started."
