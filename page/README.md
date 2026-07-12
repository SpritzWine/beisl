# beisl — landing page

The public landing page for [Beisl](https://codeberg.org/SpritzWine/beisl).

Plain static files, no build step, no external requests — it works offline
from a `file://` open and is ready for Codeberg Pages as-is.

```
index.html            the page
assets/style.css      styles (light + dark via prefers-color-scheme, with a manual toggle)
assets/site.js        the interactive layer bar, frametime figure, hero trace, theme toggle
assets/*.svg          brand icons (Das Glasl app icon, bare light/dark glasses,
                      symbolic mono variant, Der Deckel coaster mark)
releasenotes.md       verbatim copy of the repo's RELEASENOTES.md (main branch),
                      rendered into the "Release notes" section by site.js
                      (same-origin fetch; under file:// the section keeps its
                      Codeberg fallback link)
.domains              custom-domain config for Codeberg Pages (commented example)
```

## Deploying to Codeberg Pages

This site is the `page/` directory on the **`pages` branch** of
[`SpritzWine/beisl`](https://codeberg.org/SpritzWine/beisl). Codeberg serves a
repo's `pages` branch at `https://<org>.codeberg.page/<repo>/…` — no build step,
no webhook. Pushing the branch republishes it:

```sh
# from a checkout of the beisl repo, on the pages branch
git show main:RELEASENOTES.md > page/releasenotes.md   # refresh at each release
git add page
git commit -m "update landing page"
git push origin pages
```

Published at:

    https://spritzwine.codeberg.page/beisl/page/

Everything is relative-path (`assets/…`), so it works unchanged under that
sub-path — no base-href to adjust. Updates go live on push (allow a few minutes
for the Pages cache).

## Custom domain (later)

When there's a real domain, edit `.domains` (see the comments inside): put the
domain(s) one per line, first line is canonical, and point the domain's DNS at
Codeberg Pages (`CNAME` to `spritzwine.codeberg.page`, or the documented
`ALIAS`/`A` records for an apex domain). Until then the commented file is
inert and the default `*.codeberg.page` address is used.

## TODOs before shipping

- [x] Project links point at `codeberg.org/SpritzWine/beisl` (served at
      `spritzwine.codeberg.page/beisl/` via a `pages` branch).
- [ ] Real screenshots of the session window / results view would beat the
      illustrative figures — swap them in once the v1 GUI lands.
- [ ] Custom domain, if desired (`.domains` + DNS).
