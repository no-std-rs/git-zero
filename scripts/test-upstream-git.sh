#!/bin/sh
set -eu

git_source=${1:?Usage: sh scripts/test-upstream-git.sh /path/to/built/git-source [jobs]}
jobs=${2:-4}
case "$jobs" in
    ''|*[!0-9]*|0) printf '%s\n' 'jobs must be a positive integer' >&2; exit 2 ;;
esac
git_source=$(cd "$git_source" && pwd)
if [ ! -x "$git_source/git" ] || [ ! -f "$git_source/t/test-lib.sh" ]; then
    printf '%s\n' 'Provide an already built Git source tree containing git and t/test-lib.sh.' >&2
    exit 2
fi
exec make -C "$git_source/t" -j "$jobs" \
    GIT_TEST_HTTPD=true \
    T='t5550-http-fetch-dumb.sh t5551-http-fetch-smart.sh'
