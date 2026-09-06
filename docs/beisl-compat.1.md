# NAME

beisl-compat - GUI: review the compat walls beisl found and file them upstream

# SYNOPSIS

    beisl-compat              open the findings window
    beisl-compat --key <key>  open it on one finding (as beisl-setup's
                              Compatibility section does, key `steam:<appid>`)
    beisl-compat --version    print the version
    beisl-compat --help       this help

# DESCRIPTION

A front-end over gamebus-setup's compat stash: scan beisl's traces for compat
walls, read each finding's evidence, draft a ProtonDB report or an
AreWeAntiCheatYet issue, and mark what you filed. Nothing is submitted by the
program; every draft is text for you to review and file yourself.

<!-- help-end -->

On open it runs `gamebus-setup compat --json` once. That call drains gamebus's
inbox on its way, which is how a wall beisl dropped when the game exited
reaches the findings list without you asking for a scan, and it answers with
the findings themselves.

Every action is one of `gamebus-setup compat`'s own subcommands, run for you
and always with `--json`: `--scan` pulls findings from beisl over its MCP
server and records them, `--export protondb|awacy --key <key>` drafts a
submission, and `--reported <key> --target <name>` remembers what you filed.
The window reimplements none of that -- not even which targets a finding still
wants -- so it and `gamebus-setup compat` can never disagree about what a
finding is.

A finding needs a game identity to be filed under. beisl stamps every Steam
launch with its appid at spawn (see beisl(1), ENVIRONMENT), and a wall's
evidence has to reach a log beisl reads: on a default Proton launch the
kernel-driver wall is invisible, because Proton's `WINEDEBUG=-all` suppresses
the line, so turn on `PROTON_LOG` yourself for one run of the game in question.

# REQUIREMENTS

`gamebus-setup` (ships with gamebus-presenced, 0.4.0 or newer for
`compat --json`) and `beisl-mcp` (ships in the beisl bundle) on PATH or in
~/.local/bin. An older gamebus-setup ignores the flag and prints its human
list, and the window says so rather than showing an empty stash.

# FILES

    ~/.local/share/gamebus-presenced/compat-inbox/   where beisl drops a wall
                                                    it saw (gamebus-setup's)

# SEE ALSO

beisl(1), beisl-setup(1), beisl-mcp(1)
