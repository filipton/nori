#!/usr/bin/env bash
# The local Jam relay and browser invite fixture, from a neighbouring octo-fiesta checkout.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
if [ "${1:-}" != --leased ]; then
  exec python3 "$here/with-resource.py" build,jam-server bash "$0" --leased
fi
if curl -fsS -m 2 http://localhost:5274/nori/jam | grep -q 'Open in nori'; then exit 0; fi
if curl -s -m 2 http://localhost:5274/ >/dev/null; then
  echo "port 5274 serves an outdated relay; update that container before testing Jam" >&2
  exit 1
fi
common=$(git -C "$here" rev-parse --path-format=absolute --git-common-dir)
source_dir="${NORI_OCTO_ROOT:-$(dirname "$(dirname "$common")")/octo-fiesta}"
[ -f "$source_dir/Dockerfile" ] || { echo "set NORI_OCTO_ROOT to an octo-fiesta checkout" >&2; exit 1; }
"$here/dev-server.sh" >/dev/null
docker build -t nori-e2e-jam "$source_dir"
docker rm nori-e2e-jam >/dev/null 2>&1 || true
docker run -d --name nori-e2e-jam -p 5274:8080 \
  -e Subsonic__Url=http://host.docker.internal:4533 \
  -e Library__DownloadPath=/app/downloads nori-e2e-jam >/dev/null
deadline=$((SECONDS + 60))
until curl -fsS -m 2 http://localhost:5274/nori/jam 2>/dev/null | grep -q 'Open in nori'; do
  [ "$SECONDS" -lt "$deadline" ] || { docker logs nori-e2e-jam >&2; exit 1; }
  sleep 0.3
done
