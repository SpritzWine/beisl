# NAME

beisl-compat - GUI: review the compat walls beisl found, and the games gamebus could not name

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

A second tab, Identity misses, lists the launches gamebus saw without an
authoritative identity, and lets you dismiss, restore, promote or correct them.

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

# IDENTITY MISSES

The second tab is gamebus's own review of the launches it could not name, the
same list `gamebus-setup` shows in its terminal UI. It exists whether or not a
game ever hit a wall, which is why beisl-setup's gamebus row opens this window
too.

A miss can be dismissed (parked, not deleted; a switch brings the parked ones
back into view), restored, promoted into the umu-database pipeline when it was
a umu launch, and taken back out. The details pane, opened from the header,
holds the two corrections that need typing or choosing: the game's title, and
the store it came from. Which stores are offered, and whether choosing one
clears an earlier override, is gamebus's answer; the window keeps no copy of
that rule.

Unlike the findings, this tab does not run one command per action. It starts
`gamebus-setup mcp --allow-edits` once and keeps that child for as long as the
window is open, speaking MCP over its stdio: no port, no socket, no network.
When gamebus refuses something it gives a reason, and the window shows the one
remedy that reason has, such as running `gamebus-setup auth init` when gamebus
has no owner key yet.

gamebus attributes every change to the client that made it, by an Ed25519 key.
The window creates its own on first use. Reading needs no key; the first edit
enrols it in gamebus's ledger for good, so someone who only looks is never
registered. The key says which program made a change. It is readable by
anything running as you and proves nothing about who was at the keyboard.

The window also remembers the head of gamebus's ledger and asks, on the next
connection, whether that head is still part of the chain. A ledger that was
reset, and ordinary edits made elsewhere, are not reported. Two things are: a
head that is gone from its own chain, because that means history was
rewritten, and a chain gamebus itself cannot verify, in which case it refuses
its own edits until its passphrase is initialised again.

# REQUIREMENTS

`gamebus-setup` (ships with gamebus-presenced) and `beisl-mcp` (ships with
beisl) on PATH or in ~/.local/bin. The findings need gamebus-presenced 0.4.0
or newer for `compat --json`: an older gamebus-setup ignores the flag and
prints its human list, and the window says so rather than showing an empty
stash. The Identity misses tab needs `gamebus-setup mcp`, and was built
against gamebus-presenced 0.5.0.

# FILES

    ~/.local/share/gamebus-presenced/compat-inbox/   where beisl drops a wall
                                                    it saw (gamebus-setup's)
    ~/.config/beisl/gamebus-client.key               this window's key, mode
                                                    0600; never in a trace dir
    ~/.config/beisl/gamebus-ledger.json              the ledger head it last saw

# SEE ALSO

beisl(1), beisl-setup(1), beisl-mcp(1)
