#!/bin/sh
# Instala dockyard desde las releases de GitHub, en cualquier distribución Linux o en macOS.
#
#   curl -fsSL https://raw.githubusercontent.com/Angelfonseca/dockyard/main/install.sh | sh
#
# Variables opcionales:
#   DOCKYARD_VERSION   etiqueta a instalar (p. ej. v0.2.0). Por defecto, la última release.
#   PREFIX             instala en $PREFIX/bin (por defecto /usr/local si se puede escribir; si no, ~/.local).
#   DOCKYARD_RELEASE_DIR  carpeta local con los .tar.gz y sus .sha256 (en vez de descargar; para pruebas).
#
# Verifica la suma SHA-256 antes de instalar. No pide permisos de administrador: si no puede escribir en
# /usr/local/bin, instala en ~/.local/bin y te dice cómo añadirlo al PATH.
set -eu

REPO="Angelfonseca/dockyard"

die() { echo "error: $*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "falta «$1»; instálalo y vuelve a intentarlo"; }

need uname
need tar
sha_check() { # $1 = archivo .sha256 (en la misma carpeta que el tarball)
    if command -v sha256sum >/dev/null 2>&1; then sha256sum -c "$1" >/dev/null 2>&1
    elif command -v shasum >/dev/null 2>&1; then shasum -a 256 -c "$1" >/dev/null 2>&1
    else die "no hay sha256sum ni shasum para verificar la descarga"; fi
}

# Plataforma → objetivo de la release. Linux usa musl (binario estático: corre en glibc y en Alpine).
os="$(uname -s)"
arch="$(uname -m)"
case "$os:$arch" in
    Linux:x86_64 | Linux:amd64) target="x86_64-unknown-linux-musl" ;;
    Linux:aarch64 | Linux:arm64) target="aarch64-unknown-linux-gnu" ;;
    Darwin:arm64) target="aarch64-apple-darwin" ;;
    Darwin:x86_64) target="x86_64-apple-darwin" ;;
    *) die "plataforma no soportada: $os $arch (compila desde el código: cargo build --release)" ;;
esac

# Etiqueta de la release.
if [ -n "${DOCKYARD_RELEASE_DIR:-}" ]; then
    tag="${DOCKYARD_VERSION:-}"
    [ -n "$tag" ] || tag="$(ls "$DOCKYARD_RELEASE_DIR" | sed -n "s/^dockyard-\(.*\)-$target\.tar\.gz$/\1/p" | head -1)"
    [ -n "$tag" ] || die "no hay un dockyard-*-$target.tar.gz en $DOCKYARD_RELEASE_DIR"
    src="$DOCKYARD_RELEASE_DIR"
else
    need curl
    tag="${DOCKYARD_VERSION:-}"
    if [ -z "$tag" ]; then
        tag="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -1)"
        [ -n "$tag" ] || die "no se pudo saber la última versión (¿sin red?). Fija DOCKYARD_VERSION."
    fi
    src="$(mktemp -d)"
    name="dockyard-$tag-$target.tar.gz"
    base="https://github.com/$REPO/releases/download/$tag"
    curl -fsSL -o "$src/$name" "$base/$name" || die "no se pudo descargar $base/$name"
    curl -fsSL -o "$src/$name.sha256" "$base/$name.sha256" || die "no se pudo descargar la suma de $name"
fi

archive="$src/dockyard-$tag-$target.tar.gz"
[ -f "$archive" ] || die "no existe $archive"
[ -f "$archive.sha256" ] || die "falta la suma SHA-256 de $archive"
(cd "$src" && sha_check "$(basename "$archive.sha256")") || die "la suma SHA-256 no coincide: descarga corrupta o alterada"

work="$(mktemp -d)"
entry="$(tar -tzf "$archive" | sed -n '/\/dockyard$/p' | head -1)"
[ -n "$entry" ] || die "el archivo no trae el binario dockyard"
tar -xzf "$archive" -C "$work" "$entry"
bin="$work/$entry"

# Destino: PREFIX/bin si se puede escribir; si no, ~/.local/bin (sin sudo).
if [ -n "${PREFIX:-}" ]; then dest="$PREFIX/bin"
elif [ -w /usr/local/bin ] 2>/dev/null; then dest="/usr/local/bin"
else dest="$HOME/.local/bin"; fi
mkdir -p "$dest"
install -m 0755 "$bin" "$dest/dockyard" 2>/dev/null || cp "$bin" "$dest/dockyard" && chmod 0755 "$dest/dockyard"

echo "✔ dockyard $tag instalado en $dest/dockyard"
case ":$PATH:" in
    *":$dest:"*) ;;
    *) echo "  Añade $dest a tu PATH:  export PATH=\"$dest:\$PATH\"" ;;
esac
echo "  Comprueba el entorno con:  dockyard doctor"
