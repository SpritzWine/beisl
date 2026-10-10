#!/bin/sh
# beisl-appimage-shim v1 - written by beisl's setup. Do not edit: rerun setup
# from the AppImage instead, which rewrites this file.
#
# beisl runs from an AppImage. This script is what `@TOOL@` on your PATH is: it
# hands over to that file. The AppImage stays wherever you keep it; setup
# recorded the path below and rechecks it every time it runs.
#
# A missing or doubtful AppImage must never stop a game. If the checks below
# fail for `beisl <game command>`, the reason is printed and the game is started
# untraced.
appimage='@APPIMAGE@'
tool='@TOOL@'
# 1: refuse a file somebody else could have replaced. 0: you told setup to
# keep the AppImage where it is and relax that check.
strict=@STRICT@

say() { echo "beisl: $*" >&2; }

# Group or others may write here, and nothing stops them replacing the entry:
# a sticky directory (like /tmp) does, and so does a group that is only you.
others_can_replace() { # <path> <is-dir>
    m=$(stat -c %a "$1" 2>/dev/null) || return 0
    o=${m#"${m%?}"}
    rest=${m%?}
    g=${rest#"${rest%?}"}
    case "$m" in
        [1357]???) [ "$2" = dir ] && return 1 ;;
    esac
    case "$o" in [2367]) return 0 ;; esac
    case "$g" in
        [2367]) [ "$(stat -c %G "$1" 2>/dev/null)" = "$(id -un)" ] || return 0 ;;
    esac
    return 1
}

why=
if [ ! -f "$appimage" ]; then
    why="the AppImage is no longer at $appimage"
elif [ ! -x "$appimage" ]; then
    why="$appimage is not executable"
else
    # An AppImage is an ELF file with "AI" and the type byte at offset 8. This
    # keeps a same-named script from standing in; it does not prove the file
    # is beisl's.
    case "$(od -An -tx1 -N11 "$appimage" 2>/dev/null | tr -d ' \n')" in
        7f454c46????????414902) ;;
        *) why="$appimage is not an AppImage" ;;
    esac
fi

if [ -z "$why" ] && [ "$strict" = 1 ]; then
    if [ ! -O "$appimage" ]; then
        why="$appimage is not owned by you"
    elif others_can_replace "$appimage" file; then
        why="$appimage can be written by other users"
    else
        d=$(dirname "$appimage")
        while :; do
            if others_can_replace "$d" dir; then
                why="other users can replace files in $d"
                break
            fi
            [ "$d" = / ] && break
            d=$(dirname "$d")
        done
    fi
fi

if [ -z "$why" ]; then
    exec "$appimage" "--beisl-tool=$tool" "$@"
fi

say "$why"
say "open the AppImage once to set this up again, or remove this file: $0"

# Only the wrapper has something better to do than fail: start the game.
if [ "$tool" = beisl ]; then
    [ "$1" = -- ] && shift
    case "${1:-}" in
        '' | -* | install | uninstall | symbols | doctor) ;;
        *)
            say "launching untraced"
            exec "$@"
            ;;
    esac
fi
exit 127
