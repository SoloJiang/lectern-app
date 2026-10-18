#!/usr/bin/env bash
# Dependency-source shapes for docs/research/large-files-rendering.md.
# Usage: bash docs/research/large-files-rendering/deps.sh <work-dir> [cargo-registry-src]
# Needs network access to the public npm, Go module proxy, PyPI and Maven
# Central. Installs run with lifecycle scripts disabled; downloaded files are
# only read (stat, hash, declaration closures), never executed or built.
# Run run.sh first: it builds the bench binary used for the hash timings.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
work="$(mkdir -p "$1" && cd "$1" && pwd)"
bin="$work/cargo-target/release/large-files-rendering-bench"
out="$work/results"
deps="$work/deps"
mkdir -p "$out" "$deps"

vite_manifest='{
  "name": "fixture-vite-app", "private": true, "type": "module",
  "dependencies": {
    "react": "^19.1.0", "react-dom": "^19.1.0", "react-router-dom": "^7.9.0",
    "@tanstack/react-query": "^5.90.0", "axios": "^1.12.0", "zod": "^4.1.0",
    "lodash": "^4.17.21", "dayjs": "^1.11.18", "zustand": "^5.0.8"
  },
  "devDependencies": {
    "typescript": "~5.9.3", "vite": "^7.1.0", "@vitejs/plugin-react": "^5.0.0",
    "@types/react": "^19.1.0", "@types/react-dom": "^19.1.0", "@types/lodash": "^4.17.20",
    "@types/node": "^22.18.0", "eslint": "^9.36.0", "typescript-eslint": "^8.44.0",
    "vitest": "^3.2.0", "tailwindcss": "^4.1.0", "prettier": "^3.6.0"
  }
}'
next_manifest='{
  "name": "fixture-next-app", "private": true,
  "dependencies": { "next": "^15.5.0", "react": "^19.1.0", "react-dom": "^19.1.0" },
  "devDependencies": { "typescript": "~5.9.3", "@types/react": "^19.1.0", "@types/node": "^22.18.0" }
}'

install_npm() { # dir manifest
    mkdir -p "$1"
    printf '%s\n' "$2" > "$1/package.json"
    [[ -d "$1/node_modules" ]] || (cd "$1" && npm install --ignore-scripts --no-audit --no-fund --loglevel=error)
}
install_pnpm() { # dir manifest
    mkdir -p "$1"
    printf '%s\n' "$2" > "$1/package.json"
    [[ -d "$1/node_modules" ]] || (cd "$1" && pnpm install --ignore-scripts --store-dir "$deps/pnpm-store" --reporter=silent)
}

install_npm "$deps/npm-vite" "$vite_manifest"
install_pnpm "$deps/pnpm-vite" "$vite_manifest"
install_npm "$deps/npm-next" "$next_manifest"

{
    for p in npm-vite pnpm-vite npm-next; do
        python3 -I "$here/deps_stats.py" "$deps/$p"
        echo
        # Twice: the second run has a warm page cache.
        "$bin" hash "$deps/$p/node_modules" > /dev/null
        "$bin" hash "$deps/$p/node_modules"
        echo
    done
} | tee "$out/deps_npm.txt"

# Go: a module that imports two common libraries; `go mod tidy` downloads the
# module closure into a private module cache.
gomod="$deps/go-app"
mkdir -p "$gomod"
cat > "$gomod/go.mod" <<'EOF'
module example.invalid/fixture

go 1.24
EOF
cat > "$gomod/main.go" <<'EOF'
package main

import (
	_ "github.com/gin-gonic/gin"
	_ "google.golang.org/grpc"
	_ "k8s.io/client-go/kubernetes"
)

func main() {}
EOF
[[ -d "$deps/gomodcache" ]] || (cd "$gomod" && GOMODCACHE="$deps/gomodcache" GOFLAGS=-modcacherw go mod tidy)

# Python: wheels only, no build step, into a private venv.
if [[ ! -d "$deps/venv" ]]; then
    python3 -m venv "$deps/venv"
    "$deps/venv/bin/pip" install --quiet --only-binary=:all: --no-compile django requests fastapi pydantic sqlalchemy
fi
site="$(echo "$deps"/venv/lib/python3*/site-packages)"

# Maven: one -sources.jar, as context for #26.
jar="$deps/spring-core-6.2.11-sources.jar"
[[ -f "$jar" ]] || curl -sSfL -o "$jar" https://repo1.maven.org/maven2/org/springframework/spring-core/6.2.11/spring-core-6.2.11-sources.jar

{
    python3 -I "$here/deps_stats.py" --tree "$deps/gomodcache"
    "$bin" hash "$deps/gomodcache" > /dev/null
    "$bin" hash "$deps/gomodcache"
    echo
    python3 -I "$here/deps_stats.py" --tree "$site"
    "$bin" hash "$site" > /dev/null
    "$bin" hash "$site"
    echo
    if [[ -n "${2:-}" ]]; then
        python3 -I "$here/deps_stats.py" --tree "$2"
        "$bin" hash "$2" > /dev/null
        "$bin" hash "$2"
        echo
    fi
    echo "spring-core sources jar: $(stat -c %s "$jar") bytes, $(python3 -I -c 'import sys, zipfile; z = zipfile.ZipFile(sys.argv[1]); print(len(z.infolist()), "entries,", sum(i.file_size for i in z.infolist()), "bytes uncompressed")' "$jar")"
} | tee "$out/deps_other.txt"
