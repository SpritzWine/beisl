#!/usr/bin/env bash
#
# Keep the runner host's build images in step with the branches CI builds.
#
# Why this exists: the build images are LOCAL tags (force_pull is off -- they are never
# pushed to a registry, because a 7.7 GB image re-pushed on every change is a poor way to
# treat a donation-funded forge). A local tag has no upstream, so nothing can pull it and
# nothing notices it is stale. The jobs therefore assert the stamp baked into the image
# against the stamp in the commit, and fail in seconds when they differ.
#
# So this is the fixer: run it on the runner host after pushing a change to either build
# image, and it brings every local tag back in step. It rebuilds ONLY when the stamp actually
# moved -- docker caches every unchanged layer, so a no-op costs seconds.
#
# Deliberately NOT on a timer. Polling Codeberg every few minutes for something that changes
# about twice a year is a poor way to treat a donation-funded forge, and it buys little: a
# stale image is already a FIVE-SECOND failure that prints the exact command to fix it. If
# you want a safety net anyway, a systemd timer around this script is three lines -- just
# make the interval daily, not minutes.
#
# It touches nothing but a work checkout under WORK_DIR and the local image tags.
#
#     ./image-sync.sh                       # both repos
#     WORK_DIR=/tmp/x ./image-sync.sh       # somewhere else
set -euo pipefail

WORK_DIR="${WORK_DIR:-/var/lib/spritzwine-ci}"
FORGE="${FORGE:-https://codeberg.org/SpritzWine}"

# repo | branch | path to the stamp file | path to the rebuild script
REPOS=(
    "SpritzWine|rust|ci/build-env.sha256|ci/rebuild.sh"
    "SpritzWine-Prater|prater|prater/ci/build-env.sha256|prater/ci/rebuild.sh"
)

mkdir -p "$WORK_DIR"
rc=0

for entry in "${REPOS[@]}"; do
    IFS='|' read -r repo branch stamp_file rebuild <<< "$entry"
    dir="$WORK_DIR/$repo"

    echo "══ $repo ($branch)"
    if [ -d "$dir/.git" ]; then
        git -C "$dir" fetch -q origin "$branch"
        git -C "$dir" reset -q --hard "origin/$branch"
    else
        # Shallow: this checkout exists only to read the Dockerfile and its stamp. It is not
        # the tree CI builds in (that one lives in the runner's /cache).
        git clone -q --depth 1 --branch "$branch" "$FORGE/$repo.git" "$dir"
    fi
    echo "   at $(git -C "$dir" log -1 --format='%h %s')"

    want=$(cat "$dir/$stamp_file")
    # The tag each repo's rebuild.sh builds; read it from the script so this list cannot drift.
    tag=$(grep -m1 '^TAG=' "$dir/$rebuild" | cut -d= -f2-)
    have=$(docker run --rm --entrypoint cat "$tag" /etc/spritzwine/build-env.sha256 2>/dev/null || echo "")

    if [ "$want" = "$have" ]; then
        echo "   image up to date ($want)"
        continue
    fi

    echo "   stamp drift -> rebuilding $tag"
    echo "     repo:  $want"
    echo "     image: ${have:-<missing: no image, or one predating stamp-baking>}"
    if ( cd "$dir" && "./$rebuild" ); then
        echo "   ✅ $tag rebuilt"
    else
        echo "   ❌ FAILED to rebuild $tag" >&2
        rc=1
    fi
done

exit "$rc"
